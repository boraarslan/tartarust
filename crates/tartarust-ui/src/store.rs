//! The GPUI store layer: owns the daemon-interaction policy around the one
//! global `Entity<AppState>`. The sync transport lives in [`crate::ipc`];
//! everything here is the *store-coupled* half the old Dioxus `ipc.rs` carried
//! (optimistic fire + resync, offline/online marks, the initial-load overlay).
//!
//! Concurrency model: blocking IPC calls run on GPUI's background executor;
//! store writes hop back through `AsyncApp`. Entity updates from async paths
//! return `Err` only when the app is quitting — ignored deliberately.

use std::time::Duration;

use gpui::{App, AsyncApp, Entity};
use tartarust_ipc::{Request, Response};

use crate::desktop;
use crate::ipc;
use crate::model::AppState;

/// Overlay a live snapshot onto the current state, in place. Writes ONLY the
/// daemon-owned fields; transient view state (layer / selection / switch gate)
/// is untouched, so both the initial load and [`resync`] preserve whatever the
/// user did while requests were in flight (the old `load_state(base)` re-base
/// semantics, without the clone dance).
fn apply_snapshot(s: &mut AppState, snap: ipc::Snapshot) {
    s.offline = false;
    s.unsaved = snap.status.unsaved_changes;
    s.status = Some(snap.status);
    // Already resolved against `ListProfiles`' own `active` name (NOT the
    // status reply's) — the old `load_state_on` semantics.
    s.active_profile = snap.active_profile;
    s.profiles = snap.profiles;
    s.profile = snap.profile;
    if let Some(device) = snap.device {
        s.device = device;
    }
    // Macros arrive as name-only stubs; bodies are fetched on demand via
    // GetMacro, which marks the name loaded. Clearing `macros_loaded` lets the
    // editor tell a not-yet-loaded stub from a genuinely-empty macro.
    if let Some(macros) = snap.macros {
        s.macros = macros;
        s.macros_loaded.clear();
    }
    // Keep the editor selection in range after the overlay.
    if s.selected_macro >= s.macros.len() {
        s.selected_macro = 0;
    }
}

/// One live-load attempt: snapshot on the background executor, overlay on
/// success. `false` = daemon unreachable / died mid-sequence (state untouched).
async fn try_load(store: &Entity<AppState>, cx: &mut AsyncApp) -> bool {
    let snap = cx
        .background_executor()
        .spawn(async move { ipc::load_snapshot() })
        .await;
    match snap {
        Ok(snap) => {
            let _ = store.update(cx, |s, cx| {
                apply_snapshot(s, snap);
                cx.notify();
            });
            true
        }
        Err(_) => false,
    }
}

/// Initial load: probe the daemon once. If startup just autostarted it, it
/// needs a moment to grab the device and open its socket — retry briefly
/// (~12 × 500 ms ≈ 6 s) instead of settling into offline. When nothing was
/// spawned (daemon deliberately absent, e.g. the cage harness) keep the single
/// probe: the offline mock stays, instantly.
pub fn spawn_initial_load(store: Entity<AppState>, cx: &mut App) {
    const SPAWN_RETRIES: u32 = 12;
    const RETRY_DELAY: Duration = Duration::from_millis(500);

    cx.spawn(async move |cx| {
        if try_load(&store, cx).await {
            return;
        }
        if !desktop::daemon_was_spawned() {
            return;
        }
        for _ in 0..SPAWN_RETRIES {
            cx.background_executor().timer(RETRY_DELAY).await;
            if try_load(&store, cx).await {
                return;
            }
        }
    })
    .detach();
}

/// Fire a mutation at the daemon; on a daemon `Response::Error` (an
/// *accepted-then-rejected* mutation) re-pull authoritative state so the
/// caller's optimistic local edit can't linger out of sync. Connection
/// failures (offline) are NOT resynced — the edit stays local and the offline
/// badge already signals the disconnect.
///
/// Dirtying mutations also flip `AppState::unsaved` true optimistically (the
/// daemon applies them live but defers the disk write until `SaveProfile`),
/// then re-pull the authoritative flag once the daemon accepts: the daemon
/// computes unsaved as a live-vs-disk DIFF, so an edit that manually reverts
/// state back to the saved config clears the Save affordance instead of
/// leaving it stuck.
pub fn fire(store: &Entity<AppState>, request: Request, cx: &mut App) {
    let dirtying = ipc::dirties(&request);
    if dirtying {
        store.update(cx, |s, cx| {
            if !s.unsaved {
                s.unsaved = true;
                cx.notify();
            }
        });
    }
    let store = store.clone();
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { ipc::send(&request) })
            .await;
        match result {
            Ok(Response::Error { message }) => {
                tracing::warn!("daemon rejected mutation: {message}; resyncing");
                resync(&store, cx).await;
            }
            Ok(_) => {
                mark_online(&store, cx);
                if dirtying {
                    refresh_unsaved(&store, cx).await;
                }
            }
            Err(e) => mark_offline(&store, &e.to_string(), cx),
        }
    })
    .detach();
}

/// Re-pull authoritative daemon state after a rejected mutation, so the UI
/// can't silently diverge. No-op offline (the load fails and the last-known
/// state stays).
///
/// The load takes several sequential round trips and the UI stays interactive
/// underneath. If the daemon-owned fields moved while the load was in flight,
/// re-run it ONCE (bounded, so a spamming user can't livelock it) so the
/// overlay includes those edits; [`apply_snapshot`] then only writes
/// daemon-owned fields, so current transient view state always survives.
async fn resync(store: &Entity<AppState>, cx: &mut AsyncApp) {
    for attempt in 0..2 {
        let Ok(base) = store.read_with(cx, |s, _| s.clone()) else {
            return;
        };
        let snap = cx
            .background_executor()
            .spawn(async move { ipc::load_snapshot() })
            .await;
        let Ok(snap) = snap else {
            return; // offline: keep the last-known state
        };
        // Did a concurrent local edit land while the load was in flight?
        let raced = store
            .read_with(cx, |s, _| {
                s.profile != base.profile
                    || s.macros != base.macros
                    || s.profiles != base.profiles
                    || s.status != base.status
            })
            .unwrap_or(false);
        if raced && attempt == 0 {
            continue; // reload once more, with the edit now visible daemon-side
        }
        let _ = store.update(cx, |s, cx| {
            apply_snapshot(s, snap);
            cx.notify();
        });
        return;
    }
}

/// Overwrite the optimistic `unsaved` flag with the daemon's authoritative
/// answer (a live-vs-disk config diff). No-op on any failure — the optimistic
/// value stands, and the offline path is already handled by the caller.
///
/// Also mirrors the default/active profile NAMES from the same reply: the
/// mutation this refresh follows may have moved them daemon-side
/// (`RenameProfile` renames a matching default/active, `SetDefaultProfile` is
/// the point), and a stale cached name would pin the DEFAULT badge to the
/// wrong row until an unrelated resync.
async fn refresh_unsaved(store: &Entity<AppState>, cx: &mut AsyncApp) {
    let reply = cx
        .background_executor()
        .spawn(async move { ipc::send(&Request::GetStatus) })
        .await;
    if let Ok(Response::Status(status)) = reply {
        let _ = store.update(cx, |s, cx| {
            s.unsaved = status.unsaved_changes;
            if let Some(st) = s.status.as_mut() {
                st.unsaved_changes = status.unsaved_changes;
                st.default_profile = status.default_profile.clone();
                st.active_profile = status.active_profile.clone();
            }
            cx.notify();
        });
    }
}

/// Commit all pending session changes to disk (the Save affordance). On a
/// successful `Ack` the unsaved flag clears optimistically; a rejection or
/// offline send leaves it set so the user can retry.
pub fn save_profile(store: &Entity<AppState>, name: Option<String>, cx: &mut App) {
    let store = store.clone();
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { ipc::send(&ipc::req_save_profile(name)) })
            .await;
        match result {
            Ok(Response::Ack) => {
                let _ = store.update(cx, |s, cx| {
                    s.unsaved = false;
                    cx.notify();
                });
            }
            Ok(Response::Error { message }) => {
                tracing::warn!("daemon rejected save: {message}");
            }
            Ok(_) => {}
            Err(e) => mark_offline(&store, &e.to_string(), cx),
        }
    })
    .detach();
}

/// Fetch one macro's body on demand and mark it loaded so the editor
/// unblocks. On a daemon rejection (macro deleted between ListMacros and the
/// click) mark it loaded anyway so the editor resolves out of its "Loading…"
/// placeholder and the prefetch stops refetching, instead of hammering a
/// doomed request per store write. A transport failure flips offline (which
/// also gates the prefetch off) and leaves the stub unloaded for retry.
pub fn fetch_macro(store: &Entity<AppState>, name: String, cx: &mut App) {
    let store = store.clone();
    cx.spawn(async move |cx| {
        let req_name = name.clone();
        let resp = cx
            .background_executor()
            .spawn(async move { ipc::get_macro(&req_name) })
            .await;
        match resp {
            Ok(m) => {
                let _ = store.update(cx, |s, cx| {
                    if let Some(slot) = s.macros.iter_mut().find(|x| x.name == name) {
                        slot.steps = m.steps;
                    }
                    s.macros_loaded.insert(name.clone());
                    cx.notify();
                });
            }
            Err(e) if e.reached_daemon() => {
                tracing::warn!("daemon rejected GetMacro: {e}");
                let _ = store.update(cx, |s, cx| {
                    s.macros_loaded.insert(name.clone());
                    cx.notify();
                });
            }
            Err(e) => mark_offline(&store, &e.to_string(), cx),
        }
    })
    .detach();
}

/// Switch the active profile and reload its full body, flipping the picker
/// index immediately (so the offline path still reflects the choice) while
/// `switching` gates bind writes until the body lands.
///
/// Overlapping switches: nothing stops the user firing a second switch while
/// one is in flight, and the two tasks would otherwise share the one
/// `switching` bool — the FIRST to finish would clear it (re-opening the bind
/// gate mid-switch) and could land its stale body over the newer one. Each
/// task therefore captures a fresh generation; only the task whose generation
/// is still current may clear the flag or apply its result, and a superseded
/// task drops its outcome silently (the newer one owns the store).
pub fn switch_profile_and_reload(store: &Entity<AppState>, name: String, cx: &mut App) {
    let mut generation = 0u64;
    store.update(cx, |s, cx| {
        if let Some(idx) = s.profiles.iter().position(|x| x.name == name) {
            s.active_profile = idx;
        }
        s.switching = true;
        s.switch_gen = s.switch_gen.wrapping_add(1);
        generation = s.switch_gen;
        cx.notify();
    });
    let store = store.clone();
    cx.spawn(async move |cx| {
        let switch_name = name.clone();
        let resp = cx
            .background_executor()
            .spawn(async move { ipc::send(&ipc::req_switch_profile(switch_name)) })
            .await;
        match resp {
            // A reachable daemon rejected the switch (e.g. the profile was
            // deleted by another client): the optimistic index flip now points
            // at a stale name, so resync authoritative state to revert it
            // rather than leave the header disagreeing with the daemon.
            Ok(Response::Error { message }) => {
                tracing::warn!("daemon rejected switch: {message}; resyncing");
                let current = store
                    .read_with(cx, |s, _| s.switch_gen)
                    .unwrap_or(generation);
                if current != generation {
                    return; // superseded: the newer switch settles the store
                }
                let _ = store.update(cx, |s, cx| {
                    s.switching = false;
                    cx.notify();
                });
                resync(&store, cx).await;
                return;
            }
            // Offline: keep the optimistic choice (SetBind no-ops while the
            // daemon is absent, so the last-known body can't corrupt).
            Err(e) => {
                mark_offline(&store, &e.to_string(), cx);
                let _ = store.update(cx, |s, cx| {
                    if s.switch_gen == generation {
                        s.switching = false;
                        cx.notify();
                    }
                });
                return;
            }
            Ok(_) => mark_online(&store, cx),
        }
        let body_name = name.clone();
        let profile = match cx
            .background_executor()
            .spawn(async move { ipc::send(&ipc::req_get_profile(body_name)) })
            .await
        {
            Ok(Response::Profile(p)) => Some(p),
            _ => None,
        };
        let _ = store.update(cx, |s, cx| {
            if s.switch_gen != generation {
                return; // superseded mid-flight: drop the stale body silently
            }
            s.switching = false;
            // Reassign the body only on success; offline keeps the last-known
            // body (SetBind is gated + no-ops when the daemon is absent).
            if let Some(p) = profile {
                if let Some(idx) = s.profiles.iter().position(|x| x.name == p.name) {
                    s.active_profile = idx;
                }
                s.profile = p;
            }
            cx.notify();
        });
    })
    .detach();
}

/// Restart the daemon (the statusbar affordance): ask it to exit gracefully
/// (it acks FIRST, then takes the normal grab-release shutdown path), give
/// the socket a moment to close, spawn a fresh daemon detached, and retry the
/// load until it answers. Doubling as the OFFLINE recovery path: with no
/// daemon the shutdown send just fails and the spawn proceeds.
pub fn restart_daemon(store: &Entity<AppState>, cx: &mut App) {
    const SPAWN_RETRIES: u32 = 12;
    const RETRY_DELAY: Duration = Duration::from_millis(500);

    let store = store.clone();
    cx.spawn(async move |cx| {
        let _ = cx
            .background_executor()
            .spawn(async move { ipc::send(&Request::Shutdown) })
            .await;
        // Honest offline while the daemon is down; the load below recovers.
        let _ = store.update(cx, |s, cx| {
            if !s.offline {
                s.offline = true;
                cx.notify();
            }
        });
        cx.background_executor()
            .timer(Duration::from_millis(400))
            .await;
        cx.background_executor()
            .spawn(async { desktop::spawn_daemon_detached() })
            .await;
        for _ in 0..SPAWN_RETRIES {
            cx.background_executor().timer(RETRY_DELAY).await;
            if try_load(&store, cx).await {
                return;
            }
        }
    })
    .detach();
}

/// Discard all pending session changes (the Discard affordance, the inverse
/// of [`save_profile`]). The daemon restores its live config from the
/// last-saved snapshot and re-activates; on `Ack` the UI resyncs wholesale —
/// the restore can move anything (binds, lighting, profiles, macros), so a
/// full authoritative reload is the only honest refresh. A rejection or
/// offline send leaves local state alone (the affordance is hidden offline).
pub fn discard_changes(store: &Entity<AppState>, cx: &mut App) {
    let store = store.clone();
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { ipc::send(&Request::DiscardChanges) })
            .await;
        match result {
            Ok(Response::Ack) => {
                mark_online(&store, cx);
                resync(&store, cx).await;
            }
            Ok(Response::Error { message }) => {
                tracing::warn!("daemon rejected discard: {message}");
            }
            Ok(_) => {}
            Err(e) => mark_offline(&store, &e.to_string(), cx),
        }
    })
    .detach();
}

/// A transport-level failure (connect/write/read) means the daemon is
/// unreachable: log it and flip the store into the honest offline state so the
/// whole UI falls back to last-known rendering. A daemon-sent
/// `Response::Error` is a *reachable* daemon rejecting a mutation, NOT this.
fn mark_offline(store: &Entity<AppState>, err: &str, cx: &mut AsyncApp) {
    tracing::warn!("daemon unreachable: {err}");
    let _ = store.update(cx, |s, cx| {
        if !s.offline {
            s.offline = true;
            cx.notify();
        }
    });
}

/// A completed exchange (any decoded `Response`, even a rejection) proves the
/// daemon is reachable again: clear a stale offline flag so the UI recovers
/// from a transient blip. Guarded so the common already-online path never
/// re-renders.
fn mark_online(store: &Entity<AppState>, cx: &mut AsyncApp) {
    let _ = store.update(cx, |s, cx| {
        if s.offline {
            s.offline = false;
            cx.notify();
        }
    });
}

//! IPC client: talks to the running `tartarust-daemon` over its Unix socket.
//!
//! Wire format mirrors the daemon's `ipc_server` and the `tartarust` CLI: connect
//! to [`tartarust_ipc::default_socket_path`] (overridable via
//! `TARTARUST_SOCKET`), write one newline-terminated JSON [`Request`], read
//! one newline-terminated JSON [`Response`]. One request per connection.
//!
//! We run inside Dioxus desktop's ambient tokio reactor, so these are plain
//! `async fn`s — initial load is driven by `use_resource`, and UI-action
//! mutations are fire-and-forget via `dioxus::spawn`. **Connection failure is
//! never fatal**: the UI keeps its last-known/mock state and shows the offline
//! badge. See [`load_state`] (graceful) and [`send`] (returns `Result`).

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use tartarust_ipc::{AnalogState, Request, Response};

use crate::model::{AppState, Macro};

use tartarust_ipc::socket_path;

/// Connect, send one request, read one response. `Err` on any I/O / decode
/// failure — callers treat that as "daemon offline" and keep prior state.
pub async fn send(request: Request) -> Result<Response, String> {
    send_on(&socket_path(), request).await
}

/// As [`send`], but against an explicit socket path (the testable core).
pub async fn send_on(socket: &std::path::Path, request: Request) -> Result<Response, String> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| format!("connect {}: {e}", socket.display()))?;
    let (read_half, mut write_half) = stream.into_split();

    let mut line = serde_json::to_string(&request).map_err(|e| format!("encode: {e}"))?;
    line.push('\n');
    write_half
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    write_half
        .flush()
        .await
        .map_err(|e| format!("flush: {e}"))?;

    let mut reader = BufReader::new(read_half).lines();
    match reader.next_line().await.map_err(|e| format!("read: {e}"))? {
        Some(resp) => serde_json::from_str(&resp).map_err(|e| format!("decode: {e}")),
        None => Err("daemon closed connection without replying".into()),
    }
}

/// Re-pull authoritative state from the daemon and overlay it onto the store.
/// Used to correct optimistic local edits after the daemon rejects one (so the
/// UI can't silently diverge — review #2). No-op offline (load_state returns
/// None and the last-known state stays).
async fn resync(mut store: dioxus::prelude::Signal<AppState>) {
    use dioxus::prelude::{ReadableExt, WritableExt};
    let base = store.read().clone();
    if let Some(live) = load_state(base).await {
        store.with_mut(|s| *s = live);
    }
}

/// Whether a request changes persisted config and so can leave unsaved session
/// edits. The UI flips `AppState::unsaved` true for these so the Save affordance
/// appears immediately, then corrects it from the daemon's authoritative
/// live-vs-disk diff once the request completes (see [`fire_or_resync`]).
/// `SwitchProfile`/reads/`Save` are excluded.
fn dirties(request: &Request) -> bool {
    use Request::*;
    matches!(
        request,
        CreateProfile { .. }
            | DuplicateProfile { .. }
            | RenameProfile { .. }
            | DeleteProfile { .. }
            | SetProfileLinkedApp { .. }
            | SetDefaultProfile { .. }
            | SetThumbAxisMode { .. }
            | SetBind { .. }
            | SetLighting { .. }
            | SetBrightness { .. }
            | SaveMacro { .. }
            | DeleteMacro { .. }
            | SetAutoSwitch { .. }
            | SetActuation { .. }
            | ImportProfile { .. }
    )
}

/// Like [`fire`], but on a daemon `Response::Error` (an *accepted-then-rejected*
/// mutation) it re-pulls authoritative state so the optimistic local edit can't
/// linger out of sync. Connection failures (offline) are NOT resynced — the
/// edit stays local and the offline badge already signals the disconnect.
///
/// Dirtying mutations also flip `AppState::unsaved` true optimistically (the
/// daemon applies them live but defers the disk write until `SaveProfile`), then
/// re-pull the authoritative flag once the daemon accepts: the daemon computes
/// unsaved as a live-vs-disk DIFF, so an edit that manually reverts state back
/// to the saved config clears the Save affordance instead of leaving it stuck.
pub fn fire_or_resync(mut store: dioxus::prelude::Signal<AppState>, request: Request) {
    let dirtying = dirties(&request);
    if dirtying {
        use dioxus::prelude::WritableExt;
        store.with_mut(|s| s.unsaved = true);
    }
    dioxus::prelude::spawn(async move {
        match send(request).await {
            Ok(Response::Error { message }) => {
                tracing_warn(&format!("daemon rejected mutation: {message}; resyncing"));
                resync(store).await;
            }
            Ok(_) => {
                mark_online(store);
                if dirtying {
                    refresh_unsaved(store).await;
                }
            }
            Err(e) => mark_offline(store, &e),
        }
    });
}

/// Overwrite the optimistic `unsaved` flag with the daemon's authoritative
/// answer (a live-vs-disk config diff). No-op on any failure — the optimistic
/// value stands, and the offline path is already handled by the caller.
async fn refresh_unsaved(mut store: dioxus::prelude::Signal<AppState>) {
    use dioxus::prelude::WritableExt;
    if let Ok(Response::Status(status)) = send(Request::GetStatus).await {
        store.with_mut(|s| {
            s.unsaved = status.unsaved_changes;
            if let Some(st) = s.status.as_mut() {
                st.unsaved_changes = status.unsaved_changes;
            }
        });
    }
}

/// Commit all pending session changes to disk (the Save affordance). On a
/// successful `Ack` the unsaved flag clears optimistically; a rejection or
/// offline send leaves it set so the user can retry. No-op effect offline (the
/// send fails and `unsaved` stays — the Save button is hidden offline anyway).
pub fn save_profile(mut store: dioxus::prelude::Signal<AppState>, name: Option<String>) {
    use dioxus::prelude::WritableExt;
    dioxus::prelude::spawn(async move {
        match send(Request::SaveProfile { name }).await {
            Ok(Response::Ack) => store.with_mut(|s| s.unsaved = false),
            Ok(Response::Error { message }) => {
                tracing_warn(&format!("daemon rejected save: {message}"))
            }
            Err(e) => mark_offline(store, &e),
            Ok(_) => {}
        }
    });
}

/// Minimal stderr log (the UI has no tracing subscriber configured).
fn tracing_warn(msg: &str) {
    eprintln!("[tartarust-ui ipc] {msg}");
}

/// A transport-level failure (connect/write/read) means the daemon is
/// unreachable: log it and flip the store into the honest offline state so the
/// whole UI falls back to last-known rendering (review #1). A daemon-sent
/// `Response::Error` is a *reachable* daemon rejecting a mutation, NOT this.
fn mark_offline(mut store: dioxus::prelude::Signal<AppState>, err: &str) {
    use dioxus::prelude::{ReadableExt, WritableExt};
    tracing_warn(err);
    let already = store.read().offline;
    if !already {
        store.with_mut(|s| s.offline = true);
    }
}

/// A completed exchange (any decoded `Response`, even a rejection) proves the
/// daemon is reachable again: clear a stale offline flag so the UI recovers from
/// a transient blip. Guarded so the common already-online path never re-renders.
fn mark_online(mut store: dioxus::prelude::Signal<AppState>) {
    use dioxus::prelude::{ReadableExt, WritableExt};
    let offline = store.read().offline;
    if offline {
        store.with_mut(|s| s.offline = false);
    }
}

/// Probe the daemon and, if reachable, build a fresh [`AppState`] from live
/// state. On any failure, returns `None` and the caller keeps the mock/offline
/// state — this is the path the cage harness (no daemon) always takes.
///
/// Sequence: `GetStatus` -> `ListProfiles` -> `GetProfile{active}` ->
/// `GetDeviceInfo` -> `ListMacros` (+ best-effort per-macro, currently names
/// only since there is no `GetMacro` request — macros load as name-only stubs).
pub async fn load_state(base: AppState) -> Option<AppState> {
    load_state_on(&socket_path(), base).await
}

/// As [`load_state`], but against an explicit socket path (the testable core).
pub async fn load_state_on(socket: &std::path::Path, mut base: AppState) -> Option<AppState> {
    let send = |req| send_on(socket, req);

    // Status first: this is the liveness gate.
    let status = match send(Request::GetStatus).await {
        Ok(Response::Status(s)) => s,
        _ => return None,
    };

    base.offline = false;
    let active_name = status.active_profile.clone();
    base.unsaved = status.unsaved_changes;
    base.status = Some(status);

    // Profiles list.
    if let Ok(Response::Profiles(list)) = send(Request::ListProfiles).await {
        base.active_profile = list
            .profiles
            .iter()
            .position(|p| p.name == list.active)
            .unwrap_or(0);
        base.profiles = list.profiles;
    }

    // The active profile in full (binds + lighting).
    if let Ok(Response::Profile(p)) = send(Request::GetProfile {
        name: active_name.clone(),
    })
    .await
    {
        base.profile = p;
    }

    // Device info.
    if let Ok(Response::DeviceInfo(info)) = send(Request::GetDeviceInfo).await {
        base.device = info;
    }

    // Macros: ListMacros returns names only. Load them as name-only stubs and
    // clear `macros_loaded` — each body is fetched on demand via GetMacro
    // (`fetch_macro`), which marks the name loaded. This lets the editor tell a
    // not-yet-loaded stub from a genuinely-empty macro (review #5).
    if let Ok(Response::Macros { names }) = send(Request::ListMacros).await {
        base.macros = names
            .into_iter()
            .map(|name| Macro {
                name,
                steps: Vec::new(),
            })
            .collect();
        base.macros_loaded.clear();
    }

    // Keep the editor selection in range after the overlay.
    if base.selected_macro >= base.macros.len() {
        base.selected_macro = 0;
    }

    Some(base)
}

// --- Analog calibration ------------------------------------------------------

/// Poll the live analog state (mode flag + per-key pressures + actuation config).
/// `None` on any failure — the Calibration screen treats that as "offline / not
/// analog" and shows no live bars (the cage harness, with no daemon, always takes
/// this path). Unlike [`load_state`], this is read on a short interval while the
/// Calibration screen is mounted, so it does NOT touch the global store.
pub async fn get_analog_state() -> Option<AnalogState> {
    get_analog_state_on(&socket_path()).await
}

/// As [`get_analog_state`], but against an explicit socket path (testable core).
pub async fn get_analog_state_on(socket: &std::path::Path) -> Option<AnalogState> {
    match send_on(socket, Request::GetAnalogState).await {
        Ok(Response::AnalogState(a)) => Some(a),
        _ => None,
    }
}

/// Update one analog key's actuation settings (fire-and-forget, routed through
/// the same error-resync path as the other setters). Offline, the send fails and
/// the optimistic local slider edit stays; a daemon rejection (e.g. a bad
/// release/actuation ordering) re-pulls `AppState` and the next poll re-syncs the
/// analog config. Caller must uphold the schema invariants: `release_point <
/// actuation_point`, `second_actuation_point` deeper than actuation, and
/// `second_action` set iff `second_actuation_point` is set.
pub fn set_actuation(
    store: dioxus::prelude::Signal<AppState>,
    index: u8,
    actuation_point: u8,
    release_point: u8,
    second_actuation_point: Option<u8>,
    second_action: Option<String>,
) {
    fire_or_resync(
        store,
        Request::SetActuation {
            index,
            actuation_point,
            release_point,
            second_actuation_point,
            second_action,
        },
    );
}

// --- Typed mutation helpers --------------------------------------------------
//
// Each takes the store `Signal` and routes through `fire_or_resync`, so a daemon
// rejection re-pulls authoritative state instead of leaving the optimistic local
// edit diverged (review #2). Offline, the send fails (not an Error) and the edit
// stays local — the offline badge already signals that.

/// Persist one key's bind on a layer of the active profile.
pub fn set_bind(
    store: dioxus::prelude::Signal<AppState>,
    profile: String,
    layer: tartarust_ipc::Layer,
    key: String,
    action: Option<tartarust_ipc::BindAction>,
) {
    fire_or_resync(
        store,
        Request::SetBind {
            profile,
            layer,
            key,
            action,
        },
    );
}

/// Apply lighting to the active profile.
pub fn set_lighting(
    store: dioxus::prelude::Signal<AppState>,
    lighting: tartarust_ipc::LightingState,
) {
    fire_or_resync(
        store,
        Request::SetLighting {
            profile: None,
            lighting,
        },
    );
}

/// Set device brightness immediately (0–255).
pub fn set_brightness(store: dioxus::prelude::Signal<AppState>, level: u8) {
    fire_or_resync(store, Request::SetBrightness { level });
}

/// Make a profile active AND reload its full body into the store.
///
/// One awaited task (not two racing spawns): `SwitchProfile` then
/// `GetProfile{name}`, then atomically reassign `active_profile` (index) +
/// `profile` (binds + lighting) together. This closes the review #1 bug where
/// only the index flipped, leaving Customize/Lighting on the OLD profile and
/// `SetBind` targeting the stale name. The `switching` flag gates bind writes
/// while the reload is in flight (see `commit_bind`). No-op/last-known offline.
pub fn switch_profile_and_reload(mut store: dioxus::prelude::Signal<AppState>, name: String) {
    use dioxus::prelude::WritableExt;
    // Flip the active index immediately so the picker/selection updates (and the
    // offline path, where GetProfile fails, still reflects the choice), but set
    // `switching` to gate bind writes until the body lands.
    store.with_mut(|s| {
        if let Some(idx) = s.profiles.iter().position(|x| x.name == name) {
            s.active_profile = idx;
        }
        s.switching = true;
    });
    dioxus::prelude::spawn(async move {
        match send(Request::SwitchProfile { name: name.clone() }).await {
            // A reachable daemon rejected the switch (e.g. the profile was
            // deleted by another client): the optimistic index flip now points at
            // a stale name, so resync authoritative state to revert it rather than
            // leave the header disagreeing with the daemon + editors (review #2).
            Ok(Response::Error { message }) => {
                tracing_warn(&format!("daemon rejected switch: {message}; resyncing"));
                store.with_mut(|s| s.switching = false);
                resync(store).await;
                return;
            }
            // Offline: keep the optimistic choice (SetBind no-ops while the daemon
            // is absent, so the last-known body can't corrupt).
            Err(e) => {
                mark_offline(store, &e);
                store.with_mut(|s| s.switching = false);
                return;
            }
            Ok(_) => mark_online(store),
        }
        let profile = match send(Request::GetProfile { name: name.clone() }).await {
            Ok(Response::Profile(p)) => Some(p),
            _ => None,
        };
        store.with_mut(|s| {
            s.switching = false;
            // Reassign the body only on success; offline keeps the last-known
            // body (the live #1 corruption can't occur — SetBind is gated +
            // no-ops when the daemon is absent).
            if let Some(p) = profile {
                if let Some(idx) = s.profiles.iter().position(|x| x.name == p.name) {
                    s.active_profile = idx;
                }
                s.profile = p;
            }
        });
    });
}

/// Toggle per-application auto-switch (a global/per-device setting).
pub fn set_auto_switch(store: dioxus::prelude::Signal<AppState>, enabled: bool) {
    fire_or_resync(store, Request::SetAutoSwitch { enabled });
}

/// Set (or clear) the application a profile auto-activates for (M4). `None`
/// clears the link.
pub fn set_profile_linked_app(
    store: dioxus::prelude::Signal<AppState>,
    name: String,
    linked_app: Option<String>,
) {
    fire_or_resync(store, Request::SetProfileLinkedApp { name, linked_app });
}

/// Set how a profile's thumb stick quantizes direction (eight/four-way).
pub fn set_thumb_axis_mode(
    store: dioxus::prelude::Signal<AppState>,
    profile: String,
    mode: tartarust_ipc::ThumbAxisMode,
) {
    fire_or_resync(store, Request::SetThumbAxisMode { profile, mode });
}

/// Set the profile that is active by default (when no app match applies).
pub fn set_default_profile(store: dioxus::prelude::Signal<AppState>, name: String) {
    fire_or_resync(store, Request::SetDefaultProfile { name });
}

pub fn create_profile(store: dioxus::prelude::Signal<AppState>, name: String) {
    fire_or_resync(store, Request::CreateProfile { name });
}

/// Deep-copy a profile (binds, lighting, thumb mode — not the app link) under a
/// new name.
pub fn duplicate_profile(store: dioxus::prelude::Signal<AppState>, from: String, to: String) {
    fire_or_resync(store, Request::DuplicateProfile { from, to });
}

pub fn rename_profile(store: dioxus::prelude::Signal<AppState>, from: String, to: String) {
    fire_or_resync(store, Request::RenameProfile { from, to });
}

pub fn delete_profile(store: dioxus::prelude::Signal<AppState>, name: String) {
    fire_or_resync(store, Request::DeleteProfile { name });
}

/// Fetch a profile's portable export bundle (profile + referenced macros) from
/// the daemon. `Err` carries a display-ready message (daemon rejection or
/// transport failure — the latter also flips the offline badge).
pub async fn export_profile_bundle(
    store: dioxus::prelude::Signal<AppState>,
    name: String,
) -> Result<tartarust_ipc::ProfileExport, String> {
    match send(Request::ExportProfile { name }).await {
        Ok(Response::ProfileExport(bundle)) => {
            mark_online(store);
            Ok(bundle)
        }
        Ok(Response::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected daemon reply: {other:?}")),
        Err(e) => {
            mark_offline(store, &e);
            Err("daemon unreachable".into())
        }
    }
}

/// Import profile entries (from an export bundle or a whole config file) into
/// the daemon, sequentially; stops at the first rejection. Ends with a full
/// [`resync`] either way, so the profile list, macros, and the unsaved flag are
/// authoritative — like every other edit, an import is session-live until the
/// user presses Save. Returns how many profiles were imported.
pub async fn import_profile_entries(
    mut store: dioxus::prelude::Signal<AppState>,
    entries: Vec<(tartarust_ipc::Profile, Vec<Macro>)>,
    overwrite: bool,
) -> Result<usize, String> {
    use dioxus::prelude::WritableExt;
    let mut imported = 0usize;
    let mut failure = None;
    for (profile, macros) in entries {
        let shown = profile.name.clone();
        match send(Request::ImportProfile {
            profile,
            macros,
            rename: None,
            overwrite,
        })
        .await
        {
            Ok(Response::Ack) => imported += 1,
            Ok(Response::Error { message }) => {
                failure = Some(format!("importing “{shown}”: {message}"));
                break;
            }
            Ok(other) => {
                failure = Some(format!("unexpected daemon reply: {other:?}"));
                break;
            }
            Err(e) => {
                mark_offline(store, &e);
                return Err("daemon unreachable — import aborted".into());
            }
        }
    }
    // Resync even on failure: a config-file import can partially land before a
    // rejection, and the list must reflect what the daemon actually holds.
    if imported > 0 {
        store.with_mut(|s| s.unsaved = true);
    }
    resync(store).await;
    match failure {
        Some(message) => Err(message),
        None => Ok(imported),
    }
}

pub fn save_macro(store: dioxus::prelude::Signal<AppState>, r#macro: Macro) {
    fire_or_resync(store, Request::SaveMacro { r#macro });
}

/// Fetch one macro's full body from the daemon and fill its steps into the
/// store. Used when a live-loaded macro is a name-only stub (`ListMacros`
/// returns names only). No-op offline (the send fails and the local body — the
/// mock, with steps — stays). Matches by name so it's robust to reordering.
pub fn fetch_macro(mut store: dioxus::prelude::Signal<AppState>, name: String) {
    use dioxus::prelude::WritableExt;
    dioxus::prelude::spawn(async move {
        match send(Request::GetMacro { name: name.clone() }).await {
            Ok(Response::Macro(m)) => {
                // `with_mut` (not `write`) sidesteps the name clash with tokio's
                // AsyncWriteExt::write, which is glob-imported in this module.
                store.with_mut(|s| {
                    if let Some(slot) = s.macros.iter_mut().find(|x| x.name == name) {
                        slot.steps = m.steps;
                    }
                    // Mark the body loaded so the editor unblocks (review #5).
                    s.macros_loaded.insert(name.clone());
                });
            }
            // The macro is gone daemon-side (deleted between ListMacros and the
            // click): mark it loaded anyway so the editor resolves out of its
            // "Loading…" placeholder and the prefetch effect stops refetching
            // (review #3), instead of hammering a doomed request per store write.
            Ok(Response::Error { message }) => {
                tracing_warn(&format!("daemon rejected GetMacro: {message}"));
                store.with_mut(|s| {
                    s.macros_loaded.insert(name.clone());
                });
            }
            // Transport failure: flip offline (which also gates the prefetch effect
            // off) and leave the stub unloaded so a later retry can fill it.
            Err(e) => mark_offline(store, &e),
            Ok(_) => {}
        }
    });
}

/// Delete a macro by name (the ✕ on a Macros-screen rail row).
pub fn delete_macro(store: dioxus::prelude::Signal<AppState>, name: String) {
    fire_or_resync(store, Request::DeleteMacro { name });
}

#[cfg(test)]
mod tests {
    //! The cage harness runs with NO daemon, so the live IPC path never executes
    //! there. These tests stand up a throwaway `UnixListener` that replies with
    //! canned `Response`s, exercising the wire logic (`send` + `load_state`)
    //! against the real `tartarust-ipc` schema.

    use super::*;
    use tartarust_ipc::model::{DeviceInfo, Effect, LightingState, Profile};
    use tartarust_ipc::{DaemonStatus, ProfileList, ProfileSummary};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    /// Canned daemon: answers each connection by matching on the request `kind`.
    /// Serves `n` connections then returns.
    async fn serve_canned(listener: UnixListener, n: usize) {
        for _ in 0..n {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            let Ok(Some(line)) = lines.next_line().await else {
                continue;
            };
            let req: Request = serde_json::from_str(&line).expect("decode request");
            let resp = match req {
                Request::GetStatus => Response::Status(DaemonStatus {
                    protocol_version: tartarust_ipc::PROTOCOL_VERSION,
                    daemon_version: "test".into(),
                    device_connected: true,
                    active_profile: "Live".into(),
                    default_profile: "Live".into(),
                    auto_switch: false,
                    unsaved_changes: false,
                }),
                Request::ListProfiles => Response::Profiles(ProfileList {
                    profiles: vec![
                        ProfileSummary {
                            name: "Live".into(),
                            linked_app: Some("game.exe".into()),
                        },
                        ProfileSummary {
                            name: "Other".into(),
                            linked_app: None,
                        },
                    ],
                    active: "Live".into(),
                }),
                Request::GetProfile { name } => {
                    let mut p = Profile::new(name);
                    p.lighting = LightingState {
                        effect: Effect::Spectrum,
                        brightness: 200,
                    };
                    Response::Profile(p)
                }
                Request::GetDeviceInfo => Response::DeviceInfo(DeviceInfo {
                    firmware: Some("9.9".into()),
                    serial: Some("SERIAL123".into()),
                    connected: true,
                }),
                // ListMacros serializes now that Response::Macros is a struct
                // variant (#16). Return real names so the client's macro overlay
                // is exercised end-to-end.
                Request::ListMacros => Response::Macros {
                    names: vec!["M1".into(), "M2".into()],
                },
                // GetMacro returns the full body for #18 (live macro steps).
                Request::GetMacro { name } => Response::Macro(tartarust_ipc::Macro {
                    name,
                    steps: vec![
                        tartarust_ipc::MacroStep::Press {
                            key: "KEY_R".into(),
                        },
                        tartarust_ipc::MacroStep::Delay { ms: 25 },
                        tartarust_ipc::MacroStep::Release {
                            key: "KEY_R".into(),
                        },
                    ],
                }),
                // GetAnalogState: an active keypad with one configured key, so the
                // calibration client's analog overlay is exercised end-to-end.
                Request::GetAnalogState => Response::AnalogState(AnalogState {
                    analog_active: true,
                    pressures: vec![0, 0, 0x90],
                    keys: vec![tartarust_ipc::AnalogKeyConfig {
                        index: 2,
                        key: "KEY_Q".into(),
                        actuation_point: 0x60,
                        release_point: 0x50,
                        second_actuation_point: None,
                        second_action: None,
                    }],
                }),
                _ => Response::Ack,
            };
            let mut out = serde_json::to_string(&resp).unwrap();
            out.push('\n');
            write_half.write_all(out.as_bytes()).await.unwrap();
            write_half.flush().await.unwrap();
        }
    }

    /// A unique socket path per test, so tests run in parallel without sharing
    /// the env var (the explicit-socket cores `send_on`/`load_state_on` avoid
    /// any global mutation).
    fn temp_socket(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("tartarust-test-{}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[tokio::test]
    async fn send_roundtrips_one_request() {
        let sock = temp_socket("send");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(serve_canned(listener, 1));

        let resp = send_on(&sock, Request::GetStatus).await.expect("send ok");
        match resp {
            Response::Status(s) => assert_eq!(s.active_profile, "Live"),
            other => panic!("unexpected response: {other:?}"),
        }
        server.await.unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn load_state_overlays_live_state() {
        let sock = temp_socket("load");
        let listener = UnixListener::bind(&sock).unwrap();
        // load_state makes 5 sequential requests, one connection each.
        let server = tokio::spawn(serve_canned(listener, 5));

        let base = crate::model::mock_state();
        let live = load_state_on(&sock, base).await.expect("daemon reachable");

        assert!(!live.offline, "should be marked online");
        assert_eq!(live.profile.name, "Live");
        assert_eq!(live.profile.lighting.brightness, 200);
        assert_eq!(live.profiles.len(), 2);
        assert_eq!(live.device.serial.as_deref(), Some("SERIAL123"));
        // ListMacros now succeeds (#16), so the client overlays the live macro
        // names (loaded as name-only stubs — there is no GetMacro to fetch steps).
        assert_eq!(live.macros.len(), 2, "macros overlaid from ListMacros");
        assert_eq!(live.macros[0].name, "M1");
        assert!(
            live.macros[0].steps.is_empty(),
            "live macros load name-only"
        );

        server.await.unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn load_state_stays_offline_when_no_daemon() {
        let sock = temp_socket("absent"); // never bound — connect must fail
        let base = crate::model::mock_state();
        assert!(
            load_state_on(&sock, base).await.is_none(),
            "no daemon => None"
        );
    }

    /// Executable check of the mutation WIRE FORMAT: drive a real `SetBind`
    /// (the shape the Customize screen builds) through the client and assert the
    /// daemon receives exactly the expected tagged JSON. Covers the write path
    /// that the fire-and-forget setters wrap (those need a Dioxus runtime to
    /// spawn, so we exercise the underlying `send_on` directly here).
    #[tokio::test]
    async fn set_bind_emits_expected_request_json() {
        use tartarust_ipc::{BindAction, Layer};

        let sock = temp_socket("setbind");
        let listener = UnixListener::bind(&sock).unwrap();
        // Capture the raw request line the client sends.
        let captured = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            // Reply Ack so the client's read completes cleanly.
            write_half.write_all(b"{\"kind\":\"ack\"}\n").await.unwrap();
            write_half.flush().await.unwrap();
            line
        });

        let req = Request::SetBind {
            profile: "FPS — Default".into(),
            layer: Layer::Hypershift,
            key: "KEY_W".into(),
            action: Some(BindAction::Key {
                keys: vec!["KEY_F5".into()],
            }),
        };
        let resp = send_on(&sock, req).await.expect("send ok");
        assert!(matches!(resp, Response::Ack));

        let line = captured.await.unwrap();
        // Re-parse to compare structurally (field order is serde-defined but we
        // assert on the decoded value to stay robust).
        let got: Request = serde_json::from_str(&line).unwrap();
        match got {
            Request::SetBind {
                profile,
                layer,
                key,
                action,
            } => {
                assert_eq!(profile, "FPS — Default");
                assert_eq!(layer, Layer::Hypershift);
                assert_eq!(key, "KEY_W");
                assert_eq!(
                    action,
                    Some(BindAction::Key {
                        keys: vec!["KEY_F5".into()]
                    })
                );
            }
            other => panic!("expected SetBind, got {other:?}"),
        }
        // And spot-check the tag is present in the raw wire form.
        assert!(line.contains("\"kind\":\"set_bind\""), "wire: {line}");

        let _ = std::fs::remove_file(&sock);
    }

    /// GetMacro round-trip: the request shape the Macros screen sends and the
    /// `Response::Macro(Macro)` it decodes into real steps (#18).
    #[tokio::test]
    async fn get_macro_round_trips_full_body() {
        use tartarust_ipc::MacroStep;

        let sock = temp_socket("getmacro");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(serve_canned(listener, 1));

        let resp = send_on(&sock, Request::GetMacro { name: "M1".into() })
            .await
            .expect("send ok");
        match resp {
            Response::Macro(m) => {
                assert_eq!(m.name, "M1");
                assert_eq!(m.steps.len(), 3, "full body, not a name-only stub");
                assert_eq!(
                    m.steps[0],
                    MacroStep::Press {
                        key: "KEY_R".into()
                    }
                );
                assert_eq!(m.steps[1], MacroStep::Delay { ms: 25 });
            }
            other => panic!("expected Macro, got {other:?}"),
        }

        server.await.unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    /// GetAnalogState round-trip: the request the Calibration screen polls and
    /// the `Response::AnalogState(AnalogState)` it decodes into the live mode
    /// flag, per-key pressures, and actuation config it renders.
    #[tokio::test]
    async fn get_analog_state_round_trips() {
        let sock = temp_socket("analog");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(serve_canned(listener, 1));

        let analog = get_analog_state_on(&sock).await.expect("daemon reachable");
        assert!(analog.analog_active, "canned daemon is in analog mode");
        assert_eq!(analog.pressures, vec![0, 0, 0x90]);
        assert_eq!(analog.keys.len(), 1);
        assert_eq!(analog.keys[0].index, 2);
        assert_eq!(analog.keys[0].key, "KEY_Q");
        assert_eq!(analog.keys[0].actuation_point, 0x60);
        assert_eq!(analog.keys[0].release_point, 0x50);

        server.await.unwrap();
        let _ = std::fs::remove_file(&sock);

        // No daemon => None (the cage-harness / offline path: no live bars).
        let absent = temp_socket("analog-absent");
        assert!(get_analog_state_on(&absent).await.is_none());
    }

    /// `SetActuation` wire shape: the request the Calibration sliders build,
    /// including a dual-step (second actuation + action), must reach the daemon
    /// as the expected tagged JSON.
    #[tokio::test]
    async fn set_actuation_emits_expected_request_json() {
        let sock = temp_socket("setact");
        let listener = UnixListener::bind(&sock).unwrap();
        let captured = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            write_half.write_all(b"{\"kind\":\"ack\"}\n").await.unwrap();
            write_half.flush().await.unwrap();
            line
        });

        let req = Request::SetActuation {
            index: 2,
            actuation_point: 0x60,
            release_point: 0x50,
            second_actuation_point: Some(0xC0),
            second_action: Some("KEY_LEFTSHIFT".into()),
        };
        let resp = send_on(&sock, req).await.expect("send ok");
        assert!(matches!(resp, Response::Ack));

        let line = captured.await.unwrap();
        match serde_json::from_str::<Request>(&line).unwrap() {
            Request::SetActuation {
                index,
                actuation_point,
                release_point,
                second_actuation_point,
                second_action,
            } => {
                assert_eq!(index, 2);
                assert_eq!(actuation_point, 0x60);
                assert_eq!(release_point, 0x50);
                assert_eq!(second_actuation_point, Some(0xC0));
                assert_eq!(second_action.as_deref(), Some("KEY_LEFTSHIFT"));
            }
            other => panic!("expected SetActuation, got {other:?}"),
        }
        assert!(line.contains("\"kind\":\"set_actuation\""), "wire: {line}");

        let _ = std::fs::remove_file(&sock);
    }

    /// A daemon that *rejects* a mutation surfaces as `Response::Error` (not an
    /// `Err`) — this is the branch `fire_or_resync` keys on to re-pull
    /// authoritative state instead of leaving the optimistic edit diverged
    /// (review #2). A connection failure (offline) is a transport `Err` and is
    /// NOT treated as a rejection.
    #[tokio::test]
    async fn rejected_mutation_is_response_error_not_transport_err() {
        let sock = temp_socket("reject");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            let _ = lines.next_line().await.unwrap();
            write_half
                .write_all(b"{\"kind\":\"error\",\"message\":\"no such profile\"}\n")
                .await
                .unwrap();
            write_half.flush().await.unwrap();
        });

        let resp = send_on(&sock, Request::SetBrightness { level: 128 })
            .await
            .expect("transport ok");
        // The rejection is an in-band Error the client resyncs on...
        assert!(matches!(resp, Response::Error { .. }));
        server.await.unwrap();
        let _ = std::fs::remove_file(&sock);

        // ...whereas no daemon at all is a transport Err (offline: edit stays
        // local, no resync). Confirm the two are distinguishable.
        let absent = temp_socket("reject-absent");
        assert!(
            send_on(&absent, Request::SetBrightness { level: 1 })
                .await
                .is_err()
        );
    }

    /// The debounced rename (review #4) must send `RenameProfile{from, to}` with
    /// the ORIGINAL (pre-edit) name as `from` — the bug was `from == to`. This
    /// pins the wire shape the Profiles `onchange` handler builds.
    #[tokio::test]
    async fn rename_emits_from_original_to_new() {
        let sock = temp_socket("rename");
        let listener = UnixListener::bind(&sock).unwrap();
        let captured = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            write_half.write_all(b"{\"kind\":\"ack\"}\n").await.unwrap();
            write_half.flush().await.unwrap();
            line
        });

        // Shape the handler produces: from = committed (original), to = edited.
        let req = Request::RenameProfile {
            from: "FPS".into(),
            to: "FPSX".into(),
        };
        let resp = send_on(&sock, req).await.expect("send ok");
        assert!(matches!(resp, Response::Ack));

        let line = captured.await.unwrap();
        match serde_json::from_str::<Request>(&line).unwrap() {
            Request::RenameProfile { from, to } => {
                assert_eq!(
                    from, "FPS",
                    "from must be the original, not the edited name"
                );
                assert_eq!(to, "FPSX");
            }
            other => panic!("expected RenameProfile, got {other:?}"),
        }
        let _ = std::fs::remove_file(&sock);
    }
}

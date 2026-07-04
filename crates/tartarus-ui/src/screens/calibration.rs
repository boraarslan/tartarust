//! Calibration — live "heatmap" keypad you click to tune a key, wired to LIVE
//! daemon state.
//!
//! In analog mode the daemon streams per-key pressure (0..255) and exposes the
//! actuation config over IPC. This screen polls `GetAnalogState` on a short
//! interval *while mounted* into a screen-local signal (never the global store,
//! so a pressure tick doesn't repaint the whole shell) and renders the rendered
//! Tartarus in `Pressure` mode: each of the 20 grid caps fills from the bottom
//! by its live pressure. Clicking a cap selects it (the shared `store.selected`)
//! and, if it has an analog config, opens a floating actuation editor positioned
//! next to that cap:
//!   * a live pressure bar with the actuation + release points marked, and
//!   * sliders for actuation / release (with hysteresis) plus an optional
//!     dual-step (deeper second actuation + a second evdev action).
//!
//! Edits are optimistic-local and pushed via `SetActuation`; the schema
//! invariants (`release < actuation`, second step deeper, `second_action` set
//! iff `second_actuation_point` is set) are enforced here before sending. The
//! poll is the authority for *pressure*; slider values come from local edit
//! state seeded from the config (re-seeded by the editor on selection change so
//! switching keys re-seeds), so dragging never fights the refresh.
//!
//! Offline (no daemon — the cage harness) or in digital mode there are no live
//! bars; each non-editor state renders an intentional message, not dead sliders.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use dioxus::prelude::*;

use tartarus_ipc::{AnalogKeyConfig, AnalogState};

use crate::components::{CapPhase, CapPointer, KeyPicker, Keypad, KeypadMode, PageHead, Slider, Toggle};
use crate::model::{pretty_key, KeyId};
use crate::use_store;

/// Integer percentage readout for a raw 0..255 pressure value.
fn pct100(v: u8) -> u8 {
    (v as u16 * 100 / 255) as u8
}

/// How often to re-poll the live analog state while on this screen.
const POLL_INTERVAL_MS: u64 = 100;

#[component]
pub fn Calibration() -> Element {
    // Screen-local live state: `None` until the first successful poll, and back
    // to `None` whenever the daemon goes away. `None` == offline / not-yet-loaded
    // (the cage-harness path) — we render an honest offline state, never fake bars.
    let mut analog = use_signal::<Option<AnalogState>>(|| None);

    // Entering Calibration starts with NOTHING selected — don't inherit a key
    // selection (and auto-open its actuation modal) carried over from Customize
    // or a prior visit. Runs once on mount (no tracked signal reads).
    {
        let mut store = use_store();
        use_effect(move || {
            store.write().selected = None;
        });
    }

    // Poll `GetAnalogState` on an interval, but ONLY while this component is
    // mounted: `use_future`'s task is cancelled on unmount, so navigating away
    // stops the polling (no background traffic off-screen).
    use_future(move || async move {
        let mut tick = tokio::time::interval(Duration::from_millis(POLL_INTERVAL_MS));
        loop {
            tick.tick().await;
            let next = crate::ipc::get_analog_state().await;
            // Only write on a real change so an idle/offline keypad (the same
            // value every tick) doesn't repaint this component 10x/second. `peek`
            // reads without subscribing — we're outside the reactive graph here.
            if *analog.peek() != next {
                analog.set(next);
            }
        }
    });

    rsx! {
        PageHead {
            title: "Calibration",
            sub: "Click a key to tune its analog actuation. Caps fill with live pressure as you press.",
        }

        // Contextual notice for non-live states. The keypad below ALWAYS renders
        // (so the board reads as "keys" in every state, consistent with the other
        // screens); the banner just explains why there's no live pressure yet.
        match analog() {
            None => rsx! { OfflineState {} },
            Some(a) if !a.analog_active => rsx! { DigitalModeState {} },
            Some(a) if a.keys.is_empty() => rsx! { NoKeysState {} },
            Some(_) => rsx! {},
        }

        Stage { state: analog() }
    }
}

/// The keypad stage: the rendered Tartarus in `Pressure` mode (each grid cap
/// fills by its current pressure) plus a floating actuation editor next to the
/// selected key. Always rendered — offline/digital/no-keys just means an empty
/// pressure map (caps with no fill) and no openable editor. Replaces the old
/// scrollable card list.
#[component]
fn Stage(state: Option<AnalogState>) -> Element {
    let mut store = use_store();

    // Multi-select state (screen-local; separate from the global single
    // `store.selected`). `multi` is the selected set; `dragging` gates the
    // drag-extend; `multi_open` shows the bulk editor.
    let mut multi = use_signal(BTreeSet::<KeyId>::new);
    let mut dragging = use_signal(|| false);
    let mut multi_open = use_signal(|| false);

    let keys = state.as_ref().map(|s| s.keys.clone()).unwrap_or_default();
    let frame = state.as_ref().map(|s| s.pressures.clone()).unwrap_or_default();

    // Per-key pressure for the keypad overlay, keyed by evdev name: for each
    // configured analog key, look up its byte index in the live frame. Grid caps
    // the daemon hasn't mapped (and the digital thumb/wheel/space) get nothing;
    // offline the map is simply empty.
    let pressures: HashMap<String, u8> = keys
        .iter()
        .map(|cfg| {
            (
                cfg.key.clone(),
                frame.get(cfg.index as usize).copied().unwrap_or(0),
            )
        })
        .collect();

    // The selected key, resolved to its analog config when the daemon has one for
    // it. Analog actuation isn't grid-only: the spacebar (`KEY_SPACE`) and thumb
    // button (`KEY_LEFTALT`) are pressure inputs too, so we match ANY bindable cap
    // by its evdev name against the analog config (the `pressures` map is already
    // keyed the same way). `open_left` opens the 240px popover toward the stage
    // centre for right-side caps (right grid columns + the thumb cluster) so it
    // never runs off-screen. Unconfigured caps, the wheel, and the whole offline
    // path resolve to `None` — no editor opens.
    let selected_cfg: Option<(AnalogKeyConfig, bool)> =
        store.read().selected.and_then(|sel| {
            let name = sel.evdev()?;
            let cfg = keys.iter().find(|c| c.key == name).cloned()?;
            Some((cfg, opens_left(sel)))
        });

    // Resolve the multi-selection to the analog configs we can bulk-edit (caps
    // with no analog config — e.g. an unmapped key — are silently skipped).
    let multi_cfgs: Vec<AnalogKeyConfig> = multi()
        .iter()
        .filter_map(|k| {
            let name = k.evdev()?;
            keys.iter().find(|c| c.key == name).cloned()
        })
        .collect();
    let show_multi = multi_open() && !multi_cfgs.is_empty();

    // Evdev names the daemon has an analog config for; the multi-open paths (drag
    // release and Shift release) only open the bulk editor when the selection
    // resolves to at least one of these, else the highlight would wedge with
    // nothing to show.
    let configured: BTreeSet<String> = keys.iter().map(|c| c.key.clone()).collect();
    let configured_shift = configured.clone();

    // Cap pointer handler: a plain click single-selects (the cap-anchored editor);
    // a drag across >1 cap, or Shift+clicks, build the multi set; the bulk editor
    // opens on drag-end (>1 caps) or on Shift release (the stage's onkeyup below).
    let pointer = EventHandler::new(move |p: CapPointer| match p.phase {
        CapPhase::Down => {
            if p.shift {
                // Shift+click: accumulate silently; the editor waits for release.
                multi.write().insert(p.key);
                multi_open.set(false);
                store.write().selected = None;
            } else {
                // Begin a drag (also covers a plain click = a drag of one cap).
                dragging.set(true);
                multi_open.set(false);
                multi.set(BTreeSet::from([p.key]));
            }
        }
        CapPhase::Enter => {
            // Extend the drag only while the primary button is actually held — a
            // stuck `dragging` (drag released off-cap) must not let a buttonless
            // hover keep adding caps.
            if *dragging.peek() && p.primary_held {
                multi.write().insert(p.key);
            }
        }
        CapPhase::Up => {
            if *dragging.peek() {
                dragging.set(false);
                let set = multi.peek().clone();
                if set.len() > 1 {
                    // A real multi-key drag → bulk editor, but only if the drag
                    // covers ≥1 analog-configured cap; otherwise clear the set so
                    // the highlight can't wedge with no editor to open.
                    if set.iter().any(|k| {
                        k.evdev().is_some_and(|n| configured.contains(&n))
                    }) {
                        multi_open.set(true);
                        store.write().selected = None;
                    } else {
                        multi.write().clear();
                    }
                } else {
                    // A single click: fall back to the normal single-key editor,
                    // and re-clicking the already-selected cap toggles it closed.
                    multi.write().clear();
                    if let Some(&k) = set.iter().next() {
                        let mut w = store.write();
                        w.selected = if w.selected == Some(k) { None } else { Some(k) };
                    }
                }
            }
        }
    });

    rsx! {
        // The dim/dismiss layer (z-index:40). Clicking outside the stage clears
        // the selection. Only present while a single-key tunable is open.
        if selected_cfg.is_some() {
            Backdrop {}
        }

        // Stage wrapper: an inline z-index:51 lifts the bright keypad (and the
        // editor anchored inside the selected cap) ABOVE the backdrop, so the
        // heatmap stays visible while tuning and the editor isn't dimmed. The
        // CalModal is passed as the keypad's `children`: the keypad anchors it
        // INSIDE the selected cap (each cap is position:relative), so it floats
        // beside whichever cap was clicked — consistent from the first render,
        // with no row/column math that the 5/5/5/4 layout would break.
        //
        // `tabindex` lets the stage receive key events: releasing Shift after a
        // run of Shift+clicks opens the bulk editor (onkeyup).
        div {
            class: "stage",
            tabindex: "0",
            // Take keyboard focus on mount so the Shift-release detector (onkeyup
            // below) actually fires: clicking the non-focusable caps doesn't grant
            // focus to this tabindex container on WebKitGTK, so without this the
            // Shift+click multi-select path would never open the bulk editor.
            onmounted: move |e: Event<MountedData>| {
                spawn(async move {
                    let _ = e.set_focus(true).await;
                });
            },
            style: "position:relative; z-index:51; outline:none;",
            // Click the bare stage to clear the single selection and end any drag.
            // A cross-cap drag emits a trailing synthetic click on the caps' common
            // ancestor (a keypad div that doesn't stop propagation) which bubbles
            // here AFTER CapPhase::Up opened the bulk editor — so we must NOT clear
            // the multi-selection while the editor is open (multi_open), or the drag
            // would be killed. When the editor IS open, its own `.multi-backdrop`
            // handles dismissal; here we only drop a still-accumulating selection.
            onclick: move |_| {
                store.write().selected = None;
                dragging.set(false);
                if !*multi_open.peek() {
                    multi.write().clear();
                }
            },
            onkeyup: move |e: KeyboardEvent| {
                if e.key() == Key::Shift && !*dragging.peek() {
                    let set = multi.peek().clone();
                    if set.iter().any(|k| {
                        k.evdev().is_some_and(|n| configured_shift.contains(&n))
                    }) {
                        multi_open.set(true);
                        store.write().selected = None;
                    } else {
                        multi.write().clear();
                    }
                }
            },
            Keypad {
                mode: KeypadMode::Pressure,
                // Only the analog caps (19 main keys + spacebar) are calibratable,
                // so the 8-way pad, scroll wheel, and thumb button are hidden here.
                analog_only: true,
                pressures: Some(pressures),
                on_cap_pointer: Some(pointer),
                multi_selected: multi(),
                if let Some((cfg, open_left)) = selected_cfg {
                    CalModal {
                        // CalModal re-seeds its sliders internally when the byte
                        // index changes (it sits in this conditional, not an
                        // iterator, so `key:` wouldn't remount it). The live
                        // press-bar follows the poll.
                        cfg: cfg.clone(),
                        pressure: frame.get(cfg.index as usize).copied().unwrap_or(0),
                        open_left,
                    }
                }
            }

            // Bulk actuation editor for a multi-selection (drag or Shift+click).
            // Only actuation/release — never second functions — and it preserves
            // each key's own dual-step. Toggling `show_multi` remounts it, so it
            // always re-seeds for the current selection.
            if show_multi {
                MultiCalModal {
                    cfgs: multi_cfgs,
                    on_close: move |_| {
                        multi.write().clear();
                        multi_open.set(false);
                    },
                }
            }
        }
    }
}

/// Bulk actuation editor for a multi-selection: actuation + release points only
/// (no dual-step / second action, per the multi-edit spec), applied to EVERY
/// selected analog key at once. Each key keeps its own existing second function —
/// we forward `second_*` untouched, so a bulk threshold change never wipes a
/// per-key dual-step. A floating, centred dialog (not cap-anchored, since it
/// targets many caps), with its own dimming backdrop.
#[component]
fn MultiCalModal(cfgs: Vec<AnalogKeyConfig>, on_close: EventHandler<()>) -> Element {
    let store = use_store();
    let count = cfgs.len();
    // Seed the sliders from the first selected key; editing then writes all.
    let seed_act = cfgs.first().map_or(0x60, |c| c.actuation_point);
    let seed_rel = cfgs.first().map_or(0x50, |c| c.release_point);
    let cfgs = use_signal(|| cfgs);
    let mut actuation = use_signal(|| seed_act);
    let mut release = use_signal(|| seed_rel);

    // Push the current actuation/release to every selected analog key, preserving
    // each key's own dual-step (`second_*` forwarded verbatim). All captures are
    // Copy (signals), so this closure is reusable across both sliders.
    let push = move || {
        let act = actuation();
        let rel = release().min(act.saturating_sub(1)); // hysteresis: release < actuation
        for c in cfgs.read().iter() {
            let sap = c.second_actuation_point.map(|p| p.max(act.saturating_add(1))); // deeper than actuation
            crate::ipc::set_actuation(
                store,
                c.index,
                act,
                rel,
                sap,
                c.second_action.clone(),
            );
        }
    };

    let act_v = actuation();
    let rel_v = release();
    let names: String = cfgs
        .read()
        .iter()
        .map(|c| pretty_key(&c.key))
        .collect::<Vec<_>>()
        .join(", ");

    rsx! {
        div { class: "multi-backdrop", onclick: move |_| on_close.call(()) }
        div {
            class: "multi-modal",
            onclick: move |e| e.stop_propagation(),
            div { class: "cal-modal-head",
                span { "{count} keys · bulk actuation" }
                span { class: "link-x", onclick: move |_| on_close.call(()), "×" }
            }
            div { class: "field-value", style: "color:var(--text-mute);margin-bottom:12px;", "{names}" }

            Slider {
                label: "Actuation point".to_string(),
                value: act_v as f64,
                min: 1.0, max: 255.0, step: 1.0,
                readout: format!("{act_v} ({}%)", pct100(act_v)),
                on_input: move |v: f64| {
                    let nv = v as u8;
                    actuation.set(nv);
                    if release() >= nv {
                        release.set(nv.saturating_sub(1));
                    }
                    push();
                },
            }

            Slider {
                label: "Release point".to_string(),
                value: rel_v as f64,
                min: 0.0, max: 254.0, step: 1.0,
                readout: format!("{rel_v} ({}%)", pct100(rel_v)),
                on_input: move |v: f64| {
                    let nv = (v as u8).min(actuation().saturating_sub(1));
                    release.set(nv);
                    push();
                },
            }

            div { class: "field-value", style: "color:var(--text-mute);margin-top:12px;",
                "Sets actuation & release for all selected keys at once. Each key keeps its own second function."
            }
        }
    }
}

/// Whether the actuation popover should open toward the stage centre (to the
/// LEFT of its cap) so the 240px panel doesn't run off the right edge. True for
/// the right grid columns and the whole thumb cluster (spacebar / thumb button,
/// which sit on the right of the rendered keypad).
fn opens_left(sel: KeyId) -> bool {
    match sel {
        // Flat grid index -> column within its row (rows are 5/5/5/4). The right
        // two columns open left; everything else opens right.
        KeyId::Grid(g) => {
            let i = g as usize;
            let row = i / 5;
            let col = i - row * 5;
            col >= 3
        }
        KeyId::Space | KeyId::ThumbButton | KeyId::Thumb(_) => true,
        _ => false,
    }
}

/// The dim/dismiss layer behind the floating editor. A click anywhere on it
/// clears the selection (closing the editor).
#[component]
fn Backdrop() -> Element {
    let mut store = use_store();
    rsx! {
        div {
            class: "backdrop",
            onclick: move |_| store.write().selected = None,
        }
    }
}

/// Floating per-key actuation editor, positioned next to its grid cap. Lifts the
/// old `AnalogKeyRow` internals verbatim — live pressure bar + actuation/release
/// sliders + dual-step toggle + second-step slider/action — and the same
/// optimistic push with the schema invariants. Slider values are screen-local
/// edit state seeded from `cfg`; only the bar follows the live `pressure`.
///
/// CalModal sits inside an `if let Some(...)` conditional, NOT an iterator —
/// Dioxus `key:` only reconciles list children, so a direct Some(A)->Some(B)
/// selection change does NOT remount it (the `use_signal` seeds would keep key
/// A's values). Mirror Customize's explicit prev-id reset: re-seed the sliders
/// when the byte index changes. `peek()` reads without subscribing, so the poll
/// keeps updating the live bar without reseeding the sliders mid-drag.
///
/// `open_left` adds `.open-left` so the popover opens toward the stage centre
/// for right-side caps; otherwise it opens to the right of its cap. The keypad
/// anchors this INSIDE the selected cap (cap is position:relative), so no manual
/// top/left math is needed — it lands beside the right cap from the first render.
#[component]
fn CalModal(cfg: AnalogKeyConfig, pressure: u8, open_left: bool) -> Element {
    let mut store = use_store();

    // The daemon keys SetActuation by the analog BYTE index (0..22). `cfg.index`
    // is that byte index — it's also our stable per-key identity for the reseed
    // (it changes iff the selection moves to a different analog key). Capture it
    // before the push closure.
    let byte_index = cfg.index;

    // Local optimistic edit state, seeded from the daemon-reported config.
    // Pressure ticks never touch these signals — only the bar reads `pressure`.
    let mut actuation = use_signal(|| cfg.actuation_point);
    let mut release = use_signal(|| cfg.release_point);
    let mut second_enabled = use_signal(|| cfg.second_actuation_point.is_some());
    let mut second_point = use_signal(|| cfg.second_actuation_point.unwrap_or(0xC0));
    let mut second_action = use_signal(|| {
        cfg.second_action
            .clone()
            .unwrap_or_else(|| "KEY_LEFTSHIFT".to_string())
    });

    // Re-seed on selection change (byte index). Runs before the value reads below
    // so the reseed shows this render. `peek()` avoids self-subscription.
    let mut prev = use_signal(|| byte_index);
    if *prev.peek() != byte_index {
        prev.set(byte_index);
        actuation.set(cfg.actuation_point);
        release.set(cfg.release_point);
        second_enabled.set(cfg.second_actuation_point.is_some());
        second_point.set(cfg.second_actuation_point.unwrap_or(0xC0));
        second_action.set(
            cfg.second_action
                .clone()
                .unwrap_or_else(|| "KEY_LEFTSHIFT".to_string()),
        );
    }

    // Send the current edit state to the daemon, upholding the schema invariants.
    // Dual-step off => BOTH second fields cleared (never a half-set second step).
    // The second action comes from the KeyPicker, so it's already a valid evdev
    // name — no text coercion needed.
    let push = move || {
        let act = actuation();
        let rel = release().min(act.saturating_sub(1)); // hysteresis: release < actuation
        let (sap, sact) = if second_enabled() {
            let p = second_point().max(act.saturating_add(1)); // deeper than actuation
            (Some(p), Some(second_action()))
        } else {
            (None, None)
        };
        crate::ipc::set_actuation(store, byte_index, act, rel, sap, sact);
    };

    let act_v = actuation();
    let rel_v = release();
    let second_on = second_enabled();
    let second_v = second_point();
    let second_act_v = second_action();
    let cap = pretty_key(&cfg.key);

    // Marker/fill positions as 0..100% of the 0–255 scale, and an integer percent
    // for the readouts (the device reports raw pressure; % is just a convenience).
    let pct = |v: u8| (v as f32 / 255.0 * 100.0).clamp(0.0, 100.0);

    // The keypad anchors this popover inside the selected cap; `.open-left` just
    // flips which side it opens on. No top/left math — the cap is its frame.
    let modal_class = if open_left { "cal-modal open-left" } else { "cal-modal" };

    rsx! {
        div {
            class: "{modal_class}",
            // The modal lives INSIDE the selected cap; stop clicks from bubbling to
            // the cap (which would re-select / toggle) or the stage (which closes).
            // The × and the cap toggle still close it; interacting with the editor
            // never does.
            onclick: move |e| e.stop_propagation(),
            div { class: "cal-modal-head",
                span { "{cap} · #{byte_index}" }
                span {
                    class: "link-x",
                    onclick: move |_| store.write().selected = None,
                    "×"
                }
            }

            // Live pressure bar, with actuation/release (and second-step) marks.
            div { class: "field",
                label { class: "field-label", "Live pressure" }
                div { class: "press-bar",
                    div { class: "press-fill", style: "width:{pct(pressure)}%;" }
                    div { class: "press-mark release", style: "left:{pct(rel_v)}%;", title: "release" }
                    div { class: "press-mark actuate", style: "left:{pct(act_v)}%;", title: "actuation" }
                    if second_on {
                        div { class: "press-mark second", style: "left:{pct(second_v)}%;", title: "second step" }
                    }
                }
                div { class: "press-readout", "{pressure} / 255 · {pct100(pressure)}%" }
            }

            div { class: "divider" }

            Slider {
                label: "Actuation point".to_string(),
                value: act_v as f64,
                min: 1.0, max: 255.0, step: 1.0,
                readout: format!("{act_v} ({}%)", pct100(act_v)),
                on_input: move |v: f64| {
                    let nv = v as u8;
                    actuation.set(nv);
                    // Keep the dependent thresholds consistent as actuation moves,
                    // so the markers never render on the wrong side of it: release
                    // strictly below, the second step strictly above.
                    if release() >= nv {
                        release.set(nv.saturating_sub(1));
                    }
                    if second_enabled() && second_point() <= nv {
                        second_point.set(nv.saturating_add(1));
                    }
                    push();
                },
            }

            Slider {
                label: "Release point".to_string(),
                value: rel_v as f64,
                min: 0.0, max: 254.0, step: 1.0,
                readout: format!("{rel_v} ({}%)", pct100(rel_v)),
                on_input: move |v: f64| {
                    // Clamp below actuation (hysteresis): a release ≥ actuation
                    // would chatter, and the daemon rejects it.
                    let nv = (v as u8).min(actuation().saturating_sub(1));
                    release.set(nv);
                    push();
                },
            }

            div { class: "divider" }

            Toggle {
                label: "Dual-step (second function)".to_string(),
                on: second_on,
                on_toggle: move |_| {
                    let now = !second_enabled();
                    second_enabled.set(now);
                    // Turning it on seeds a sensible deeper point if unset.
                    if now && second_point() <= actuation() {
                        second_point.set(actuation().saturating_add(1).max(actuation()));
                    }
                    push();
                },
            }

            if second_on {
                Slider {
                    label: "Second actuation point".to_string(),
                    value: second_v as f64,
                    min: 1.0, max: 255.0, step: 1.0,
                    readout: format!("{second_v} ({}%)", pct100(second_v)),
                    amber: true,
                    on_input: move |v: f64| {
                        // Must be deeper than the primary actuation point.
                        let nv = (v as u8).max(actuation().saturating_add(1));
                        second_point.set(nv);
                        push();
                    },
                }
                div { class: "field",
                    label { class: "field-label", "Second action (key)" }
                    // A searchable dropdown (same picker as the Customize editor),
                    // not free-text evdev: it always yields a valid evdev name, so
                    // the push needs no text coercion. Picking sets + pushes live.
                    KeyPicker {
                        value: second_act_v,
                        on_pick: move |picked: String| {
                            second_action.set(picked);
                            push();
                        },
                    }
                    div { class: "field-value", style: "color:var(--text-mute);margin-top:6px;",
                        "Fires when pressed past the second point. Must be deeper than actuation."
                    }
                }
            }
        }
    }
}

/// Offline state — no daemon reachable (the cage harness, or daemon down). Mirror
/// the shell's "daemon offline" language so it reads as intentional. The keypad
/// still renders below (inert, no fills) so the board is always visible.
#[component]
fn OfflineState() -> Element {
    rsx! {
        div { class: "banner",
            span { class: "b-ico", "○" }
            b { "Daemon offline" }
            " — start tartarus-daemon to read live key pressure and edit actuation."
        }
    }
}

/// Daemon up but running in DIGITAL mode: nothing to calibrate. Point at the fix.
#[component]
fn DigitalModeState() -> Element {
    rsx! {
        div { class: "banner",
            span { class: "b-ico", "⚠" }
            b { "Analog mode is off" }
            " — set "
            span { style: "font-family:var(--mono);", "input_mode = \"analog\"" }
            " in the daemon config to calibrate per-key pressure."
        }
    }
}

/// Analog mode, but no keys are mapped to actuation yet.
#[component]
fn NoKeysState() -> Element {
    rsx! {
        div { class: "banner",
            span { class: "b-ico", "⌖" }
            b { "No analog keys configured" }
            " — map a byte index to a key in the daemon config, then click a cap to tune it."
        }
    }
}

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
//! Bulk edits go through **Edit Mode** (the toolbar toggle, top right): entering
//! selects every cap and slides in the right-hand [`EditPanel`] — actuation and
//! release sliders that apply to the whole selection, plus a live bar showing
//! the deepest press among the selected caps. While Edit Mode is on, clicking a
//! cap toggles it in/out of the selection and a drag sweeps caps in. Outside
//! Edit Mode, a drag across >1 caps or a Shift+click enters it with that
//! selection (a plain click still opens the single-key editor).
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

use tartarust_ipc::{AnalogKeyConfig, AnalogState, DEFAULT_ACTUATION_POINT, DEFAULT_RELEASE_POINT};

use crate::components::{
    CapPhase, CapPointer, KeyPicker, Keypad, KeypadMode, PageHead, Slider, Toggle,
};
use crate::model::{GRID_EVDEV, KeyId, pretty_key};
use crate::use_store;

/// Integer percentage readout for a raw 0..255 pressure value.
fn pct100(v: u8) -> u8 {
    (v as u16 * 100 / 255) as u8
}

/// Every analog-calibratable cap: the 19 grid keys plus the spacebar. The thumb
/// button, thumbpad, and wheel are digital-only (no pressure byte), so they are
/// not selectable here — the keypad renders in `analog_only` mode anyway.
fn all_analog_ids() -> BTreeSet<KeyId> {
    (0..GRID_EVDEV.len() as u8)
        .map(KeyId::Grid)
        .chain([KeyId::Space])
        .collect()
}

/// How often to re-poll the live analog state while on this screen.
const POLL_INTERVAL_MS: u64 = 100;

#[component]
pub fn Calibration() -> Element {
    // Screen-local live state: `None` until the first successful poll, and back
    // to `None` whenever the daemon goes away. `None` == offline / not-yet-loaded
    // (the cage-harness path) — we render an honest offline state, never fake bars.
    let mut analog = use_signal::<Option<AnalogState>>(|| None);

    let mut store = use_store();

    // Edit Mode state, lifted here so the toolbar toggle can drive it. `multi`
    // is the selected cap set; the right-hand EditPanel is open iff `edit_mode`.
    let mut edit_mode = use_signal(|| false);
    let mut multi = use_signal(BTreeSet::<KeyId>::new);

    // Entering Calibration starts with NOTHING selected — don't inherit a key
    // selection (and auto-open its actuation modal) carried over from Customize
    // or a prior visit. Runs once on mount (no tracked signal reads).
    use_effect(move || {
        store.write().selected = None;
        // Dev-only: let the headless screenshot harness open Edit Mode directly
        // (TARTARUST_CAL_EDIT=1) — the panel is otherwise unreachable without a
        // click. No effect in normal use (the var is unset).
        if std::env::var("TARTARUST_CAL_EDIT").as_deref() == Ok("1") {
            multi.set(all_analog_ids());
            edit_mode.set(true);
        }
    });

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

    // Read the signal ONCE per render (the subscription is identical): this
    // body re-runs up to 10x/sec while pressure moves, and each `analog()` call
    // clones the full AnalogState (keys + pressures Vecs).
    let state = analog();

    rsx! {
        // While the Edit Mode drawer is open, `.panel-open` shifts the whole
        // page left of it — otherwise the fixed drawer would cover the toolbar
        // (including the "Exit Edit Mode" button, the mode's only exit).
        div { class: if edit_mode() { "cal-page panel-open" } else { "cal-page" },
        PageHead { sub: "Click a key to tune its analog actuation — drag or use Edit Mode to tune many at once." }

        // Contextual notice for non-live states. The keypad below ALWAYS renders
        // (so the board reads as "keys" in every state, consistent with the other
        // screens); the banner just explains why there's no live pressure yet.
        match &state {
            None => rsx! { OfflineState {} },
            Some(a) if !a.analog_active => rsx! { DigitalModeState {} },
            Some(_) => rsx! {},
        }

        // Page toolbar, top right: the conditional "Reset all" plus the Edit
        // Mode toggle (always present — it is the bulk-edit entry point).
        div { class: "cal-toolbar",
            // Page-level reset: return EVERY analog key to the default thresholds
            // (dropping any dual-step). Only offered while something is actually
            // custom; like any other edit it applies live and persists on Save.
            if state.as_ref().is_some_and(|a| a.keys.iter().any(|k| k.is_custom())) {
                button {
                    class: "btn",
                    onclick: move |_| {
                        let keys: Vec<AnalogKeyConfig> = analog
                            .peek()
                            .as_ref()
                            .map(|a| a.keys.clone())
                            .unwrap_or_default();
                        // Close an open editor: its sliders hold local edit
                        // state that would go stale under the bulk reset.
                        store.write().selected = None;
                        for k in keys.iter().filter(|k| k.is_custom()) {
                            crate::ipc::set_actuation(
                                store,
                                k.index,
                                DEFAULT_ACTUATION_POINT,
                                DEFAULT_RELEASE_POINT,
                                None,
                                None,
                            );
                        }
                    },
                    "Reset all to defaults"
                }
            }
            // Entering selects EVERY cap and opens the panel; exiting clears the
            // selection. While on, cap clicks toggle membership (see `pointer`).
            button {
                class: if edit_mode() { "btn edit-toggle on" } else { "btn edit-toggle" },
                onclick: move |_| {
                    if *edit_mode.peek() {
                        edit_mode.set(false);
                        multi.write().clear();
                    } else {
                        store.write().selected = None;
                        multi.set(all_analog_ids());
                        edit_mode.set(true);
                    }
                },
                if edit_mode() { "Exit Edit Mode" } else { "Edit Mode" }
            }
        }

        Stage { state, edit_mode, multi }
        }
    }
}

/// The keypad stage: the rendered Tartarus in `Pressure` mode (each grid cap
/// fills by its current pressure) plus a floating actuation editor next to the
/// selected key. Always rendered — offline/digital just means an empty pressure
/// map (caps with no fill); the daemon's built-in analog map means every grid
/// cap is configured, so in digital mode the editor still opens and its edits
/// persist for the next analog run. Replaces the old scrollable card list.
#[component]
fn Stage(
    state: Option<AnalogState>,
    edit_mode: Signal<bool>,
    multi: Signal<BTreeSet<KeyId>>,
) -> Element {
    let mut store = use_store();

    // `edit_mode`/`multi` live in Calibration (the toolbar toggle drives them);
    // `dragging` is stage-local — it only gates the drag-extend.
    let mut dragging = use_signal(|| false);

    // Move the two Vecs OUT of the by-value prop instead of cloning them —
    // nothing below reads `state` itself, and this runs at the poll rate.
    let (keys, frame) = state.map(|s| (s.keys, s.pressures)).unwrap_or_default();

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
    // it. Analog actuation isn't grid-only: the spacebar (`KEY_SPACE`) is analog
    // byte 19 too (the thumb button/thumbpad/wheel are digital-only, with no
    // pressure byte), so we match ANY bindable cap by its evdev name against the
    // analog config (the `pressures` map is already keyed the same way) — digital
    // caps simply never appear in that config. `open_left` opens the 240px popover toward the stage
    // centre for right-side caps (right grid columns + the thumb cluster) so it
    // never runs off-screen. Unconfigured caps, the wheel, and the whole offline
    // path resolve to `None` — no editor opens.
    // In Edit Mode the single-key editor never opens — the selection belongs to
    // the multi set and the EditPanel.
    let selected_cfg: Option<(AnalogKeyConfig, bool)> = if edit_mode() {
        None
    } else {
        store.read().selected.and_then(|sel| {
            let name = sel.evdev()?;
            let cfg = keys.iter().find(|c| c.key == name).cloned()?;
            Some((cfg, opens_left(sel)))
        })
    };

    // Resolve the multi-selection to the analog configs we can bulk-edit (caps
    // with no analog config — e.g. an unmapped key — are silently skipped).
    let multi_cfgs: Vec<AnalogKeyConfig> = multi()
        .iter()
        .filter_map(|k| {
            let name = k.evdev()?;
            keys.iter().find(|c| c.key == name).cloned()
        })
        .collect();

    // The deepest live press among the SELECTED caps — the EditPanel's pressure
    // indicator. Follows the poll (this component re-renders on each change).
    let live_max: u8 = multi_cfgs
        .iter()
        .filter_map(|c| frame.get(c.index as usize).copied())
        .max()
        .unwrap_or(0);

    // Caps whose actuation deviates from the defaults — the keypad marks them
    // with a dot so customized keys read at a glance.
    let customized: BTreeSet<String> = keys
        .iter()
        .filter(|c| c.is_custom())
        .map(|c| c.key.clone())
        .collect();

    // Cap pointer handler. In Edit Mode: Down toggles the cap in/out of the
    // selection and starts a drag; Enter (button held) sweeps caps in. Outside:
    // a plain click single-selects (the cap-anchored editor); a drag across >1
    // caps — or a Shift+click — enters Edit Mode with that selection. Shift no
    // longer waits for release (the old bulk modal did; the EditPanel handles a
    // live selection), which is what let the stage drop its whole DOM-focus /
    // onkeyup Shift-release apparatus.
    let pointer = EventHandler::new(move |p: CapPointer| match p.phase {
        CapPhase::Down => {
            if *edit_mode.peek() {
                // Toggle membership; a drag from here sweeps more caps in.
                let mut m = multi.write();
                if !m.remove(&p.key) {
                    m.insert(p.key);
                }
                drop(m);
                dragging.set(true);
            } else if p.shift {
                // Shift+click enters Edit Mode immediately with this cap.
                multi.write().insert(p.key);
                edit_mode.set(true);
                dragging.set(true);
                store.write().selected = None;
            } else {
                // Begin a drag (also covers a plain click = a drag of one cap).
                dragging.set(true);
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
                // In Edit Mode there is nothing to resolve on release: the panel
                // is already open and Down/Enter built the selection live.
                if *edit_mode.peek() {
                    return;
                }
                let set = multi.peek().clone();
                if set.len() > 1 {
                    // A real multi-key drag → Edit Mode with that selection.
                    edit_mode.set(true);
                    store.write().selected = None;
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
        div {
            class: "stage",
            style: "position:relative; z-index:51; outline:none;",
            // Click the bare stage to clear the single selection and end any drag.
            // A cross-cap drag emits a trailing synthetic click on the caps' common
            // ancestor (a keypad div that doesn't stop propagation) which bubbles
            // here AFTER CapPhase::Up entered Edit Mode — so in Edit Mode the
            // multi-selection must survive (exiting is the toolbar button's job);
            // here we only drop a still-accumulating selection.
            onclick: move |_| {
                store.write().selected = None;
                dragging.set(false);
                if !*edit_mode.peek() {
                    multi.write().clear();
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
                customized,
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

        }

        // The Edit Mode drawer. A sibling of the stage (position:fixed, so it
        // escapes the stage's stacking context cleanly) with NO backdrop — the
        // caps must stay clickable so the selection can be reshaped live.
        if edit_mode() {
            EditPanel {
                cfgs: multi_cfgs,
                // The RAW selection size — offline, caps select fine but resolve
                // to no configs, and the head must not claim "0 selected" while
                // 20 caps visibly carry the ring.
                selected: multi().len(),
                live_max,
                on_select_all: move |_| multi.set(all_analog_ids()),
                on_clear: move |_| multi.write().clear(),
            }
        }
    }
}

/// The Edit Mode drawer: actuation + release points only (no dual-step / second
/// action, per the multi-edit spec), applied to EVERY selected analog key at
/// once. Each key keeps its own existing second function — we forward `second_*`
/// untouched, so a bulk threshold change never wipes a per-key dual-step.
///
/// Docked to the right edge and slid in via `.cal-panel`, with NO backdrop: the
/// caps stay clickable, so the selection can be reshaped while the panel is
/// open. That is also why `cfgs` is used as a plain prop, not a mount-time
/// snapshot — the apply closures are rebuilt each render and always target the
/// CURRENT selection. The sliders seed from the first selected key when the
/// panel mounts (entering Edit Mode); reshaping the selection keeps the current
/// slider values (they are what you're about to apply, not a per-key readback).
/// The live bar shows the deepest press among the selected caps right now.
#[component]
fn EditPanel(
    cfgs: Vec<AnalogKeyConfig>,
    selected: usize,
    live_max: u8,
    on_select_all: EventHandler<()>,
    on_clear: EventHandler<()>,
) -> Element {
    let store = use_store();
    let count = cfgs.len();
    let seed_act = cfgs
        .first()
        .map_or(DEFAULT_ACTUATION_POINT, |c| c.actuation_point);
    let seed_rel = cfgs
        .first()
        .map_or(DEFAULT_RELEASE_POINT, |c| c.release_point);
    let mut actuation = use_signal(|| seed_act);
    let mut release = use_signal(|| seed_rel);

    // Push the given actuation/release to every selected analog key, preserving
    // each key's own dual-step (`second_*` forwarded verbatim). Cloned per
    // slider — the capture is the current `cfgs`, not a signal.
    let apply = {
        let cfgs = cfgs.clone();
        move |act_raw: u8, rel_raw: u8| {
            for c in &cfgs {
                // A key WITH a dual-step caps its primary at 254: the second
                // point must be STRICTLY deeper, and `saturating_add(1)` at 255
                // would otherwise send second == actuation, violating the
                // SetActuation contract. Release re-derives from the (possibly
                // capped) value so hysteresis holds too.
                let act = if c.second_actuation_point.is_some() {
                    act_raw.min(254)
                } else {
                    act_raw
                };
                let rel = rel_raw.min(act.saturating_sub(1)); // hysteresis: release < actuation
                let sap = c
                    .second_actuation_point
                    .map(|p| p.max(act.saturating_add(1))); // deeper than actuation
                crate::ipc::set_actuation(store, c.index, act, rel, sap, c.second_action.clone());
            }
        }
    };
    let apply_rel = apply.clone();

    let act_v = actuation();
    let rel_v = release();
    let names: String = cfgs
        .iter()
        .map(|c| pretty_key(&c.key))
        .collect::<Vec<_>>()
        .join(", ");

    // Marker/fill positions as 0..100% of the 0–255 scale (same as CalModal).
    let pct = |v: u8| (v as f32 / 255.0 * 100.0).clamp(0.0, 100.0);

    rsx! {
        div {
            class: "cal-panel",
            onclick: move |e| e.stop_propagation(),
            div { class: "cal-modal-head",
                span { "Edit Mode · {selected} selected" }
            }

            // Selection controls, top of the panel per the spec.
            div { class: "btn-row", style: "margin-bottom:14px;",
                button { class: "btn", onclick: move |_| on_clear.call(()), "Clear all" }
                button { class: "btn", onclick: move |_| on_select_all.call(()), "Select all" }
            }

            if selected == 0 {
                div { class: "field-value", style: "color:var(--mute);",
                    "No keys selected — click caps on the board to add them."
                }
            } else if count == 0 {
                // Caps are selected but none resolves to an analog config — the
                // offline case. Honest, instead of sliders that would edit nothing.
                div { class: "field-value", style: "color:var(--mute);",
                    "The selected caps have no analog config to edit — daemon offline?"
                }
            } else {
                // The deepest press among the selected caps at this moment, with
                // the pending actuation/release marks for reference.
                div { class: "field",
                    label { class: "field-label", "Live pressure (max of selection)" }
                    div { class: "press-bar",
                        div { class: "press-fill", style: "width:{pct(live_max)}%;" }
                        div { class: "press-mark release", style: "left:{pct(rel_v)}%;", title: "release" }
                        div { class: "press-mark actuate", style: "left:{pct(act_v)}%;", title: "actuation" }
                    }
                    div { class: "press-readout", "{live_max} / 255 · {pct100(live_max)}%" }
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
                        if release() >= nv {
                            release.set(nv.saturating_sub(1));
                        }
                        apply(actuation(), release());
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
                        apply_rel(actuation(), release());
                    },
                }

                div { class: "field-value", style: "color:var(--mute);margin-top:12px;",
                    "Applies to every selected key. Each key keeps its own second function."
                }
                div { class: "field-value", style: "color:var(--mute);margin-top:8px;", "{names}" }
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
        // With a dual-step the primary caps at 254: the second point must be
        // STRICTLY deeper, and `saturating_add(1)` at 255 would otherwise send
        // second == actuation, violating the SetActuation contract. (The slider
        // max and the toggle handler keep the local signals ≤254 while the
        // dual-step is on; this is the backstop.)
        let act = if second_enabled() {
            actuation().min(254)
        } else {
            actuation()
        };
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
    let modal_class = if open_left {
        "cal-modal open-left"
    } else {
        "cal-modal"
    };

    rsx! {
        div {
            class: "{modal_class}",
            // The modal lives INSIDE the selected cap; stop clicks from bubbling to
            // the cap (which would re-select / toggle) or the stage (which closes).
            // The × and the cap toggle still close it; interacting with the editor
            // never does.
            onclick: move |e| e.stop_propagation(),
            // Mouse press/release must not bubble either: the host cap listens to
            // mousedown/mouseup for the drag/multi-select pointer protocol, and an
            // unstopped down+up pair on any control here (the dual-step toggle was
            // the visible victim) reads as a cap click — closing the editor.
            onmousedown: move |e| e.stop_propagation(),
            onmouseup: move |e| e.stop_propagation(),
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
                // 254 while the dual-step is on: the second point needs strictly
                // deeper headroom (see `push`).
                min: 1.0, max: if second_on { 254.0 } else { 255.0 }, step: 1.0,
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
                    if now {
                        // A dual-step needs strictly-deeper headroom: pull a
                        // bottomed-out primary back to 254 (the slider max also
                        // drops to 254 while enabled), then seed a sensible
                        // deeper point if unset.
                        if actuation() > 254 {
                            actuation.set(254);
                        }
                        if second_point() <= actuation() {
                            second_point.set(actuation().saturating_add(1));
                        }
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
                    div { class: "field-value", style: "color:var(--mute);margin-top:6px;",
                        "Fires when pressed past the second point. Must be deeper than actuation."
                    }
                }
            }

            div { class: "divider" }

            // Reset this key to the default thresholds and drop any dual-step
            // (mirrors the Customize editor's reset affordance).
            div { class: "btn-row",
                button {
                    class: "btn",
                    onclick: move |_| {
                        actuation.set(DEFAULT_ACTUATION_POINT);
                        release.set(DEFAULT_RELEASE_POINT);
                        second_enabled.set(false);
                        push();
                    },
                    "Reset to default"
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
            " — start tartarust-daemon to read live key pressure and edit actuation."
        }
    }
}

/// Daemon up but running in DIGITAL mode: no live pressure to show (edits still
/// apply and persist for the next analog run). Point at the fix.
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

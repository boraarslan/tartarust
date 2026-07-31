//! Calibration — live "heatmap" keypad you click to tune a key, wired to LIVE
//! daemon state.
//!
//! In analog mode the daemon streams per-key pressure (0..255) and exposes the
//! actuation config over IPC. This screen polls `GetAnalogState` on a short
//! interval *while this view is alive* into a SCREEN-LOCAL field (never the
//! global store, so a pressure tick doesn't repaint the whole shell) and renders
//! the Tartarus in `Pressure` mode: each of the 20 analog caps fills from the
//! bottom by its live pressure. Clicking a cap selects it (the shared
//! `AppState::selected`) and, if it has an analog config, opens a floating
//! actuation editor anchored INSIDE that cap:
//!   * a live pressure bar with the actuation + release points marked, and
//!   * sliders for actuation / release (with hysteresis) plus an optional
//!     dual-step (deeper second actuation + a second evdev action).
//!
//! Bulk edits go through **Edit Mode** (the toolbar toggle, top right): entering
//! selects every cap and slides in the right-hand edit panel — actuation and
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
//! state seeded from the config (re-seeded when the selection moves to another
//! analog key), so dragging never fights the refresh.
//!
//! Offline (no daemon — the cage harness) or in digital mode there are no live
//! bars; each non-live state renders an intentional banner, not dead sliders.
//!
//! **Poll lifetime.** The [`gpui::Task`] holding the loop lives in
//! [`Calibration::_poll`], and dropping a task cancels it — so the poll stops
//! when this view is dropped, which is what the old `use_future`'s
//! cancel-on-unmount bought. A shell that CACHES the screen entity across tab
//! switches keeps polling off-screen; drop the entity (or re-create it per
//! visit) to preserve the old behaviour.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, MouseButton, Rgba, SharedString, Task,
    Window, deferred, div, linear_color_stop, linear_gradient, relative, rgb, rgba,
};
use gpui_component::slider::{SliderEvent, SliderState};

use tartarust_ipc::{AnalogKeyConfig, AnalogState, DEFAULT_ACTUATION_POINT, DEFAULT_RELEASE_POINT};

use crate::components::widgets::{btn_ghost, labeled_slider, page_head, switch_row};
use crate::components::{
    CapPhase, CapPointer, KeyPicker, KeyPickerEvent, Keypad, KeypadMode, key_picker_trigger,
};
use crate::ipc;
use crate::model::{AppState, GRID_EVDEV, KeyId, Layer, pretty_key};
use crate::store;
use crate::theme;

/// How often to re-poll the live analog state while this view is alive.
const POLL_INTERVAL_MS: u64 = 100;

/// The floating single-key editor's width. The old stylesheet said 240px; the
/// ported [`KeyPicker`] panel is a fixed 280px and now renders INSIDE this
/// editor (it no longer carries its own popover), so the editor takes that width
/// and the picker fits exactly.
const MODAL_W: f32 = 280.0;

/// The Edit Mode drawer's width and its inset from the screen region's edge.
const PANEL_W: f32 = 300.0;
const PANEL_INSET: f32 = 10.0;
/// Right padding the page takes while the drawer is open, so the toolbar — and
/// with it the "Exit Edit Mode" button, the mode's only exit — is never covered
/// (the stylesheet's `.cal-page.panel-open { margin-right: 332px }`).
const PANEL_SHIFT: f32 = PANEL_W + PANEL_INSET + 22.0;

/// Cap widths the actuation popover can anchor against. The keypad's geometry
/// constants are private to that module, so the two this screen can host on —
/// a grid cap and the (much wider) spacebar — are mirrored here. The stylesheet
/// opened the popover at `left/right: 105%` of its host cap.
const GRID_CAP_W: f32 = 48.0;
const SPACE_CAP_W: f32 = 130.0;

/// Seed for a dual-step's second point when the key has none yet, and for its
/// second action. Both are what the daemon's own defaults imply, not policy.
const SEED_SECOND_POINT: u8 = 0xC0;
const SEED_SECOND_ACTION: &str = "KEY_LEFTSHIFT";

/// Local, optimistic edit state for the single-key actuation editor. Seeded from
/// the daemon-reported config and re-seeded when the selection moves to a
/// different analog key; a pressure tick never touches it (only the live bar
/// reads the poll), so dragging a slider can't be yanked mid-drag.
struct KeyEdit {
    /// The analog BYTE index `SetActuation` is keyed by — also this edit's
    /// identity, so the re-seed fires exactly when the selection changes key.
    index: u8,
    actuation: u8,
    release: u8,
    second_on: bool,
    second_point: u8,
    second_action: String,
}

impl KeyEdit {
    fn seed(cfg: &AnalogKeyConfig) -> Self {
        Self {
            index: cfg.index,
            actuation: cfg.actuation_point,
            release: cfg.release_point,
            second_on: cfg.second_actuation_point.is_some(),
            second_point: cfg.second_actuation_point.unwrap_or(SEED_SECOND_POINT),
            second_action: cfg
                .second_action
                .clone()
                .unwrap_or_else(|| SEED_SECOND_ACTION.to_string()),
        }
    }
}

pub struct Calibration {
    store: Entity<AppState>,
    /// Live analog state: `None` until the first successful poll, and back to
    /// `None` whenever the daemon goes away. `None` == offline / not-yet-loaded
    /// (the cage-harness path) — we render an honest offline state, never fake
    /// bars.
    analog: Option<AnalogState>,
    /// Edit Mode: the docked bulk editor is open iff this is set.
    edit_mode: bool,
    /// The multi-selection (Edit Mode's target, and a drag's accumulator).
    multi: BTreeSet<KeyId>,
    /// A cap drag is in progress — gates the drag-extend on `CapPhase::Enter`.
    dragging: bool,

    /// The single-key editor's local edit state, live only while its cap is
    /// selected AND resolves to an analog config.
    edit: Option<KeyEdit>,
    act_slider: Entity<SliderState>,
    rel_slider: Entity<SliderState>,
    second_slider: Entity<SliderState>,

    /// Bulk (Edit Mode) values. Seeded from the first selected key when Edit
    /// Mode opens; reshaping the selection keeps them — they are what you are
    /// about to apply, not a per-key readback.
    bulk_act: u8,
    bulk_rel: u8,
    bulk_act_slider: Entity<SliderState>,
    bulk_rel_slider: Entity<SliderState>,

    /// The second-action picker, and whether its panel is showing.
    picker: Entity<KeyPicker>,
    picker_open: bool,

    /// The 100 ms poll. Held, not detached: dropping the task cancels it.
    _poll: Task<()>,
}

impl Calibration {
    pub fn new(store: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();

        // Entering Calibration starts with NOTHING selected — don't inherit a
        // key selection (and auto-open its actuation editor) carried over from
        // Customize or a prior visit.
        store.update(cx, |s, cx| {
            if s.selected.is_some() {
                s.selected = None;
                cx.notify();
            }
        });

        let act_slider = cx.new(|_| slider_state(1.0, 255.0, DEFAULT_ACTUATION_POINT));
        let rel_slider = cx.new(|_| slider_state(0.0, 254.0, DEFAULT_RELEASE_POINT));
        let second_slider = cx.new(|_| slider_state(1.0, 255.0, SEED_SECOND_POINT));
        let bulk_act_slider = cx.new(|_| slider_state(1.0, 255.0, DEFAULT_ACTUATION_POINT));
        let bulk_rel_slider = cx.new(|_| slider_state(0.0, 254.0, DEFAULT_RELEASE_POINT));

        // Each slider drives its value + the dependent thresholds, then pushes.
        // The clamps mirror the old `on_input` handlers exactly; where a clamp
        // moves a value the owning slider is re-seeded so its thumb follows.
        cx.subscribe(&act_slider, |this: &mut Self, _, ev: &SliderEvent, cx| {
            let SliderEvent::Change(v) = ev;
            this.set_actuation(slider_u8(v.start()), cx);
        })
        .detach();
        cx.subscribe(&rel_slider, |this: &mut Self, _, ev: &SliderEvent, cx| {
            let SliderEvent::Change(v) = ev;
            this.set_release(slider_u8(v.start()), cx);
        })
        .detach();
        cx.subscribe(
            &second_slider,
            |this: &mut Self, _, ev: &SliderEvent, cx| {
                let SliderEvent::Change(v) = ev;
                this.set_second_point(slider_u8(v.start()), cx);
            },
        )
        .detach();
        cx.subscribe(
            &bulk_act_slider,
            |this: &mut Self, _, ev: &SliderEvent, cx| {
                let SliderEvent::Change(v) = ev;
                this.set_bulk_actuation(slider_u8(v.start()), cx);
            },
        )
        .detach();
        cx.subscribe(
            &bulk_rel_slider,
            |this: &mut Self, _, ev: &SliderEvent, cx| {
                let SliderEvent::Change(v) = ev;
                this.set_bulk_release(slider_u8(v.start()), cx);
            },
        )
        .detach();

        let picker = cx.new(|cx| KeyPicker::new(window, cx));
        cx.subscribe(&picker, |this: &mut Self, _, ev: &KeyPickerEvent, cx| {
            this.picker_open = false;
            if let KeyPickerEvent::Picked(evdev) = ev {
                if let Some(e) = this.edit.as_mut() {
                    e.second_action = (*evdev).to_string();
                }
                this.push_key_edit(cx);
            }
            cx.notify();
        })
        .detach();

        // Poll `GetAnalogState` on an interval. The blocking read runs on the
        // background executor; the result lands back on the foreground. Writing
        // only on a real change keeps an idle keypad (the same frame every tick)
        // from repainting this view 10x/second.
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(POLL_INTERVAL_MS))
                    .await;
                let next = cx
                    .background_executor()
                    .spawn(async { ipc::get_analog_state() })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if this.analog != next {
                        this.analog = next;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return; // the view (or the app) is gone
                }
            }
        });

        let mut this = Self {
            store,
            analog: None,
            edit_mode: false,
            multi: BTreeSet::new(),
            dragging: false,
            edit: None,
            act_slider,
            rel_slider,
            second_slider,
            bulk_act: DEFAULT_ACTUATION_POINT,
            bulk_rel: DEFAULT_RELEASE_POINT,
            bulk_act_slider,
            bulk_rel_slider,
            picker,
            picker_open: false,
            _poll: poll,
        };

        // Dev-only: let the headless screenshot harness open Edit Mode directly
        // (TARTARUST_CAL_EDIT=1) — the drawer is otherwise unreachable without a
        // click. No effect in normal use (the var is unset).
        if std::env::var("TARTARUST_CAL_EDIT").as_deref() == Ok("1") {
            this.multi = all_analog_ids();
            this.enter_edit_mode(cx);
        }
        this
    }

    // --- Derived views of the polled state ---------------------------------

    /// Per-key pressure for the keypad overlay, keyed by evdev name: for each
    /// configured analog key, its byte in the live frame. Grid caps the daemon
    /// hasn't mapped (and the digital thumb/wheel) get nothing; offline the map
    /// is simply empty.
    fn pressures_map(&self) -> HashMap<String, u8> {
        let Some(a) = self.analog.as_ref() else {
            return HashMap::new();
        };
        a.keys
            .iter()
            .map(|cfg| {
                (
                    cfg.key.clone(),
                    a.pressures.get(cfg.index as usize).copied().unwrap_or(0),
                )
            })
            .collect()
    }

    /// One analog key's live pressure, by byte index.
    fn pressure_of(&self, index: u8) -> u8 {
        self.analog
            .as_ref()
            .and_then(|a| a.pressures.get(index as usize).copied())
            .unwrap_or(0)
    }

    /// The analog config for a cap, when the daemon has one. Analog actuation
    /// isn't grid-only — the spacebar (`KEY_SPACE`) is an analog byte too — so
    /// any bindable cap is matched by evdev name; digital caps (the thumb
    /// button, thumbpad, wheel) simply never appear in that config.
    fn cfg_for(&self, key: KeyId) -> Option<AnalogKeyConfig> {
        let name = key.evdev()?;
        self.analog
            .as_ref()?
            .keys
            .iter()
            .find(|c| c.key == name)
            .cloned()
    }

    /// The multi-selection resolved to the configs we can bulk-edit (caps with
    /// no analog config — e.g. offline — are silently skipped).
    fn multi_cfgs(&self) -> Vec<AnalogKeyConfig> {
        self.multi.iter().filter_map(|k| self.cfg_for(*k)).collect()
    }

    /// Caps whose actuation deviates from the defaults — the keypad marks them
    /// with a dot so customized keys read at a glance.
    fn customized_set(&self) -> BTreeSet<String> {
        self.analog
            .as_ref()
            .map(|a| {
                a.keys
                    .iter()
                    .filter(|c| c.is_custom())
                    .map(|c| c.key.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- Selection / mode --------------------------------------------------

    /// Drop the single-key selection (closing its editor). The local edit state
    /// goes with it, so re-selecting the same key re-seeds from the daemon.
    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.edit = None;
        self.picker_open = false;
        self.store.update(cx, |s, cx| {
            if s.selected.is_some() {
                s.selected = None;
                cx.notify();
            }
        });
    }

    /// Open Edit Mode over the CURRENT `multi` set, seeding the bulk sliders
    /// from the first selected key (defaults when nothing resolves — the
    /// offline case). Every entry point (toolbar, Shift+click, drag release)
    /// comes through here, which is what mounting the old panel did.
    fn enter_edit_mode(&mut self, cx: &mut Context<Self>) {
        let first = self.multi_cfgs().into_iter().next();
        self.bulk_act = first
            .as_ref()
            .map_or(DEFAULT_ACTUATION_POINT, |c| c.actuation_point);
        self.bulk_rel = first
            .as_ref()
            .map_or(DEFAULT_RELEASE_POINT, |c| c.release_point);
        seed_slider(&self.bulk_act_slider, 1.0, 255.0, self.bulk_act, cx);
        seed_slider(&self.bulk_rel_slider, 0.0, 254.0, self.bulk_rel, cx);
        self.edit_mode = true;
        nudge_next_frame(cx);
    }

    /// The toolbar toggle: entering selects EVERY analog cap, exiting clears the
    /// selection.
    fn toggle_edit_mode(&mut self, cx: &mut Context<Self>) {
        if self.edit_mode {
            self.edit_mode = false;
            self.multi.clear();
        } else {
            self.clear_selection(cx);
            self.multi = all_analog_ids();
            self.enter_edit_mode(cx);
        }
        cx.notify();
    }

    /// Cap pointer protocol. In Edit Mode: Down toggles the cap in/out of the
    /// selection and starts a drag; Enter (button held) sweeps caps in. Outside:
    /// a plain click single-selects (the cap-anchored editor); a drag across >1
    /// caps — or a Shift+click — enters Edit Mode with that selection.
    ///
    /// `Enter` arrives once per mouse-move sample over a cap rather than once
    /// per entry (the ported keypad rides `on_mouse_move`), so the extend must
    /// stay idempotent — inserting an already-selected cap is.
    fn on_cap_pointer(&mut self, p: CapPointer, cx: &mut Context<Self>) {
        match p.phase {
            CapPhase::Down => {
                if self.edit_mode {
                    // Toggle membership; a drag from here sweeps more caps in.
                    if !self.multi.remove(&p.key) {
                        self.multi.insert(p.key);
                    }
                    self.dragging = true;
                } else if p.shift {
                    // Shift+click enters Edit Mode immediately with this cap.
                    self.multi.insert(p.key);
                    self.enter_edit_mode(cx);
                    self.dragging = true;
                    self.clear_selection(cx);
                } else {
                    // Begin a drag (also covers a plain click = a drag of one).
                    self.dragging = true;
                    self.multi = BTreeSet::from([p.key]);
                }
                cx.notify();
            }
            CapPhase::Enter => {
                // Extend only while the primary button is actually held — a
                // stuck `dragging` (drag released off-cap) must not let a
                // buttonless hover keep adding caps. `insert` reports whether
                // the set actually grew, so the repeat samples over one cap
                // cost no repaint.
                if self.dragging && p.primary_held && self.multi.insert(p.key) {
                    cx.notify();
                }
            }
            CapPhase::Up => {
                if !self.dragging {
                    return;
                }
                self.dragging = false;
                // In Edit Mode there is nothing to resolve on release: the panel
                // is already open and Down/Enter built the selection live.
                if self.edit_mode {
                    cx.notify();
                    return;
                }
                if self.multi.len() > 1 {
                    // A real multi-key drag → Edit Mode with that selection.
                    self.enter_edit_mode(cx);
                    self.clear_selection(cx);
                } else {
                    // A single click: fall back to the normal single-key editor,
                    // and re-clicking the already-selected cap closes it.
                    let one = self.multi.iter().next().copied();
                    self.multi.clear();
                    if let Some(k) = one {
                        self.edit = None;
                        self.picker_open = false;
                        self.store.update(cx, |s, cx| {
                            s.selected = if s.selected == Some(k) { None } else { Some(k) };
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            }
        }
    }

    // --- Single-key edits ---------------------------------------------------

    /// Re-seed the editor (and its sliders) for a newly selected analog key.
    fn seed_key_edit(&mut self, cfg: &AnalogKeyConfig, cx: &mut Context<Self>) {
        let edit = KeyEdit::seed(cfg);
        seed_slider(
            &self.act_slider,
            1.0,
            act_max(edit.second_on),
            edit.actuation,
            cx,
        );
        seed_slider(&self.rel_slider, 0.0, 254.0, edit.release, cx);
        seed_slider(&self.second_slider, 1.0, 255.0, edit.second_point, cx);
        self.edit = Some(edit);
        self.picker_open = false;
        nudge_next_frame(cx);
    }

    /// Send the current edit state to the daemon, upholding the schema
    /// invariants. Dual-step off => BOTH second fields cleared (never a half-set
    /// second step). The second action comes from the picker, so it is already a
    /// valid evdev name — no text coercion needed.
    fn push_key_edit(&self, cx: &mut App) {
        let Some(e) = self.edit.as_ref() else { return };
        // With a dual-step the primary caps at 254: the second point must be
        // STRICTLY deeper, and `saturating_add(1)` at 255 would otherwise send
        // second == actuation, violating the SetActuation contract. (The slider
        // max and the toggle handler keep the local value <= 254 while the
        // dual-step is on; this is the backstop.)
        let act = if e.second_on {
            e.actuation.min(254)
        } else {
            e.actuation
        };
        let rel = e.release.min(act.saturating_sub(1)); // hysteresis: release < actuation
        let (sap, sact) = if e.second_on {
            let p = e.second_point.max(act.saturating_add(1)); // deeper than actuation
            (Some(p), Some(e.second_action.clone()))
        } else {
            (None, None)
        };
        store::fire(
            &self.store,
            ipc::req_set_actuation(e.index, act, rel, sap, sact),
            cx,
        );
    }

    fn set_actuation(&mut self, value: u8, cx: &mut Context<Self>) {
        let Some(e) = self.edit.as_mut() else { return };
        e.actuation = value;
        // Keep the dependent thresholds consistent as actuation moves, so the
        // markers never render on the wrong side of it: release strictly below,
        // the second step strictly above.
        let moved_release = e.release >= value;
        if moved_release {
            e.release = value.saturating_sub(1);
        }
        let moved_second = e.second_on && e.second_point <= value;
        if moved_second {
            e.second_point = value.saturating_add(1);
        }
        let (release, second) = (e.release, e.second_point);
        if moved_release {
            seed_slider(&self.rel_slider, 0.0, 254.0, release, cx);
        }
        if moved_second {
            seed_slider(&self.second_slider, 1.0, 255.0, second, cx);
        }
        self.push_key_edit(cx);
        cx.notify();
    }

    fn set_release(&mut self, value: u8, cx: &mut Context<Self>) {
        let Some(e) = self.edit.as_mut() else { return };
        // Clamp below actuation (hysteresis): a release >= actuation would
        // chatter, and the daemon rejects it.
        let clamped = value.min(e.actuation.saturating_sub(1));
        e.release = clamped;
        if clamped != value {
            seed_slider(&self.rel_slider, 0.0, 254.0, clamped, cx);
        }
        self.push_key_edit(cx);
        cx.notify();
    }

    fn set_second_point(&mut self, value: u8, cx: &mut Context<Self>) {
        let Some(e) = self.edit.as_mut() else { return };
        // Must be deeper than the primary actuation point.
        let clamped = value.max(e.actuation.saturating_add(1));
        e.second_point = clamped;
        if clamped != value {
            seed_slider(&self.second_slider, 1.0, 255.0, clamped, cx);
        }
        self.push_key_edit(cx);
        cx.notify();
    }

    fn toggle_second(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(e) = self.edit.as_mut() else { return };
        e.second_on = on;
        if on {
            // A dual-step needs strictly-deeper headroom: pull a bottomed-out
            // primary back to 254 (the slider max also drops to 254 while
            // enabled), then seed a sensible deeper point if unset.
            e.actuation = e.actuation.min(254);
            if e.second_point <= e.actuation {
                e.second_point = e.actuation.saturating_add(1);
            }
        }
        let (act, second) = (e.actuation, e.second_point);
        seed_slider(&self.act_slider, 1.0, act_max(on), act, cx);
        seed_slider(&self.second_slider, 1.0, 255.0, second, cx);
        self.push_key_edit(cx);
        cx.notify();
    }

    /// Reset this key to the default thresholds and drop any dual-step (mirrors
    /// the Customize editor's reset affordance).
    fn reset_key(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.edit.as_mut() else { return };
        e.actuation = DEFAULT_ACTUATION_POINT;
        e.release = DEFAULT_RELEASE_POINT;
        e.second_on = false;
        seed_slider(&self.act_slider, 1.0, 255.0, DEFAULT_ACTUATION_POINT, cx);
        seed_slider(&self.rel_slider, 0.0, 254.0, DEFAULT_RELEASE_POINT, cx);
        self.picker_open = false;
        self.push_key_edit(cx);
        cx.notify();
    }

    /// Page-level reset: return EVERY custom analog key to the default
    /// thresholds (dropping any dual-step). Like any other edit it applies live
    /// and persists on Save.
    fn reset_all(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<AnalogKeyConfig> = self
            .analog
            .as_ref()
            .map(|a| a.keys.clone())
            .unwrap_or_default();
        // Close an open editor: its sliders hold local edit state that would go
        // stale under the bulk reset.
        self.clear_selection(cx);
        for k in keys.iter().filter(|k| k.is_custom()) {
            store::fire(
                &self.store,
                ipc::req_set_actuation(
                    k.index,
                    DEFAULT_ACTUATION_POINT,
                    DEFAULT_RELEASE_POINT,
                    None,
                    None,
                ),
                cx,
            );
        }
        cx.notify();
    }

    // --- Bulk (Edit Mode) edits ---------------------------------------------

    /// Push the current bulk actuation/release to every selected analog key,
    /// preserving each key's own dual-step (`second_*` forwarded verbatim), so a
    /// bulk threshold change never wipes a per-key second function.
    fn apply_bulk(&self, cx: &mut App) {
        for c in self.multi_cfgs() {
            // A key WITH a dual-step caps its primary at 254: the second point
            // must be STRICTLY deeper, and `saturating_add(1)` at 255 would
            // otherwise send second == actuation. Release re-derives from the
            // (possibly capped) value so hysteresis holds too.
            let act = if c.second_actuation_point.is_some() {
                self.bulk_act.min(254)
            } else {
                self.bulk_act
            };
            let rel = self.bulk_rel.min(act.saturating_sub(1));
            let sap = c
                .second_actuation_point
                .map(|p| p.max(act.saturating_add(1)));
            store::fire(
                &self.store,
                ipc::req_set_actuation(c.index, act, rel, sap, c.second_action.clone()),
                cx,
            );
        }
    }

    fn set_bulk_actuation(&mut self, value: u8, cx: &mut Context<Self>) {
        self.bulk_act = value;
        if self.bulk_rel >= value {
            self.bulk_rel = value.saturating_sub(1);
            seed_slider(&self.bulk_rel_slider, 0.0, 254.0, self.bulk_rel, cx);
        }
        self.apply_bulk(cx);
        cx.notify();
    }

    fn set_bulk_release(&mut self, value: u8, cx: &mut Context<Self>) {
        let clamped = value.min(self.bulk_act.saturating_sub(1));
        self.bulk_rel = clamped;
        if clamped != value {
            seed_slider(&self.bulk_rel_slider, 0.0, 254.0, clamped, cx);
        }
        self.apply_bulk(cx);
        cx.notify();
    }

    // --- Pieces -------------------------------------------------------------

    /// Page toolbar, top right: the conditional "Reset all" plus the Edit Mode
    /// toggle (always present — it is the bulk-edit entry point).
    fn toolbar(&self, any_custom: bool, cx: &Context<Self>) -> Div {
        let on = self.edit_mode;
        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_end()
            .gap(theme::sp(10.))
            .pb(theme::sp(12.))
            .when(any_custom, |el| {
                el.child(btn_ghost(
                    "cal-reset-all",
                    "Reset all to defaults",
                    cx.listener(|this, _: &ClickEvent, _, cx| this.reset_all(cx)),
                ))
            })
            .child(
                // The toggle's ON state is a rust ring — the same accent as the
                // selected caps — so the mode reads as "armed" at a glance.
                // `btn_ghost` has no lit variant, so the chassis is repeated
                // here rather than widened for one call site.
                div()
                    .id("cal-edit-toggle")
                    .px(theme::sp(12.))
                    .py(theme::sp(5.))
                    .rounded(theme::sp(theme::RADIUS_SM))
                    .border_1()
                    .text_size(theme::sp(12.))
                    .when(on, |el| {
                        el.border_color(theme::rust()).text_color(theme::rust())
                    })
                    .when(!on, |el| {
                        el.border_color(theme::line())
                            .text_color(theme::dim())
                            .hover(|s| s.text_color(theme::ink()).border_color(theme::ink()))
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_edit_mode(cx)))
                    .child(if on { "Exit Edit Mode" } else { "Edit Mode" }),
            )
    }

    /// The floating per-key actuation editor, anchored INSIDE its cap by the
    /// keypad (every cap is `relative()`), so it lands beside whichever cap was
    /// clicked with no row/column math the 5/5/5/4 layout would break.
    ///
    /// `open_left` opens it toward the board's centre for right-side caps so the
    /// panel never runs off-screen.
    fn cal_modal(
        &self,
        cfg: &AnalogKeyConfig,
        pressure: u8,
        open_left: bool,
        offset: f32,
        mono: &SharedString,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(e) = self.edit.as_ref() else {
            return div().into_any_element();
        };
        let cap = pretty_key(&cfg.key);
        let mut marks = vec![(e.release, theme::mute()), (e.actuation, theme::rust())];
        if e.second_on {
            marks.push((e.second_point, theme::amber()));
        }

        div()
            .absolute()
            .top(theme::sp(0.))
            .when(open_left, |el| el.right(theme::sp(offset)))
            .when(!open_left, |el| el.left(theme::sp(offset)))
            .w(theme::sp(MODAL_W))
            .flex()
            .flex_col()
            .gap(theme::sp(12.))
            .p(theme::sp(14.))
            .rounded(theme::sp(theme::RADIUS))
            .bg(theme::panel_1())
            .border_1()
            .border_color(theme::line())
            .shadow_lg()
            // The editor lives INSIDE the selected cap, whose pointer protocol
            // listens on mouse down/up: an unstopped pair on any control here
            // would read as a cap click and close the editor. The × and the cap
            // toggle still close it; interacting with the editor never does.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(theme::sp(10.))
                    .font_family(mono.clone())
                    .text_size(theme::sp(12.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("{cap} · #{}", e.index))
                    .child(
                        div()
                            .id("cal-modal-close")
                            .size(theme::sp(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme::sp(3.))
                            .text_size(theme::sp(11.))
                            .text_color(theme::mute())
                            .hover(|s| s.bg(theme::red_wash()).text_color(theme::red()))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.clear_selection(cx)),
                            )
                            .child("✕"),
                    ),
            )
            // Live pressure bar, with the actuation/release (and second-step)
            // marks. Only this follows the poll.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::sp(6.))
                    .child(field_label("Live pressure"))
                    .child(press_bar(pressure, &marks))
                    .child(press_readout(pressure, mono)),
            )
            .child(divider())
            .child(labeled_slider(
                "Actuation point",
                &self.act_slider,
                readout(e.actuation),
                false,
                cx,
            ))
            .child(labeled_slider(
                "Release point",
                &self.rel_slider,
                readout(e.release),
                false,
                cx,
            ))
            .child(divider())
            .child(switch_row(
                "cal-dual-step",
                "Dual-step (second function)",
                e.second_on,
                cx.listener(|this, on: &bool, _, cx| this.toggle_second(*on, cx)),
            ))
            .when(e.second_on, |el| {
                el.child(labeled_slider(
                    "Second actuation point",
                    &self.second_slider,
                    readout(e.second_point),
                    true,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(theme::sp(6.))
                        .child(field_label("Second action (key)"))
                        // A searchable picker (the same one the Customize editor
                        // uses), not free-text evdev: it always yields a valid
                        // evdev name, so the push needs no coercion. Picking
                        // sets + pushes live.
                        .child(key_picker_trigger(
                            "cal-second-key",
                            &e.second_action,
                            None,
                            mono.clone(),
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.picker_open = !this.picker_open;
                                if this.picker_open {
                                    this.picker.update(cx, |p, cx| p.reset(window, cx));
                                }
                                cx.notify();
                            }),
                        ))
                        .when(self.picker_open, |el| el.child(self.picker.clone()))
                        .child(note(
                            "Fires when pressed past the second point. Must be deeper than \
                             actuation.",
                        )),
                )
            })
            .child(divider())
            .child(btn_ghost(
                "cal-reset-key",
                "Reset to default",
                cx.listener(|this, _: &ClickEvent, _, cx| this.reset_key(cx)),
            ))
            .into_any_element()
    }

    /// The Edit Mode drawer: actuation + release only (no dual-step / second
    /// action, per the multi-edit spec), applied to EVERY selected analog key at
    /// once.
    ///
    /// Docked to the right edge with NO backdrop: the caps stay clickable, so
    /// the selection can be reshaped while the drawer is open. The apply closures
    /// therefore always target the CURRENT selection, and the live bar shows the
    /// deepest press among the selected caps right now.
    fn edit_panel(
        &self,
        cfgs: &[AnalogKeyConfig],
        mono: &SharedString,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        // The RAW selection size — offline, caps select fine but resolve to no
        // configs, and the head must not claim "0 selected" while 20 caps
        // visibly carry the ring.
        let selected = self.multi.len();
        let count = cfgs.len();
        let live_max = cfgs
            .iter()
            .map(|c| self.pressure_of(c.index))
            .max()
            .unwrap_or(0);
        let names: String = cfgs
            .iter()
            .map(|c| pretty_key(&c.key))
            .collect::<Vec<_>>()
            .join(", ");

        div()
            .id("cal-edit-panel")
            .absolute()
            .top(theme::sp(PANEL_INSET))
            .right(theme::sp(PANEL_INSET))
            .bottom(theme::sp(PANEL_INSET))
            .w(theme::sp(PANEL_W))
            .flex()
            .flex_col()
            .gap(theme::sp(12.))
            .p(theme::sp(18.))
            .rounded(theme::sp(theme::RADIUS))
            .bg(theme::panel_1())
            .border_1()
            .border_color(theme::line())
            .shadow_lg()
            .overflow_y_scroll()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .font_family(mono.clone())
                    .text_size(theme::sp(12.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("Edit Mode · {selected} selected")),
            )
            // Selection controls, top of the panel per the spec.
            .child(
                div()
                    .flex()
                    .gap(theme::sp(8.))
                    .child(btn_ghost(
                        "cal-clear-all",
                        "Clear all",
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.multi.clear();
                            cx.notify();
                        }),
                    ))
                    .child(btn_ghost(
                        "cal-select-all",
                        "Select all",
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.multi = all_analog_ids();
                            cx.notify();
                        }),
                    )),
            )
            .when(selected == 0, |el| {
                el.child(note(
                    "No keys selected — click caps on the board to add them.",
                ))
            })
            // Caps are selected but none resolves to an analog config — the
            // offline case. Honest, instead of sliders that would edit nothing.
            .when(selected > 0 && count == 0, |el| {
                el.child(note(
                    "The selected caps have no analog config to edit — daemon offline?",
                ))
            })
            .when(count > 0, |el| {
                el.child(
                    // The deepest press among the selected caps at this moment,
                    // with the pending actuation/release marks for reference.
                    div()
                        .flex()
                        .flex_col()
                        .gap(theme::sp(6.))
                        .child(field_label("Live pressure (max of selection)"))
                        .child(press_bar(
                            live_max,
                            &[
                                (self.bulk_rel, theme::mute()),
                                (self.bulk_act, theme::rust()),
                            ],
                        ))
                        .child(press_readout(live_max, mono)),
                )
                .child(divider())
                .child(labeled_slider(
                    "Actuation point",
                    &self.bulk_act_slider,
                    readout(self.bulk_act),
                    false,
                    cx,
                ))
                .child(labeled_slider(
                    "Release point",
                    &self.bulk_rel_slider,
                    readout(self.bulk_rel),
                    false,
                    cx,
                ))
                .child(note(
                    "Applies to every selected key. Each key keeps its own second function.",
                ))
                .child(note(names))
            })
    }
}

impl Render for Calibration {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = theme::mono(cx);
        let selected = self.store.read(cx).selected;

        // The selected key, resolved to its analog config when the daemon has
        // one for it. Unconfigured caps and the whole offline path resolve to
        // `None` — no editor opens. In Edit Mode the single-key editor never
        // opens: the selection belongs to the multi set and the drawer.
        let editing: Option<(KeyId, AnalogKeyConfig)> = if self.edit_mode {
            None
        } else {
            selected.and_then(|k| self.cfg_for(k).map(|c| (k, c)))
        };

        // Re-seed the sliders when the editor's target key changes (the old
        // component remounted; here the view outlives the selection). Runs
        // before the values are read below, so the re-seed shows this frame.
        match &editing {
            Some((_, cfg)) if self.edit.as_ref().map(|e| e.index) != Some(cfg.index) => {
                self.seed_key_edit(cfg, cx);
            }
            None if self.edit.is_some() => {
                self.edit = None;
                self.picker_open = false;
            }
            _ => {}
        }

        let multi_cfgs = self.multi_cfgs();
        let any_custom = self
            .analog
            .as_ref()
            .is_some_and(|a| a.keys.iter().any(|k| k.is_custom()));

        // Contextual notice for non-live states. The keypad below ALWAYS renders
        // (so the board reads as "keys" in every state, consistent with the other
        // screens); the banner just explains why there is no live pressure yet.
        let notice = match self.analog.as_ref() {
            None => Some(banner(
                "○",
                "Daemon offline",
                " — start tartarust-daemon to read live key pressure and edit actuation.",
                &mono,
            )),
            Some(a) if !a.analog_active => Some(banner(
                "⚠",
                "Analog mode is off",
                " — set input_mode = \"analog\" in the daemon config to calibrate per-key \
                 pressure.",
                &mono,
            )),
            Some(_) => None,
        };

        // The keypad's pointer protocol takes its `CapPointer` BY VALUE, and a
        // view listener takes its event by reference, so the listener is wrapped
        // rather than handed over raw.
        let listener = cx.listener(|this, p: &CapPointer, _, cx| this.on_cap_pointer(*p, cx));
        let pointer = Rc::new(move |p: CapPointer, window: &mut Window, cx: &mut App| {
            listener(&p, window, cx)
        });

        // The editor is passed as the keypad's popover: it anchors it INSIDE the
        // selected cap. `deferred` is the ported `z-index: 60` — it paints after
        // the rest of the frame (so neighbouring caps can't cover it) while its
        // layout, and with it the in-cap anchoring, stays in place.
        let popover = editing.as_ref().map(|(key, cfg)| {
            let (open_left, offset) = popover_anchor(*key);
            deferred(self.cal_modal(
                cfg,
                self.pressure_of(cfg.index),
                open_left,
                offset,
                &mono,
                cx,
            ))
            .with_priority(3)
            .into_any_element()
        });

        let keypad = Keypad::new(KeypadMode::Pressure, Layer::Base)
            // Only the analog caps (19 main keys + spacebar) are calibratable,
            // so the 8-way pad, scroll wheel, and thumb button are hidden here.
            .analog_only(true)
            .pressures(Some(self.pressures_map()))
            .multi_selected(self.multi.clone())
            .customized(self.customized_set())
            .selected(selected)
            .on_cap_pointer(pointer)
            .when_some(popover, |kp, el| kp.popover(el));

        // The bench: a mat under the rendered device, with the stylesheet's two
        // registration ticks. A click on the BARE stage clears the single
        // selection and ends any drag — a cap click can't reach here, because
        // the cap stops the mouse-down that would arm this element's click.
        let stage = div()
            .id("cal-stage")
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .pt(theme::sp(28.))
            .px(theme::sp(26.))
            .pb(theme::sp(24.))
            .rounded(theme::sp(8.))
            .bg(theme::panel_0())
            .border_1()
            .border_color(theme::line_soft())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.clear_selection(cx);
                this.dragging = false;
                // A drag that entered Edit Mode must keep its selection —
                // exiting is the toolbar button's job; only a still-accumulating
                // selection is dropped here.
                if !this.edit_mode {
                    this.multi.clear();
                }
                cx.notify();
            }))
            .child(corner_tick(true))
            .child(corner_tick(false))
            .child(keypad);

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .p(theme::sp(22.))
            // While the drawer is open the page sits left of it, so the fixed
            // drawer never covers the toolbar.
            .when(self.edit_mode, |el| el.pr(theme::sp(PANEL_SHIFT)))
            .child(page_head(
                "Click a key to tune its analog actuation — drag or use Edit Mode to tune many \
                 at once.",
            ))
            .when_some(notice, |el, b| el.child(b))
            .child(self.toolbar(any_custom, cx))
            // The dim/dismiss layer. Painted BEFORE the stage, so the bright
            // keypad (and the editor anchored in its cap) stays above it and
            // undimmed while tuning; a click anywhere else clears the selection.
            .when(editing.is_some(), |el| {
                el.child(
                    div()
                        .id("cal-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x0A09079E))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.clear_selection(cx);
                                cx.notify();
                            }),
                        ),
                )
            })
            .child(stage)
            .when(self.edit_mode, |el| {
                el.child(deferred(self.edit_panel(&multi_cfgs, &mono, cx)).with_priority(1))
            })
    }
}

// --- Small pieces ------------------------------------------------------------

/// An annunciator bar, not a toast: an amber wash with a rule down its left edge.
/// (The stylesheet also drew a faint full border; GPUI takes one border colour
/// per element, so the accent rule carries it alone.)
fn banner(icon: &'static str, head: &'static str, tail: &'static str, mono: &SharedString) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(theme::sp(12.))
        .px(theme::sp(14.))
        .py(theme::sp(11.))
        .mb(theme::sp(18.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .bg(theme::amber_wash())
        .border_l_3()
        .border_color(theme::amber())
        .text_size(theme::sp(12.5))
        .text_color(theme::amber())
        .child(
            div()
                .font_family(mono.clone())
                .text_size(theme::sp(15.))
                .child(icon),
        )
        .child(
            div()
                .flex()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(0xEFCB8B))
                        .child(head),
                )
                .child(tail),
        )
}

/// One L-shaped corner tick, like a bench-mat print (the stage's `::before` /
/// `::after`): top-left, then bottom-right.
fn corner_tick(top_left: bool) -> Div {
    let base = div()
        .absolute()
        .size(theme::sp(11.))
        .border_color(theme::line());
    if top_left {
        base.top(theme::sp(9.))
            .left(theme::sp(9.))
            .border_t_1()
            .border_l_1()
    } else {
        base.bottom(theme::sp(9.))
            .right(theme::sp(9.))
            .border_b_1()
            .border_r_1()
    }
}

/// The live pressure track: a phosphor fill (the device talking) with threshold
/// marks in the colours of who set them — actuation = rust (your setting),
/// second step = amber (drift from single-action), release = mute.
///
/// The marks are 2px wide and start AT their value. CSS nudged each one 1px left
/// with a transform, which GPUI has no equivalent for, so a mark reads a hair
/// right of its exact position and one at the full 255 hides under the track's
/// right edge.
fn press_bar(fill: u8, marks: &[(u8, Rgba)]) -> Div {
    div()
        .relative()
        .h(theme::sp(22.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .bg(theme::panel_1())
        .border_1()
        .border_color(theme::line())
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(relative(frac(fill)))
                .bg(linear_gradient(
                    90.,
                    linear_color_stop(theme::live_dim(), 0.),
                    linear_color_stop(theme::live(), 1.),
                )),
        )
        .children(marks.iter().map(|&(v, color)| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(frac(v)))
                .w(theme::sp(2.))
                .bg(color)
        }))
}

/// The raw/percent readout under a pressure bar. The device reports raw
/// pressure; the percentage is a convenience.
fn press_readout(v: u8, mono: &SharedString) -> Div {
    div()
        .text_right()
        .font_family(mono.clone())
        .text_size(theme::sp(11.))
        .text_color(theme::mute())
        .child(format!("{v} / 255 · {}%", pct100(v)))
}

/// A field's caption, matching the widget kit's slider labels.
fn field_label(label: &'static str) -> Div {
    div()
        .text_size(theme::sp(11.))
        .text_color(theme::dim())
        .child(label)
}

/// Quiet explanatory copy under a control.
fn note(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(theme::sp(11.))
        .text_color(theme::mute())
        .child(text.into())
}

fn divider() -> Div {
    div().flex_none().h(theme::sp(1.)).bg(theme::line_soft())
}

// --- Pure helpers ------------------------------------------------------------

/// Integer percentage readout for a raw 0..255 pressure value.
fn pct100(v: u8) -> u8 {
    (v as u16 * 100 / 255) as u8
}

/// A raw 0..255 value as a 0..1 fraction, for bar fills and mark positions.
fn frac(v: u8) -> f32 {
    (v as f32 / 255.0).clamp(0.0, 1.0)
}

/// A slider's monospace readout: the raw value plus its percentage.
fn readout(v: u8) -> String {
    format!("{v} ({}%)", pct100(v))
}

/// The actuation slider's ceiling. A dual-step needs strictly-deeper headroom,
/// so the primary stops one short of the bottom while one is enabled.
fn act_max(second_on: bool) -> f32 {
    if second_on { 254.0 } else { 255.0 }
}

/// A `SliderEvent` value as the device's raw byte.
fn slider_u8(v: f32) -> u8 {
    v.round() as u8
}

fn slider_state(min: f32, max: f32, value: u8) -> SliderState {
    SliderState::new()
        .min(min)
        .max(max)
        .step(1.)
        .default_value(value as f32)
}

/// Re-seed a slider in place. gpui-component's `min`/`max` are build-time only
/// and the actuation slider's ceiling moves with the dual-step, so the state
/// is rebuilt — the ENTITY, and with it every subscription on it, survives.
/// Setting a value this way emits no `Change`, so a re-seed can't feed back
/// into the handler that caused it.
///
/// Rebuilt THROUGH the builder methods on the previous state, not from
/// `SliderState::new()`: the state privately caches its track bounds during
/// paint, and a fresh state would zero that cache — the next frame then draws
/// the thumb at `percentage * 0 = 0` (far left, no fill) until some later
/// event forces another render. The builder path recomputes the thumb from
/// the new min/max/value while the cached bounds survive.
fn seed_slider(slider: &Entity<SliderState>, min: f32, max: f32, value: u8, cx: &mut App) {
    slider.update(cx, |state, cx| {
        let prev = std::mem::replace(state, SliderState::new());
        *state = prev.min(min).max(max).step(1.).default_value(value as f32);
        cx.notify();
    });
}

/// One-frame render nudge after seeding never-painted sliders. The slider
/// caches its track bounds during paint SILENTLY (no notify), so a slider's
/// very first frame draws the thumb at x=0; a follow-up notify on the next
/// tick lets the second frame position the thumb with the freshly cached
/// bounds. Harmless when the bounds were already cached (one extra render).
fn nudge_next_frame(cx: &mut Context<Calibration>) {
    cx.spawn(async move |this, cx| {
        let _ = this.update(cx, |_, cx| cx.notify());
    })
    .detach();
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

/// Which side the actuation popover opens on, and how far from its cap's edge.
///
/// It opens toward the board's centre (to the LEFT of its cap) for the right
/// grid columns and the whole thumb cluster, so the panel never runs off-screen.
fn popover_anchor(sel: KeyId) -> (bool, f32) {
    let cap_w = if matches!(sel, KeyId::Space) {
        SPACE_CAP_W
    } else {
        GRID_CAP_W
    };
    (opens_left(sel), cap_w * 1.05)
}

/// Whether the popover should open toward the board's centre. True for the right
/// grid columns and the whole thumb cluster.
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

#[cfg(test)]
mod tests {
    //! The pure geometry/scale helpers. The schema invariants the pushes uphold
    //! (`release < actuation`, second step strictly deeper, `second_action` set
    //! iff a second point is) are exercised against a real daemon by
    //! `ipc::tests::set_actuation_emits_expected_request_json`.

    use super::*;

    #[test]
    fn every_analog_cap_is_selectable() {
        let ids = all_analog_ids();
        assert_eq!(ids.len(), 20, "19 grid caps + the spacebar");
        assert!(ids.contains(&KeyId::Space));
        assert!(ids.contains(&KeyId::Grid(18)));
        // Digital-only inputs carry no pressure byte and must not be selectable.
        assert!(!ids.contains(&KeyId::ThumbButton));
    }

    #[test]
    fn popover_opens_toward_the_board_centre_on_the_right() {
        // Row 0 is caps 0..4: the two rightmost open left.
        assert!(!opens_left(KeyId::Grid(0)));
        assert!(!opens_left(KeyId::Grid(2)));
        assert!(opens_left(KeyId::Grid(3)));
        assert!(opens_left(KeyId::Grid(4)));
        // ...and the same column rule holds in the short final row (15..18).
        assert!(!opens_left(KeyId::Grid(17)));
        assert!(opens_left(KeyId::Grid(18)));
        // The spacebar always opens left, and anchors off its own wider cap.
        let (left, offset) = popover_anchor(KeyId::Space);
        assert!(left);
        assert!(offset > popover_anchor(KeyId::Grid(0)).1);
    }

    #[test]
    fn scale_helpers_cover_the_full_byte_range() {
        assert_eq!(pct100(0), 0);
        assert_eq!(pct100(255), 100);
        assert_eq!(frac(0), 0.0);
        assert_eq!(frac(255), 1.0);
        assert_eq!(readout(255), "255 (100%)");
        // The dual-step's headroom rule, as the slider ceiling sees it.
        assert_eq!(act_max(true), 254.0);
        assert_eq!(act_max(false), 255.0);
    }
}

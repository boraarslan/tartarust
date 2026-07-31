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
//! **Travel readouts.** Every actuation slider's readout is an EDITABLE field in
//! millimetres, captioned with the depth range it accepts. A valid depth applies
//! AS YOU TYPE — the field behaves like the slider beside it, not like a form —
//! while text that is not yet a usable depth ("1.", "-", or a figure outside the
//! range) applies nothing and is left alone under the caret. Enter and blur
//! commit, which is where such text is normalized back to what the threshold
//! actually holds. The device speaks a raw 0..255 byte and no byte-exact byte→mm table is public
//! (`docs/research/03-analog-actuation.md`: "mm → byte value mapping unknown"),
//! so the byte is mapped LINEARLY over the switches' 4.0 mm optical travel — a
//! documented approximation, not a calibration. The BYTE stays the authority:
//! every clamp, invariant, and push is byte-domain, and a typed depth is
//! converted, clamped to its slider's range, then applied through the same
//! setter the drag uses. A depth between two bytes therefore snaps to the
//! nearest one and the field redraws with what the device actually took.
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
use gpui_component::input::{InputEvent, InputState};
use gpui_component::slider::{SliderEvent, SliderState};

use tartarust_ipc::{AnalogKeyConfig, AnalogState, DEFAULT_ACTUATION_POINT, DEFAULT_RELEASE_POINT};

use crate::components::widgets::{
    btn_ghost, labeled_slider_input, nudge_next_frame, page_head, switch_row,
};
use crate::components::{
    CapPhase, CapPointer, KeyPicker, KeyPickerEvent, Keypad, KeypadMode, key_picker_trigger,
};
use crate::ipc;
use crate::model::{AppState, GRID_EVDEV, KeyId, Layer, pretty_key};
use crate::store;
use crate::theme;

/// How often to re-poll the live analog state while this view is alive.
const POLL_INTERVAL_MS: u64 = 100;

/// How long the single-key editor must have been quiet before a daemon config
/// that disagrees with it is treated as an EXTERNAL change and re-seeded.
///
/// The poll trails our own pushes by up to one interval, so right after a drag
/// or a keystroke the polled config legitimately disagrees with the editor
/// without anything external having happened. Comfortably longer than
/// [`POLL_INTERVAL_MS`], so that lag alone can never trip the re-seed.
const EXTERNAL_RESEED_GRACE: Duration = Duration::from_millis(400);

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

/// Total optical travel of the Tartarus Pro's analog switches, in mm. Synapse
/// exposes 1.5–3.6 mm of it as the actuation range; the full stroke is 4.0 mm.
/// See the module header for why the byte→mm map is linear.
const TRAVEL_MM: f32 = 4.0;

/// Which threshold an editable travel field commits to. One subscription body
/// serves all five: the field parses to a byte and hands it to the same setter
/// its slider's `SliderEvent::Change` calls.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Actuation,
    Release,
    Second,
    BulkActuation,
    BulkRelease,
}

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
    /// When this edit last pushed to the daemon. Stamped by
    /// [`Calibration::push_key_edit`] and read only by the external-change
    /// re-seed, which needs to tell the poll's lag behind OUR pushes from a
    /// config someone else changed. See [`EXTERNAL_RESEED_GRACE`].
    last_push: std::time::Instant,
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
            last_push: std::time::Instant::now(),
        }
    }

    /// Whether re-seeding from `cfg` would actually change this edit — i.e. the
    /// daemon's copy of the key no longer says what the editor shows.
    ///
    /// Phrased as "would a re-seed move anything", NOT as a field-by-field
    /// comparison against the config, and that distinction is load-bearing: this
    /// decides a re-seed that runs from `render`, so every difference it reports
    /// must be settled by one re-seed. A config that [`KeyEdit::seed`] cannot
    /// round-trip would otherwise report the same difference on every frame and
    /// spin the render loop. Hence the second step compares as the pair it is
    /// seeded from — its point and action only count while the config HAS one,
    /// and an absent action leaves the seeded default alone.
    fn differs_from(&self, cfg: &AnalogKeyConfig) -> bool {
        let second_on = cfg.second_actuation_point.is_some();
        if second_on != self.second_on
            || cfg.actuation_point != self.actuation
            || cfg.release_point != self.release
        {
            return true;
        }
        second_on
            && (cfg.second_actuation_point != Some(self.second_point)
                || cfg
                    .second_action
                    .as_ref()
                    .is_some_and(|a| *a != self.second_action))
    }
}

pub struct Calibration {
    store: Entity<AppState>,
    /// Live analog state: `None` until the first successful poll, and back to
    /// `None` whenever the daemon goes away. `None` == offline / not-yet-loaded
    /// (the cage-harness path) — we render an honest offline state, never fake
    /// bars.
    analog: Option<AnalogState>,
    /// True once the FIRST poll has answered (success or failure). Gates the
    /// offline banner: before the first answer, "no analog state yet" is not
    /// evidence of anything, and showing the banner flashed it on every screen
    /// entry while the daemon was perfectly reachable.
    polled_once: bool,
    /// Displayed per-key pressures, gliding toward the polled values — the
    /// poll ticks at 100 ms and painting those raw steps reads as stutter.
    smooth: HashMap<String, f32>,
    /// Edit Mode: the docked bulk editor is open iff this is set.
    edit_mode: bool,
    /// The multi-selection (Edit Mode's target, and a drag's accumulator).
    multi: BTreeSet<KeyId>,
    /// A cap drag is in progress — gates the drag-extend on `CapPhase::Enter`.
    dragging: bool,

    /// The single-key editor's local edit state, live only while its cap is
    /// selected AND resolves to an analog config.
    edit: Option<KeyEdit>,
    /// Each threshold's slider and its editable travel field. The two are two
    /// views of one byte: the slider owns the drag, the field owns the typed
    /// depth, and [`Calibration::sync_inputs`] keeps the field showing whatever
    /// the byte became.
    act_slider: Entity<SliderState>,
    act_input: Entity<InputState>,
    rel_slider: Entity<SliderState>,
    rel_input: Entity<InputState>,
    second_slider: Entity<SliderState>,
    second_input: Entity<InputState>,
    /// The field being typed in right now, for the duration of ONE live apply.
    /// [`Calibration::sync_inputs`] leaves this field's text alone — see
    /// [`Calibration::live_field`] for why rewriting it mid-keystroke is a caret
    /// fight — and still writes the other four.
    live_edit: Option<Field>,

    /// Bulk (Edit Mode) values. Seeded from the first selected key when Edit
    /// Mode opens; reshaping the selection keeps them — they are what you are
    /// about to apply, not a per-key readback.
    bulk_act: u8,
    bulk_rel: u8,
    bulk_act_slider: Entity<SliderState>,
    bulk_act_input: Entity<InputState>,
    bulk_rel_slider: Entity<SliderState>,
    bulk_rel_input: Entity<InputState>,

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

        let act_input = cx.new(|cx| travel_input(DEFAULT_ACTUATION_POINT, window, cx));
        let rel_input = cx.new(|cx| travel_input(DEFAULT_RELEASE_POINT, window, cx));
        let second_input = cx.new(|cx| travel_input(SEED_SECOND_POINT, window, cx));
        let bulk_act_input = cx.new(|cx| travel_input(DEFAULT_ACTUATION_POINT, window, cx));
        let bulk_rel_input = cx.new(|cx| travel_input(DEFAULT_RELEASE_POINT, window, cx));

        // Each slider drives its value + the dependent thresholds, then pushes.
        // The clamps mirror the old `on_input` handlers exactly; where a clamp
        // moves a value the owning slider is re-seeded so its thumb follows.
        // Subscribed WITH the window: every setter ends by writing the new byte
        // back into the editable fields, and that write needs one.
        cx.subscribe_in(
            &act_slider,
            window,
            |this: &mut Self, _, ev: &SliderEvent, window, cx| {
                let SliderEvent::Change(v) = ev;
                this.set_actuation(slider_u8(v.start()), window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &rel_slider,
            window,
            |this: &mut Self, _, ev: &SliderEvent, window, cx| {
                let SliderEvent::Change(v) = ev;
                this.set_release(slider_u8(v.start()), window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &second_slider,
            window,
            |this: &mut Self, _, ev: &SliderEvent, window, cx| {
                let SliderEvent::Change(v) = ev;
                this.set_second_point(slider_u8(v.start()), window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &bulk_act_slider,
            window,
            |this: &mut Self, _, ev: &SliderEvent, window, cx| {
                let SliderEvent::Change(v) = ev;
                this.set_bulk_actuation(slider_u8(v.start()), window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &bulk_rel_slider,
            window,
            |this: &mut Self, _, ev: &SliderEvent, window, cx| {
                let SliderEvent::Change(v) = ev;
                this.set_bulk_release(slider_u8(v.start()), window, cx);
            },
        )
        .detach();

        // Three events, two jobs. `Change` (every keystroke, and every
        // programmatic write) APPLIES what is typed when it is already a valid
        // depth, so the field tracks the device like its slider does; Enter and
        // blur COMMIT, which additionally normalizes the text — the same
        // contract the Customize screen's Launch field carries.
        for (input, field) in [
            (&act_input, Field::Actuation),
            (&rel_input, Field::Release),
            (&second_input, Field::Second),
            (&bulk_act_input, Field::BulkActuation),
            (&bulk_rel_input, Field::BulkRelease),
        ] {
            cx.subscribe_in(
                input,
                window,
                move |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        this.commit_field(field, window, cx)
                    }
                    InputEvent::Change => this.live_field(field, window, cx),
                    _ => {}
                },
            )
            .detach();
        }

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
        // Fetch-first, THEN tick: the first poll answers immediately instead of
        // one interval late, which both shrinks the not-yet-polled window and
        // keeps the offline banner from flashing on screen entry (see
        // `polled_once`).
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let next = cx
                    .background_executor()
                    .spawn(async { ipc::get_analog_state() })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if !this.polled_once {
                        this.polled_once = true;
                        cx.notify();
                    }
                    if this.analog != next {
                        this.analog = next;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return; // the view (or the app) is gone
                }
                cx.background_executor()
                    .timer(Duration::from_millis(POLL_INTERVAL_MS))
                    .await;
            }
        });

        let mut this = Self {
            store,
            analog: None,
            polled_once: false,
            smooth: HashMap::new(),
            edit_mode: false,
            multi: BTreeSet::new(),
            dragging: false,
            edit: None,
            act_slider,
            act_input,
            rel_slider,
            rel_input,
            second_slider,
            second_input,
            live_edit: None,
            bulk_act: DEFAULT_ACTUATION_POINT,
            bulk_rel: DEFAULT_RELEASE_POINT,
            bulk_act_slider,
            bulk_act_input,
            bulk_rel_slider,
            bulk_rel_input,
            picker,
            picker_open: false,
            _poll: poll,
        };

        // Dev-only: let the headless screenshot harness open Edit Mode directly
        // (TARTARUST_CAL_EDIT=1) — the drawer is otherwise unreachable without a
        // click. No effect in normal use (the var is unset).
        if std::env::var("TARTARUST_CAL_EDIT").as_deref() == Ok("1") {
            this.multi = all_analog_ids();
            this.enter_edit_mode(window, cx);
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

    /// Advance the DISPLAYED pressures one exponential step toward the live
    /// poll values and return them for the keypad. The poll ticks at 100 ms;
    /// painting those steps raw reads as stutter, so the fill glides at frame
    /// rate — while any key is still gliding, the next frame is requested so
    /// the animation keeps running between poll ticks.
    fn step_pressure_smoothing(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> HashMap<String, u8> {
        let target = self.pressures_map();
        // Keys that vanished (daemon restart, mode change) stop animating.
        self.smooth.retain(|k, _| target.contains_key(k));
        let mut animating = false;
        for (key, &t) in &target {
            let shown = self.smooth.entry(key.clone()).or_insert(t as f32);
            let delta = t as f32 - *shown;
            if delta.abs() < 1.0 {
                *shown = t as f32;
            } else {
                *shown += delta * 0.35;
                animating = true;
            }
        }
        if animating {
            let entity = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = entity.update(cx, |_, cx| cx.notify());
            });
        }
        self.smooth
            .iter()
            .map(|(k, v)| (k.clone(), v.round() as u8))
            .collect()
    }

    /// One analog key's live pressure, by byte index.
    fn pressure_of(&self, index: u8) -> u8 {
        self.analog
            .as_ref()
            .and_then(|a| a.pressures.get(index as usize).copied())
            .unwrap_or(0)
    }

    /// One analog key's DISPLAYED (smoothed) pressure, by byte index — the
    /// same gliding value the keypad paints, so the single-key editor's and
    /// the bulk panel's live bars move smoothly too instead of stepping at
    /// the raw 100 ms poll rate.
    fn smoothed_of(&self, index: u8) -> u8 {
        self.analog
            .as_ref()
            .and_then(|a| a.keys.iter().find(|k| k.index == index))
            .and_then(|k| self.smooth.get(&k.key))
            .map(|v| v.round() as u8)
            .unwrap_or_else(|| self.pressure_of(index))
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
    fn enter_edit_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self.multi_cfgs().into_iter().next();
        self.bulk_act = first
            .as_ref()
            .map_or(DEFAULT_ACTUATION_POINT, |c| c.actuation_point);
        self.bulk_rel = first
            .as_ref()
            .map_or(DEFAULT_RELEASE_POINT, |c| c.release_point);
        seed_slider(&self.bulk_act_slider, 1.0, 255.0, self.bulk_act, cx);
        seed_slider(&self.bulk_rel_slider, 0.0, 254.0, self.bulk_rel, cx);
        self.sync_inputs(window, cx);
        self.edit_mode = true;
        nudge_next_frame(cx);
    }

    /// The toolbar toggle: entering selects EVERY analog cap, exiting clears the
    /// selection.
    fn toggle_edit_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit_mode {
            self.edit_mode = false;
            self.multi.clear();
        } else {
            self.clear_selection(cx);
            self.multi = all_analog_ids();
            self.enter_edit_mode(window, cx);
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
    fn on_cap_pointer(&mut self, p: CapPointer, window: &mut Window, cx: &mut Context<Self>) {
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
                    self.enter_edit_mode(window, cx);
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
                    self.enter_edit_mode(window, cx);
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

    /// Re-seed the editor (its sliders and their travel fields) for a newly
    /// selected analog key.
    fn seed_key_edit(
        &mut self,
        cfg: &AnalogKeyConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        self.sync_inputs(window, cx);
        nudge_next_frame(cx);
    }

    // --- Editable travel fields ---------------------------------------------

    /// The `InputState` a field's typed depth lives in.
    fn input_of(&self, field: Field) -> &Entity<InputState> {
        match field {
            Field::Actuation => &self.act_input,
            Field::Release => &self.rel_input,
            Field::Second => &self.second_input,
            Field::BulkActuation => &self.bulk_act_input,
            Field::BulkRelease => &self.bulk_rel_input,
        }
    }

    /// The byte range a field accepts — exactly the min/max its slider carries,
    /// so a typed depth outside it lands where dragging the thumb to that end
    /// would. The actuation ceiling moves with the dual-step, same as the
    /// slider's.
    fn range_of(&self, field: Field) -> (u8, u8) {
        match field {
            Field::Actuation => {
                let second_on = self.edit.as_ref().is_some_and(|e| e.second_on);
                (1, act_max(second_on) as u8)
            }
            Field::Release => (0, 254),
            Field::Second => (1, 255),
            Field::BulkActuation => (1, 255),
            Field::BulkRelease => (0, 254),
        }
    }

    /// That range as the field's caption states it. Built from the SAME
    /// [`Calibration::range_of`] the validation reads and re-taken every render,
    /// so a ceiling that moves — the actuation row gives up its last byte to a
    /// dual-step — moves the caption with it. A hint that quietly disagreed with
    /// the clamp would be worse than no hint.
    fn range_hint(&self, field: Field) -> SharedString {
        let (lo, hi) = self.range_of(field);
        format!("{:.2}–{:.2} mm", byte_to_mm(lo), byte_to_mm(hi)).into()
    }

    /// The byte a field stands for right now, when it stands for one (the
    /// single-key fields have no value while no analog key is selected).
    fn value_of(&self, field: Field) -> Option<u8> {
        match field {
            Field::Actuation => self.edit.as_ref().map(|e| e.actuation),
            Field::Release => self.edit.as_ref().map(|e| e.release),
            Field::Second => self.edit.as_ref().map(|e| e.second_point),
            Field::BulkActuation => Some(self.bulk_act),
            Field::BulkRelease => Some(self.bulk_rel),
        }
    }

    /// Commit one editable travel field (Enter or blur). Text that is not a
    /// number — including an empty field — changes nothing and the current value
    /// is put back, so a half-typed figure can never push a threshold. A parsed
    /// depth becomes the raw byte, is clamped to the field's range, then goes
    /// through the SAME setter the slider's `Change` calls: the dependent
    /// clamps, the schema invariants, and the push are identical either way.
    fn commit_field(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.input_of(field).read(cx).value();
        let Ok(mm) = typed.trim().parse::<f32>() else {
            self.sync_inputs(window, cx);
            return;
        };
        let (lo, hi) = self.range_of(field);
        let value = mm_to_byte(mm).clamp(lo, hi);
        match field {
            Field::Actuation => self.set_actuation(value, window, cx),
            Field::Release => self.set_release(value, window, cx),
            Field::Second => self.set_second_point(value, window, cx),
            Field::BulkActuation => self.set_bulk_actuation(value, window, cx),
            Field::BulkRelease => self.set_bulk_release(value, window, cx),
        }
    }

    /// Apply a typed depth ON EVERY KEYSTROKE, so a field tracks the device the
    /// way its slider does instead of holding the change until you press Enter.
    /// Only a depth that is already valid applies; anything else — a half-typed
    /// "1.", a lone "-", an empty field, a figure past the end of the range —
    /// applies nothing AND is left alone. Putting the old text back here (which
    /// is what [`Calibration::commit_field`] does) would make the field
    /// impossible to type in; normalizing is the commit's job.
    ///
    /// The range check runs in MM SPACE, against the field's byte range
    /// converted back to depths, and it must: [`mm_to_byte`] saturates, so
    /// checking the converted byte instead would read "40" — ten times the whole
    /// stroke — as a perfectly in-range 255 and bottom the threshold out under a
    /// user who is still typing.
    fn live_field(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.input_of(field).read(cx).value();
        let Ok(mm) = typed.trim().parse::<f32>() else {
            return;
        };
        let (lo, hi) = self.range_of(field);
        if !(byte_to_mm(lo)..=byte_to_mm(hi)).contains(&mm) {
            return;
        }
        let value = mm_to_byte(mm);
        // `Change` also fires for the writes `sync_inputs` itself makes, so this
        // arrives on a re-seed and after every setter — not just on a keystroke.
        // Applying a byte the threshold already holds would turn selecting a key
        // into a `SetActuation` push, and a dependent clamp into a second,
        // redundant one. Nothing changed, so nothing is sent.
        if self.value_of(field) == Some(value) {
            return;
        }
        // The setters end in `sync_inputs`, which would rewrite this field with
        // the canonical 2-decimal text — a typed "2.3" becoming "2.31" under the
        // caret, which `set_value` then throws to the end of the line. Claim the
        // field for the length of the dispatch: `sync_inputs` skips it and still
        // writes the other four, so a clamp that drags release along with
        // actuation is visible while you type.
        self.live_edit = Some(field);
        match field {
            Field::Actuation => self.set_actuation(value, window, cx),
            Field::Release => self.set_release(value, window, cx),
            Field::Second => self.set_second_point(value, window, cx),
            Field::BulkActuation => self.set_bulk_actuation(value, window, cx),
            Field::BulkRelease => self.set_bulk_release(value, window, cx),
        }
        self.live_edit = None;
    }

    /// Write the current bytes back into the travel fields, so a drag, a
    /// re-seed, or a dependent clamp shows up in the numbers the moment it
    /// lands. Every setter ends here.
    ///
    /// The text is written only when it actually differs: `set_value` moves the
    /// caret to the end and emits `Change`, so a no-op rewrite would fight the
    /// field while you type in it. That emitted `Change` reaches
    /// [`Calibration::live_field`], which applies nothing when the value has not
    /// moved — so a sync can never loop back into a push.
    ///
    /// The field named by `live_edit` is skipped entirely: it is the one being
    /// typed in, and it already says what the user meant. Every OTHER field
    /// still syncs, so a dependent clamp shows up live.
    fn sync_inputs(&self, window: &mut Window, cx: &mut App) {
        if let Some(e) = self.edit.as_ref() {
            if self.live_edit != Some(Field::Actuation) {
                set_travel(&self.act_input, e.actuation, window, cx);
            }
            if self.live_edit != Some(Field::Release) {
                set_travel(&self.rel_input, e.release, window, cx);
            }
            if self.live_edit != Some(Field::Second) {
                set_travel(&self.second_input, e.second_point, window, cx);
            }
        }
        if self.live_edit != Some(Field::BulkActuation) {
            set_travel(&self.bulk_act_input, self.bulk_act, window, cx);
        }
        if self.live_edit != Some(Field::BulkRelease) {
            set_travel(&self.bulk_rel_input, self.bulk_rel, window, cx);
        }
    }

    /// Send the current edit state to the daemon, upholding the schema
    /// invariants. Dual-step off => BOTH second fields cleared (never a half-set
    /// second step). The second action comes from the picker, so it is already a
    /// valid evdev name — no text coercion needed.
    fn push_key_edit(&mut self, cx: &mut App) {
        let Some(e) = self.edit.as_mut() else { return };
        // Stamp the push. The poll answers up to `POLL_INTERVAL_MS` later, so
        // for that long the config it reports is a picture from BEFORE this
        // push; the external-change re-seed reads this stamp to know that.
        e.last_push = std::time::Instant::now();
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
        // Read the index out before `self.store`: `e` still holds a mutable
        // borrow of `self.edit` until its last use.
        let index = e.index;
        store::fire(
            &self.store,
            ipc::req_set_actuation(index, act, rel, sap, sact),
            cx,
        );
    }

    fn set_actuation(&mut self, value: u8, window: &mut Window, cx: &mut Context<Self>) {
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
        self.sync_inputs(window, cx);
        cx.notify();
    }

    fn set_release(&mut self, value: u8, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.edit.as_mut() else { return };
        // Clamp below actuation (hysteresis): a release >= actuation would
        // chatter, and the daemon rejects it.
        let clamped = value.min(e.actuation.saturating_sub(1));
        e.release = clamped;
        if clamped != value {
            seed_slider(&self.rel_slider, 0.0, 254.0, clamped, cx);
        }
        self.push_key_edit(cx);
        self.sync_inputs(window, cx);
        cx.notify();
    }

    fn set_second_point(&mut self, value: u8, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.edit.as_mut() else { return };
        // Must be deeper than the primary actuation point.
        let clamped = value.max(e.actuation.saturating_add(1));
        e.second_point = clamped;
        if clamped != value {
            seed_slider(&self.second_slider, 1.0, 255.0, clamped, cx);
        }
        self.push_key_edit(cx);
        self.sync_inputs(window, cx);
        cx.notify();
    }

    fn toggle_second(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
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
        self.sync_inputs(window, cx);
        cx.notify();
    }

    /// Reset this key to the default thresholds and drop any dual-step (mirrors
    /// the Customize editor's reset affordance).
    fn reset_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.edit.as_mut() else { return };
        e.actuation = DEFAULT_ACTUATION_POINT;
        e.release = DEFAULT_RELEASE_POINT;
        e.second_on = false;
        seed_slider(&self.act_slider, 1.0, 255.0, DEFAULT_ACTUATION_POINT, cx);
        seed_slider(&self.rel_slider, 0.0, 254.0, DEFAULT_RELEASE_POINT, cx);
        self.picker_open = false;
        self.push_key_edit(cx);
        self.sync_inputs(window, cx);
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

    fn set_bulk_actuation(&mut self, value: u8, window: &mut Window, cx: &mut Context<Self>) {
        self.bulk_act = value;
        if self.bulk_rel >= value {
            self.bulk_rel = value.saturating_sub(1);
            seed_slider(&self.bulk_rel_slider, 0.0, 254.0, self.bulk_rel, cx);
        }
        self.apply_bulk(cx);
        self.sync_inputs(window, cx);
        cx.notify();
    }

    fn set_bulk_release(&mut self, value: u8, window: &mut Window, cx: &mut Context<Self>) {
        let clamped = value.min(self.bulk_act.saturating_sub(1));
        self.bulk_rel = clamped;
        if clamped != value {
            seed_slider(&self.bulk_rel_slider, 0.0, 254.0, clamped, cx);
        }
        self.apply_bulk(cx);
        self.sync_inputs(window, cx);
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
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_edit_mode(window, cx)
                    }))
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
            .child(labeled_slider_input(
                "Actuation point",
                self.range_hint(Field::Actuation),
                &self.act_slider,
                &self.act_input,
                "mm",
                false,
                cx,
            ))
            .child(labeled_slider_input(
                "Release point",
                self.range_hint(Field::Release),
                &self.rel_slider,
                &self.rel_input,
                "mm",
                false,
                cx,
            ))
            .child(divider())
            .child(switch_row(
                "cal-dual-step",
                "Dual-step (second function)",
                e.second_on,
                cx.listener(|this, on: &bool, window, cx| this.toggle_second(*on, window, cx)),
            ))
            .when(e.second_on, |el| {
                el.child(labeled_slider_input(
                    "Second actuation point",
                    self.range_hint(Field::Second),
                    &self.second_slider,
                    &self.second_input,
                    "mm",
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
                cx.listener(|this, _: &ClickEvent, window, cx| this.reset_key(window, cx)),
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
            .map(|c| self.smoothed_of(c.index))
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
                .child(labeled_slider_input(
                    "Actuation point",
                    self.range_hint(Field::BulkActuation),
                    &self.bulk_act_slider,
                    &self.bulk_act_input,
                    "mm",
                    false,
                    cx,
                ))
                .child(labeled_slider_input(
                    "Release point",
                    self.range_hint(Field::BulkRelease),
                    &self.bulk_rel_slider,
                    &self.bulk_rel_input,
                    "mm",
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let smoothed_pressures = self.step_pressure_smoothing(window, cx);
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

        // Re-seed the sliders on either thing that invalidates them: the
        // editor's target key changing (the old component remounted; here the
        // view outlives the selection), or the daemon changing the key we are
        // already on. Runs before the values are read below, so a re-seed shows
        // this frame.
        match &editing {
            Some((_, cfg)) if self.edit.as_ref().map(|e| e.index) != Some(cfg.index) => {
                self.seed_key_edit(cfg, window, cx);
            }
            // Same key, but the daemon's copy of it no longer says what the
            // editor shows: something changed the config from outside — a
            // Discard restoring the saved actuation, most visibly. The editor is
            // otherwise seeded ONLY when the selection moves to another key, so
            // without this arm it keeps displaying the pre-discard values until
            // you click away and back.
            Some((_, cfg)) if self.edit.as_ref().is_some_and(|e| e.differs_from(cfg)) => {
                // Telling "someone else changed it" from "we changed it and the
                // poll has not caught up" is purely a matter of timing, so the
                // last push is what decides. Every push re-stamps it, which is
                // what keeps a drag or a typing run — where the two disagree
                // constantly — from being yanked out from under the user.
                if self
                    .edit
                    .as_ref()
                    .is_some_and(|e| e.last_push.elapsed() > EXTERNAL_RESEED_GRACE)
                {
                    self.seed_key_edit(cfg, window, cx);
                } else {
                    // Too soon to tell the two apart — but nothing else promises
                    // another frame in which to decide again. The poll notifies
                    // only when the analog state CHANGES, and on an idle keypad
                    // the change that got us here was the last one it will
                    // report. Ask for the frame ourselves, or an external edit
                    // that lands mid-grace would sit unseen until the next
                    // keypress.
                    nudge_next_frame(cx);
                }
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
            // Before the first poll answers, "no state yet" is not evidence:
            // showing the banner here flashed it on every screen entry.
            None if !self.polled_once => None,
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
        let listener =
            cx.listener(|this, p: &CapPointer, window, cx| this.on_cap_pointer(*p, window, cx));
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
                self.smoothed_of(cfg.index),
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
            .pressures(Some(smoothed_pressures))
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

/// A raw actuation byte as its approximate travel depth, in mm. Linear over
/// [`TRAVEL_MM`] — see the module header for why.
fn byte_to_mm(b: u8) -> f32 {
    b as f32 / 255.0 * TRAVEL_MM
}

/// A typed travel depth back to the raw byte, saturating at both ends of the
/// stroke so a wild figure cannot wrap. The caller still clamps to the control's
/// own range.
fn mm_to_byte(mm: f32) -> u8 {
    (mm / TRAVEL_MM * 255.0).round().clamp(0.0, 255.0) as u8
}

/// A threshold's editable readout: its travel depth to 2 decimals.
fn mm_text(v: u8) -> String {
    format!("{:.2}", byte_to_mm(v))
}

/// One threshold's travel field, seeded with its byte's depth.
fn travel_input(value: u8, window: &mut Window, cx: &mut Context<InputState>) -> InputState {
    InputState::new(window, cx).default_value(mm_text(value))
}

/// Show a byte's travel depth in a field, unless the field already reads that —
/// see [`Calibration::sync_inputs`] for why the no-op write is skipped.
fn set_travel(input: &Entity<InputState>, value: u8, window: &mut Window, cx: &mut App) {
    let text = mm_text(value);
    input.update(cx, |state, cx| {
        if state.value() != *text {
            state.set_value(text, window, cx);
        }
    });
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
        // The dual-step's headroom rule, as the slider ceiling sees it.
        assert_eq!(act_max(true), 254.0);
        assert_eq!(act_max(false), 255.0);
    }

    #[test]
    fn travel_readouts_span_the_optical_stroke() {
        // The ends are exact; everything between is the documented linear map.
        assert_eq!(mm_text(0), "0.00");
        assert_eq!(mm_text(255), "4.00");
        assert_eq!(mm_text(148), "2.32");
        assert_eq!(mm_text(DEFAULT_ACTUATION_POINT), "1.51");
    }

    #[test]
    fn every_byte_survives_a_trip_through_mm() {
        // A field shows a depth and reads one back, so the map must not lose a
        // byte anywhere in the range — otherwise a blur alone would move a
        // threshold the user never touched.
        for b in 0..=255u8 {
            assert_eq!(mm_to_byte(byte_to_mm(b)), b, "byte {b}");
        }
    }

    /// A daemon-reported config to seed an edit from. The dual-step is the
    /// interesting axis, so it is what the arguments carry.
    fn cfg(second: Option<u8>, action: Option<&str>) -> AnalogKeyConfig {
        AnalogKeyConfig {
            index: 3,
            key: "KEY_Q".to_string(),
            actuation_point: DEFAULT_ACTUATION_POINT,
            release_point: DEFAULT_RELEASE_POINT,
            second_actuation_point: second,
            second_action: action.map(str::to_string),
        }
    }

    #[test]
    fn a_seeded_edit_never_differs_from_the_config_that_seeded_it() {
        // `differs_from` gates a re-seed that runs from `render`, so every
        // difference it reports MUST be settled by one re-seed — a difference
        // that survives seeding would spin the frame loop. That has to hold for
        // configs the seed cannot round-trip, too: a second action with no
        // second point, and the reverse.
        for c in [
            cfg(None, None),
            cfg(Some(0xD0), Some("KEY_LEFTSHIFT")),
            cfg(None, Some("KEY_LEFTSHIFT")),
            cfg(Some(0xD0), None),
        ] {
            assert!(!KeyEdit::seed(&c).differs_from(&c), "{c:?}");
        }
    }

    #[test]
    fn an_external_change_to_the_key_is_seen() {
        // What a daemon-side Discard looks like from the editor: the same key,
        // different values.
        let base = cfg(None, None);
        let edit = KeyEdit::seed(&base);
        let mut moved = base.clone();
        moved.actuation_point = 0x40;
        assert!(edit.differs_from(&moved));
        let mut moved = base.clone();
        moved.release_point = 0x20;
        assert!(edit.differs_from(&moved));
        // A dual-step appearing...
        let dual = cfg(Some(0xD0), Some("KEY_LEFTSHIFT"));
        assert!(edit.differs_from(&dual));
        // ...its point moving, its action changing, and it going away again.
        let dual_edit = KeyEdit::seed(&dual);
        assert!(dual_edit.differs_from(&cfg(Some(0xE0), Some("KEY_LEFTSHIFT"))));
        assert!(dual_edit.differs_from(&cfg(Some(0xD0), Some("KEY_LEFTALT"))));
        assert!(dual_edit.differs_from(&base));
    }

    #[test]
    fn a_live_typed_depth_is_range_checked_in_mm_not_on_its_byte() {
        // `live_field` checks the typed DEPTH against the field's range in mm.
        // Checking the converted byte instead would wave through anything past
        // the end of the stroke, because `mm_to_byte` saturates there — here is
        // that difference, on the actuation row's 1..=255.
        let (lo, hi) = (1u8, 255u8);
        let typed = 40.0; // ten times the whole travel
        assert!(!(byte_to_mm(lo)..=byte_to_mm(hi)).contains(&typed));
        assert!(
            (lo..=hi).contains(&mm_to_byte(typed)),
            "a byte-domain check would have accepted it"
        );
        // In range, the two agree — a valid depth converts inside the range.
        let typed = byte_to_mm(200);
        assert!((byte_to_mm(lo)..=byte_to_mm(hi)).contains(&typed));
        assert_eq!(mm_to_byte(typed), 200);
    }

    #[test]
    fn typed_depths_outside_the_stroke_saturate() {
        assert_eq!(mm_to_byte(-1.0), 0);
        assert_eq!(mm_to_byte(0.0), 0);
        assert_eq!(mm_to_byte(TRAVEL_MM), 255);
        assert_eq!(mm_to_byte(99.0), 255);
        // A depth between two bytes snaps to the nearer one.
        assert_eq!(mm_to_byte(byte_to_mm(100) + 0.004), 100);
    }
}

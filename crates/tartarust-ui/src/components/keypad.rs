//! The signature element: a rendered Tartarus Pro, reused across three screens.
//!
//! One component, one geometry. The `mode` decides what each cap shows and how
//! it reacts — bind labels (Customize), colour glow (Lighting), or live analog
//! pressure (Calibration) — but selection and click-to-inspect work the same on
//! all three. The main block is **19 keys in rows of 5/5/5/4** (numbers on top,
//! no F-keys); the thumb cluster is the 8-way pad, scroll wheel, spacebar, and
//! the (digital-only) thumb button.
//!
//! This is a [`RenderOnce`] element and holds no state. Under Dioxus it read the
//! global store directly; here the host screen passes everything in — the
//! selected cap, this layer's binds, the lighting state — and every interaction
//! goes back out through a callback. Two inputs serve the screens that compose
//! it:
//!   * `layer` — which bind layer this keypad edits (Customize renders TWO,
//!     Base + Hypershift). It also namespaces the element ids, so the two boards
//!     can coexist in one frame.
//!   * `popover` — Calibration passes its floating actuation editor here; the
//!     keypad renders it INSIDE the selected cap (every cap is `.relative()`)
//!     so the popover anchors to whichever cap was clicked. The popover element
//!     supplies its own `.absolute()` offsets.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Background, Div, ElementId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Rgba, SharedString, Stateful, Window, div, linear_color_stop, linear_gradient,
    rgb,
};

use crate::model::{
    BindAction, Effect, GRID_ROWS, KeyId, Layer, LightingState, Rgb, ThumbAxisMode, ThumbDir,
    WheelDir, cap_function,
};
use crate::theme;

// ---------------------------------------------------------------------------
// Geometry.
//
// The stylesheet sized the whole board in container-query units — `--kp:
// clamp(42px, 13.5cqw, 66px)` for a cap, everything else derived from it — so
// the keypad scaled with the window. GPUI has no container queries and the
// window is fixed at 1280x800, so the tokens below are that scale resolved
// once. Every CSS ratio survives (thumb box = 2.71 caps, direction cap = 0.74,
// wheel = 0.54 x 0.65 of a cap, thumb button = 1.5 direction caps), so moving
// [`CAP`] still rescales the board in proportion.
// ---------------------------------------------------------------------------

/// One grid cap, square (CSS `--kp`).
pub const CAP: f32 = 48.0;
/// Inter-cap and inter-row gap (CSS `--kp-gap`).
const GAP: f32 = 8.0;
/// Gap between the main grid and the thumb cluster (CSS `--kp-side`).
const SIDE_GAP: f32 = 24.0;
/// Vertical gap inside the thumb cluster (CSS `calc(var(--kp-gap) * 1.6)`).
const SIDE_ROW_GAP: f32 = GAP * 1.6;
/// Thumb box edge — the octagon's bounding square (CSS `--kp-thumb`).
const THUMB_BOX: f32 = CAP * 2.71;
/// One thumbpad direction cap (CSS `--kp-dir`).
const DIR_CAP: f32 = CAP * 0.74;
/// Cap corner radius: machined, not rounded (CSS `--kp-r`).
const CAP_R: f32 = 4.0;
/// Scroll wheel body (CSS `calc(var(--kp-thumb) * 0.54)` by `calc(var(--kp) * 0.65)`).
const WHEEL_W: f32 = THUMB_BOX * 0.54;
const WHEEL_H: f32 = CAP * 0.65;
/// The thumb button disc (CSS `calc(var(--kp-dir) * 1.5)`, a circle).
const THUMB_BTN: f32 = DIR_CAP * 1.5;
/// The hub's box. The CSS centred an auto-width button with
/// `transform: translate(-50%, -50%)`; GPUI has no transform, so the hub takes
/// the CSS max-width (the clear gap between opposing direction caps) as a real
/// width and is positioned by arithmetic instead.
const HUB_W: f32 = THUMB_BOX - 2.0 * DIR_CAP - 6.0;
const HUB_H: f32 = 22.0;

/// Deck plate padding (CSS `clamp(16px, 4cqw, 26px) clamp(18px, 4.5cqw, 30px)
/// clamp(22px, 5cqw, 32px)` at this scale).
const DECK_PT: f32 = 16.0;
const DECK_PX: f32 = 18.0;
const DECK_PB: f32 = 22.0;

// Type sizes, each the CSS `clamp()` resolved at this scale.
const LEGEND_SIZE: f32 = 11.0; // clamp(10px, 3.1cqw, 13px)
const FUNC_SIZE: f32 = 8.0; //    clamp(7.5px, 2.2cqw, 9.5px)
const DIR_SIZE: f32 = 9.5; //     clamp(9px, 2.6cqw, 11px)
const HUB_SIZE: f32 = 7.5; //     clamp(7.5px, 2.1cqw, 9px)
const HUB_TOGGLE_SIZE: f32 = 7.0; // clamp(6.5px, 1.7cqw, 8.5px)
const WHEEL_SIZE: f32 = 10.0; //  clamp(10px, 2.8cqw, 12px)
const BAR_SIZE: f32 = 9.5; //     clamp(9px, 2.6cqw, 11px)
const ENGRAVE_SIZE: f32 = 8.0;

/// How many distinct inputs the board has, for element-id namespacing.
const KEY_SLOTS: usize = 31;

/// (direction, left %, top %) within the thumb box — the device's octagon as
/// fractions of the box edge, straight from the stylesheet.
const THUMB_SLOTS: [(ThumbDir, f32, f32); 8] = [
    (ThumbDir::NW, 4.76, 4.76),
    (ThumbDir::N, 36.31, 0.0),
    (ThumbDir::NE, 67.86, 4.76),
    (ThumbDir::W, 0.0, 36.31),
    (ThumbDir::E, 72.62, 36.31),
    (ThumbDir::SW, 4.76, 67.86),
    (ThumbDir::S, 36.31, 72.62),
    (ThumbDir::SE, 67.86, 67.86),
];

// ---------------------------------------------------------------------------
// Public surface.
// ---------------------------------------------------------------------------

/// What the keypad is being used for on the current screen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeypadMode {
    /// Customize: each cap shows its bind label + type tag.
    Bind,
    /// Lighting: each cap glows in its effect/custom colour.
    Lighting,
    /// Calibration: grid/spacebar caps fill from the bottom by live analog
    /// pressure; clicking a configured cap anchors the actuation editor inside it.
    Pressure,
}

/// One phase of a cap pointer interaction, for Calibration's drag/shift
/// multi-select.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CapPhase {
    /// Mouse button pressed on the cap (drag start / shift-click).
    Down,
    /// Mouse moved onto the cap mid-drag (extends an in-progress drag).
    Enter,
    /// Mouse released on the cap (drag end).
    Up,
}

/// A cap pointer event surfaced to the host screen (Calibration) so it can drive
/// drag-to-select and shift-click multi-select. Carries which cap, the phase,
/// whether Shift was held, and whether the primary mouse button is currently down
/// (so a drag-extend on `Enter` can't fire on a buttonless hover).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CapPointer {
    /// The cap the pointer is on.
    pub key: KeyId,
    /// The interaction phase.
    pub phase: CapPhase,
    /// Whether Shift was held at the time.
    pub shift: bool,
    /// Whether the primary mouse button is held (true during an active drag).
    pub primary_held: bool,
}

/// Fires when a cap is clicked outside Calibration's multi-select. Carries the
/// cap and the keypad's own layer, because Customize renders one board per layer
/// and a click must tell the host WHICH board it came from.
pub type SelectHandler = Rc<dyn Fn(KeyId, Layer, &mut Window, &mut App)>;
/// Fires for every pointer phase on an analog cap (Calibration multi-select).
pub type PointerHandler = Rc<dyn Fn(CapPointer, &mut Window, &mut App)>;
/// Fires when the thumbpad hub toggles between 4-way and 8-way; carries the
/// mode the host should switch TO.
pub type AxisModeHandler = Rc<dyn Fn(ThumbAxisMode, &mut Window, &mut App)>;

/// The rendered keypad. Build with [`Keypad::new`] and layer the optional inputs
/// on with the builder methods; everything not set renders as a static board.
#[derive(IntoElement)]
pub struct Keypad {
    mode: KeypadMode,
    layer: Layer,
    analog_only: bool,
    binds: BTreeMap<String, BindAction>,
    lighting: LightingState,
    thumb_axis_mode: ThumbAxisMode,
    pressures: Option<HashMap<String, u8>>,
    multi_selected: BTreeSet<KeyId>,
    customized: BTreeSet<String>,
    selected: Option<KeyId>,
    popover: Option<AnyElement>,
    on_select: Option<SelectHandler>,
    on_cap_pointer: Option<PointerHandler>,
    on_axis_mode: Option<AxisModeHandler>,
}

impl Keypad {
    /// A keypad in `mode`, editing `layer`. Lighting and Calibration have a
    /// single board and pass `Layer::Base`; Customize passes the real layer so
    /// its two boards get distinct element ids and distinct click callbacks.
    pub fn new(mode: KeypadMode, layer: Layer) -> Self {
        Self {
            mode,
            layer,
            analog_only: false,
            binds: BTreeMap::new(),
            lighting: LightingState::default(),
            thumb_axis_mode: ThumbAxisMode::default(),
            pressures: None,
            multi_selected: BTreeSet::new(),
            customized: BTreeSet::new(),
            selected: None,
            popover: None,
            on_select: None,
            on_cap_pointer: None,
            on_axis_mode: None,
        }
    }

    /// Render ONLY the analog-calibratable caps — the 19 main keys and the
    /// spacebar — and hide the 8-way thumbpad, scroll wheel, and thumb button,
    /// none of which are analog. Calibration sets this; the other screens leave
    /// it false and show the whole device.
    pub fn analog_only(mut self, analog_only: bool) -> Self {
        self.analog_only = analog_only;
        self
    }

    /// This layer's binds, keyed by evdev name — the same map the profile holds
    /// (`profile.layer(layer)`). Drives the Bind-mode cap labels.
    pub fn binds(mut self, binds: BTreeMap<String, BindAction>) -> Self {
        self.binds = binds;
        self
    }

    /// The active profile's lighting, for the Lighting-mode cap faces.
    pub fn lighting(mut self, lighting: LightingState) -> Self {
        self.lighting = lighting;
        self
    }

    /// The active profile's thumb axis mode. `Four` hides the diagonals.
    pub fn thumb_axis_mode(mut self, mode: ThumbAxisMode) -> Self {
        self.thumb_axis_mode = mode;
        self
    }

    /// Live per-key analog pressure (evdev name -> 0..255), driven by Calibration.
    /// `None` renders exactly as a static keypad; `Some` fills the grid and
    /// spacebar caps from the bottom. The 8-way pad, wheel, and thumb button are
    /// digital-only and never carry pressure.
    pub fn pressures(mut self, pressures: Option<HashMap<String, u8>>) -> Self {
        self.pressures = pressures;
        self
    }

    /// Caps currently in the multi-selection set (Calibration); highlighted.
    pub fn multi_selected(mut self, keys: BTreeSet<KeyId>) -> Self {
        self.multi_selected = keys;
        self
    }

    /// Analog caps whose actuation settings deviate from the defaults, keyed by
    /// evdev name like `pressures`. Each gets a small marker dot so customized
    /// keys read at a glance (Calibration; empty elsewhere).
    pub fn customized(mut self, keys: BTreeSet<String>) -> Self {
        self.customized = keys;
        self
    }

    /// The selected cap on THIS board. Customize must pass `None` to the layer
    /// that is not holding the selection — the old component resolved that
    /// itself by comparing the store's edit layer against its own.
    pub fn selected(mut self, key: Option<KeyId>) -> Self {
        self.selected = key;
        self
    }

    /// An overlay (Calibration's actuation editor) rendered INSIDE the currently
    /// selected cap so it floats beside the clicked key. Only reaches a cap in
    /// `Pressure` mode, matching the old behaviour.
    pub fn popover(mut self, popover: AnyElement) -> Self {
        self.popover = Some(popover);
        self
    }

    /// Single-cap selection. Ignored while [`Keypad::on_cap_pointer`] is set:
    /// the host owns selection then, exactly as before.
    pub fn on_select(mut self, handler: SelectHandler) -> Self {
        self.on_select = Some(handler);
        self
    }

    /// Calibration multi-select: pointer events per analog cap. When set, a cap's
    /// normal single-select click is suppressed and Down/Enter/Up pointer events
    /// (with Shift state) are forwarded here instead, so the screen can drive
    /// drag- and shift-click selection.
    pub fn on_cap_pointer(mut self, handler: PointerHandler) -> Self {
        self.on_cap_pointer = Some(handler);
        self
    }

    /// The thumbpad hub's 4-way/8-way toggle (Bind mode only). Dropping to
    /// `Four` hides the diagonals, so the host must also clear a selection that
    /// just vanished — that write lived in the store before.
    pub fn on_axis_mode(mut self, handler: AxisModeHandler) -> Self {
        self.on_axis_mode = Some(handler);
        self
    }
}

// ---------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------

/// The mode-specific part of a cap face, resolved once per cap.
struct CapLook {
    /// The physical device legend.
    legend: String,
    /// The cap's CURRENT function, beneath the legend (Bind mode; else empty).
    func: String,
    /// The bind is `Disable` — dim the cap and strike the legend.
    disabled: bool,
    /// Lighting mode: the lit face colour, and the legend ink chosen for it.
    face: Option<Rgba>,
    ink: Option<Rgba>,
}

impl RenderOnce for Keypad {
    fn render(mut self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mono = theme::mono(cx);

        // Which cap (if any) hosts the popover — the selected cap, in Pressure
        // mode. Moved out of `self` up front so exactly one cap can consume it.
        let host = (self.mode == KeypadMode::Pressure)
            .then_some(self.selected)
            .flatten();
        let mut popover = self.popover.take();

        // Main block: 19 keys in rows of 5 / 5 / 5 / 4. The short final row
        // aligns LEFT so there is no phantom gap where a 5th cap would sit.
        let mut rows: Vec<AnyElement> = Vec::with_capacity(GRID_ROWS.len());
        for (r, &len) in GRID_ROWS.iter().enumerate() {
            let start = GRID_ROWS[..r].iter().sum::<usize>() as u8;
            let mut caps: Vec<AnyElement> = Vec::with_capacity(len);
            for c in 0..len {
                let key = KeyId::Grid(start + c as u8);
                let slot = (host == Some(key)).then(|| popover.take()).flatten();
                caps.push(self.grid_cap(key, slot, &mono));
            }
            rows.push(
                div()
                    .flex()
                    .gap(theme::sp(GAP))
                    .justify_start()
                    .children(caps)
                    .into_any_element(),
            );
        }
        let main_block = div().flex().flex_col().gap(theme::sp(GAP)).children(rows);

        // Thumb cluster: 8-way pad, scroll wheel, spacebar, thumb button. In
        // `analog_only` mode (Calibration) only the spacebar — the one analog cap
        // here — is shown; the rest aren't analog, so they're hidden.
        let space_popover = (host == Some(KeyId::Space))
            .then(|| popover.take())
            .flatten();
        let side = div()
            .flex()
            .flex_col()
            .gap(theme::sp(SIDE_ROW_GAP))
            .items_center()
            .when(!self.analog_only, |el| {
                el.child(self.thumbpad(&mono)).child(self.wheel(&mono))
            })
            .child(self.spacebar(space_popover, &mono))
            .when(!self.analog_only, |el| el.child(self.thumb_button(&mono)));

        // Calibration's analog-only board has no thumb cluster: the spacebar
        // drops BELOW the grid (a column) instead of leaving a dead right bay.
        let board = div()
            .flex()
            .gap(theme::sp(SIDE_GAP))
            .when(self.analog_only, |el| el.flex_col().items_center())
            .when(!self.analog_only, |el| el.items_start().justify_center())
            .child(main_block)
            .child(side);

        // The deck plate: one continuous panel behind both clusters, so the
        // keypad reads as the physical device sitting on the stage. The engraved
        // designation line is part of the plate, not UI chrome.
        div().flex().flex_col().items_center().child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .pt(theme::sp(DECK_PT))
                .px(theme::sp(DECK_PX))
                .pb(theme::sp(DECK_PB))
                .rounded(theme::sp(12.))
                .bg(plate_face())
                .border_1()
                .border_color(theme::line())
                .shadow_lg()
                .child(board)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .justify_end()
                        .mt(theme::sp(10.))
                        .child(
                            div()
                                .font_family(mono)
                                .text_size(theme::sp(ENGRAVE_SIZE))
                                .text_color(theme::mute())
                                .opacity(0.75)
                                .child("TARTARUS PRO · USB 1532:0244"),
                        ),
                ),
        )
    }
}

impl Keypad {
    /// True when `key` is the selected cap on THIS board. Selection used to be
    /// layer-scoped inside the component; the host resolves that now and passes
    /// the already-scoped value, so this is a plain comparison.
    fn is_selected(&self, key: KeyId) -> bool {
        self.selected == Some(key)
    }

    /// This cap's live pressure, if the host is feeding any.
    fn press_of(&self, key: KeyId) -> Option<u8> {
        self.pressures
            .as_ref()
            .and_then(|m| key.evdev().and_then(|n| m.get(n).copied()))
    }

    /// Whether this cap's actuation deviates from the defaults.
    fn custom_of(&self, key: KeyId) -> bool {
        key.evdev().is_some_and(|n| self.customized.contains(n))
    }

    /// This cap's bind on this keypad's layer.
    fn bind_of(&self, key: KeyId) -> Option<BindAction> {
        key.evdev().and_then(|n| self.binds.get(n).cloned())
    }

    /// A per-frame-unique element id. Customize renders TWO keypads at once
    /// (Base + Hypershift) and GPUI element ids must not collide within a frame,
    /// so the layer is folded into every id this board mints.
    fn el_id(&self, prefix: &'static str, slot: usize) -> ElementId {
        let offset = match self.layer {
            Layer::Base => 0,
            Layer::Hypershift => KEY_SLOTS,
        };
        (prefix, offset + slot).into()
    }

    /// The cap's mode-specific face. In Bind mode the cap shows a readable label
    /// of its CURRENT function (bound value, or the arrow default for a thumb
    /// cardinal) beneath the physical legend — never a cryptic kind tag.
    fn look(&self, key: KeyId) -> CapLook {
        let legend = key.cap();
        match self.mode {
            KeypadMode::Bind => {
                let bind = self.bind_of(key);
                CapLook {
                    legend,
                    func: cap_function(key, bind.as_ref()),
                    disabled: matches!(bind, Some(BindAction::Disable)),
                    face: None,
                    ink: None,
                }
            }
            KeypadMode::Lighting => {
                let (face, ink) = lit_face_style(&lit_color(&self.lighting, key));
                CapLook {
                    legend,
                    func: String::new(),
                    disabled: false,
                    face: Some(face),
                    ink: Some(ink),
                }
            }
            // Calibration: bare cap, decoration comes from the pressure fill.
            KeypadMode::Pressure => CapLook {
                legend,
                func: String::new(),
                disabled: false,
                face: None,
                ink: None,
            },
        }
    }

    /// A single main-grid cap.
    fn grid_cap(&self, key: KeyId, popover: Option<AnyElement>, mono: &SharedString) -> AnyElement {
        let look = self.look(key);
        let selected = self.is_selected(key);
        let multi = self.multi_selected.contains(&key);

        let cap = cap_shell(self.el_id("kp-cap", key_slot(key)), selected, multi)
            .w(theme::sp(CAP))
            .h(theme::sp(CAP))
            .when_some(look.face, |el, face| el.bg(face));

        self.wire(cap, key)
            .when(look.disabled, |el| el.opacity(0.4))
            .when(multi, |el| el.child(multi_wash(CAP_R)))
            .when_some(self.press_of(key), |el, v| {
                el.child(pressure_fill(v, CAP, CAP_R))
            })
            .child(cap_legend(&look, mono, LEGEND_SIZE))
            .when(!look.func.is_empty(), |el| {
                el.child(cap_func(&look.func, selected, mono, CAP - 6.0))
            })
            .when(self.custom_of(key), |el| el.child(cal_dot()))
            .children(popover)
            .into_any_element()
    }

    /// 8-way directional thumbpad, laid out octagonally by absolute position
    /// inside the thumb box. In `Four`-way mode the diagonals (NW/NE/SW/SE) are
    /// hidden, leaving N/E/S/W.
    fn thumbpad(&self, mono: &SharedString) -> AnyElement {
        let four = self.thumb_axis_mode == ThumbAxisMode::Four;
        let caps = THUMB_SLOTS
            .into_iter()
            .filter(|(d, _, _)| !(four && d.is_diagonal()))
            .map(|(dir, left, top)| self.thumb_dir_cap(dir, left, top, mono))
            .collect::<Vec<_>>();

        div()
            .relative()
            .w(theme::sp(THUMB_BOX))
            .h(theme::sp(THUMB_BOX))
            .children(caps)
            .child(self.hub(four, mono))
            .into_any_element()
    }

    /// One direction cap of the thumbpad.
    fn thumb_dir_cap(
        &self,
        dir: ThumbDir,
        left: f32,
        top: f32,
        mono: &SharedString,
    ) -> impl IntoElement {
        let key = KeyId::Thumb(dir);
        let selected = self.is_selected(key);

        // In Bind mode show the cap's current function: the arrow a cardinal
        // defaults to (↑→↓←), or a bound value; fall back to the compass label
        // (diagonals, other modes). Dim it when the bind is Disable, like the
        // grid caps.
        let bind = (self.mode == KeypadMode::Bind)
            .then(|| self.bind_of(key))
            .flatten();
        let disabled = matches!(bind, Some(BindAction::Disable));
        let label = if self.mode == KeypadMode::Bind {
            let f = cap_function(key, bind.as_ref());
            if f.is_empty() {
                dir.label().to_string()
            } else {
                f
            }
        } else {
            dir.label().to_string()
        };

        let lit = (self.mode == KeypadMode::Lighting)
            .then(|| lit_face_style(&lit_color(&self.lighting, key)));
        let ink = match (selected, disabled, lit) {
            (_, true, _) => theme::mute(),
            (true, _, _) => theme::ink(),
            (_, _, Some((_, ink))) => ink,
            _ => theme::dim(),
        };

        let cap = cap_shell(self.el_id("kp-dir", key_slot(key)), selected, false)
            .absolute()
            .left(theme::sp(THUMB_BOX * left / 100.0))
            .top(theme::sp(THUMB_BOX * top / 100.0))
            .size(theme::sp(DIR_CAP))
            .font_family(mono.clone())
            .text_size(theme::sp(DIR_SIZE))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(ink)
            .when_some(lit.map(|(face, _)| face), |el, face| el.bg(face));

        self.wire(cap, key)
            .when(disabled, |el| el.opacity(0.4).line_through())
            .child(label)
    }

    /// The thumbpad hub. In Bind mode it is a single 4-way/8-way TOGGLE that
    /// shows the CURRENT mode and flips to the other on click; the other modes
    /// keep a static label.
    fn hub(&self, four: bool, mono: &SharedString) -> AnyElement {
        let label = if four { "4-WAY" } else { "8-WAY" };

        if self.mode != KeypadMode::Bind {
            // Static hub: a dashed circle dead-centre of the box. Centred by
            // arithmetic — GPUI has no `transform: translate(-50%, -50%)`.
            return div()
                .absolute()
                .left(theme::sp((THUMB_BOX - DIR_CAP) / 2.0))
                .top(theme::sp((THUMB_BOX - DIR_CAP) / 2.0))
                .size(theme::sp(DIR_CAP))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme::panel_1())
                .border_1()
                .border_dashed()
                .border_color(theme::line())
                .font_family(mono.clone())
                .text_size(theme::sp(HUB_SIZE))
                .text_color(theme::mute())
                .child(label)
                .into_any_element();
        }

        // The `four` state fills rust so the active mode reads at a glance.
        let next = if four {
            ThumbAxisMode::Eight
        } else {
            ThumbAxisMode::Four
        };
        let on_axis_mode = self.on_axis_mode.clone();
        div()
            .id(self.el_id("kp-hub", 0))
            .absolute()
            .left(theme::sp((THUMB_BOX - HUB_W) / 2.0))
            .top(theme::sp((THUMB_BOX - HUB_H) / 2.0))
            .w(theme::sp(HUB_W))
            .h(theme::sp(HUB_H))
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme::sp(theme::RADIUS_SM))
            .border_1()
            .font_family(mono.clone())
            .text_size(theme::sp(HUB_TOGGLE_SIZE))
            .font_weight(gpui::FontWeight::BOLD)
            .when(four, |el| {
                el.bg(theme::rust())
                    .border_color(theme::rust())
                    .text_color(theme::chassis())
            })
            .when(!four, |el| {
                el.bg(theme::panel_2())
                    .border_color(theme::line())
                    .text_color(theme::dim())
                    .hover(|s| s.text_color(theme::ink()).border_color(theme::mute()))
            })
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                if let Some(h) = &on_axis_mode {
                    h(next, window, cx);
                }
            })
            .child(label)
            .into_any_element()
    }

    /// The scroll wheel: one body, two selectable segments. In Lighting mode the
    /// body takes the effect colour and the glyphs a luminance-matched ink —
    /// except on a selected segment, which keeps rust, the wheel's only
    /// selection feedback.
    fn wheel(&self, mono: &SharedString) -> AnyElement {
        let up = KeyId::Wheel(WheelDir::Up);
        let down = KeyId::Wheel(WheelDir::Down);
        let lit = (self.mode == KeypadMode::Lighting)
            .then(|| lit_face_style(&lit_color(&self.lighting, up)));

        div()
            .flex()
            .items_center()
            .justify_around()
            .w(theme::sp(WHEEL_W))
            .h(theme::sp(WHEEL_H))
            .rounded_full()
            .bg(cap_face())
            .border_1()
            .border_color(theme::line())
            .shadow_sm()
            .font_family(mono.clone())
            .text_size(theme::sp(WHEEL_SIZE))
            .when_some(lit.map(|(face, _)| face), |el, face| el.bg(face))
            .child(self.wheel_seg(up, "▲", lit.map(|(_, ink)| ink)))
            .child(self.wheel_seg(down, "▼", lit.map(|(_, ink)| ink)))
            .into_any_element()
    }

    /// One wheel segment.
    fn wheel_seg(
        &self,
        key: KeyId,
        glyph: &'static str,
        lit_ink: Option<Rgba>,
    ) -> impl IntoElement {
        let selected = self.is_selected(key);
        let on_select = self.on_select.clone();
        let layer = self.layer;
        div()
            .id(self.el_id("kp-wseg", key_slot(key)))
            .px(theme::sp(6.))
            .py(theme::sp(4.))
            .rounded(theme::sp(theme::RADIUS_SM))
            .text_color(match (selected, lit_ink) {
                (true, _) => theme::rust(),
                (_, Some(ink)) => ink,
                _ => theme::dim(),
            })
            .when(!selected, |el| el.hover(|s| s.text_color(theme::ink())))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                if let Some(h) = &on_select {
                    h(key, layer, window, cx);
                }
            })
            .child(glyph)
    }

    /// The big thumb actuator. Bindable AND analog, so it carries a bind label,
    /// the pressure fill, the customized marker, and the Calibration popover.
    fn spacebar(&self, popover: Option<AnyElement>, mono: &SharedString) -> AnyElement {
        let key = KeyId::Space;
        let selected = self.is_selected(key);
        let multi = self.multi_selected.contains(&key);
        let bind = (self.mode == KeypadMode::Bind)
            .then(|| self.bind_of(key))
            .flatten();
        let disabled = matches!(bind, Some(BindAction::Disable));
        let func = cap_function(key, bind.as_ref());
        let lit = (self.mode == KeypadMode::Lighting)
            .then(|| lit_face_style(&lit_color(&self.lighting, key)));

        let bar = cap_shell(self.el_id("kp-space", key_slot(key)), selected, multi)
            .w(theme::sp(THUMB_BOX))
            .h(theme::sp(DIR_CAP))
            .font_family(mono.clone())
            .text_size(theme::sp(BAR_SIZE))
            .text_color(match (selected, lit) {
                (true, _) => theme::ink(),
                (_, Some((_, ink))) => ink,
                _ => theme::dim(),
            })
            .when_some(lit.map(|(face, _)| face), |el, face| el.bg(face));

        self.wire(bar, key)
            .when(disabled, |el| el.opacity(0.4))
            .when(multi, |el| el.child(multi_wash(CAP_R)))
            .when_some(self.press_of(key), |el, v| {
                el.child(pressure_fill(v, DIR_CAP, CAP_R))
            })
            // Bind mode stacks the legend over the current function, like the
            // grid caps — the spacebar is bindable, so it must not read as inert.
            .when(self.mode == KeypadMode::Bind, |el| {
                el.child(
                    div()
                        .when(disabled, |l| l.line_through().text_color(theme::mute()))
                        .child("SPACE"),
                )
            })
            .when(self.mode != KeypadMode::Bind, |el| {
                el.child("THUMB · SPACE")
            })
            .when(self.mode == KeypadMode::Bind && !func.is_empty(), |el| {
                el.child(cap_func(&func, selected, mono, THUMB_BOX - 16.0))
            })
            .when(self.custom_of(key), |el| el.child(cal_dot()))
            .children(popover)
            .into_any_element()
    }

    /// The thumb button (emits `KEY_LEFTALT`); digital-only — no analog pressure
    /// byte, so it carries neither a pressure fill nor the Calibration popover
    /// (Calibration hides it entirely via `analog_only`). Round: a distinct,
    /// thumb-sized disc, not a bar.
    fn thumb_button(&self, mono: &SharedString) -> AnyElement {
        let key = KeyId::ThumbButton;
        let selected = self.is_selected(key);
        let bind = (self.mode == KeypadMode::Bind)
            .then(|| self.bind_of(key))
            .flatten();
        let disabled = matches!(bind, Some(BindAction::Disable));
        // Bind mode shows the bound function in place of the "ALT" legend.
        let label = if self.mode == KeypadMode::Bind {
            let f = cap_function(key, bind.as_ref());
            if f.is_empty() { "ALT".to_string() } else { f }
        } else {
            "ALT".to_string()
        };
        let lit = (self.mode == KeypadMode::Lighting)
            .then(|| lit_face_style(&lit_color(&self.lighting, key)));

        let disc = cap_shell(self.el_id("kp-tbtn", key_slot(key)), selected, false)
            .size(theme::sp(THUMB_BTN))
            .rounded_full()
            .font_family(mono.clone())
            .text_size(theme::sp(BAR_SIZE))
            .text_color(match (selected, disabled, lit) {
                (_, true, _) => theme::mute(),
                (true, _, _) => theme::ink(),
                (_, _, Some((_, ink))) => ink,
                _ => theme::dim(),
            })
            .when_some(lit.map(|(face, _)| face), |el, face| el.bg(face));

        self.wire(disc, key)
            .when(disabled, |el| el.opacity(0.4))
            .child(
                // The disc's label truncates so a bound value can't spill out.
                div()
                    .max_w(theme::sp(THUMB_BTN - 12.0))
                    .truncate()
                    .when(disabled, |el| el.line_through())
                    .child(label),
            )
            .into_any_element()
    }

    /// Wire a cap's pointer surface. Selection and Calibration's drag-select stay
    /// mutually exclusive, exactly as before: when the host takes
    /// `on_cap_pointer`, the plain click no longer selects — the host resolves
    /// selection from the pointer phases instead.
    fn wire(&self, el: Stateful<Div>, key: KeyId) -> Stateful<Div> {
        let layer = self.layer;
        let on_select = self.on_select.clone();
        let takes_pointer = self.on_cap_pointer.is_some();
        let (down, moved, up) = (
            self.on_cap_pointer.clone(),
            self.on_cap_pointer.clone(),
            self.on_cap_pointer.clone(),
        );

        el.on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, window, cx| {
            cx.stop_propagation();
            if let Some(h) = &down {
                h(
                    CapPointer {
                        key,
                        phase: CapPhase::Down,
                        shift: e.modifiers.shift,
                        primary_held: true,
                    },
                    window,
                    cx,
                );
            }
        })
        .on_mouse_up(MouseButton::Left, move |e: &MouseUpEvent, window, cx| {
            if let Some(h) = &up {
                h(
                    CapPointer {
                        key,
                        phase: CapPhase::Up,
                        shift: e.modifiers.shift,
                        primary_held: false,
                    },
                    window,
                    cx,
                );
            }
        })
        // Drag-extend. Dioxus gave an `onmouseenter` that carried the modifiers
        // and the held buttons; GPUI's `on_hover` carries neither, so the drag
        // sweep rides `on_mouse_move` instead and the buttonless case is filtered
        // here rather than in the host. The trade: this fires on every move
        // sample over the cap, not once per entry, so the host's Enter handling
        // must be idempotent (adding an already-selected cap to the set is).
        .on_mouse_move(move |e: &MouseMoveEvent, window, cx| {
            if e.pressed_button != Some(MouseButton::Left) {
                return;
            }
            if let Some(h) = &moved {
                h(
                    CapPointer {
                        key,
                        phase: CapPhase::Enter,
                        shift: e.modifiers.shift,
                        primary_held: true,
                    },
                    window,
                    cx,
                );
            }
        })
        .when(!takes_pointer, |el| {
            el.on_click(move |_, window, cx| {
                if let Some(h) = &on_select {
                    h(key, layer, window, cx);
                }
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Cap chassis — the shared pieces every cap is built from.
// ---------------------------------------------------------------------------

/// The machined cap face (CSS `linear-gradient(180deg, #2B2720, #1E1B16)`, taken
/// onto the palette's panel tokens).
fn cap_face() -> Background {
    linear_gradient(
        180.,
        linear_color_stop(theme::panel_2(), 0.),
        linear_color_stop(theme::panel_0(), 1.),
    )
}

/// The deck plate (CSS `linear-gradient(180deg, #232019, #1D1B16)`).
fn plate_face() -> Background {
    linear_gradient(
        180.,
        linear_color_stop(theme::panel_1(), 0.),
        linear_color_stop(theme::panel_0(), 1.),
    )
}

/// The cap's border colour: rust for either selection state, the machined line
/// otherwise. Nothing else feeds it — in particular not the lit face, which is
/// the point of the regression test below.
fn cap_border(selected: bool, multi: bool) -> Rgba {
    if selected || multi {
        theme::rust()
    } else {
        theme::line()
    }
}

/// The shared cap chassis: machined plate, border, drop shadow, and the pointer
/// affordance. The caller adds the size and the mode-specific face.
///
/// The stylesheet drew selection as a border PLUS an outer `box-shadow` ring and
/// an inset highlight; GPUI 0.2 has neither inset nor spread-only shadows, so
/// the rust border carries selection alone.
fn cap_shell(id: ElementId, selected: bool, multi: bool) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .rounded(theme::sp(CAP_R))
        .bg(cap_face())
        .border_1()
        .border_color(cap_border(selected, multi))
        .shadow_sm()
        .when(!selected && !multi, |el| {
            el.hover(|s| s.border_color(theme::mute()))
        })
        .cursor_pointer()
}

/// The warm plate under a multi-selected cap (CSS swapped the face gradient for
/// a rust-leaning one). A rust wash over the standard plate reads the same and
/// stays on the palette.
fn multi_wash(radius: f32) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .rounded(theme::sp(radius))
        .bg(theme::rust_wash())
}

/// Live pressure: a phosphor overlay rising from the bottom of the cap, behind
/// the legend. `v` is the raw 0..255 device byte; the fill height is that as a
/// fraction of the cap. The radius matches the cap so the fill follows the
/// rounded rect instead of poking past it.
fn pressure_fill(v: u8, height: f32, radius: f32) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(theme::sp(height * v as f32 / 255.0))
        .rounded(theme::sp(radius))
        .bg(linear_gradient(
            0.,
            linear_color_stop(theme::live(), 0.),
            linear_color_stop(theme::live_dim(), 1.),
        ))
        .opacity(0.3)
}

/// Caps whose actuation deviates from the defaults carry a small amber marker
/// (drift) so customized keys read at a glance on the Calibration board.
fn cal_dot() -> impl IntoElement {
    div()
        .absolute()
        .top(theme::sp(5.))
        .right(theme::sp(5.))
        .size(theme::sp(5.))
        .rounded_full()
        .bg(theme::amber())
}

/// The cap's physical legend.
fn cap_legend(look: &CapLook, mono: &SharedString, size: f32) -> impl IntoElement {
    div()
        .font_family(mono.clone())
        .text_size(theme::sp(size))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if look.disabled {
            theme::mute()
        } else {
            look.ink.unwrap_or_else(theme::ink)
        })
        .when(look.disabled, |el| el.line_through())
        .child(look.legend.clone())
}

/// The cap's current-function label: readable, quiet (dim — persistent info, not
/// selection), single-line and ellipsised so a long bind never overflows the cap.
fn cap_func(func: &str, selected: bool, mono: &SharedString, max_w: f32) -> impl IntoElement {
    div()
        .mt(theme::sp(3.))
        .max_w(theme::sp(max_w))
        .truncate()
        .font_family(mono.clone())
        .text_size(theme::sp(FUNC_SIZE))
        .text_color(if selected {
            theme::rust()
        } else {
            theme::dim()
        })
        .child(func.to_string())
}

/// A dense 0..30 index for an input, used to mint element ids. The order matches
/// the spectrum spread in [`lit_color`].
fn key_slot(key: KeyId) -> usize {
    match key {
        KeyId::Grid(g) => g as usize,
        KeyId::Thumb(d) => 19 + ThumbDir::ALL.iter().position(|&x| x == d).unwrap_or(0),
        KeyId::Wheel(WheelDir::Up) => 27,
        KeyId::Wheel(WheelDir::Down) => 28,
        KeyId::Space => 29,
        KeyId::ThumbButton => 30,
    }
}

// ---------------------------------------------------------------------------
// Lighting colour helpers — pure, and pinned by the tests at the bottom.
// ---------------------------------------------------------------------------

/// The lit-face colours for Lighting mode: `(face, legend ink)`. The cap face
/// takes the key's colour, with the legend colour picked by the face's luminance
/// so it stays legible on any hue — dark ink on bright faces, light ink on dim
/// ones (so an unlit / near-black face, e.g. `Effect::None` or brightness 0,
/// keeps a readable legend instead of black-on-black). It deliberately yields NO
/// border colour: under the stylesheet an inline `border-color` out-cascaded the
/// `.selected` border and erased selection feedback, and here the border stays
/// [`cap_border`]'s alone.
fn lit_face_style(c: &str) -> (Rgba, Rgba) {
    (face_color(c), lit_legend_ink(c))
}

/// Legend ink for text sitting on face colour `c`. The two literals are the
/// stylesheet's own — they belong to this ported colour logic, not the palette.
fn lit_legend_ink(c: &str) -> Rgba {
    if face_is_bright(c) {
        rgb(0x0A0B0C)
    } else {
        rgb(0xD9D6CF)
    }
}

/// The face colour itself. Unparsable input falls back to the chassis — the same
/// "treat it as dark" default [`face_is_bright`] takes.
fn face_color(c: &str) -> Rgba {
    parse_css_color(c).map_or_else(theme::chassis, |(r, g, b)| {
        rgb(u32::from_be_bytes([0, r, g, b]))
    })
}

/// Parse the three formats [`lit_color`] emits — `#rrggbb`, `rgb(r,g,b)`, and
/// `hsl(h, s%, l%)` — into 8-bit sRGB. Anything else is `None`.
fn parse_css_color(c: &str) -> Option<(u8, u8, u8)> {
    let c = c.trim();
    if let Some(hex) = c.strip_prefix('#') {
        if hex.len() == 6
            && let Ok(v) = u32::from_str_radix(hex, 16)
        {
            return Some(((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return None;
    }
    if let Some(body) = c.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let mut it = body.split(',').map(|p| p.trim().parse::<u8>());
        if let (Some(Ok(r)), Some(Ok(g)), Some(Ok(b))) = (it.next(), it.next(), it.next()) {
            return Some((r, g, b));
        }
        return None;
    }
    if let Some(body) = c.strip_prefix("hsl(").and_then(|s| s.strip_suffix(')')) {
        let mut it = body
            .split(',')
            .map(|p| p.trim().trim_end_matches('%').parse::<f32>());
        if let (Some(Ok(h)), Some(Ok(s)), Some(Ok(l))) = (it.next(), it.next(), it.next()) {
            return Some(hsl_to_rgb(h, s / 100.0, l / 100.0));
        }
    }
    None
}

/// Whether a face colour reads as bright (dark legend) or dark (light legend).
/// Compares perceived luminance against a mid threshold. Anything unparsable is
/// treated as dark: a light legend is the safe default on this dark deck.
fn face_is_bright(c: &str) -> bool {
    const THRESHOLD: f32 = 100.0; // of 0..255 weighted luma
    parse_css_color(c).is_some_and(|(r, g, b)| luma(r, g, b) >= THRESHOLD)
}

/// Perceived (Rec. 601 weighted) luminance of an sRGB colour, 0..255.
fn luma(r: u8, g: u8, b: u8) -> f32 {
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}

/// Standard HSL -> RGB (h in degrees, s/l in 0..1), for the luminance check.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

/// Resolve the colour a key shows in Lighting mode from the active profile's
/// [`LightingState`]. The IPC `Effect` carries its colour(s) inside the variant
/// (no separate top-level colour), and brightness is 0–255.
fn lit_color(l: &LightingState, key: KeyId) -> String {
    let dim = l.brightness as f32 / 255.0;

    // Colour-bearing variants reduce to a single representative colour.
    let solid = |c: Rgb| {
        let r = (c.r as f32 * dim) as u8;
        let g = (c.g as f32 * dim) as u8;
        let b = (c.b as f32 * dim) as u8;
        format!("rgb({r},{g},{b})")
    };

    match &l.effect {
        Effect::None => "#0e0f10".into(),
        Effect::Static { color } | Effect::Reactive { color, .. } => solid(*color),
        Effect::Breathing { colors } | Effect::Starlight { colors, .. } => solid(
            colors
                .first()
                .copied()
                .unwrap_or(Rgb::new(0x44, 0xD6, 0x2C)),
        ),
        Effect::Custom { frame } => {
            // Frame is a flat 0..21 column row; map grid keys onto it.
            let idx = match key {
                KeyId::Grid(g) => g as usize,
                _ => return "#101418".into(),
            };
            frame
                .get(idx)
                .copied()
                .map_or_else(|| "#101418".into(), solid)
        }
        Effect::Spectrum | Effect::Wave { .. } => {
            // Spread a hue across every input for a believable preview.
            let i = match key {
                KeyId::Grid(g) => g as f32,
                KeyId::Thumb(d) => {
                    19.0 + ThumbDir::ALL.iter().position(|&x| x == d).unwrap_or(0) as f32 * 0.4
                }
                KeyId::Wheel(WheelDir::Up) => 27.0,
                KeyId::Wheel(WheelDir::Down) => 28.0,
                KeyId::Space => 29.0,
                KeyId::ThumbButton => 30.0,
            };
            hsl_css(i / 31.0 * 360.0, dim)
        }
    }
}

fn hsl_css(h: f32, dim: f32) -> String {
    format!("hsl({h:.0}, 90%, {}%)", (50.0 * dim).max(8.0) as u32)
}

#[cfg(test)]
mod tests {
    //! The lit-face legend must flip by face luminance in every colour format
    //! `lit_color` emits — the black-on-black regression (Effect::None,
    //! brightness 0, Custom on non-grid caps) is pinned here.

    use super::*;

    #[test]
    fn near_black_faces_get_a_light_legend() {
        // Effect::None / Custom-non-grid faces, and brightness dialed to 0.
        assert!(!face_is_bright("#0e0f10"));
        assert!(!face_is_bright("#101418"));
        assert!(!face_is_bright("rgb(0,0,0)"));
        assert_eq!(lit_face_style("#0e0f10").1, rgb(0xD9D6CF));
    }

    #[test]
    fn bright_faces_get_a_dark_legend() {
        // Razer green at ~75% brightness (the mock default), and plain white.
        assert!(face_is_bright("rgb(51,160,33)"));
        assert!(face_is_bright("#ffffff"));
        assert_eq!(lit_face_style("rgb(51,160,33)").1, rgb(0x0A0B0C));
    }

    #[test]
    fn hsl_faces_are_judged_by_real_luminance_not_lightness() {
        // Same 50% lightness: green reads bright, blue reads dark.
        assert!(face_is_bright("hsl(120, 90%, 50%)"));
        assert!(!face_is_bright("hsl(240, 90%, 50%)"));
        // The dimmed floor `hsl_css` emits at brightness 0 stays dark.
        assert!(!face_is_bright(&hsl_css(120.0, 0.0)));
    }

    #[test]
    fn unparsable_colours_default_to_dark() {
        assert!(!face_is_bright("var(--rust)"));
        assert!(!face_is_bright("#abc"));
    }

    #[test]
    fn lit_face_style_never_sets_border_color() {
        // Under CSS an inline border-colour out-cascaded the `.selected` border,
        // erasing selection feedback (the review-#11 regression). GPUI has no
        // cascade, so the invariant becomes structural: the lit face yields a
        // face and an ink, never a border, and `cap_border` alone decides.
        let (face, ink) = lit_face_style("rgb(51,160,33)");
        assert_eq!(face, rgb(0x33A021));
        assert_eq!(ink, rgb(0x0A0B0C));
        assert_eq!(cap_border(true, false), theme::rust());
        assert_eq!(cap_border(false, true), theme::rust());
        assert_eq!(cap_border(false, false), theme::line());
    }
}

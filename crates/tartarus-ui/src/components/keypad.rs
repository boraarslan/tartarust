//! The signature element: a rendered Tartarus Pro, reused across three screens.
//!
//! One component, one geometry. The `mode` decides what each cap shows and how
//! it reacts — bind labels (Customize), colour glow (Lighting), or live analog
//! pressure (Calibration) — but selection and click-to-inspect work the same on
//! all three. The main block is **19 keys in rows of 5/5/5/4** (numbers on top,
//! no F-keys); the thumb cluster is the 8-way pad, scroll wheel, spacebar, and
//! the analog thumb button.
//!
//! Two extra props serve the screens that compose it:
//!   * `layer` — which bind layer this keypad edits (Customize renders TWO,
//!     Base + Hypershift; a cap click sets `store.selected` AND `store.layer`).
//!   * `children` — Calibration passes its floating actuation editor here; the
//!     keypad renders it INSIDE the selected cap (each cap is `position:relative`)
//!     so the popover anchors to whichever cap was clicked, at any scale.

use std::collections::{BTreeSet, HashMap};

use dioxus::prelude::*;

use tartarus_ipc::Layer;

use crate::model::{cap_function, BindAction, KeyId, ThumbAxisMode, ThumbDir, WheelDir, GRID_ROWS};
use crate::use_store;

/// What the keypad is being used for on the current screen.
#[derive(Clone, Copy, PartialEq)]
pub enum KeypadMode {
    /// Customize: each cap shows its bind label + type tag.
    Bind,
    /// Lighting: each cap glows in its effect/custom colour.
    Lighting,
    /// Calibration: grid/space/thumb caps fill from the bottom by live analog
    /// pressure; clicking a configured cap anchors the actuation editor inside it.
    Pressure,
}

/// One phase of a cap pointer interaction, for Calibration's drag/shift
/// multi-select.
#[derive(Clone, Copy, PartialEq)]
pub enum CapPhase {
    /// Mouse button pressed on the cap (drag start / shift-click).
    Down,
    /// Mouse entered the cap (extends an in-progress drag).
    Enter,
    /// Mouse released on the cap (drag end).
    Up,
}

/// A cap pointer event surfaced to the host screen (Calibration) so it can drive
/// drag-to-select and shift-click multi-select. Carries which cap, the phase,
/// whether Shift was held, and whether the primary mouse button is currently down
/// (so a drag-extend on `Enter` can't fire on a buttonless hover).
#[derive(Clone, Copy, PartialEq)]
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

#[component]
pub fn Keypad(
    mode: KeypadMode,
    /// Which bind layer this keypad shows/edits. Customize renders one keypad per
    /// layer (Base + Hypershift); Lighting/Calibration leave it at the default.
    #[props(default = Layer::Base)]
    layer: Layer,
    /// When true, render ONLY the analog-calibratable caps — the 19 main keys and
    /// the spacebar — and hide the 8-way thumbpad, scroll wheel, and thumb button,
    /// none of which are analog. Calibration sets this; the other screens leave it
    /// false and show the whole device.
    #[props(default = false)]
    analog_only: bool,
    /// Live per-key analog pressure (evdev name -> 0..255), driven by Calibration.
    /// `None` renders exactly as a static keypad; `Some` fills the grid, spacebar
    /// and thumb-button caps from the bottom. The 8-way pad and wheel never carry
    /// pressure.
    pressures: Option<HashMap<String, u8>>,
    /// Calibration multi-select: pointer events per analog cap. When `Some`, a
    /// cap's normal single-select click is suppressed and Down/Enter/Up pointer
    /// events (with Shift state) are forwarded here instead, so the screen can
    /// drive drag- and shift-click selection.
    #[props(default = None)]
    on_cap_pointer: Option<EventHandler<CapPointer>>,
    /// Caps currently in the multi-selection set (Calibration); highlighted.
    #[props(default = BTreeSet::new())]
    multi_selected: BTreeSet<KeyId>,
    /// Optional overlay (Calibration's actuation editor) rendered INSIDE the
    /// currently-selected cap so it floats beside the clicked key.
    children: Element,
) -> Element {
    // Which cap (if any) hosts the `children` overlay — the selected cap, in
    // Pressure mode. Read once here so only that one cap pays for a clone.
    let sel = use_store().read().selected;
    let host = |key: KeyId| mode == KeypadMode::Pressure && sel == Some(key);

    let press_of = |key: KeyId| -> Option<u8> {
        pressures
            .as_ref()
            .and_then(|m| key.evdev().and_then(|n| m.get(&n).copied()))
    };

    let space_press = press_of(KeyId::Space);
    let thumb_btn_press = press_of(KeyId::ThumbButton);
    let space_modal = if host(KeyId::Space) { children.clone() } else { rsx! {} };
    let thumb_btn_modal = if host(KeyId::ThumbButton) { children.clone() } else { rsx! {} };

    rsx! {
        div { class: "keypad",
            // Main block: 19 keys in rows of 5 / 5 / 5 / 4 (last row left-aligned).
            div { class: "keypad-main",
                for (r , & len) in GRID_ROWS.iter().enumerate() {
                    div { class: "kp-row",
                        for c in 0..len {
                            {
                                let start = GRID_ROWS[..r].iter().sum::<usize>() as u8;
                                let key_id = KeyId::Grid(start + c as u8);
                                let press = press_of(key_id);
                                let modal = if host(key_id) { children.clone() } else { rsx! {} };
                                let multi = multi_selected.contains(&key_id);
                                rsx! { Cap { key_id, mode, layer, press, modal, on_cap_pointer, multi } }
                            }
                        }
                    }
                }
            }

            // Thumb cluster: 8-way pad, scroll wheel, spacebar, analog thumb button.
            // In `analog_only` mode (Calibration) only the spacebar — the one
            // analog cap here — is shown; the rest aren't analog, so they're hidden.
            div { class: "keypad-side",
                if !analog_only {
                    Thumbpad { mode, layer }
                    Wheel { mode, layer }
                }
                Space {
                    mode, layer, press: space_press, modal: space_modal,
                    on_cap_pointer, multi: multi_selected.contains(&KeyId::Space),
                }
                if !analog_only {
                    ThumbButton { mode, layer, press: thumb_btn_press, modal: thumb_btn_modal }
                }
            }
        }
    }
}

/// True when this key is the selected one for THIS keypad. In Bind mode selection
/// is layer-scoped (so only the clicked layer's keypad highlights); other modes
/// have a single keypad, so the layer is ignored.
fn is_selected(s: &crate::model::AppState, key: KeyId, mode: KeypadMode, layer: Layer) -> bool {
    s.selected == Some(key) && (mode != KeypadMode::Bind || s.layer == layer)
}

/// Toggle selection of a key: clicking an unselected key selects it (and, in Bind
/// mode, records which layer's keypad it came from); clicking the ALREADY-selected
/// key deselects it — closing the inspector / the Calibration actuation modal.
fn select(store: &mut Signal<crate::model::AppState>, key: KeyId, mode: KeypadMode, layer: Layer) {
    use dioxus::prelude::WritableExt;
    store.with_mut(|s| {
        let already = s.selected == Some(key) && (mode != KeypadMode::Bind || s.layer == layer);
        if already {
            s.selected = None;
        } else {
            s.selected = Some(key);
            if mode == KeypadMode::Bind {
                s.layer = layer;
            }
        }
    });
}

/// A single main-grid cap.
#[component]
fn Cap(
    key_id: KeyId,
    mode: KeypadMode,
    layer: Layer,
    press: Option<u8>,
    modal: Element,
    #[props(default = None)] on_cap_pointer: Option<EventHandler<CapPointer>>,
    #[props(default = false)] multi: bool,
) -> Element {
    let mut store = use_store();
    let s = store.read();
    let selected = is_selected(&s, key_id, mode, layer);

    let mut classes = String::from("keycap");
    if selected {
        classes.push_str(" selected");
    }
    if multi {
        classes.push_str(" multi");
    }

    // Mode-specific cap content + styling. In Bind mode the cap shows a readable
    // label of its CURRENT function (bound value, or the arrow default for a thumb
    // cardinal) beneath the physical legend — never the old cryptic kind tag.
    let (legend, tag, mut style) = match mode {
        KeypadMode::Bind => {
            let bind = s.bind_for(key_id, layer);
            if matches!(bind, Some(BindAction::Disable)) {
                classes.push_str(" disabled");
            }
            (key_id.cap(), cap_function(key_id, bind.as_ref()), String::new())
        }
        KeypadMode::Lighting => {
            classes.push_str(" lit");
            let color = lit_color(&s, key_id);
            (key_id.cap(), String::new(), format!("--key-color:{color};"))
        }
        // Calibration: bare cap, decoration comes from the pressure overlay below.
        KeypadMode::Pressure => (key_id.cap(), String::new(), String::new()),
    };
    drop(s);

    push_pressure_fill(press, &mut classes, &mut style);

    rsx! {
        div {
            class: "{classes}",
            style: "{style}",
            onclick: move |e| {
                e.stop_propagation();
                // When the host drives multi-select (Calibration), the pointer
                // handlers below own selection — a plain click is resolved there.
                if on_cap_pointer.is_none() {
                    select(&mut store, key_id, mode, layer);
                }
            },
            onmousedown: move |e| cap_pointer(&on_cap_pointer, key_id, CapPhase::Down, &e),
            onmouseenter: move |e| cap_pointer(&on_cap_pointer, key_id, CapPhase::Enter, &e),
            onmouseup: move |e| cap_pointer(&on_cap_pointer, key_id, CapPhase::Up, &e),
            span { class: "cap-legend", "{legend}" }
            if !tag.is_empty() {
                span { class: "cap-func", "{tag}" }
            }
            {modal}
        }
    }
}

/// Forward a cap pointer event to the host's multi-select handler, if any.
fn cap_pointer(
    handler: &Option<EventHandler<CapPointer>>,
    key: KeyId,
    phase: CapPhase,
    e: &Event<MouseData>,
) {
    if let Some(h) = handler {
        h.call(CapPointer {
            key,
            phase,
            shift: e.modifiers().contains(Modifiers::SHIFT),
            primary_held: e
                .held_buttons()
                .contains(dioxus::html::input_data::MouseButton::Primary),
        });
    }
}

/// 8-way directional thumbpad, laid out octagonally via absolute positioning in
/// PERCENTAGES of the thumb box (so it scales with the responsive box). In
/// `Four`-way mode the diagonals (NW/NE/SW/SE) are hidden, leaving N/E/S/W.
#[component]
fn Thumbpad(mode: KeypadMode, layer: Layer) -> Element {
    let mut store = use_store();
    let four = store.read().profile.thumb_axis_mode == ThumbAxisMode::Four;
    let hub_label = if four { "4-WAY" } else { "8-WAY" };

    // (dir, left%, top%) within the thumb box — the device's 168px octagon as
    // fractions, so the layout survives responsive scaling.
    let slots: [(ThumbDir, &str, &str); 8] = [
        (ThumbDir::NW, "4.76", "4.76"),
        (ThumbDir::N, "36.31", "0"),
        (ThumbDir::NE, "67.86", "4.76"),
        (ThumbDir::W, "0", "36.31"),
        (ThumbDir::E, "72.62", "36.31"),
        (ThumbDir::SW, "4.76", "67.86"),
        (ThumbDir::S, "36.31", "72.62"),
        (ThumbDir::SE, "67.86", "67.86"),
    ];
    rsx! {
        div { class: "thumbpad",
            for (dir , left , top) in slots
                .into_iter()
                .filter(|(d, _, _)| !(four && d.is_diagonal()))
            {
                ThumbDirCap { dir, left: left.to_string(), top: top.to_string(), mode, layer }
            }
            // Bind mode: a single toggle button — it shows the CURRENT mode and
            // flips to the other on click. Other modes keep a static hub label.
            if mode == KeypadMode::Bind {
                button {
                    class: if four { "thumb-hub toggle four" } else { "thumb-hub toggle" },
                    title: "Toggle 4-way / 8-way thumb stick",
                    onclick: move |_| {
                        let next = if four { ThumbAxisMode::Eight } else { ThumbAxisMode::Four };
                        set_axis_mode(&mut store, next);
                    },
                    "{hub_label}"
                }
            } else {
                div { class: "thumb-hub", "{hub_label}" }
            }
        }
    }
}

/// Flip the active profile's thumb axis mode: optimistic local write (clearing a
/// now-hidden diagonal selection) plus a persist to the daemon.
fn set_axis_mode(store: &mut Signal<crate::model::AppState>, mode: ThumbAxisMode) {
    use dioxus::prelude::WritableExt;
    // Name the profile before the write to keep the borrow simple.
    let profile = store.read().active_profile_name();
    store.with_mut(|s| {
        s.profile.thumb_axis_mode = mode;
        // Dropping to Four hides the diagonals; clear a selection that just
        // vanished so the inspector doesn't dangle on an invisible cap.
        if mode == ThumbAxisMode::Four
            && let Some(KeyId::Thumb(d)) = s.selected
            && d.is_diagonal()
        {
            s.selected = None;
        }
    });
    crate::ipc::set_thumb_axis_mode(*store, profile, mode);
}

#[component]
fn ThumbDirCap(dir: ThumbDir, left: String, top: String, mode: KeypadMode, layer: Layer) -> Element {
    let mut store = use_store();
    let key = KeyId::Thumb(dir);
    let s = store.read();
    let selected = is_selected(&s, key, mode, layer);
    let style = if mode == KeypadMode::Lighting {
        format!("left:{left}%;top:{top}%;{}", lit_face_style(&lit_color(&s, key)))
    } else {
        format!("left:{left}%;top:{top}%;")
    };
    // In Bind mode show the cap's current function: the arrow a cardinal defaults
    // to (↑→↓←), or a bound value; fall back to the compass label (diagonals,
    // other modes). Dim it when the bind is Disable, like the grid caps.
    let mut disabled = false;
    let content = if mode == KeypadMode::Bind {
        let bind = s.bind_for(key, layer);
        disabled = matches!(bind, Some(BindAction::Disable));
        let f = cap_function(key, bind.as_ref());
        if f.is_empty() { dir.label().to_string() } else { f }
    } else {
        dir.label().to_string()
    };
    drop(s);
    let mut classes = String::from("thumb-dir");
    if selected {
        classes.push_str(" selected");
    }
    if disabled {
        classes.push_str(" disabled");
    }
    rsx! {
        div {
            class: "{classes}",
            style: "{style}",
            onclick: move |e| {
                e.stop_propagation();
                select(&mut store, key, mode, layer);
            },
            "{content}"
        }
    }
}

#[component]
fn Wheel(mode: KeypadMode, layer: Layer) -> Element {
    let mut store = use_store();
    let s = store.read();
    let up_sel = is_selected(&s, KeyId::Wheel(WheelDir::Up), mode, layer);
    let down_sel = is_selected(&s, KeyId::Wheel(WheelDir::Down), mode, layer);
    let wheel_style = if mode == KeypadMode::Lighting {
        format!("background:{};", lit_color(&s, KeyId::Wheel(WheelDir::Up)))
    } else {
        String::new()
    };
    let seg_style = if mode == KeypadMode::Lighting {
        "color:#0a0b0c;text-shadow:0 1px 1px rgba(255,255,255,0.25);".to_string()
    } else {
        String::new()
    };
    drop(s);
    rsx! {
        div { class: "wheel", style: "{wheel_style}",
            div {
                class: if up_sel { "w-seg selected" } else { "w-seg" },
                style: "{seg_style}",
                onclick: move |e| {
                    e.stop_propagation();
                    select(&mut store, KeyId::Wheel(WheelDir::Up), mode, layer);
                },
                "▲"
            }
            div {
                class: if down_sel { "w-seg selected" } else { "w-seg" },
                style: "{seg_style}",
                onclick: move |e| {
                    e.stop_propagation();
                    select(&mut store, KeyId::Wheel(WheelDir::Down), mode, layer);
                },
                "▼"
            }
        }
    }
}

#[component]
fn Space(
    mode: KeypadMode,
    layer: Layer,
    press: Option<u8>,
    modal: Element,
    #[props(default = None)] on_cap_pointer: Option<EventHandler<CapPointer>>,
    #[props(default = false)] multi: bool,
) -> Element {
    let mut store = use_store();
    let s = store.read();
    let selected = is_selected(&s, KeyId::Space, mode, layer);
    let mut classes = String::from("spacebar");
    if selected {
        classes.push_str(" selected");
    }
    if multi {
        classes.push_str(" multi");
    }
    // Bind mode: reflect the current function beneath the legend, like the grid
    // caps (the spacebar is bindable + analog, so it must not read as inert).
    let cap_func = if mode == KeypadMode::Bind {
        let bind = s.bind_for(KeyId::Space, layer);
        if matches!(bind, Some(BindAction::Disable)) {
            classes.push_str(" disabled");
        }
        cap_function(KeyId::Space, bind.as_ref())
    } else {
        String::new()
    };
    let mut style = if mode == KeypadMode::Lighting {
        lit_face_style(&lit_color(&s, KeyId::Space))
    } else {
        String::new()
    };
    drop(s);
    push_pressure_fill(press, &mut classes, &mut style);
    rsx! {
        div {
            class: "{classes}",
            style: "{style}",
            onclick: move |e| {
                e.stop_propagation();
                if on_cap_pointer.is_none() {
                    select(&mut store, KeyId::Space, mode, layer);
                }
            },
            onmousedown: move |e| cap_pointer(&on_cap_pointer, KeyId::Space, CapPhase::Down, &e),
            onmouseenter: move |e| cap_pointer(&on_cap_pointer, KeyId::Space, CapPhase::Enter, &e),
            onmouseup: move |e| cap_pointer(&on_cap_pointer, KeyId::Space, CapPhase::Up, &e),
            if mode == KeypadMode::Bind {
                span { class: "cap-legend", "SPACE" }
                if !cap_func.is_empty() {
                    span { class: "cap-func", "{cap_func}" }
                }
            } else {
                "THUMB · SPACE"
            }
            {modal}
        }
    }
}

/// The analog thumb button (emits `KEY_LEFTALT`); pressure-capable like the grid.
#[component]
fn ThumbButton(mode: KeypadMode, layer: Layer, press: Option<u8>, modal: Element) -> Element {
    let mut store = use_store();
    let s = store.read();
    let selected = is_selected(&s, KeyId::ThumbButton, mode, layer);
    let mut classes = String::from("thumb-button");
    if selected {
        classes.push_str(" selected");
    }
    // Bind mode: show the bound function in place of the "ALT" legend (the thumb
    // button is bindable), and dim it when disabled — like the grid caps.
    let label = if mode == KeypadMode::Bind {
        let bind = s.bind_for(KeyId::ThumbButton, layer);
        if matches!(bind, Some(BindAction::Disable)) {
            classes.push_str(" disabled");
        }
        let f = cap_function(KeyId::ThumbButton, bind.as_ref());
        if f.is_empty() { "ALT".to_string() } else { f }
    } else {
        "ALT".to_string()
    };
    let mut style = if mode == KeypadMode::Lighting {
        lit_face_style(&lit_color(&s, KeyId::ThumbButton))
    } else {
        String::new()
    };
    drop(s);
    push_pressure_fill(press, &mut classes, &mut style);
    rsx! {
        div {
            class: "{classes}",
            style: "{style}",
            onclick: move |e| {
                e.stop_propagation();
                select(&mut store, KeyId::ThumbButton, mode, layer);
            },
            span { class: "tb-label", "{label}" }
            {modal}
        }
    }
}

/// The lit-face inline style for Lighting mode: the cap face takes the key's
/// colour, with a dark, subtly-lifted legend that stays legible on any hue.
fn lit_face_style(c: &str) -> String {
    format!("background:{c};color:#0a0b0c;text-shadow:0 1px 1px rgba(255,255,255,0.25);border-color:{c};")
}

/// Live pressure: fill the cap's cloned `.pressed::after` overlay from the
/// bottom. `--press` is the fill height as a percentage of 0..255.
fn push_pressure_fill(press: Option<u8>, classes: &mut String, style: &mut String) {
    if let Some(v) = press {
        classes.push_str(" pressed");
        style.push_str(&format!("--press:{:.1}%;", v as f32 / 255.0 * 100.0));
    }
}

/// Resolve the colour a key shows in Lighting mode from the active profile's
/// [`LightingState`]. The IPC `Effect` carries its colour(s) inside the variant
/// (no separate top-level colour), and brightness is 0–255.
fn lit_color(s: &crate::model::AppState, key: KeyId) -> String {
    use crate::model::{Effect, Rgb};
    let l = &s.profile.lighting;
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
        Effect::Breathing { colors } | Effect::Starlight { colors, .. } => {
            solid(colors.first().copied().unwrap_or(Rgb::new(0x44, 0xD6, 0x2C)))
        }
        Effect::Custom { frame } => {
            // Frame is a flat 0..21 column row; map grid keys onto it.
            let idx = match key {
                KeyId::Grid(g) => g as usize,
                _ => return "#101418".into(),
            };
            frame.get(idx).copied().map_or_else(|| "#101418".into(), solid)
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

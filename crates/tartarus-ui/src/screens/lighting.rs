//! Lighting — choose an effect, then dial in its per-effect controls.
//!
//! Each firmware effect has its own parameter shape (1–2 colours, a speed, a
//! wave direction, or a per-key frame). The inspector shows a control panel
//! that morphs to match the selected effect. In Custom mode the keypad itself
//! is the per-key colour editor: click a key, then pick its colour.

use dioxus::prelude::*;

use crate::components::{Keypad, KeypadMode, Slider};
use crate::model::{Effect, KeyId, Rgb, WaveDirection};
use crate::use_store;

/// A curated palette: Razer green, warm/cool primaries, neutrals.
const PRESETS: &[(u8, u8, u8)] = &[
    (0x44, 0xD6, 0x2C), // Razer green
    (0xFF, 0xFF, 0xFF), // white
    (0x00, 0xD4, 0xFF), // cyan
    (0x00, 0x80, 0xFF), // sky blue
    (0x00, 0x40, 0xFF), // deep blue
    (0x80, 0x00, 0xFF), // violet
    (0xD6, 0x00, 0xFF), // magenta
    (0xFF, 0x00, 0x80), // hot pink
    (0xFF, 0x22, 0x22), // red
    (0xFF, 0x88, 0x00), // orange
    (0xF5, 0xB9, 0x42), // amber
    (0x00, 0xFF, 0xA0), // mint
];

/// Default first/second colours for multi-colour effects.
const COLOR1: Rgb = Rgb::new(0x44, 0xD6, 0x2C); // Razer green
const COLOR2: Rgb = Rgb::new(0x00, 0xD4, 0xFF); // cyan

/// Fixed effect list — same display order every time.
const EFFECTS: [&str; 8] = [
    "None", "Static", "Breathing", "Spectrum", "Wave", "Reactive", "Starlight", "Custom",
];

/// Discriminant name for an effect — used for active-chip comparison.
fn effect_name(e: &Effect) -> &'static str {
    match e {
        Effect::None => "None",
        Effect::Static { .. } => "Static",
        Effect::Breathing { .. } => "Breathing",
        Effect::Spectrum => "Spectrum",
        Effect::Wave { .. } => "Wave",
        Effect::Reactive { .. } => "Reactive",
        Effect::Starlight { .. } => "Starlight",
        Effect::Custom { .. } => "Custom",
    }
}

/// Sensible default variant for each effect name.
fn default_effect(name: &str) -> Effect {
    match name {
        "None" => Effect::None,
        "Static" => Effect::Static { color: COLOR1 },
        "Breathing" => Effect::Breathing { colors: vec![COLOR1] },
        "Spectrum" => Effect::Spectrum,
        "Wave" => Effect::Wave { direction: WaveDirection::Right },
        "Reactive" => Effect::Reactive { speed: 2, color: COLOR1 },
        "Starlight" => Effect::Starlight { speed: 2, colors: vec![COLOR1] },
        "Custom" => Effect::Custom { frame: vec![COLOR1; 21] },
        _ => Effect::None,
    }
}

/// Write a new effect into the store and push it to the daemon. The store is
/// updated first so the live keypad preview re-renders immediately.
fn apply(mut store: Signal<crate::model::AppState>, effect: Effect) {
    let lighting = {
        let mut w = store.write();
        w.profile.lighting.effect = effect;
        w.profile.lighting.clone()
    };
    crate::ipc::set_lighting(store, lighting);
}

#[component]
pub fn Lighting() -> Element {
    let store = use_store();
    let s = store.read();
    let effect = s.profile.lighting.effect.clone();
    let brightness = s.profile.lighting.brightness;
    drop(s);

    let current_name = effect_name(&effect);

    rsx! {
        div { class: "page-head",
            h1 { class: "page-title", "Lighting" }
            p { class: "page-sub", "Pick an effect, dial in your colour, and set how bright the keys glow." }
        }

        div { class: "work",
            // Left: live keypad preview
            div { class: "stage",
                Keypad { mode: KeypadMode::Lighting }
            }

            // Right: inspector cards
            div { class: "inspector",

                // ── Effect picker ──────────────────────────────────────────
                div { class: "card",
                    p { class: "card-title", "Effect" }
                    div { class: "field",
                        div { class: "select-grid",
                            for name in EFFECTS {
                                {
                                    let active = name == current_name;
                                    rsx! {
                                        div {
                                            class: if active { "chip active" } else { "chip" },
                                            onclick: move |_| apply(store, default_effect(name)),
                                            "{name}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Per-effect controls ────────────────────────────────────
                EffectControls { store, effect: effect.clone() }

                // ── Brightness slider ──────────────────────────────────────
                div { class: "card",
                    p { class: "card-title", "Brightness" }
                    Slider {
                        label: "Brightness".to_string(),
                        value: brightness as f64,
                        min: 0.0,
                        max: 255.0,
                        step: 1.0,
                        readout: format!("{}%", (brightness as f32 / 255.0 * 100.0).round() as u32),
                        on_input: move |v: f64| {
                            let mut store = store;
                            store.write().profile.lighting.brightness = v as u8;
                            crate::ipc::set_brightness(store, v as u8);
                        },
                    }
                }
            }
        }
    }
}

/// The control panel that morphs to the selected effect.
#[component]
fn EffectControls(store: Signal<crate::model::AppState>, effect: Effect) -> Element {
    match effect {
        Effect::None | Effect::Spectrum => rsx! {},

        Effect::Static { color } => rsx! {
            div { class: "card",
                p { class: "card-title", "Colour" }
                div { class: "field",
                    ColorPicker {
                        value: color,
                        on_pick: move |c| apply(store, Effect::Static { color: c }),
                    }
                }
            }
        },

        Effect::Breathing { colors } => rsx! {
            ColorModeCard {
                store,
                colors,
                make: Callback::new(|cs: Vec<Rgb>| Effect::Breathing { colors: cs }),
            }
        },

        Effect::Wave { direction } => rsx! {
            div { class: "card",
                p { class: "card-title", "Direction" }
                div { class: "field",
                    div { class: "seg",
                        button {
                            class: if direction == WaveDirection::Left { "active" } else { "" },
                            onclick: move |_| apply(store, Effect::Wave { direction: WaveDirection::Left }),
                            "Left"
                        }
                        button {
                            class: if direction == WaveDirection::Right { "active" } else { "" },
                            onclick: move |_| apply(store, Effect::Wave { direction: WaveDirection::Right }),
                            "Right"
                        }
                    }
                }
            }
        },

        Effect::Reactive { speed, color } => rsx! {
            div { class: "card",
                p { class: "card-title", "Colour" }
                div { class: "field",
                    ColorPicker {
                        value: color,
                        on_pick: move |c| apply(store, Effect::Reactive { speed, color: c }),
                    }
                }
            }
            div { class: "card",
                p { class: "card-title", "Duration" }
                div { class: "field",
                    SpeedSeg {
                        speed,
                        labels: ["Short", "Medium", "Long"],
                        on_pick: move |sp| apply(store, Effect::Reactive { speed: sp, color }),
                    }
                }
            }
        },

        Effect::Starlight { speed, colors } => rsx! {
            ColorModeCard {
                store,
                colors: colors.clone(),
                make: Callback::new(move |cs: Vec<Rgb>| Effect::Starlight { speed, colors: cs }),
            }
            div { class: "card",
                p { class: "card-title", "Speed" }
                div { class: "field",
                    SpeedSeg {
                        speed,
                        labels: ["Fast", "Medium", "Slow"],
                        on_pick: move |sp| apply(store, Effect::Starlight { speed: sp, colors: colors.clone() }),
                    }
                }
            }
        },

        Effect::Custom { frame } => rsx! { CustomPaint { store, frame } },
    }
}

/// A [Random | 1 Colour | 2 Colours] card: a segmented count toggle plus 0/1/2
/// colour pickers, each editing its own slot. `make` reconstructs the effect
/// from the colour list (so Breathing and Starlight can share this).
#[component]
fn ColorModeCard(
    store: Signal<crate::model::AppState>,
    colors: Vec<Rgb>,
    make: Callback<Vec<Rgb>, Effect>,
) -> Element {
    let count = colors.len().min(2);

    // Toggling the count preserves existing colours and supplies a default for
    // a newly-added second slot.
    let set_count = {
        let colors = colors.clone();
        Callback::new(move |n: usize| {
            let mut next: Vec<Rgb> = colors.clone();
            next.truncate(n);
            while next.len() < n {
                let fill = if next.is_empty() { COLOR1 } else { COLOR2 };
                next.push(fill);
            }
            apply(store, make.call(next));
        })
    };

    rsx! {
        div { class: "card",
            p { class: "card-title", "Colours" }
            div { class: "field",
                div { class: "seg",
                    button {
                        class: if count == 0 { "active" } else { "" },
                        onclick: move |_| set_count.call(0),
                        "Random"
                    }
                    button {
                        class: if count == 1 { "active" } else { "" },
                        onclick: move |_| set_count.call(1),
                        "1 Colour"
                    }
                    button {
                        class: if count == 2 { "active" } else { "" },
                        onclick: move |_| set_count.call(2),
                        "2 Colours"
                    }
                }
            }

            for i in 0..count {
                div { class: "field",
                    ColorPicker {
                        value: colors[i],
                        on_pick: {
                            let colors = colors.clone();
                            move |c: Rgb| {
                                let mut next = colors.clone();
                                next[i] = c;
                                apply(store, make.call(next));
                            }
                        },
                    }
                }
            }
        }
    }
}

/// A 1/2/3 speed segmented control. `labels[0..3]` map to speed 1/2/3. An
/// out-of-range stored speed (e.g. a stale 128) displays as Medium without
/// mutating anything.
#[component]
fn SpeedSeg(speed: u8, labels: [&'static str; 3], on_pick: EventHandler<u8>) -> Element {
    let active = if (1..=3).contains(&speed) { speed } else { 2 };
    rsx! {
        div { class: "seg",
            for (i, label) in labels.iter().enumerate() {
                {
                    let sp = (i + 1) as u8;
                    rsx! {
                        button {
                            class: if active == sp { "active" } else { "" },
                            onclick: move |_| on_pick.call(sp),
                            "{label}"
                        }
                    }
                }
            }
        }
    }
}

/// Per-key paint surface for Custom mode. Click a grid key on the preview to
/// select it, then pick its colour here; the chosen colour writes into the
/// matching frame slot and the keypad re-renders live.
#[component]
fn CustomPaint(store: Signal<crate::model::AppState>, frame: Vec<Rgb>) -> Element {
    let selected = store.read().selected;
    let grid_idx = match selected {
        Some(KeyId::Grid(g)) => Some(g as usize),
        _ => None,
    };

    rsx! {
        div { class: "card",
            p { class: "card-title", "Per-key colours" }
            if let Some(idx) = grid_idx {
                {
                    let current = frame.get(idx).copied().unwrap_or(COLOR1);
                    rsx! {
                        div { class: "field",
                            ColorPicker {
                                value: current,
                                on_pick: {
                                    let frame = frame.clone();
                                    move |c: Rgb| {
                                        let mut next = frame.clone();
                                        if next.len() < 21 {
                                            next.resize(21, COLOR1);
                                        }
                                        if let Some(slot) = next.get_mut(idx) {
                                            *slot = c;
                                        }
                                        apply(store, Effect::Custom { frame: next });
                                    }
                                },
                            }
                        }
                    }
                }
            } else {
                div { class: "field",
                    div { class: "field-value",
                        "Click a key on the preview to paint its colour."
                    }
                }
            }
        }
    }
}

/// A reusable colour picker: the preset swatch grid plus a live hex readout.
#[component]
fn ColorPicker(value: Rgb, on_pick: EventHandler<Rgb>) -> Element {
    let color_css = format!("rgb({},{},{})", value.r, value.g, value.b);
    let color_hex = format!("#{:02X}{:02X}{:02X}", value.r, value.g, value.b);

    rsx! {
        div { class: "swatch-grid",
            for &(r, g, b) in PRESETS {
                {
                    let preset = Rgb::new(r, g, b);
                    let active = preset == value;
                    let preset_css = format!("rgb({},{},{})", r, g, b);
                    rsx! {
                        div {
                            class: if active { "swatch active" } else { "swatch" },
                            style: "background:{preset_css}",
                            onclick: move |_| on_pick.call(preset),
                        }
                    }
                }
            }
        }

        div { class: "color-readout",
            div {
                class: "color-preview",
                style: "background:{color_css}",
            }
            span { class: "color-hex", "{color_hex}" }
        }
    }
}

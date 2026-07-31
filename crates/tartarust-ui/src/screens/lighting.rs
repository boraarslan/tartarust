//! Lighting — choose an effect, then dial in its per-effect controls.
//!
//! Each firmware effect has its own parameter shape (1–2 colours, a speed, a
//! wave direction, or a per-key frame). The inspector shows a control panel
//! that morphs to match the selected effect. In Custom mode the keypad itself
//! is the per-key colour editor: click a key, then pick its colour.
//!
//! The view holds no state of its own except the brightness slider's
//! [`SliderState`] (an interactive widget must own one). Every control reads the
//! global store and writes back through [`store::fire`], so the live keypad
//! preview repaints from the same value the daemon just received — the old
//! screen's "no local signals" shape, carried over intact.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, ClickEvent, Context, Entity, Rgba, Window, div, rgb};
use gpui_component::slider::{SliderEvent, SliderState};

use crate::components::widgets::{card, labeled_slider, page_head, segmented};
use crate::components::{Keypad, KeypadMode};
use crate::model::{AppState, Effect, KeyId, Layer, Rgb, WaveDirection};
use crate::{ipc, store, theme};

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
const EFFECTS: &[&str] = &[
    "None",
    "Static",
    "Breathing",
    "Spectrum",
    "Wave",
    "Reactive",
    "Starlight",
    "Custom",
];

// Segmented-control labels. The ported `segmented` chassis does no
// text-transform (the stylesheet's `.seg button` did), so the labels carry
// their own case.
const WAVE_LABELS: &[&str] = &["LEFT", "RIGHT"];
const COUNT_LABELS: &[&str] = &["RANDOM", "1 COLOUR", "2 COLOURS"];
const REACTIVE_LABELS: &[&str] = &["SHORT", "MEDIUM", "LONG"];
const STARLIGHT_LABELS: &[&str] = &["FAST", "MEDIUM", "SLOW"];

/// A Custom frame is one row of 21 columns (the device's per-key LED map).
const CUSTOM_FRAME_LEN: usize = 21;

/// Inspector column width (CSS `clamp(290px, 27vw, 380px)` at the fixed 1280px
/// window, like the keypad's resolved geometry tokens).
const INSPECTOR_W: f32 = 346.0;

// Preset swatch grid: 6 across (CSS `repeat(6, 1fr)`), each roughly square at
// this column width.
const SWATCH_COLS: usize = 6;
const SWATCH_GAP: f32 = 7.0;
const SWATCH_H: f32 = 44.0;

/// Applies a colour picked from the preset grid.
type PickHandler = Rc<dyn Fn(Rgb, &mut App)>;
/// Rebuilds an effect from a colour list, so Breathing and Starlight can share
/// one colour-count card.
type EffectMaker = Rc<dyn Fn(Vec<Rgb>) -> Effect>;

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
        "Breathing" => Effect::Breathing {
            colors: vec![COLOR1],
        },
        "Spectrum" => Effect::Spectrum,
        "Wave" => Effect::Wave {
            direction: WaveDirection::Right,
        },
        "Reactive" => Effect::Reactive {
            speed: 2,
            color: COLOR1,
        },
        "Starlight" => Effect::Starlight {
            speed: 2,
            colors: vec![COLOR1],
        },
        "Custom" => Effect::Custom {
            frame: vec![COLOR1; CUSTOM_FRAME_LEN],
        },
        _ => Effect::None,
    }
}

/// Write a new effect into the store and push it to the daemon. The store is
/// updated first so the live keypad preview re-renders immediately.
fn apply(store: &Entity<AppState>, effect: Effect, cx: &mut App) {
    let lighting = store.update(cx, |s, cx| {
        s.profile.lighting.effect = effect;
        cx.notify();
        s.profile.lighting.clone()
    });
    store::fire(store, ipc::req_set_lighting(lighting), cx);
}

/// A domain colour as a GPUI paint colour.
fn paint(c: Rgb) -> Rgba {
    rgb(u32::from_be_bytes([0, c.r, c.g, c.b]))
}

pub struct Lighting {
    store: Entity<AppState>,
    /// The brightness slider's own state. The store stays authoritative: this
    /// follows it whenever brightness moves from anywhere else.
    brightness: Entity<SliderState>,
}

impl Lighting {
    pub fn new(store: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let level = store.read(cx).profile.lighting.brightness;
        let brightness = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(255.)
                .step(1.)
                .default_value(level as f32)
        });

        // Every drag sample writes the store and fires SetBrightness — the old
        // range input's `oninput` did exactly this, with no debounce, so the
        // device tracks the drag live.
        cx.subscribe(&brightness, |this: &mut Self, _, ev: &SliderEvent, cx| {
            let SliderEvent::Change(v) = ev;
            let level = v.start() as u8;
            if this.store.read(cx).profile.lighting.brightness == level {
                return;
            }
            this.store.update(cx, |s, cx| {
                s.profile.lighting.brightness = level;
                cx.notify();
            });
            store::fire(&this.store, ipc::req_set_brightness(level), cx);
        })
        .detach();

        // One observer covers the whole screen; it also pulls the slider back in
        // line when brightness moved without the slider (profile switch, resync).
        cx.observe_in(&store, window, |this, store, window, cx| {
            let level = store.read(cx).profile.lighting.brightness;
            this.sync_brightness(level, window, cx);
            cx.notify();
        })
        .detach();

        // First-paint fix: the slider caches its track bounds silently during
        // paint, so a never-painted slider draws its thumb at x=0 on the first
        // frame regardless of value (user review: brightness showed unseeded).
        crate::components::widgets::nudge_next_frame(cx);

        Self { store, brightness }
    }

    /// Point the slider at `level`, unless it is already there — `set_value`
    /// emits no event, so this cannot loop back into the subscription above.
    fn sync_brightness(&mut self, level: u8, window: &mut Window, cx: &mut Context<Self>) {
        if self.brightness.read(cx).value().start() as u8 == level {
            return;
        }
        self.brightness.update(cx, |state, cx| {
            state.set_value(level as f32, window, cx);
        });
    }

    /// The effect chooser: one chip per firmware effect, two to a row.
    fn effect_card(&self, current: &Effect, cx: &App) -> AnyElement {
        let active = effect_name(current);
        let rows = EFFECTS
            .chunks(2)
            .enumerate()
            .map(|(r, pair)| {
                div()
                    .flex()
                    .gap(theme::sp(6.))
                    .children(pair.iter().enumerate().map(|(c, &label)| {
                        let store = self.store.clone();
                        chip(r * 2 + c, label, label == active, move |_, _, cx| {
                            apply(&store, default_effect(label), cx);
                        })
                    }))
            })
            .collect::<Vec<_>>();

        card()
            .child(card_title("EFFECT", cx))
            .child(div().flex().flex_col().gap(theme::sp(6.)).children(rows))
            .into_any_element()
    }

    /// The control panel that morphs to the selected effect.
    fn effect_controls(
        &self,
        effect: &Effect,
        selected: Option<KeyId>,
        cx: &App,
    ) -> Vec<AnyElement> {
        match effect {
            Effect::None | Effect::Spectrum => Vec::new(),

            Effect::Static { color } => {
                let store = self.store.clone();
                let on_pick: PickHandler = Rc::new(move |c: Rgb, cx: &mut App| {
                    apply(&store, Effect::Static { color: c }, cx);
                });
                vec![
                    card()
                        .child(card_title("COLOUR", cx))
                        .child(color_picker(0, *color, on_pick, cx))
                        .into_any_element(),
                ]
            }

            Effect::Breathing { colors } => {
                let make: EffectMaker = Rc::new(|cs: Vec<Rgb>| Effect::Breathing { colors: cs });
                vec![self.color_mode_card(colors, make, cx)]
            }

            Effect::Wave { direction } => {
                let store = self.store.clone();
                let active = usize::from(*direction == WaveDirection::Right);
                vec![
                    card()
                        .child(card_title("DIRECTION", cx))
                        .child(segmented(
                            "lt-wave",
                            WAVE_LABELS,
                            active,
                            move |i, _, cx| {
                                let direction = if *i == 0 {
                                    WaveDirection::Left
                                } else {
                                    WaveDirection::Right
                                };
                                apply(&store, Effect::Wave { direction }, cx);
                            },
                        ))
                        .into_any_element(),
                ]
            }

            Effect::Reactive { speed, color } => {
                let (speed, color) = (*speed, *color);
                let color_store = self.store.clone();
                let speed_store = self.store.clone();
                let on_pick: PickHandler = Rc::new(move |c: Rgb, cx: &mut App| {
                    apply(&color_store, Effect::Reactive { speed, color: c }, cx);
                });
                vec![
                    card()
                        .child(card_title("COLOUR", cx))
                        .child(color_picker(0, color, on_pick, cx))
                        .into_any_element(),
                    card()
                        .child(card_title("DURATION", cx))
                        .child(speed_seg(
                            "lt-speed",
                            REACTIVE_LABELS,
                            speed,
                            move |sp, cx| {
                                apply(&speed_store, Effect::Reactive { speed: sp, color }, cx);
                            },
                        ))
                        .into_any_element(),
                ]
            }

            Effect::Starlight { speed, colors } => {
                let speed = *speed;
                let store = self.store.clone();
                let speed_colors = colors.clone();
                let make: EffectMaker =
                    Rc::new(move |cs: Vec<Rgb>| Effect::Starlight { speed, colors: cs });
                vec![
                    self.color_mode_card(colors, make, cx),
                    card()
                        .child(card_title("SPEED", cx))
                        .child(speed_seg(
                            "lt-speed",
                            STARLIGHT_LABELS,
                            speed,
                            move |sp, cx| {
                                apply(
                                    &store,
                                    Effect::Starlight {
                                        speed: sp,
                                        colors: speed_colors.clone(),
                                    },
                                    cx,
                                );
                            },
                        ))
                        .into_any_element(),
                ]
            }

            Effect::Custom { frame } => vec![self.custom_paint_card(frame, selected, cx)],
        }
    }

    /// A [Random | 1 Colour | 2 Colours] card: a segmented count toggle plus
    /// 0/1/2 colour pickers, each editing its own slot. `make` reconstructs the
    /// effect from the colour list (so Breathing and Starlight can share this).
    fn color_mode_card(&self, colors: &[Rgb], make: EffectMaker, cx: &App) -> AnyElement {
        let count = colors.len().min(2);

        // Toggling the count preserves existing colours and supplies a default
        // for a newly-added second slot.
        let count_store = self.store.clone();
        let count_make = make.clone();
        let base = colors.to_vec();
        let mut el = card().child(card_title("COLOURS", cx)).child(segmented(
            "lt-count",
            COUNT_LABELS,
            count,
            move |n, _, cx| {
                let mut next = base.clone();
                next.truncate(*n);
                while next.len() < *n {
                    let fill = if next.is_empty() { COLOR1 } else { COLOR2 };
                    next.push(fill);
                }
                apply(&count_store, count_make(next), cx);
            },
        ));

        for (i, &color) in colors.iter().enumerate().take(count) {
            let store = self.store.clone();
            let make = make.clone();
            let slot_base = colors.to_vec();
            let on_pick: PickHandler = Rc::new(move |c: Rgb, cx: &mut App| {
                let mut next = slot_base.clone();
                if let Some(slot) = next.get_mut(i) {
                    *slot = c;
                }
                apply(&store, make(next), cx);
            });
            el = el.child(color_picker(i, color, on_pick, cx));
        }
        el.into_any_element()
    }

    /// Per-key paint surface for Custom mode. Click a grid key on the preview to
    /// select it, then pick its colour here; the chosen colour writes into the
    /// matching frame slot and the keypad re-renders live.
    fn custom_paint_card(&self, frame: &[Rgb], selected: Option<KeyId>, cx: &App) -> AnyElement {
        let grid_idx = match selected {
            Some(KeyId::Grid(g)) => Some(g as usize),
            _ => None,
        };

        let body = match grid_idx {
            Some(idx) => {
                let current = frame.get(idx).copied().unwrap_or(COLOR1);
                let store = self.store.clone();
                let frame = frame.to_vec();
                let on_pick: PickHandler = Rc::new(move |c: Rgb, cx: &mut App| {
                    let mut next = frame.clone();
                    if next.len() < CUSTOM_FRAME_LEN {
                        next.resize(CUSTOM_FRAME_LEN, COLOR1);
                    }
                    if let Some(slot) = next.get_mut(idx) {
                        *slot = c;
                    }
                    apply(&store, Effect::Custom { frame: next }, cx);
                });
                color_picker(0, current, on_pick, cx).into_any_element()
            }
            None => div()
                .font_family(theme::mono(cx))
                .text_size(theme::sp(12.))
                .text_color(theme::dim())
                .child("Click a key on the preview to paint its colour.")
                .into_any_element(),
        };

        card()
            .child(card_title("PER-KEY COLOURS", cx))
            .child(body)
            .into_any_element()
    }

    /// The brightness card. The card title already names the control, so the
    /// slider row carries no field label.
    fn brightness_card(&self, brightness: u8, cx: &App) -> AnyElement {
        let readout = format!("{}%", (brightness as f32 / 255.0 * 100.0).round() as u32);
        card()
            .child(card_title("BRIGHTNESS", cx))
            .child(labeled_slider("", &self.brightness, readout, false, cx))
            .into_any_element()
    }
}

impl Render for Lighting {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (lighting, axis_mode, selected) = {
            let s = self.store.read(cx);
            (
                s.profile.lighting.clone(),
                s.profile.thumb_axis_mode,
                s.selected,
            )
        };
        let effect = lighting.effect.clone();
        let brightness = lighting.brightness;

        // Cap clicks TOGGLE, exactly as the old component's own handler did:
        // clicking the selected cap clears the Custom paint target.
        let store = self.store.clone();
        let keypad = Keypad::new(KeypadMode::Lighting, Layer::Base)
            .lighting(lighting)
            .thumb_axis_mode(axis_mode)
            .selected(selected)
            .on_select(Rc::new(
                move |key: KeyId, _layer: Layer, _window: &mut Window, cx: &mut App| {
                    store.update(cx, |s, cx| {
                        s.selected = if s.selected == Some(key) {
                            None
                        } else {
                            Some(key)
                        };
                        cx.notify();
                    });
                },
            ));

        // The stylesheet scrolled the whole content region (`.content`), not the
        // inspector alone — a two-colour Starlight inspector is taller than the
        // 800px window. The screen root carries that scroll.
        div()
            .id("lighting")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(theme::sp(22.))
            .child(page_head(
                "Pick an effect, dial in your colour, and set how bright the keys glow.",
            ))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(theme::sp(22.))
                    .child(stage(keypad))
                    .child(
                        div()
                            .flex_none()
                            .w(theme::sp(INSPECTOR_W))
                            .flex()
                            .flex_col()
                            .gap(theme::sp(16.))
                            .child(self.effect_card(&effect, cx))
                            .children(self.effect_controls(&effect, selected, cx))
                            .child(self.brightness_card(brightness, cx)),
                    ),
            )
    }
}

/// A card's heading: mono, small, and quiet. (The stylesheet also tracked the
/// letters out; GPUI 0.2 has no letter-spacing, so the case carries it.)
fn card_title(label: &'static str, cx: &App) -> impl IntoElement {
    div()
        .font_family(theme::mono(cx))
        .text_size(theme::sp(10.))
        .text_color(theme::mute())
        .child(label)
}

/// The bench mat the keypad sits on. The CSS drew a faint graph grid here with
/// repeating gradients, which GPUI has no equivalent for — the panel surface and
/// the machined corner ticks carry the stage on their own.
fn stage(content: impl IntoElement) -> impl IntoElement {
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .flex()
        .justify_center()
        .pt(theme::sp(28.))
        .px(theme::sp(26.))
        .pb(theme::sp(24.))
        .rounded(theme::sp(8.))
        .bg(theme::panel_0())
        .border_1()
        .border_color(theme::line_soft())
        .child(content)
        .child(corner_tick(true))
        .child(corner_tick(false))
}

/// One machined corner tick (CSS `.stage::before` / `.stage::after`).
fn corner_tick(top_left: bool) -> impl IntoElement {
    let tick = div()
        .absolute()
        .size(theme::sp(11.))
        .border_color(theme::line());
    if top_left {
        tick.top(theme::sp(9.))
            .left(theme::sp(9.))
            .border_t_1()
            .border_l_1()
    } else {
        tick.bottom(theme::sp(9.))
            .right(theme::sp(9.))
            .border_b_1()
            .border_r_1()
    }
}

/// One effect chip in the chooser grid.
fn chip(
    index: usize,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(("lt-effect", index))
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(theme::sp(10.))
        .py(theme::sp(8.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .border_1()
        .text_size(theme::sp(12.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(active, |el| {
            el.border_color(theme::rust())
                .bg(theme::rust_wash())
                .text_color(theme::ink())
        })
        .when(!active, |el| {
            el.border_color(theme::line())
                .bg(theme::panel_1())
                .text_color(theme::dim())
                .hover(|s| s.border_color(theme::mute()).text_color(theme::ink()))
        })
        .cursor_pointer()
        .on_click(on_click)
        .child(label)
}

/// A 1/2/3 speed segmented control. `labels[0..3]` map to speed 1/2/3. An
/// out-of-range stored speed (e.g. a stale 128) displays as Medium without
/// mutating anything.
fn speed_seg(
    id_prefix: &'static str,
    labels: &'static [&'static str],
    speed: u8,
    on_pick: impl Fn(u8, &mut App) + 'static,
) -> impl IntoElement {
    let active = if (1..=3).contains(&speed) {
        speed as usize - 1
    } else {
        1
    };
    segmented(id_prefix, labels, active, move |i, _, cx| {
        on_pick(*i as u8 + 1, cx);
    })
}

/// A reusable colour picker: the preset swatch grid plus a live hex readout.
/// `slot` namespaces the swatch element ids, so two pickers (a dual-colour
/// effect) can coexist in one frame.
fn color_picker(slot: usize, value: Rgb, on_pick: PickHandler, cx: &App) -> impl IntoElement {
    let rows = PRESETS
        .chunks(SWATCH_COLS)
        .enumerate()
        .map(|(r, row)| {
            div()
                .flex()
                .gap(theme::sp(SWATCH_GAP))
                .children(row.iter().enumerate().map(|(c, &(pr, pg, pb))| {
                    let preset = Rgb::new(pr, pg, pb);
                    let active = preset == value;
                    let on_pick = on_pick.clone();
                    div()
                        .id(("lt-swatch", slot * PRESETS.len() + r * SWATCH_COLS + c))
                        .flex_1()
                        .h(theme::sp(SWATCH_H))
                        .rounded(theme::sp(theme::RADIUS_SM))
                        .bg(paint(preset))
                        // The stylesheet ringed the active swatch with a
                        // two-tone box-shadow; GPUI 0.2 has no spread-only
                        // shadow, so a doubled rust border carries it.
                        .when(active, |el| el.border_2().border_color(theme::rust()))
                        .when(!active, |el| {
                            el.border_1()
                                .border_color(theme::line())
                                .hover(|s| s.border_color(theme::mute()))
                        })
                        .cursor_pointer()
                        .on_click(move |_, _, cx| on_pick(preset, cx))
                }))
        })
        .collect::<Vec<_>>();

    div()
        .flex()
        .flex_col()
        .gap(theme::sp(12.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(theme::sp(SWATCH_GAP))
                .children(rows),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme::sp(12.))
                .child(
                    div()
                        .size(theme::sp(42.))
                        .rounded(theme::sp(theme::RADIUS_SM))
                        .bg(paint(value))
                        .border_1()
                        .border_color(theme::line()),
                )
                .child(
                    div()
                        .font_family(theme::mono(cx))
                        .text_size(theme::sp(14.))
                        .text_color(theme::ink())
                        .child(format!("#{:02X}{:02X}{:02X}", value.r, value.g, value.b)),
                ),
        )
}

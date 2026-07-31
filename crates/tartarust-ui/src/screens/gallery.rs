//! Dev-only widget gallery, reachable via `TARTARUST_SCREEN=gallery`. Lets
//! the headless harness screenshot the whole Phase 2 kit in one frame; never
//! linked from the tab rail.

use gpui::prelude::*;
use gpui::{Context, Entity, Window, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::slider::{SliderEvent, SliderState};

use crate::components::widgets::{
    btn_danger, btn_ghost, btn_primary, card, labeled_slider, led, led_off, page_head, segmented,
    switch_row,
};
use crate::components::{Keypad, KeypadMode};
use crate::model::{self, KeyId, Layer};
use crate::theme;

/// Swatch colors for the color-picker chassis preview (the Lighting screen's
/// preset grid reuses this pattern in Phase 4).
const SWATCHES: [u32; 8] = [
    0xD96A2B, 0x7FBF3F, 0xD9A44A, 0xD0574C, 0x3F8FBF, 0x8F3FBF, 0xECE8DF, 0x131110,
];

pub struct Gallery {
    slider: Entity<SliderState>,
    amber_slider: Entity<SliderState>,
    slider_value: f32,
    input: Entity<InputState>,
    switch_on: bool,
    seg_active: usize,
}

impl Gallery {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(255.)
                .step(1.)
                .default_value(192.)
        });
        let amber_slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(255.)
                .step(1.)
                .default_value(64.)
        });
        cx.subscribe(&slider, |this: &mut Self, _, ev: &SliderEvent, cx| {
            let SliderEvent::Change(v) = ev;
            this.slider_value = v.start();
            cx.notify();
        })
        .detach();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type here — Input"));
        Self {
            slider,
            amber_slider,
            slider_value: 192.,
            input,
            switch_on: true,
            seg_active: 1,
        }
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let readout = format!(
            "{} ({}%)",
            self.slider_value as u32,
            (self.slider_value / 255. * 100.) as u32
        );
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(22.))
            .size_full()
            .child(page_head("Phase 2 widget kit — dev-only gallery."))
            .child(
                div()
                    .flex()
                    .gap(px(14.))
                    .child(
                        card()
                            .flex_1()
                            .child(labeled_slider(
                                "Actuation point",
                                &self.slider,
                                readout,
                                false,
                                cx,
                            ))
                            .child(labeled_slider(
                                "Second actuation",
                                &self.amber_slider,
                                "64 (25%)".into(),
                                true,
                                cx,
                            ))
                            .child(switch_row(
                                "gal-switch",
                                "Auto-switch profiles",
                                self.switch_on,
                                cx.listener(|this, on: &bool, _, cx| {
                                    this.switch_on = *on;
                                    cx.notify();
                                }),
                            ))
                            .child(segmented(
                                "gal-seg",
                                &["SLOW", "MED", "FAST"],
                                self.seg_active,
                                cx.listener(|this, i: &usize, _, cx| {
                                    this.seg_active = *i;
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(
                        card()
                            .flex_1()
                            .child(div().w_full().child(Input::new(&self.input)))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .child(btn_primary("gal-save", "Save", |_, _, _| {}))
                                    .child(btn_ghost("gal-cancel", "Cancel", |_, _, _| {}))
                                    .child(btn_danger("gal-del", "Delete", |_, _, _| {})),
                            )
                            .child(div().flex().gap(px(6.)).children(
                                SWATCHES.iter().enumerate().map(|(i, c)| {
                                    div()
                                        .id(("gal-swatch", i))
                                        .size(px(22.))
                                        .rounded(px(theme::RADIUS_SM))
                                        .bg(gpui::rgb(*c))
                                        .border_1()
                                        .border_color(theme::line())
                                        .cursor_pointer()
                                }),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(led(theme::live()))
                                    .child(led(theme::amber()))
                                    .child(led(theme::red()))
                                    .child(led_off())
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme::dim())
                                            .child("annunciators"),
                                    ),
                            ),
                    ),
            )
            // Phase 3 keypad, fed the offline mock profile: Bind mode with a
            // selected cap so the screenshot exercises labels + selection.
            .child({
                let mock = model::mock_state();
                card().items_center().child(
                    Keypad::new(KeypadMode::Bind, Layer::Base)
                        .binds(mock.profile.base.clone())
                        .thumb_axis_mode(mock.profile.thumb_axis_mode)
                        .selected(Some(KeyId::Grid(7)))
                        .customized(["KEY_W".to_string()].into()),
                )
            })
    }
}

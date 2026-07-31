//! Small shared widgets: the page header, form-control rows, buttons, LEDs,
//! and the segmented control. Interactive primitives with real state (slider,
//! switch, text input) come from gpui-component, themed by
//! [`crate::theme::apply_component_theme`]; the rows here only lay them out in
//! the bench-instrument voice.

use gpui::prelude::*;
use gpui::{App, ClickEvent, Div, Entity, Rgba, SharedString, Window, div, px};
use gpui_component::slider::{Slider, SliderState};

use crate::theme;

/// The screen's context line. The active tab already names the screen, so the
/// head deliberately carries no title — just one quiet instructional sentence.
pub fn page_head(sub: &'static str) -> impl IntoElement {
    div()
        .pb(px(14.))
        .text_size(px(12.))
        .text_color(theme::dim())
        .child(sub)
}

/// A status LED dot. `on` colors are literal annunciators (live/amber/red);
/// [`led_off`] is the dead state.
pub fn led(color: Rgba) -> Div {
    div().size(px(8.)).rounded_full().bg(color)
}

pub fn led_off() -> Div {
    div()
        .size(px(8.))
        .rounded_full()
        .bg(theme::panel_2())
        .border_1()
        .border_color(theme::line())
}

/// A labelled slider row with a monospace value readout. An empty `label`
/// skips the field label (for sliders whose card title already names them).
/// The amber variant marks drift-colored controls (secondary actuation).
pub fn labeled_slider(
    label: &'static str,
    state: &Entity<SliderState>,
    readout: String,
    amber: bool,
    cx: &App,
) -> impl IntoElement {
    let slider = if amber {
        // Per-element override: the shared theme colors the default rust.
        div().child(Slider::new(state)).text_color(theme::amber())
    } else {
        div().child(Slider::new(state))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .when(!label.is_empty(), |el| {
            el.child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::dim())
                    .child(label),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_1().child(slider))
                .child(
                    div()
                        .font_family(theme::mono(cx))
                        .text_size(px(11.))
                        .text_color(if amber { theme::amber() } else { theme::ink() })
                        .child(readout),
                ),
        )
}

/// A row: label on the left, a gpui-component `Switch` on the right.
pub fn switch_row(
    id: &'static str,
    label: impl Into<SharedString>,
    on: bool,
    on_toggle: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(div().text_size(px(12.)).child(label.into()))
        .child(
            gpui_component::switch::Switch::new(id)
                .checked(on)
                .on_click(on_toggle),
        )
}

/// A 3-to-4-way segmented control (the old `SpeedSeg` chassis, generalized).
/// The callback takes `&usize` so a `cx.listener(..)` slots in directly.
pub fn segmented(
    id_prefix: &'static str,
    options: &'static [&'static str],
    active: usize,
    on_select: impl Fn(&usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_select = std::rc::Rc::new(on_select);
    div()
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(theme::RADIUS_SM))
        .bg(theme::panel_0())
        .border_1()
        .border_color(theme::line_soft())
        .children(options.iter().enumerate().map(|(i, label)| {
            let on_select = on_select.clone();
            let is_active = i == active;
            div()
                .id((id_prefix, i))
                .px(px(10.))
                .py(px(4.))
                .rounded(px(theme::RADIUS_SM - 1.0))
                .text_size(px(11.))
                .when(is_active, |el| {
                    el.bg(theme::panel_2()).text_color(theme::ink())
                })
                .when(!is_active, |el| {
                    el.text_color(theme::dim())
                        .hover(|s| s.text_color(theme::ink()))
                })
                .cursor_pointer()
                .on_click(move |_: &ClickEvent, window, cx| on_select(&i, window, cx))
                .child(*label)
        }))
}

/// Button variants. All press affordances share this chassis; color carries
/// the meaning (rust = primary action, red = destructive, quiet = ghost).
fn btn_base(id: (&'static str, u32), label: SharedString) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .px(px(12.))
        .py(px(5.))
        .rounded(px(theme::RADIUS_SM))
        .text_size(px(12.))
        .cursor_pointer()
        .child(label)
}

pub fn btn_primary(
    id: &'static str,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    btn_base((id, 0), label.into())
        .bg(theme::rust())
        .text_color(theme::chassis())
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .hover(|s| s.bg(theme::rust_hi()))
        .on_click(on_click)
}

pub fn btn_ghost(
    id: &'static str,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    btn_base((id, 1), label.into())
        .border_1()
        .border_color(theme::line())
        .text_color(theme::dim())
        .hover(|s| s.text_color(theme::ink()).border_color(theme::ink()))
        .on_click(on_click)
}

pub fn btn_danger(
    id: &'static str,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    btn_base((id, 2), label.into())
        .border_1()
        .border_color(theme::red())
        .text_color(theme::red())
        .hover(|s| s.bg(theme::red_wash()))
        .on_click(on_click)
}

/// An inspector card: panel surface + machined border, the right-pane chassis
/// every screen's editor panels share.
pub fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(14.))
        .rounded(px(theme::RADIUS))
        .bg(theme::panel_1())
        .border_1()
        .border_color(theme::line_soft())
}

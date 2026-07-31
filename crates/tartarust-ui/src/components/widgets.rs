//! Small shared widgets: the page header, form-control rows, buttons, LEDs,
//! and the segmented control. Interactive primitives with real state (slider,
//! switch, text input) come from gpui-component, themed by
//! [`crate::theme::apply_component_theme`]; the rows here only lay them out in
//! the bench-instrument voice.

use gpui::prelude::*;
use gpui::{App, ClickEvent, Div, Entity, Rgba, SharedString, Window, div};
use gpui_component::Sizable;
use gpui_component::input::{Input, InputState};
use gpui_component::slider::{Slider, SliderState};

use crate::theme;

/// The screen's context line. The active tab already names the screen, so the
/// head deliberately carries no title — just one quiet instructional sentence.
pub fn page_head(sub: &'static str) -> impl IntoElement {
    div()
        .pb(theme::sp(14.))
        .text_size(theme::sp(12.5))
        .text_color(theme::dim())
        .child(sub)
}

/// One-frame render nudge after creating or re-seeding sliders. The
/// gpui-component slider caches its track bounds during paint SILENTLY (no
/// notify), so a slider that has never painted draws its thumb at x=0 on its
/// first frame; a follow-up notify lets the second frame position the thumb
/// with the freshly cached bounds. Harmless when bounds were already cached.
pub fn nudge_next_frame<V: 'static>(cx: &mut gpui::Context<V>) {
    cx.spawn(async move |this, cx| {
        let _ = this.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

/// A status LED dot. `on` colors are literal annunciators (live/amber/red);
/// [`led_off`] is the dead state.
pub fn led(color: Rgba) -> Div {
    div().size(theme::sp(8.)).rounded_full().bg(color)
}

pub fn led_off() -> Div {
    div()
        .size(theme::sp(8.))
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
        .gap(theme::sp(6.))
        .when(!label.is_empty(), |el| {
            el.child(
                div()
                    .text_size(theme::sp(11.))
                    .text_color(theme::dim())
                    .child(label),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme::sp(10.))
                .child(div().flex_1().child(slider))
                .child(
                    div()
                        .font_family(theme::mono(cx))
                        .text_size(theme::sp(11.))
                        .text_color(if amber { theme::amber() } else { theme::ink() })
                        .child(readout),
                ),
        )
}

/// The editable readout's width in [`labeled_slider_input`] — wide enough for
/// a signed 2-decimal figure plus the caret, narrow enough that the slider keeps
/// the row.
const FIELD_W: f32 = 64.0;

/// A slider row whose readout is EDITABLE: the [`labeled_slider`] chassis, with
/// a narrow text field carrying the value and a quiet unit suffix after it. The
/// caller owns the `InputState` and decides when a typed value commits (Enter,
/// blur) — this only lays the row out.
///
/// `hint` rides the right end of the label row and states what the field will
/// accept. The caller owns the clamps, so it owns the wording too — and a hint
/// that moves with the control (a ceiling that depends on another setting) stays
/// correct because it is passed per render, not baked in here. An empty hint
/// leaves the label row as it was.
///
/// The amber variant tints the field's border and the suffix rather than the
/// field's text: gpui-component paints an input's glyphs in the theme's
/// `foreground` and ignores the inherited text color, so the tint rides what the
/// element style can actually reach. The mono family DOES inherit, so the typed
/// figure keeps the readout's data voice.
pub fn labeled_slider_input(
    label: &'static str,
    hint: SharedString,
    state: &Entity<SliderState>,
    input: &Entity<InputState>,
    suffix: &'static str,
    amber: bool,
    cx: &App,
) -> impl IntoElement {
    let slider = if amber {
        // Per-element override: the shared theme colors the default rust.
        div().child(Slider::new(state)).text_color(theme::amber())
    } else {
        div().child(Slider::new(state))
    };
    // Sized off the kit's scale, not a fixed `Size`, so the field's text tracks
    // the UI scale like every other readout (gpui-component takes 0.875 of it).
    // A custom `Size` falls back to fixed px for the box itself, so the height
    // and padding are restated in `sp` — the element style is refined last, so
    // these win.
    let field = Input::new(input)
        .with_size(theme::sp(12.5))
        .h(theme::sp(24.))
        .px(theme::sp(8.));
    let field = if amber {
        field.border_color(theme::amber())
    } else {
        field
    };
    // The label row: caption on the left, hint against the right edge. The
    // caption's `flex_1` is what pushes the hint over — with an empty caption it
    // is a bare spacer, so a hint-only row still lands where a labelled one's
    // would.
    let head = (!label.is_empty() || !hint.is_empty()).then(|| {
        div()
            .flex()
            .items_center()
            .gap(theme::sp(8.))
            .child(
                div()
                    .flex_1()
                    .text_size(theme::sp(11.))
                    .text_color(theme::dim())
                    .child(label),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(theme::sp(10.))
                    .text_color(theme::mute())
                    .child(hint),
            )
    });
    div()
        .flex()
        .flex_col()
        .gap(theme::sp(6.))
        .when_some(head, |el, h| el.child(h))
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme::sp(8.))
                .child(div().flex_1().child(slider))
                .child(
                    div()
                        .flex_none()
                        .w(theme::sp(FIELD_W))
                        .font_family(theme::mono(cx))
                        .child(field),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(theme::sp(11.))
                        .text_color(if amber { theme::amber() } else { theme::mute() })
                        .child(suffix),
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
        .gap(theme::sp(12.))
        .child(div().text_size(theme::sp(12.)).child(label.into()))
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
        .p(theme::sp(2.))
        .gap(theme::sp(2.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .bg(theme::panel_0())
        .border_1()
        .border_color(theme::line_soft())
        .children(options.iter().enumerate().map(|(i, label)| {
            let on_select = on_select.clone();
            let is_active = i == active;
            div()
                .id((id_prefix, i))
                .px(theme::sp(10.))
                .py(theme::sp(4.))
                .rounded(theme::sp(theme::RADIUS_SM - 1.0))
                .text_size(theme::sp(11.))
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
        .px(theme::sp(12.))
        .py(theme::sp(5.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .text_size(theme::sp(12.))
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
        .gap(theme::sp(12.))
        .p(theme::sp(14.))
        .rounded(theme::sp(theme::RADIUS))
        .bg(theme::panel_1())
        .border_1()
        .border_color(theme::line_soft())
}

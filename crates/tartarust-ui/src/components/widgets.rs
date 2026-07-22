//! Small shared widgets: the page header plus form controls used by the
//! inspector panels.

use dioxus::prelude::*;

/// The screen's context line. The active tab already names the screen, so the
/// head deliberately carries no title — just one quiet instructional sentence.
#[component]
pub fn PageHead(sub: &'static str) -> Element {
    rsx! {
        div { class: "page-head",
            p { class: "page-sub", "{sub}" }
        }
    }
}

/// A labelled range slider with a monospace value readout. An empty `label`
/// skips the field label row (for sliders whose card title already names them).
#[component]
pub fn Slider(
    label: String,
    /// Current value.
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    /// Pre-formatted readout (e.g. "192 (75%)").
    readout: String,
    /// Amber variant (used for the secondary actuation point).
    #[props(default = false)]
    amber: bool,
    on_input: EventHandler<f64>,
) -> Element {
    let cls = if amber { "slider amber" } else { "slider" };
    rsx! {
        div { class: "field",
            if !label.is_empty() {
                label { class: "field-label", "{label}" }
            }
            div { class: "slider-row",
                input {
                    class: "{cls}",
                    r#type: "range",
                    min: "{min}", max: "{max}", step: "{step}",
                    value: "{value}",
                    oninput: move |e| {
                        if let Ok(v) = e.value().parse::<f64>() {
                            on_input.call(v);
                        }
                    },
                }
                span { class: "slider-val", "{readout}" }
            }
        }
    }
}

/// A pill toggle switch with an on/off state.
#[component]
pub fn Switch(on: bool, on_toggle: EventHandler<()>) -> Element {
    rsx! {
        div {
            class: if on { "switch on" } else { "switch" },
            onclick: move |_| on_toggle.call(()),
        }
    }
}

/// A row: label on the left, switch on the right.
#[component]
pub fn Toggle(label: String, on: bool, on_toggle: EventHandler<()>) -> Element {
    rsx! {
        div { class: "switch-row",
            span { class: "field-value", "{label}" }
            Switch { on, on_toggle: move |_| on_toggle.call(()) }
        }
    }
}

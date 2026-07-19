//! Small shared widgets: the page header plus form controls used by the
//! inspector panels.

use dioxus::prelude::*;

/// The standard screen header: title + one-line subtitle.
#[component]
pub fn PageHead(title: &'static str, sub: &'static str) -> Element {
    rsx! {
        div { class: "page-head",
            h1 { class: "page-title", "{title}" }
            p { class: "page-sub", "{sub}" }
        }
    }
}

/// A labelled range slider with a monospace value readout.
#[component]
pub fn Slider(
    label: String,
    /// Current value.
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    /// Pre-formatted readout (e.g. "1.5 mm" or "75%").
    readout: String,
    /// Amber variant (used for the secondary actuation point).
    #[props(default = false)]
    amber: bool,
    on_input: EventHandler<f64>,
) -> Element {
    let cls = if amber { "slider amber" } else { "slider" };
    rsx! {
        div { class: "field",
            label { class: "field-label", "{label}" }
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

//! `tartarus-ui` — Dioxus desktop front-end for the Razer Tartarus Pro.
//!
//! A Synapse-*inspired* (not cloned) control surface: dark instrument theme,
//! Razer green used only for active state. Five screens — Customize, Lighting,
//! Calibration, Macros, Profiles — wired over a router with a persistent
//! sidebar. State lives in a single `Signal<AppState>` global store (mock data
//! for now; see `model.rs` for the promotion-to-IPC list).
//!
//! ALWAYS run inside the headless `cage` harness via
//! `scripts/headless-screenshot.sh` — never spawn a window on real displays.

mod components;
mod ipc;
mod model;
mod screens;
mod style;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;

use components::Shell;
use model::{mock_state, AppState};
use screens::{Calibration, Customize, Lighting, Macros, Profiles};

fn main() {
    // No `dx` here: we run a plain cargo binary under cage. Configure the wry
    // window directly and strip the default GTK menubar (`with_menu(None)`).
    let window = WindowBuilder::new()
        .with_title("rusty-tartarus")
        .with_inner_size(LogicalSize::new(1280.0, 800.0));

    let config = Config::new()
        .with_window(window)
        .with_menu(None)
        .with_background_color((10, 11, 12, 255));

    dioxus::LaunchBuilder::desktop()
        .with_cfg(config)
        .launch(App);
}

/// Routes. Every screen is wrapped in the persistent [`Shell`] layout.
#[derive(Clone, Debug, PartialEq, Routable)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Shell)]
        #[route("/")]
        Customize {},
        #[route("/lighting")]
        Lighting {},
        #[route("/calibration")]
        Calibration {},
        #[route("/macros")]
        Macros {},
        #[route("/profiles")]
        Profiles {},
}

#[allow(non_snake_case)]
fn App() -> Element {
    // The one global store. Starts as the offline mock so the UI renders before
    // (and without) the daemon; the loader below overlays live state if present.
    let mut store = use_context_provider(|| Signal::new(mock_state()));

    // Initial load: probe the daemon once on mount. On success, overlay live
    // state; on failure (e.g. the cage harness, which runs with no daemon),
    // load_state returns None and the offline mock stays — the UI never blocks.
    use_resource(move || async move {
        if let Some(live) = ipc::load_state(mock_state()).await {
            store.set(live);
        }
    });

    rsx! {
        // Inject the whole stylesheet once. This is the loading path proven to
        // render under the bare-cargo + cage harness (no asset! pipeline).
        style { {style::APP_CSS} }
        Router::<Route> {}
    }
}

/// Convenience: read+write access to the global store from any component.
pub fn use_store() -> Signal<AppState> {
    use_context::<Signal<AppState>>()
}

//! `tartarust-ui` — Dioxus desktop front-end for the Razer Tartarus Pro.
//!
//! A Synapse-*inspired* (not cloned) control surface: dark instrument theme,
//! Razer green used only for active state. Five screens — Customize, Lighting,
//! Calibration, Macros, Profiles — wired over a router with a persistent
//! sidebar. State lives in a single `Signal<AppState>` global store (mock data
//! for now; see `model.rs` for the promotion-to-IPC list).
//!
//! ALWAYS run inside the headless `cage` harness via
//! `scripts/headless-screenshot.sh` — never spawn a window on real displays.
//!
//! Lib-only crate: the app root (routes, store, launcher) lives HERE so the
//! internal `crate::Route` / `crate::use_store` paths keep resolving, and the
//! workspace's root package exposes the `tartarust-ui` bin as a thin shim over
//! [`run_app`] — one `cargo install --path .` ships every binary.
//!
//! ## Desktop integration (see [`desktop`])
//!
//! On startup the installed binary (a) idempotently installs/refreshes its XDG
//! desktop entry + icon and (b) autostarts the daemon when none answers on the
//! control socket — both best-effort, never blocking the UI. Opt-outs:
//! `TARTARUST_UI_NO_DESKTOP_INSTALL=1` / `TARTARUST_UI_NO_AUTOSTART=1` (the cage
//! harness sets both). `tartarust-ui install-desktop` runs the entry/icon
//! install explicitly (ignoring the gate) and exits without opening a window;
//! any other argument is ignored.

mod components;
mod desktop;
mod ipc;
mod model;
mod screens;
mod style;

use std::time::Duration;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;

use components::Shell;
use model::{AppState, mock_state};
use screens::{Calibration, Customize, Lighting, Macros, Profiles};

/// Binary entry point: build the window config and launch the Dioxus app.
/// Called by the root package's `src/bin/tartarust-ui.rs` shim — and by nothing
/// else during dev (headless-cage rule above).
pub fn run_app() {
    // Idempotent (checks `has_been_set`) and the same initializer `launch`
    // calls later — installing it early makes the pre-launch desktop/daemon
    // tracing lines below actually visible.
    dioxus::logger::initialize_default();

    // Explicit CLI form: refresh the desktop entry + icon and exit — no
    // window. Deliberately ignores the NO_DESKTOP_INSTALL gate (an explicit
    // ask wins). Any other argument is ignored, matching the previous
    // takes-no-args behavior.
    if std::env::args().nth(1).as_deref() == Some("install-desktop") {
        desktop::install_desktop_cli();
        return;
    }

    // Best-effort, env-gated, never fatal — the UI must come up regardless.
    desktop::ensure_desktop_integration();
    desktop::ensure_daemon_running();

    // No `dx` here: we run a plain cargo binary under cage. Configure the wry
    // window directly and strip the default GTK menubar (`with_menu(None)`).
    // Decorations are off: the masthead doubles as the titlebar (drag +
    // window controls live in `Shell`); tao still hit-tests the borders of
    // undecorated windows so edge-resize keeps working.
    let window = WindowBuilder::new()
        .with_title("tartarust")
        .with_decorations(false)
        .with_inner_size(LogicalSize::new(1280.0, 800.0));

    let config = Config::new()
        .with_window(window)
        .with_menu(None)
        .with_background_color((19, 17, 16, 255));

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

    // Initial load: probe the daemon on mount. On success, overlay live state.
    // If startup just autostarted the daemon, it needs a moment to grab the
    // device and open its socket — retry briefly (inside the async resource,
    // so first render is never blocked) instead of settling into offline.
    // When nothing was spawned (daemon deliberately absent, e.g. the cage
    // harness) keep the single probe: the offline mock stays, instantly.
    use_resource(move || async move {
        // ~12 × 500ms ≈ 6s: generous for daemon startup (device grab + config
        // load), short enough that a crash-looping daemon leaves us offline fast.
        const SPAWN_RETRIES: u32 = 12;
        const RETRY_DELAY: Duration = Duration::from_millis(500);

        if let Some(live) = ipc::load_state(mock_state()).await {
            store.set(live);
            return;
        }
        if !desktop::daemon_was_spawned() {
            return;
        }
        for _ in 0..SPAWN_RETRIES {
            tokio::time::sleep(RETRY_DELAY).await;
            if let Some(live) = ipc::load_state(mock_state()).await {
                store.set(live);
                return;
            }
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

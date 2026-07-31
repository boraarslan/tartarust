//! `tartarust-ui` — GPUI desktop front-end for the Razer Tartarus Pro.
//!
//! A Synapse-*inspired* (not cloned) control surface: dark instrument theme,
//! Razer green used only for active state. Five screens — Customize, Lighting,
//! Calibration, Macros, Profiles — behind a persistent shell. State lives in a
//! single `Entity<AppState>` store (mock data offline; `store` overlays live
//! daemon state when reachable).
//!
//! ALWAYS run inside the headless `cage` harness via
//! `scripts/headless-screenshot.sh` (with `HEADLESS_WLR_RENDERER=gles2` — GPUI
//! renders through Vulkan and needs a dmabuf-capable compositor) — never spawn
//! a window on real displays.
//!
//! Lib-only crate: the workspace's root package exposes the `tartarust-ui` bin
//! as a thin shim over [`run_app`] — one `cargo install --path .` ships every
//! binary.
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
mod store;
mod theme;

use gpui::{
    App, AppContext as _, Application, Bounds, WindowBounds, WindowDecorations, WindowOptions, px,
    size,
};

use components::Shell;

/// Binary entry point: set up logging/desktop integration and launch the GPUI
/// app. Called by the root package's `src/bin/tartarust-ui.rs` shim — and by
/// nothing else during dev (headless-cage rule above).
pub fn run_app() {
    // `RUST_LOG` wins; default to info so the desktop/daemon startup lines
    // below are visible (parity with the old `dioxus::logger` default) — but
    // cap gpui's chatty render internals, which log every texture at info.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,naga=warn,blade_graphics=warn,zbus=warn")
            }),
        )
        .init();

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

    Application::new().run(|cx: &mut App| {
        // Order matters: component init installs the default theme, ours then
        // overrides it with the bench-instrument palette.
        gpui_component::init(cx);
        theme::init(cx);
        theme::apply_component_theme(cx);

        // The one global store. Starts as the offline mock so the UI renders
        // before (and without) the daemon; the loader overlays live state if
        // the daemon answers (with a retry window if we just spawned it).
        let store = cx.new(|_| model::mock_state());
        store::spawn_initial_load(store.clone(), cx);

        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // Client-side decorations: the masthead doubles as the
                // titlebar (drag + window controls live in `Shell`). An early
                // "CSD renders black" scare was a harness first-present race,
                // since fixed with capture retries — Client verifies clean.
                window_decorations: Some(WindowDecorations::Client),
                // Wayland app_id / X11 WM_CLASS — MUST match the installed
                // desktop entry basename (see `desktop::WM_CLASS`).
                app_id: Some(desktop::WM_CLASS.into()),
                ..Default::default()
            },
            |window, cx| {
                // gpui-component widgets (popovers, modals, inputs) need a
                // `Root` at the window's first level for their layer stack.
                let shell = cx.new(|cx| Shell::new(store.clone(), window, cx));
                cx.new(|cx| gpui_component::Root::new(shell, window, cx))
            },
        )
        .expect("failed to open the tartarust-ui window");
    });
}

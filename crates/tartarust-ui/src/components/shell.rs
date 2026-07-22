//! The persistent app shell, laid out like an instrument faceplate:
//! masthead (nameplate + link readout + profile/save), tab rail, content
//! outlet, and a bottom status strip with the persistent readouts.

use std::time::{Duration, Instant};

use dioxus::desktop::use_window;
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::Route;
use crate::use_store;

/// Tab entries: engraved label + route. No icons — the faceplate voice is
/// typographic.
fn tabs() -> [(&'static str, Route); 5] {
    [
        ("Customize", Route::Customize {}),
        ("Lighting", Route::Lighting {}),
        ("Calibration", Route::Calibration {}),
        ("Macros", Route::Macros {}),
        ("Profiles", Route::Profiles {}),
    ]
}

#[component]
pub fn Shell() -> Element {
    // Capture the store handle once, before the effect, rather than calling
    // use_store() inside the closure (hooks/context lookups belong in the
    // component body, not deferred into an effect run).
    let mut store = use_store();

    // Dev-only: let the headless screenshot harness jump straight to a screen
    // via TARTARUST_SCREEN=lighting|calibration|macros|profiles. No effect in
    // normal use (the var is unset), so this never alters real navigation.
    let nav = use_navigator();
    use_effect(move || {
        if let Ok(screen) = std::env::var("TARTARUST_SCREEN") {
            let target = match screen.as_str() {
                "lighting" => Some(Route::Lighting {}),
                "calibration" => Some(Route::Calibration {}),
                "macros" => Some(Route::Macros {}),
                "profiles" => Some(Route::Profiles {}),
                _ => None,
            };
            if let Some(t) = target {
                nav.replace(t);
            }
        }
        // Dev-only: force the Customize edit layer for a Hypershift screenshot.
        if std::env::var("TARTARUST_LAYER").as_deref() == Ok("hypershift") {
            store.write().layer = crate::model::Layer::Hypershift;
        }
    });
    let s = store.read();
    let route: Route = use_route();

    let active_profile = s.profiles.get(s.active_profile).cloned();
    let profiles = s.profiles.clone();
    let active_idx = s.active_profile;
    let offline = s.offline;
    // `device.connected` is a daemon startup cache, not live hotplug state — we
    // fold it into the readout but the leading signal is daemon reachability.
    let device_connected = s.device.connected;
    // Pending unsaved session edits (binds/lighting/calibration/etc.). Drives the
    // Save button beside the profile picker; meaningless offline (nothing to save
    // to), so the button only shows when the daemon is reachable.
    let unsaved = s.unsaved;
    drop(s);

    // The window has no OS decorations (`with_decorations(false)`): the
    // masthead IS the titlebar. Its background starts an OS move-drag; a quick
    // second press toggles maximize. We time presses ourselves instead of
    // relying on `ondoubleclick` because the first press hands the pointer to
    // the compositor's move loop, which may swallow the dblclick synthesis.
    let window = use_window();
    let mut last_down: Signal<Option<Instant>> = use_signal(|| None);
    let on_masthead_down = {
        let window = window.clone();
        move |e: Event<MouseData>| {
            if e.data().trigger_button() != Some(MouseButton::Primary) {
                return;
            }
            let now = Instant::now();
            let prev = last_down.peek().clone();
            last_down.set(Some(now));
            match prev {
                Some(t) if now.duration_since(t) < Duration::from_millis(400) => {
                    window.toggle_maximized();
                }
                _ => window.drag(),
            }
        }
    };
    let win_min = {
        let window = window.clone();
        move |_| window.window.set_minimized(true)
    };
    let win_max = {
        let window = window.clone();
        move |_| window.toggle_maximized()
    };
    let win_close = {
        let window = window.clone();
        move |_| window.close()
    };

    rsx! {
        div { class: "app",
            // Masthead: nameplate, link annunciator, profile + save — and,
            // decorations being off, the drag region + window controls.
            header { class: "masthead",
                onmousedown: on_masthead_down,
                div { class: "brand",
                    div { class: "brand-mark" }
                    div {
                        div { class: "brand-name",
                            "TARTA"
                            span { class: "accent", "RUST" }
                        }
                        div { class: "brand-type", "TARTARUS PRO · CONTROL SURFACE" }
                    }
                }
                div { class: "masthead-spacer" }
                div { class: "ann",
                    div { class: if offline { "led off" } else { "led live" } }
                    if offline {
                        span { "NO LINK" }
                    } else if device_connected {
                        span { class: "strong", "LINK 1532:0244" }
                    } else {
                        span { "DAEMON · NO DEVICE" }
                    }
                }
                div { class: "profile-select",
                    // Keep presses on the picker/save cluster from bubbling to
                    // the masthead's drag handler.
                    onmousedown: move |e| e.stop_propagation(),
                    "Profile"
                    select {
                        onchange: move |e| {
                            if let Ok(i) = e.value().parse::<usize>() {
                                // Switch + reload the full profile body so the
                                // editor screens render the NEW profile, not the
                                // old one. The helper flips index + body together
                                // when GetProfile resolves.
                                let name = store.read().profiles.get(i).map(|p| p.name.clone());
                                if let Some(name) = name {
                                    crate::ipc::switch_profile_and_reload(store, name);
                                }
                            }
                        },
                        for (i, p) in profiles.iter().enumerate() {
                            option { value: "{i}", selected: i == active_idx, "{p.name}" }
                        }
                    }
                    // Save: edits apply to the live session immediately but aren't
                    // written to disk until this is pressed. Only shown when the
                    // daemon is reachable AND there are pending changes.
                    if !offline && unsaved {
                        button {
                            class: "btn primary save-profile",
                            title: "Save pending changes to this profile",
                            onclick: move |_| crate::ipc::save_profile(store, None),
                            "Save"
                        }
                    }
                }
                // Window controls (min / max / close): the machined stand-ins
                // for the OS titlebar buttons.
                div { class: "winctl",
                    onmousedown: move |e| e.stop_propagation(),
                    button { class: "wbtn", title: "Minimize", onclick: win_min, "–" }
                    button { class: "wbtn", title: "Maximize / restore", onclick: win_max, "□" }
                    button { class: "wbtn close", title: "Close", onclick: win_close, "✕" }
                }
            }

            // Tab rail: the faceplate's section switches.
            nav { class: "tab-rail",
                for (label, target) in tabs() {
                    Link {
                        to: target.clone(),
                        class: if route == target { "tab active" } else { "tab" },
                        "{label}"
                    }
                }
            }

            // Content
            main { class: "content",
                Outlet::<Route> {}
            }

            // Status strip: persistent instrument readouts.
            footer { class: "statusbar",
                div { class: "sb-item",
                    div { class: if offline { "led off" } else { "led live" } }
                    if offline { "OFFLINE — MOCK DATA" } else { "DAEMON LIVE" }
                }
                if let Some(p) = active_profile {
                    div { class: "sb-item", "PROFILE · {p.name}" }
                }
                div { class: "sb-spacer" }
                if unsaved && !offline {
                    div { class: "sb-item drift",
                        div { class: "led amber" }
                        "UNSAVED SESSION"
                    }
                }
                div { class: "sb-item", {concat!("V", env!("CARGO_PKG_VERSION"))} }
            }
        }
    }
}

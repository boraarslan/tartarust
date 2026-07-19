//! The persistent app shell: brand block, sidebar nav, top bar, content outlet.

use dioxus::prelude::*;

use crate::Route;
use crate::use_store;

/// One sidebar entry: its route, label, and a small inline-SVG glyph.
struct NavEntry {
    route: Route,
    label: &'static str,
    icon: Element,
}

#[component]
pub fn Shell() -> Element {
    // Capture the store handle once, before the effect, rather than calling
    // use_store() inside the closure (review #7 — hooks/context lookups belong in
    // the component body, not deferred into an effect run).
    let mut store = use_store();

    // Dev-only: let the headless screenshot harness jump straight to a screen
    // via TARTARUS_SCREEN=lighting|calibration|macros|profiles. No effect in
    // normal use (the var is unset), so this never alters real navigation.
    let nav = use_navigator();
    use_effect(move || {
        if let Ok(screen) = std::env::var("TARTARUS_SCREEN") {
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
        if std::env::var("TARTARUS_LAYER").as_deref() == Ok("hypershift") {
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
    // fold it into the chip but the leading signal is daemon reachability.
    let device_connected = s.device.connected;
    // Pending unsaved session edits (binds/lighting/calibration/etc.). Drives the
    // Save button beside the profile picker; meaningless offline (nothing to save
    // to), so the button only shows when the daemon is reachable.
    let unsaved = s.unsaved;
    drop(s);

    let nav = nav_entries();

    rsx! {
        div { class: "app",
            // Brand
            div { class: "brand",
                div { class: "brand-mark" }
                div { class: "brand-name",
                    "rusty"
                    span { class: "accent", "·tartarus" }
                }
            }

            // Top bar
            div { class: "topbar",
                div { class: "device-chip",
                    div { class: if offline { "dot off" } else { "dot" } }
                    span { class: "name", "Tartarus Pro" }
                    span { class: "status",
                        if offline {
                            "daemon offline"
                        } else if device_connected {
                            "1532:0244 · connected"
                        } else {
                            "daemon up · no device"
                        }
                    }
                }
                div { class: "topbar-spacer" }
                div { class: "profile-select",
                    "Profile"
                    select {
                        onchange: move |e| {
                            if let Ok(i) = e.value().parse::<usize>() {
                                // Switch + reload the full profile body so the
                                // editor screens render the NEW profile, not the
                                // old one. The helper flips index + body together
                                // when GetProfile resolves (review #1).
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
            }

            // Sidebar nav
            nav { class: "nav",
                div { class: "nav-section", "Configure" }
                for entry in nav {
                    Link {
                        to: entry.route.clone(),
                        class: if route == entry.route { "nav-item active" } else { "nav-item" },
                        span { class: "ico", {entry.icon} }
                        "{entry.label}"
                    }
                }
                div { class: "nav-footer",
                    if let Some(p) = active_profile {
                        "active: {p.name}"
                    }
                    br {}
                    if offline { "v0.0.0 · offline (mock)" } else { "v0.0.0 · live" }
                }
            }

            // Content
            main { class: "content",
                Outlet::<Route> {}
            }
        }
    }
}

fn nav_entries() -> Vec<NavEntry> {
    vec![
        NavEntry {
            route: Route::Customize {},
            label: "Customize",
            icon: icon_grid(),
        },
        NavEntry {
            route: Route::Lighting {},
            label: "Lighting",
            icon: icon_sun(),
        },
        NavEntry {
            route: Route::Calibration {},
            label: "Calibration",
            icon: icon_gauge(),
        },
        NavEntry {
            route: Route::Macros {},
            label: "Macros",
            icon: icon_macro(),
        },
        NavEntry {
            route: Route::Profiles {},
            label: "Profiles",
            icon: icon_layers(),
        },
    ]
}

// --- Inline SVG glyphs (stroke = currentColor so active state recolours) ----

fn svg(children: Element) -> Element {
    rsx! {
        svg {
            width: "18", height: "18", view_box: "0 0 24 24",
            fill: "none", stroke: "currentColor", stroke_width: "1.8",
            stroke_linecap: "round", stroke_linejoin: "round",
            {children}
        }
    }
}

fn icon_grid() -> Element {
    svg(rsx! {
        rect { x: "3", y: "3", width: "7", height: "7", rx: "1" }
        rect { x: "14", y: "3", width: "7", height: "7", rx: "1" }
        rect { x: "3", y: "14", width: "7", height: "7", rx: "1" }
        rect { x: "14", y: "14", width: "7", height: "7", rx: "1" }
    })
}

fn icon_sun() -> Element {
    svg(rsx! {
        circle { cx: "12", cy: "12", r: "4" }
        path { d: "M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M19 5l-1.5 1.5M6.5 17.5L5 19" }
    })
}

fn icon_gauge() -> Element {
    svg(rsx! {
        path { d: "M12 21a9 9 0 1 0-9-9" }
        path { d: "M12 12l4-3" }
        circle { cx: "12", cy: "12", r: "1.4", fill: "currentColor" }
    })
}

fn icon_macro() -> Element {
    svg(rsx! {
        path { d: "M4 7h10M4 12h7M4 17h12" }
        circle { cx: "18", cy: "8", r: "2.2" }
    })
}

fn icon_layers() -> Element {
    svg(rsx! {
        path { d: "M12 3l9 5-9 5-9-5 9-5z" }
        path { d: "M3 13l9 5 9-5" }
    })
}

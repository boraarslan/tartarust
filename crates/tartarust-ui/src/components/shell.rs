//! The persistent app shell, laid out like an instrument faceplate:
//! masthead (nameplate + link readout + profile/save), tab rail, content
//! region, and a bottom status strip with the persistent readouts.
//!
//! This is the GPUI root view. It owns the active [`Screen`] (the router
//! replacement) and, decorations being client-side, the masthead is also the
//! titlebar: drag region + window controls.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{ClickEvent, Context, Entity, MouseButton, MouseDownEvent, Window, div};

use super::widgets::{led, led_off};
use crate::model::{AppState, Layer};
use crate::screens::{Calibration, Customize, Gallery, Lighting, Macros, Profiles};
use crate::store;
use crate::theme;

/// The five screens behind the tab rail. Replaces the Dioxus router: switching
/// is a plain field write + re-render, there is no URL surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Customize,
    Lighting,
    Calibration,
    Macros,
    Profiles,
    /// Dev-only widget gallery (`TARTARUST_SCREEN=gallery`); not in the rail.
    Gallery,
}

impl Screen {
    const ALL: [Screen; 5] = [
        Screen::Customize,
        Screen::Lighting,
        Screen::Calibration,
        Screen::Macros,
        Screen::Profiles,
    ];

    fn label(self) -> &'static str {
        match self {
            Screen::Customize => "Customize",
            Screen::Lighting => "Lighting",
            Screen::Calibration => "Calibration",
            Screen::Macros => "Macros",
            Screen::Profiles => "Profiles",
            Screen::Gallery => "Gallery",
        }
    }
}

pub struct Shell {
    store: Entity<AppState>,
    screen: Screen,
    /// Timestamp of the last masthead press, for hand-rolled double-press
    /// maximize (see [`Shell::on_masthead_down`]).
    last_masthead_down: Option<Instant>,
    /// Profile dropdown open state (masthead picker).
    picker_open: bool,
    /// Dev-only gallery view, created only under `TARTARUST_SCREEN=gallery`.
    gallery: Option<Entity<Gallery>>,
    /// Screen views, created lazily on first visit (each holds its own input/
    /// slider entities, so construction needs a `Window`).
    profiles_screen: Option<Entity<Profiles>>,
    lighting_screen: Option<Entity<Lighting>>,
    customize_screen: Option<Entity<Customize>>,
    macros_screen: Option<Entity<Macros>>,
    calibration_screen: Option<Entity<Calibration>>,
}

impl Shell {
    pub fn new(store: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The shell re-renders on every store write; screens hang off this
        // view, so one observer covers the whole tree.
        cx.observe(&store, |_, _, cx| cx.notify()).detach();

        // Dev-only: let the headless screenshot harness jump straight to a
        // screen via TARTARUST_SCREEN=lighting|calibration|macros|profiles.
        // No effect in normal use (the var is unset).
        let screen = match std::env::var("TARTARUST_SCREEN").as_deref() {
            Ok("lighting") => Screen::Lighting,
            Ok("calibration") => Screen::Calibration,
            Ok("macros") => Screen::Macros,
            Ok("profiles") => Screen::Profiles,
            Ok("gallery") => Screen::Gallery,
            _ => Screen::Customize,
        };
        let gallery = (screen == Screen::Gallery).then(|| cx.new(|cx| Gallery::new(window, cx)));
        // Dev-only: force the Customize edit layer for a Hypershift screenshot.
        if std::env::var("TARTARUST_LAYER").as_deref() == Ok("hypershift") {
            store.update(cx, |s, cx| {
                s.layer = Layer::Hypershift;
                cx.notify();
            });
        }

        let mut this = Self {
            store,
            screen,
            last_masthead_down: None,
            picker_open: false,
            gallery,
            profiles_screen: None,
            lighting_screen: None,
            customize_screen: None,
            macros_screen: None,
            calibration_screen: None,
        };
        this.ensure_screen(screen, window, cx);
        this
    }

    /// Create the active screen's view on first visit. Lazy on purpose: each
    /// screen owns input/slider entities that need a live `Window`, and most
    /// sessions never visit all five.
    fn ensure_screen(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        match screen {
            Screen::Profiles if self.profiles_screen.is_none() => {
                self.profiles_screen = Some(cx.new(|cx| Profiles::new(store, window, cx)));
            }
            Screen::Lighting if self.lighting_screen.is_none() => {
                self.lighting_screen = Some(cx.new(|cx| Lighting::new(store, window, cx)));
            }
            Screen::Customize if self.customize_screen.is_none() => {
                self.customize_screen = Some(cx.new(|cx| Customize::new(store, window, cx)));
            }
            Screen::Macros if self.macros_screen.is_none() => {
                self.macros_screen = Some(cx.new(|cx| Macros::new(store, window, cx)));
            }
            Screen::Calibration if self.calibration_screen.is_none() => {
                self.calibration_screen = Some(cx.new(|cx| Calibration::new(store, window, cx)));
            }
            _ => {}
        }
    }

    /// The masthead IS the titlebar. Its background starts an OS move-drag; a
    /// quick second press toggles maximize. We time presses ourselves instead
    /// of relying on a double-click event because the first press hands the
    /// pointer to the compositor's move loop, which may swallow the synthesis.
    fn on_masthead_down(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        match self.last_masthead_down {
            Some(t) if now.duration_since(t) < Duration::from_millis(400) => {
                // Double press: toggle, and RESET the timer — otherwise every
                // press <400ms after the previous one keeps toggling. Standard
                // titlebar behavior: the third press begins a fresh cycle.
                self.last_masthead_down = None;
                window.zoom_window();
            }
            _ => {
                self.last_masthead_down = Some(now);
                window.start_window_move();
            }
        }
    }
}

/// A `.wbtn` window-control button: machined stand-in for the OS titlebar
/// buttons. Presses must not bubble to the masthead's drag handler.
fn window_button(
    id: &'static str,
    glyph: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(theme::sp(34.))
        .h(theme::sp(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::sp(theme::RADIUS_SM))
        .text_color(theme::mute())
        .hover(|s| s.bg(theme::panel_1()).text_color(theme::ink()))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .child(glyph)
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Snapshot what the chrome needs, then drop the borrow before building
        // listeners.
        let (offline, device_connected, unsaved, active_idx, profile_names) = {
            let s = self.store.read(cx);
            (
                s.offline,
                s.device.connected,
                s.unsaved,
                s.active_profile,
                s.profiles
                    .iter()
                    .map(|p| p.name.clone())
                    .collect::<Vec<_>>(),
            )
        };
        let active_profile = profile_names.get(active_idx).cloned();
        let mono = theme::mono(cx);
        let screen = self.screen;

        // --- Masthead -------------------------------------------------------
        let brand = div()
            .flex()
            .items_center()
            .gap(theme::sp(11.))
            .child(
                div()
                    .size(theme::sp(10.))
                    .rounded(theme::sp(2.))
                    .bg(theme::rust()),
            )
            .child(
                div()
                    .child(
                        div()
                            .flex()
                            .font_family(mono.clone())
                            .text_size(theme::sp(13.5))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(theme::ink())
                            .child("TARTA")
                            .child(div().text_color(theme::rust()).child("RUST")),
                    )
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(theme::sp(8.5))
                            .text_color(theme::mute())
                            .child("TARTARUS PRO · CONTROL SURFACE"),
                    ),
            );

        // Link annunciator: daemon reachability leads; `device.connected` is a
        // daemon startup cache, not live hotplug state.
        let annunciator = div()
            .flex()
            .items_center()
            .gap(theme::sp(8.))
            .font_family(mono.clone())
            .text_size(theme::sp(10.))
            .text_color(theme::dim())
            .child(if offline {
                led_off()
            } else {
                led(theme::live())
            })
            .child(match (offline, device_connected) {
                (true, _) => div().child("NO LINK"),
                (false, true) => div()
                    .text_color(theme::ink())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("LINK 1532:0244"),
                (false, false) => div().child("DAEMON · NO DEVICE"),
            });

        // Profile cluster: dropdown trigger + Save. Save appears when the
        // daemon is reachable AND there are pending changes.
        let profile_cluster = div()
            .flex()
            .items_center()
            .gap(theme::sp(8.))
            .text_size(theme::sp(12.))
            .text_color(theme::dim())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child("Profile")
            .child(
                div()
                    .id("profile-picker")
                    .flex()
                    .items_center()
                    .gap(theme::sp(6.))
                    .px(theme::sp(8.))
                    .py(theme::sp(3.))
                    .rounded(theme::sp(theme::RADIUS_SM))
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::panel_0())
                    .font_family(mono.clone())
                    .text_color(theme::ink())
                    .hover(|s| s.bg(theme::panel_1()))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.picker_open = !this.picker_open;
                        cx.notify();
                    }))
                    .child(active_profile.clone().unwrap_or_else(|| "—".into()))
                    .child(
                        div()
                            .text_size(theme::sp(9.))
                            .text_color(theme::dim())
                            .child("▾"),
                    ),
            )
            .when(!offline && unsaved, |el| {
                el.child(
                    div()
                        .id("save-profile")
                        .px(theme::sp(10.))
                        .py(theme::sp(4.))
                        .rounded(theme::sp(theme::RADIUS_SM))
                        .bg(theme::rust())
                        .text_color(theme::chassis())
                        .text_size(theme::sp(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .hover(|s| s.bg(theme::rust_hi()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            store::save_profile(&this.store, None, cx);
                        }))
                        .child("Save"),
                )
            });

        let window_controls = div()
            .flex()
            .items_center()
            .gap(theme::sp(2.))
            .child(window_button("win-min", "–", |_, window, _| {
                window.minimize_window();
            }))
            .child(window_button("win-max", "□", |_, window, _| {
                window.zoom_window();
            }))
            .child(window_button("win-close", "✕", |_, window, _| {
                window.remove_window();
            }));

        let masthead = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(theme::sp(18.))
            .h(theme::sp(54.))
            .pl(theme::sp(26.))
            .pr(theme::sp(14.))
            .border_b_1()
            .border_color(theme::line_soft())
            .bg(theme::masthead_bg())
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_masthead_down))
            .child(brand)
            .child(div().flex_1())
            .child(annunciator)
            .child(profile_cluster)
            .child(window_controls);

        // --- Tab rail -------------------------------------------------------
        let tab_rail = div()
            .flex_none()
            .flex()
            .items_end()
            .gap(theme::sp(4.))
            .pl(theme::sp(22.))
            .border_b_1()
            .border_color(theme::line_soft())
            .children(Screen::ALL.map(|s| {
                let active = s == screen;
                div()
                    .id(s.label())
                    .px(theme::sp(12.))
                    .py(theme::sp(9.))
                    .text_size(theme::sp(12.5))
                    .text_color(if active { theme::ink() } else { theme::dim() })
                    .border_b_2()
                    .border_color(if active {
                        theme::rust()
                    } else {
                        gpui::transparent_black().into()
                    })
                    .hover(|st| st.text_color(theme::ink()))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        // Calibration polls the daemon at 100 ms while its view
                        // is ALIVE (the task dies with the entity). Dropping it
                        // on the way out reproduces the old cancel-on-unmount;
                        // the other screens keep their state across visits.
                        if this.screen == Screen::Calibration && s != Screen::Calibration {
                            this.calibration_screen = None;
                        }
                        this.screen = s;
                        this.ensure_screen(s, window, cx);
                        cx.notify();
                    }))
                    .child(s.label())
            }));

        // --- Content (stubs shrink as Phases 2–4 land) ----------------------
        let content = match (screen, self.gallery.clone(), self.profiles_screen.clone()) {
            (Screen::Gallery, Some(gallery), _) => {
                div().flex_1().min_h_0().child(gallery).into_any_element()
            }
            (Screen::Profiles, _, Some(profiles)) => {
                div().flex_1().min_h_0().child(profiles).into_any_element()
            }
            (Screen::Lighting, _, _) if self.lighting_screen.is_some() => div()
                .flex_1()
                .min_h_0()
                .child(self.lighting_screen.clone().unwrap())
                .into_any_element(),
            (Screen::Customize, _, _) if self.customize_screen.is_some() => div()
                .flex_1()
                .min_h_0()
                .child(self.customize_screen.clone().unwrap())
                .into_any_element(),
            (Screen::Macros, _, _) if self.macros_screen.is_some() => div()
                .flex_1()
                .min_h_0()
                .child(self.macros_screen.clone().unwrap())
                .into_any_element(),
            (Screen::Calibration, _, _) if self.calibration_screen.is_some() => div()
                .flex_1()
                .min_h_0()
                .child(self.calibration_screen.clone().unwrap())
                .into_any_element(),
            _ => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(theme::sp(6.))
                        .child(
                            div()
                                .font_family(mono.clone())
                                .text_size(theme::sp(14.))
                                .text_color(theme::ink())
                                .child(screen.label().to_uppercase()),
                        )
                        .child(
                            div()
                                .text_size(theme::sp(11.))
                                .text_color(theme::mute())
                                .child("GPUI port in progress — docs/GPUI_MIGRATION.md"),
                        ),
                )
                .into_any_element(),
        };

        // --- Status strip ---------------------------------------------------
        let statusbar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(theme::sp(16.))
            .h(theme::sp(28.))
            .px(theme::sp(14.))
            .border_t_1()
            .border_color(theme::line_soft())
            .font_family(mono.clone())
            .text_size(theme::sp(10.))
            .text_color(theme::dim())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme::sp(6.))
                    .child(if offline {
                        led_off()
                    } else {
                        led(theme::live())
                    })
                    .child(if offline {
                        "OFFLINE — MOCK DATA"
                    } else {
                        "DAEMON LIVE"
                    }),
            )
            .when_some(active_profile, |el, name| {
                el.child(div().child(format!("PROFILE · {name}")))
            })
            .child(div().flex_1())
            .when(unsaved && !offline, |el| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme::sp(6.))
                        .text_color(theme::amber())
                        .child(led(theme::amber()))
                        .child("UNSAVED SESSION"),
                )
            })
            .child(div().child(concat!("V", env!("CARGO_PKG_VERSION"))));

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::chassis())
            .font_family(theme::sans(cx))
            .text_size(theme::sp(13.))
            .text_color(theme::ink())
            .child(masthead)
            .child(tab_rail)
            .child(content)
            .child(statusbar)
            // Profile dropdown, painted last so it sits above everything.
            // First a click-away dismiss overlay, then the list itself,
            // anchored under the masthead's picker cluster.
            .when(self.picker_open, |el| {
                el.child(
                    div()
                        .id("picker-dismiss")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.picker_open = false;
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top(theme::sp(46.))
                        .right(theme::sp(120.))
                        .w(theme::sp(220.))
                        .flex()
                        .flex_col()
                        .p(theme::sp(4.))
                        .rounded(theme::sp(theme::RADIUS))
                        .bg(theme::panel_1())
                        .border_1()
                        .border_color(theme::line())
                        .shadow_lg()
                        .children(profile_names.iter().enumerate().map(|(i, name)| {
                            let switch_to = name.clone();
                            let is_active = i == active_idx;
                            div()
                                .id(("prof-row", i))
                                .px(theme::sp(8.))
                                .py(theme::sp(5.))
                                .rounded(theme::sp(theme::RADIUS_SM))
                                .font_family(mono.clone())
                                .text_size(theme::sp(12.))
                                .text_color(if is_active {
                                    theme::rust()
                                } else {
                                    theme::ink()
                                })
                                .hover(|s| s.bg(theme::panel_2()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.picker_open = false;
                                    store::switch_profile_and_reload(
                                        &this.store,
                                        switch_to.clone(),
                                        cx,
                                    );
                                    cx.notify();
                                }))
                                .child(name.clone())
                        })),
                )
            })
    }
}

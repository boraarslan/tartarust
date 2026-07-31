//! Phase 0 spike: GPUI hello-world + gpui-component Input + font-stack check,
//! rendered under the headless cage harness (docs/GPUI_MIGRATION.md §7).

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, Entity, SharedString,
    Window, WindowBounds, WindowOptions,
};
use gpui_component::input::{Input, InputState};
use gpui_component::Root;

struct HelloWorld {
    text: SharedString,
    input: Entity<InputState>,
}

impl HelloWorld {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("gpui-component Input renders here")
        });
        Self {
            text: "World".into(),
            input,
        }
    }
}

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(0x131110)) // tartarust window background
            .size_full()
            .justify_center()
            .items_center()
            .text_xl()
            .text_color(rgb(0xffffff))
            .font_family("IBM Plex Sans")
            .child(format!("Hello, {}! (GPUI spike)", &self.text))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().size_8().bg(gpui::red()))
                    .child(div().size_8().bg(gpui::green()))
                    .child(div().size_8().bg(gpui::blue()))
                    .child(div().size_8().bg(gpui::yellow()))
                    .child(div().size_8().bg(gpui::black()))
                    .child(div().size_8().bg(gpui::white())),
            )
            .child(
                div()
                    .p_4()
                    .border_1()
                    .border_color(rgb(0xd97706))
                    .rounded_md()
                    .shadow_lg()
                    .text_sm()
                    .child("borders, radius, shadow"),
            )
            .child(
                div()
                    .text_sm()
                    .font_family("TX-02")
                    .child("mono check TX-02: 0O 1lI 0123456789"),
            )
            .child(
                div()
                    .text_sm()
                    .font_family("Berkeley Mono")
                    .child("mono check Berkeley Mono: 0O 1lI 0123456789"),
            )
            .child(div().w(px(420.)).child(Input::new(&self.input)))
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("tartarust-ui".into()),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| HelloWorld::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .unwrap();
    });
}

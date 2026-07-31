//! The v5 "bench instrument" design tokens as Rust constants — the GPUI
//! replacement for the old `style.rs` CSS `:root` variable block (kept on disk,
//! undeclared, as the porting reference until the migration completes).
//!
//! Annunciator color system — every color has exactly one meaning:
//!   rust  #D96A2B  your hand: selection, primary actions, editing focus
//!   live  #7FBF3F  the device talking: link LED, live analog pressure
//!   amber #D9A44A  drift: unsaved session, custom actuation
//!   red   #D0574C  destructive, macro record
//! Nothing is colored for decoration; there are no glows except actual LEDs.

use gpui::{App, Global, Rgba, SharedString, rgb, rgba};

// --- Surfaces ----------------------------------------------------------------

pub fn chassis() -> Rgba {
    rgb(0x131110)
}
pub fn panel_0() -> Rgba {
    rgb(0x1A1815)
}
pub fn panel_1() -> Rgba {
    rgb(0x211E1A)
}
pub fn panel_2() -> Rgba {
    rgb(0x2A2620)
}
pub fn line() -> Rgba {
    rgb(0x39332B)
}
pub fn line_soft() -> Rgba {
    rgb(0x2A2621)
}
/// Masthead background (slightly off-chassis, per the CSS `.masthead`).
pub fn masthead_bg() -> Rgba {
    rgb(0x171512)
}

// --- Ink ---------------------------------------------------------------------

pub fn ink() -> Rgba {
    rgb(0xECE8DF)
}
pub fn dim() -> Rgba {
    rgb(0xA39C8D)
}
pub fn mute() -> Rgba {
    rgb(0x6F695C)
}

// --- Annunciators ------------------------------------------------------------

pub fn rust() -> Rgba {
    rgb(0xD96A2B)
}
pub fn rust_hi() -> Rgba {
    rgb(0xE87F3E)
}
pub fn rust_deep() -> Rgba {
    rgb(0x8A3F17)
}
pub fn rust_wash() -> Rgba {
    rgba(0xD96A2B1A) // 10%
}
pub fn live() -> Rgba {
    rgb(0x7FBF3F)
}
pub fn live_dim() -> Rgba {
    rgb(0x4C7A26)
}
pub fn live_wash() -> Rgba {
    rgba(0x7FBF3F1F) // 12%
}
pub fn amber() -> Rgba {
    rgb(0xD9A44A)
}
pub fn amber_wash() -> Rgba {
    rgba(0xD9A44A1A) // 10%
}
pub fn red() -> Rgba {
    rgb(0xD0574C)
}
pub fn red_wash() -> Rgba {
    rgba(0xD0574C1F) // 12%
}

// --- Radii (logical px, used with gpui::px at call sites) --------------------

pub const RADIUS: f32 = 6.0;
pub const RADIUS_SM: f32 = 4.0;

// --- Fonts -------------------------------------------------------------------

/// The CSS font stacks, in preference order. GPUI takes ONE family per element
/// (cosmic-text only falls back per-glyph, not per-family), so [`init`]
/// resolves each stack against the installed families once at startup.
const MONO_STACK: [&str; 4] = [
    "TX-02",
    "Berkeley Mono",
    "JetBrains Mono",
    "DejaVu Sans Mono",
];
const SANS_STACK: [&str; 4] = ["IBM Plex Sans", "Fira Sans", "DejaVu Sans", "sans-serif"];

/// Resolved font families, stored as a GPUI global at startup.
pub struct Fonts {
    pub mono: SharedString,
    pub sans: SharedString,
}

impl Global for Fonts {}

/// Resolve the font stacks against the platform text system and stash the
/// result as a global. The last stack entry is the unconditional fallback.
pub fn init(cx: &mut App) {
    let installed = cx.text_system().all_font_names();
    let pick = |stack: &[&'static str]| -> SharedString {
        stack
            .iter()
            .find(|f| installed.iter().any(|i| i == *f))
            .copied()
            .unwrap_or(stack[stack.len() - 1])
            .into()
    };
    let fonts = Fonts {
        mono: pick(&MONO_STACK),
        sans: pick(&SANS_STACK),
    };
    cx.set_global(fonts);
}

/// Bridge the palette into gpui-component's theme so its widgets (input,
/// slider, switch, dropdown, modal) wear the bench-instrument look instead of
/// shadcn defaults. Force dark mode FIRST — `Theme::change` re-applies the
/// built-in config and would clobber these overrides if run after.
pub fn apply_component_theme(cx: &mut App) {
    use gpui_component::theme::{Theme, ThemeMode};

    Theme::change(ThemeMode::Dark, None, cx);
    let t = Theme::global_mut(cx);

    t.background = chassis().into();
    t.foreground = ink().into();
    t.border = line().into();
    t.input = line().into();
    t.ring = rust().into();
    t.selection = rust_wash().into();

    t.primary = rust().into();
    t.primary_hover = rust_hi().into();
    t.primary_active = rust_deep().into();
    t.primary_foreground = chassis().into();

    t.secondary = panel_1().into();
    t.secondary_hover = panel_2().into();
    t.secondary_active = panel_2().into();
    t.secondary_foreground = ink().into();

    t.danger = red().into();
    t.danger_hover = red().into();
    t.danger_active = red().into();
    t.danger_foreground = ink().into();

    t.muted = panel_1().into();
    t.muted_foreground = dim().into();
    t.accent = panel_2().into();
    t.accent_foreground = ink().into();

    t.popover = panel_1().into();
    t.popover_foreground = ink().into();
    t.overlay = gpui::rgba(0x0000_0066).into();

    t.list = panel_0().into();
    t.list_hover = panel_1().into();
    t.list_active = panel_2().into();
    t.list_active_border = rust().into();

    t.slider_bar = rust().into();
    t.slider_thumb = ink().into();
    t.switch = panel_2().into();
    t.switch_thumb = ink().into();

    t.scrollbar = chassis().into();
    t.scrollbar_thumb = panel_2().into();
    t.scrollbar_thumb_hover = line().into();

    t.title_bar = masthead_bg().into();
    t.title_bar_border = line_soft().into();
    t.window_border = line_soft().into();

    t.tab_bar = chassis().into();
    t.tab = chassis().into();
    t.tab_foreground = dim().into();
    t.tab_active = chassis().into();
    t.tab_active_foreground = ink().into();
}

/// The resolved mono family (identity + data voice).
pub fn mono(cx: &App) -> SharedString {
    cx.global::<Fonts>().mono.clone()
}

/// The resolved sans family (controls voice).
pub fn sans(cx: &App) -> SharedString {
    cx.global::<Fonts>().sans.clone()
}

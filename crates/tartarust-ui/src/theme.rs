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

use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{App, Global, Pixels, Rgba, SharedString, px, rgb, rgba};

/// Global UI scale. Every dimension in the UI goes through [`sp`] ("scaled
/// pixels"), so density is tuned in exactly one place — and adjustable at
/// runtime (the statusbar's UI ± control). Default 1.5 per user review of the
/// 1.0 port: the CSS-era sizes read too small outside a webview.
pub const DEFAULT_SCALE: f32 = 1.5;
pub const MIN_SCALE: f32 = 1.0;
pub const MAX_SCALE: f32 = 2.25;
pub const SCALE_STEP: f32 = 0.25;

/// The live scale, stored as f32 bits so [`sp`] stays context-free (it is
/// called from every render path, most without an `App` handle).
static SCALE_BITS: AtomicU32 = AtomicU32::new(DEFAULT_SCALE.to_bits());

pub fn scale() -> f32 {
    f32::from_bits(SCALE_BITS.load(Ordering::Relaxed))
}

/// Set the scale (clamped) and re-render every window — layout, text, and
/// spacing all re-derive through [`sp`] on the next frame.
pub fn set_scale(scale: f32, cx: &mut App) {
    let clamped = scale.clamp(MIN_SCALE, MAX_SCALE);
    SCALE_BITS.store(clamped.to_bits(), Ordering::Relaxed);
    cx.refresh_windows();
}

/// A scaled logical dimension: `sp(13.)` = 13 design-px at the global scale.
pub fn sp(v: f32) -> Pixels {
    px(v * scale())
}

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
/// result as a global.
///
/// Matching is normalized (case/space/hyphen-insensitive) because the text
/// system's family names don't always match fontconfig's spelling — and an
/// unmatched literal falls through to the platform DEFAULT face (a serif on
/// this stack), which is exactly the wrong voice. The class-hint pass makes
/// the worst case "some installed sans/mono", never the serif default.
pub fn init(cx: &mut App) {
    let installed = cx.text_system().all_font_names();
    let norm = |s: &str| s.to_lowercase().replace([' ', '-', '_'], "");
    let pick = |stack: &[&'static str], class_hint: &str| -> SharedString {
        for want in stack {
            if let Some(hit) = installed.iter().find(|i| norm(i) == norm(want)) {
                return hit.clone().into();
            }
        }
        // Family variants ("IBM Plex Sans Text") and PostScript-ish names.
        for want in stack {
            if let Some(hit) = installed.iter().find(|i| norm(i).contains(&norm(want))) {
                return hit.clone().into();
            }
        }
        if let Some(hit) = installed.iter().find(|i| norm(i).contains(class_hint)) {
            return hit.clone().into();
        }
        stack[stack.len() - 1].into()
    };
    let fonts = Fonts {
        mono: pick(&MONO_STACK, "mono"),
        sans: pick(&SANS_STACK, "sans"),
    };
    tracing::info!(mono = %fonts.mono, sans = %fonts.sans, "resolved font stacks");
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

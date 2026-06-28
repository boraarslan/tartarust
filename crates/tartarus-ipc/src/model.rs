//! Shared domain model: profiles, binds, lighting, macros, device info.
//!
//! These types are the canonical, on-the-wire representation of everything the
//! daemon owns and the UI/CLI render. They carry **no device dependency** — keys
//! are plain evdev names (`"KEY_A"`, `"BTN_LEFT"`) as `String`, and colors are a
//! simple `Rgb` struct — so the contract crate compiles anywhere. The daemon
//! converts these to `evdev`/`tartarus-protocol` types at its boundary.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Which binding layer an action belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Default layer (Hypershift not held).
    Base,
    /// Secondary layer, active while the Hypershift key is held.
    Hypershift,
}

/// How the thumb stick's directional output is quantized.
///
/// `Eight` resolves to the four cardinals plus four diagonals (each diagonal a
/// chord of its two adjacent cardinals — see [`THUMB_DIAGONAL_CARDINALS`]);
/// `Four` collapses to cardinals only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThumbAxisMode {
    /// Eight-way: cardinals + diagonals.
    #[default]
    Eight,
    /// Four-way: cardinals only.
    Four,
}

/// What a physical key does when pressed.
///
/// Keys named here are evdev `KEY_*` / `BTN_*` strings. `Key` carries an ordered
/// list so a single bind can be a modifier chord (`["KEY_LEFTCTRL", "KEY_C"]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BindAction {
    /// Emit one or more keys (chord; pressed in order, released in reverse).
    Key {
        /// evdev key names.
        keys: Vec<String>,
    },
    /// Press/release a mouse button (`BTN_LEFT`, `BTN_RIGHT`, ...).
    MouseButton {
        /// evdev button name.
        button: String,
    },
    /// Run a named macro (see [`Macro`]).
    Macro {
        /// Macro name.
        name: String,
    },
    /// Switch the active profile.
    SwitchProfile {
        /// Target profile name.
        name: String,
    },
    /// Launch an external program (argv; first element is the executable).
    LaunchProgram {
        /// Program and arguments.
        command: Vec<String>,
    },
    /// While held, activate the Hypershift (secondary) layer. This is the only
    /// way to set the Hypershift trigger — bind it to a key in a profile's base
    /// layer. The trigger is therefore per-profile.
    Hypershift,
    /// Consume the key and emit nothing.
    Disable,
    /// Forward the original key through unchanged.
    Passthrough,
}

/// A lighting effect plus the parameters its device command needs.
///
/// The variant set matches the Tartarus Pro's supported Chroma effects. The
/// daemon maps each to a `tartarus-protocol::lighting` builder.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    /// LEDs off.
    None,
    /// Single static color.
    Static {
        /// The color.
        color: Rgb,
    },
    /// Breathing. Zero colors = random; one = single; two = dual.
    Breathing {
        /// 0, 1, or 2 colors.
        colors: Vec<Rgb>,
    },
    /// Cycle the full spectrum.
    #[default]
    Spectrum,
    /// A moving wave.
    Wave {
        /// Direction of travel.
        direction: WaveDirection,
    },
    /// Light keys as they are pressed.
    Reactive {
        /// Reaction speed (1–4, device-defined).
        speed: u8,
        /// Reaction color.
        color: Rgb,
    },
    /// Twinkling "starlight". Zero colors = random; one/two = single/dual.
    Starlight {
        /// Twinkle speed (device-defined).
        speed: u8,
        /// 0, 1, or 2 colors.
        colors: Vec<Rgb>,
    },
    /// A fully custom per-key frame (one row, 21 columns).
    Custom {
        /// Per-key colors; index = column (0..21). Missing entries are off.
        frame: Vec<Rgb>,
    },
}

/// Wave travel direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaveDirection {
    /// Left to right.
    Left,
    /// Right to left.
    Right,
}

/// An 8-bit-per-channel RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// Construct a color.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// A profile's complete lighting state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightingState {
    /// The active effect and its parameters.
    pub effect: Effect,
    /// Overall brightness (0–255).
    pub brightness: u8,
}

impl Default for LightingState {
    fn default() -> Self {
        LightingState {
            effect: Effect::default(),
            brightness: 255,
        }
    }
}

/// One step of a macro.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum MacroStep {
    /// Press a key (evdev name), no release.
    Press {
        /// Key to press.
        key: String,
    },
    /// Release a key.
    Release {
        /// Key to release.
        key: String,
    },
    /// Wait this many milliseconds.
    Delay {
        /// Milliseconds.
        ms: u64,
    },
}

/// A named macro: an ordered sequence of steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Macro {
    /// Macro name (referenced by [`BindAction::Macro`]).
    pub name: String,
    /// Ordered steps.
    pub steps: Vec<MacroStep>,
}

/// A complete profile: binds for both layers, lighting, and an optional linked
/// application (M4 auto-switch; data-only for now).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Profile {
    /// Profile name (unique within the config).
    pub name: String,
    /// Application that auto-activates this profile (M4; unused at runtime yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_app: Option<String>,
    /// Base-layer binds: evdev key name -> action. Defaults to empty so a
    /// hand-written profile may omit it.
    #[serde(default)]
    pub base: BTreeMap<String, BindAction>,
    /// Hypershift-layer binds: evdev key name -> action. Defaults to empty.
    #[serde(default)]
    pub hypershift: BTreeMap<String, BindAction>,
    /// This profile's lighting. Defaults to spectrum at full brightness so a
    /// hand-written profile may omit the `[...lighting]` table.
    #[serde(default)]
    pub lighting: LightingState,
    /// How the thumb stick quantizes direction (eight-way vs four-way). Defaults
    /// so a hand-written profile may omit it.
    #[serde(default)]
    pub thumb_axis_mode: ThumbAxisMode,
}

impl Profile {
    /// A new empty profile with default lighting.
    pub fn new(name: impl Into<String>) -> Self {
        Profile {
            name: name.into(),
            linked_app: None,
            base: BTreeMap::new(),
            hypershift: BTreeMap::new(),
            lighting: LightingState::default(),
            thumb_axis_mode: ThumbAxisMode::default(),
        }
    }

    /// The bind map for a given layer.
    pub fn layer(&self, layer: Layer) -> &BTreeMap<String, BindAction> {
        match layer {
            Layer::Base => &self.base,
            Layer::Hypershift => &self.hypershift,
        }
    }

    /// The mutable bind map for a given layer.
    pub fn layer_mut(&mut self, layer: Layer) -> &mut BTreeMap<String, BindAction> {
        match layer {
            Layer::Base => &mut self.base,
            Layer::Hypershift => &mut self.hypershift,
        }
    }
}

/// Device identity + connection state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Firmware version string (e.g. `"1.5"`), or `None` if not read.
    pub firmware: Option<String>,
    /// Device serial, or `None` if not read.
    pub serial: Option<String>,
    /// `true` when the control interface is reachable.
    pub connected: bool,
}

/// Thumb-stick diagonal → its two adjacent cardinal directions, keyed by evdev
/// NAME. In eight-way mode a diagonal is emitted as a chord of these two
/// cardinals.
///
/// NOTE: these diagonal keycodes (`KEY_HOME`/`KEY_PAGEUP`/`KEY_END`/
/// `KEY_PAGEDOWN`) are UNCONFIRMED placeholders, kept in sync with
/// tartarus-ui's `KeyId::evdev`, pending a real `discover-keys` pass against
/// the hardware.
pub const THUMB_DIAGONAL_CARDINALS: [(&str, [&str; 2]); 4] = [
    ("KEY_HOME", ["KEY_UP", "KEY_LEFT"]),
    ("KEY_PAGEUP", ["KEY_UP", "KEY_RIGHT"]),
    ("KEY_END", ["KEY_DOWN", "KEY_LEFT"]),
    ("KEY_PAGEDOWN", ["KEY_DOWN", "KEY_RIGHT"]),
];

/// The two cardinal directions a thumb-stick diagonal (by evdev `name`)
/// decomposes into, or `None` if `name` is not a diagonal.
pub fn thumb_diagonal_cardinals(name: &str) -> Option<[&'static str; 2]> {
    THUMB_DIAGONAL_CARDINALS
        .iter()
        .find(|(d, _)| *d == name)
        .map(|(_, c)| *c)
}

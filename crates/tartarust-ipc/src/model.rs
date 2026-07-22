//! Shared domain model: profiles, binds, lighting, macros, device info.
//!
//! These types are the canonical, on-the-wire representation of everything the
//! daemon owns and the UI/CLI render. They carry **no device dependency** — keys
//! are plain evdev names (`"KEY_A"`, `"BTN_LEFT"`) as `String`, and colors are a
//! simple `Rgb` struct — so the contract crate compiles anywhere. The daemon
//! converts these to `evdev`/`tartarust-protocol` types at its boundary.

use std::collections::{BTreeMap, BTreeSet};

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
/// daemon maps each to a `tartarust-protocol::lighting` builder.
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

/// File-format marker for an exported profile bundle (the `format` field).
pub const PROFILE_EXPORT_FORMAT: &str = "tartarust-profile";
/// Current export-bundle schema version. Bump when the bundle shape changes;
/// importers refuse bundles newer than they understand.
pub const PROFILE_EXPORT_VERSION: u32 = 1;

/// A portable profile bundle: one profile plus the macros its binds reference.
///
/// This is BOTH the on-disk shape of a `tartarust profile export` file (as
/// TOML — scalars first so the envelope serializes before the tables) and the
/// payload of the export/import IPC round-trip. Everything in it is the
/// device-free wire model, so a bundle written on one machine imports on any
/// other (or into a fresh install after a config-path change).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileExport {
    /// Always [`PROFILE_EXPORT_FORMAT`]; lets an importer reject foreign TOML.
    pub format: String,
    /// Bundle schema version ([`PROFILE_EXPORT_VERSION`] when written).
    pub version: u32,
    /// The exported profile, in full.
    pub profile: Profile,
    /// The macros the profile's binds reference (only those; a bind referencing
    /// a macro that did not exist at export time is carried as-is, dangling).
    #[serde(default)]
    pub macros: Vec<Macro>,
}

impl ProfileExport {
    /// Wrap a profile + its macros in a current-version envelope.
    pub fn new(profile: Profile, macros: Vec<Macro>) -> Self {
        ProfileExport {
            format: PROFILE_EXPORT_FORMAT.to_string(),
            version: PROFILE_EXPORT_VERSION,
            profile,
            macros,
        }
    }

    /// Check the envelope: right `format` marker, version not from the future.
    pub fn validate(&self) -> Result<(), String> {
        if self.format != PROFILE_EXPORT_FORMAT {
            return Err(format!(
                "not a {PROFILE_EXPORT_FORMAT} bundle (format = \"{}\")",
                self.format
            ));
        }
        if self.version > PROFILE_EXPORT_VERSION {
            return Err(format!(
                "bundle version {} is newer than this build supports ({PROFILE_EXPORT_VERSION}) — upgrade tartarust",
                self.version
            ));
        }
        Ok(())
    }
}

/// The names of all macros a profile's binds reference, across both layers.
/// Sorted (`BTreeSet`) so exports are deterministic.
pub fn referenced_macro_names(profile: &Profile) -> BTreeSet<String> {
    profile
        .base
        .values()
        .chain(profile.hypershift.values())
        .filter_map(|action| match action {
            BindAction::Macro { name } => Some(name.clone()),
            _ => None,
        })
        .collect()
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
/// tartarust-ui's `KeyId::evdev`, pending a real `discover-keys` pass against
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

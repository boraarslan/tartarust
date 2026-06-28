//! On-disk config: the daemon's persisted state.
//!
//! The config is the shared `tartarus-ipc` model serialized to TOML. There is
//! exactly one serde schema for the domain — the shared model — so the file the
//! user edits, the IPC wire form, and the engine's in-memory state are the same
//! shapes. The engine ([`crate::engine::Engine`]) owns a live copy; the daemon
//! loop persists snapshots back here atomically after mutating requests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use tartarus_ipc::model::{Macro, Profile};

/// The full persisted configuration.
///
/// The Hypershift trigger is NOT a top-level field — it is expressed as a key
/// bound to [`tartarus_ipc::model::BindAction::Hypershift`] in a profile's base
/// layer (so the trigger is per-profile and the UI maps 1:1 to bind types).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Profile active at startup; must be a key in `profiles`.
    pub default_profile: String,
    /// Whether per-application auto-switch is enabled. Defaults to `true` — it
    /// only acts on profiles that opt in via a `linked_app`, so on-by-default is
    /// safe (no links = nothing auto-switches).
    #[serde(default = "default_auto_switch")]
    pub auto_switch: bool,
    /// Whether the daemon shows a system-tray icon (StatusNotifierItem). Defaults
    /// to `true`. The tray is best-effort: it spawns on its own D-Bus tasks after
    /// the device is grabbed and degrades to headless if no tray host is present,
    /// so turning it off is purely opt-out. Kept BEFORE the table-valued fields
    /// below because TOML requires all scalar keys to precede any `[table]`.
    #[serde(default = "default_tray")]
    pub tray: bool,
    /// How the daemon reads the keypad. `Digital` (default) grabs the evdev
    /// nodes and remaps normal HID keystrokes (the proven M1 path). `Analog`
    /// instead puts the device in DRIVER mode and derives key-down/up from raw
    /// per-key pressure via [`AnalogConfig`] — the two are mutually exclusive
    /// (DRIVER mode disables the normal HID keyboard), so analog REPLACES the
    /// grab. Opt-in until the byte->key map is confirmed on hardware.
    #[serde(default)]
    pub input_mode: InputMode,
    /// Analog actuation settings (only consulted when `input_mode = Analog`).
    #[serde(default)]
    pub analog: AnalogConfig,
    /// All profiles, keyed by name.
    pub profiles: BTreeMap<String, Profile>,
    /// All macros, keyed by name.
    #[serde(default)]
    pub macros: BTreeMap<String, Macro>,
}

fn default_auto_switch() -> bool {
    true
}

fn default_tray() -> bool {
    true
}

/// Which physical-input path the daemon uses. Serialized lowercase
/// (`input_mode = "digital"` / `"analog"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    /// Grab the evdev nodes and remap normal HID keystrokes (M1 path). Default.
    #[default]
    Digital,
    /// Read raw per-key pressure in DRIVER mode and run the actuation engine.
    Analog,
}

/// Default actuation point: the pressure at which a key registers as DOWN.
pub const DEFAULT_ACTUATION_POINT: u8 = 0x60;
/// Default hysteresis margin: how far below the actuation point pressure must
/// fall to register a release. Prevents chatter around the threshold.
pub const DEFAULT_HYSTERESIS: u8 = 0x10;
/// Default deeper second-actuation point for dual-step keys (`~0xC0`).
pub const DEFAULT_SECOND_ACTUATION_POINT: u8 = 0xC0;

/// Analog actuation configuration: a per-byte-index actuation map plus the loop
/// poll cadence. Only used when `input_mode = Analog`.
///
/// The map is keyed by the analog byte index (0..[`ANALOG_KEY_COUNT`], matching
/// `AnalogFrame::pressures`). Each entry names the logical evdev key the engine
/// resolves that physical key's bind by, plus its actuation thresholds.
///
/// [`ANALOG_KEY_COUNT`]: tartarus_protocol::ANALOG_KEY_COUNT
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalogConfig {
    /// Poll timeout in milliseconds for the analog read loop. Bounds how often
    /// the loop wakes to re-check its stop flag when the device is idle (the
    /// device only emits while pressure changes), so it also bounds shutdown
    /// latency. Default 50ms.
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    /// Per-byte-index key map. Keyed by the analog byte index rendered as a
    /// string — TOML table keys are always strings, so `[analog.keys.5]` parses
    /// here as the key `"5"`. The daemon parses the index back to a number when
    /// building the actuator (a non-numeric or out-of-range key is warned and
    /// skipped there), so the on-disk form stays a plain TOML table.
    #[serde(default)]
    pub keys: BTreeMap<String, AnalogKey>,
}

fn default_poll_interval_ms() -> u64 {
    50
}

impl Default for AnalogConfig {
    fn default() -> Self {
        AnalogConfig {
            poll_interval_ms: default_poll_interval_ms(),
            keys: BTreeMap::new(),
        }
    }
}

/// One analog key's actuation settings: the logical key it maps to and its
/// rising/falling thresholds (plus an optional deeper second step).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalogKey {
    /// The evdev key name (`"KEY_Q"`) this byte index represents. The engine
    /// resolves THIS key's bind — so analog is a drop-in for the grabbed
    /// keyboard, and Hypershift/macros/profiles all work on top of it.
    pub key: String,
    /// Pressure at/above which the key registers as DOWN (rising edge). Default
    /// [`DEFAULT_ACTUATION_POINT`].
    #[serde(default = "default_actuation_point")]
    pub actuation_point: u8,
    /// Pressure below which a held key registers as UP (falling edge). Must be
    /// `< actuation_point` for hysteresis. Default `actuation_point -`
    /// [`DEFAULT_HYSTERESIS`].
    #[serde(default = "default_release_point")]
    pub release_point: u8,
    /// Optional deeper actuation point for a dual-step ("second function") key.
    /// When pressure crosses this (while already past `actuation_point`), the
    /// `second_action` key additionally fires — the primary stays held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_actuation_point: Option<u8>,
    /// The evdev key the second step fires (required iff `second_actuation_point`
    /// is set; ignored otherwise). Released when pressure falls back below the
    /// second actuation point minus the hysteresis margin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_action: Option<String>,
}

fn default_actuation_point() -> u8 {
    DEFAULT_ACTUATION_POINT
}

fn default_release_point() -> u8 {
    DEFAULT_ACTUATION_POINT.saturating_sub(DEFAULT_HYSTERESIS)
}

/// Errors loading or validating a config.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// The config file could not be read.
    #[error("reading config {path}: {source}")]
    Read {
        /// The path that failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The config TOML could not be parsed.
    #[error("parsing config: {0}")]
    Parse(#[from] toml::de::Error),
    /// The config TOML could not be serialized.
    #[error("serializing config: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The config failed validation.
    #[error("invalid config: {0}")]
    Invalid(String),
    /// Writing the config back to disk failed.
    #[error("writing config {path}: {source}")]
    Write {
        /// The path that failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

impl Config {
    /// A minimal default config: one empty `default` profile.
    pub fn default_single() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert("default".to_string(), Profile::new("default"));
        Config {
            default_profile: "default".to_string(),
            auto_switch: default_auto_switch(),
            tray: default_tray(),
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles,
            macros: BTreeMap::new(),
        }
    }

    /// Parse and validate a config from TOML text.
    pub fn from_toml(s: &str) -> Result<Self, StateError> {
        let cfg: Config = toml::from_str(s)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Serialize to TOML text.
    pub fn to_toml(&self) -> Result<String, StateError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Load a config from `path`. If the file does not exist, returns a default
    /// single-profile config (so a fresh install Just Works).
    pub fn load(path: &Path) -> Result<Self, StateError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::from_toml(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(path = %path.display(), "no config found; using default");
                Ok(Config::default_single())
            }
            Err(source) => Err(StateError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Write the config to `path` atomically (temp file + rename), creating
    /// parent directories as needed. A crash mid-write cannot truncate the
    /// user's config — the rename is atomic and only swaps in a complete file.
    ///
    /// Note: this rewrites the file from the in-memory model, so any comments in
    /// a hand-edited config are dropped. That's expected for daemon-managed
    /// state; users editing by hand should stop the daemon first.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let text = self.to_toml()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StateError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text.as_bytes()).map_err(|source| StateError::Write {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| StateError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Validate the config. **Structural** problems (no profiles, missing
    /// default) are hard errors. **Referential** problems (a bind pointing at a
    /// missing macro/profile) are only warnings — the runtime already tolerates
    /// them (a missing macro warns; a switch to a missing profile is a no-op).
    ///
    /// This asymmetry is deliberate: the daemon must always be able to reload a
    /// config it just wrote. Since IPC edits (delete a profile/macro still
    /// referenced by a bind) can produce dangling references, rejecting them on
    /// load would brick startup. So we warn and carry on.
    pub fn validate(&self) -> Result<(), StateError> {
        if self.profiles.is_empty() {
            return Err(StateError::Invalid("no profiles defined".into()));
        }
        if !self.profiles.contains_key(&self.default_profile) {
            return Err(StateError::Invalid(format!(
                "default_profile `{}` is not defined",
                self.default_profile
            )));
        }
        for profile in self.profiles.values() {
            for action in profile.base.values().chain(profile.hypershift.values()) {
                use tartarus_ipc::model::BindAction;
                match action {
                    BindAction::Macro { name } if !self.macros.contains_key(name) => {
                        tracing::warn!(
                            profile = %profile.name,
                            %name,
                            "bind references undefined macro (will no-op at runtime)"
                        );
                    }
                    BindAction::SwitchProfile { name } if !self.profiles.contains_key(name) => {
                        tracing::warn!(
                            profile = %profile.name,
                            %name,
                            "bind references undefined profile (will no-op at runtime)"
                        );
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tartarus_ipc::model::{BindAction, Effect, LightingState, Rgb};

    #[test]
    fn roundtrips_through_toml() {
        let mut cfg = Config::default_single();
        let p = cfg.profiles.get_mut("default").unwrap();
        p.base.insert(
            "KEY_A".into(),
            BindAction::Key {
                keys: vec!["KEY_LEFTCTRL".into(), "KEY_C".into()],
            },
        );
        p.lighting = LightingState {
            effect: Effect::Static {
                color: Rgb::new(0, 255, 0),
            },
            brightness: 180,
        };
        let text = cfg.to_toml().unwrap();
        let back = Config::from_toml(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn missing_default_profile_rejected() {
        let cfg = Config {
            default_profile: "nope".into(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles: {
                let mut m = BTreeMap::new();
                m.insert("real".to_string(), Profile::new("real"));
                m
            },
            macros: BTreeMap::new(),
        };
        let text = cfg.to_toml().unwrap();
        assert!(Config::from_toml(&text).is_err());
    }

    #[test]
    fn save_load_roundtrip_via_tempfile() {
        let dir = std::env::temp_dir().join(format!("tartarus-cfg-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let cfg = Config::default_single();
        cfg.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(cfg, loaded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let path = Path::new("/nonexistent/tartarus/definitely/not/here.toml");
        let cfg = Config::load(path).unwrap();
        assert_eq!(cfg, Config::default_single());
    }

    /// A hand-written profile that omits lighting, both layers, and linked_app
    /// must still deserialize (serde defaults fill them in).
    #[test]
    fn hand_written_minimal_profile_loads() {
        let src = r#"
            default_profile = "default"

            [profiles.default]
            name = "default"

            [profiles.default.base]
            KEY_A = { type = "passthrough" }
        "#;
        let cfg = Config::from_toml(src).expect("minimal profile should load");
        let p = &cfg.profiles["default"];
        assert!(p.linked_app.is_none());
        assert!(p.hypershift.is_empty());
        // lighting defaulted to spectrum / full brightness
        assert_eq!(p.lighting, LightingState::default());
        assert_eq!(p.base.len(), 1);
    }

    /// The shipped example config (`docs/examples/config.toml`) must parse and
    /// validate against the current schema — it's a user-facing deliverable.
    #[test]
    fn shipped_example_config_parses() {
        // CARGO_MANIFEST_DIR = crates/tartarus-daemon; the example is two up.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/examples/config.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let cfg = Config::from_toml(&text).expect("example config must parse + validate");
        assert!(cfg.profiles.contains_key(&cfg.default_profile));
        assert!(cfg.profiles.contains_key("fps"));
        assert!(cfg.macros.contains_key("gg_wp"));
        // Convention: the default profile ships EMPTY; demo/feature binds live in
        // a separate `test` profile, never in `default`.
        assert_eq!(cfg.default_profile, "default");
        let default = &cfg.profiles["default"];
        assert!(
            default.base.is_empty() && default.hypershift.is_empty(),
            "default profile must ship empty"
        );
        assert!(cfg.profiles.contains_key("test"));
        assert!(
            !cfg.profiles["test"].base.is_empty(),
            "test profile carries the showcase binds"
        );
    }

    /// Invariant: the daemon must always reload a config it wrote, even one with
    /// a dangling reference (which an IPC edit sequence can produce).
    #[test]
    fn config_with_dangling_reference_still_loads() {
        let mut cfg = Config::default_single();
        cfg.profiles.get_mut("default").unwrap().base.insert(
            "KEY_A".into(),
            BindAction::SwitchProfile {
                name: "ghost".into(), // does not exist
            },
        );
        let text = cfg.to_toml().unwrap();
        // Must round-trip: validate() warns but does not reject.
        let back = Config::from_toml(&text).expect("dangling ref must not brick load");
        assert_eq!(cfg, back);
    }

    /// `tray` defaults to `true` and is omitted by an existing (pre-tray) config,
    /// so on-by-default holds; and an explicit `tray = false` round-trips through
    /// TOML (the daemon rewrites the file from the in-memory model on every
    /// mutating IPC request, so a lossy serialize would silently re-enable it).
    #[test]
    fn tray_defaults_to_true_and_roundtrips() {
        // Omitted => default true (a config written before the tray existed).
        let src = r#"
            default_profile = "default"
            [profiles.default]
            name = "default"
        "#;
        let cfg = Config::from_toml(src).expect("config without tray must load");
        assert!(cfg.tray, "tray must default to true when absent");

        // Explicit false survives a serialize/parse round-trip.
        let mut cfg = Config::default_single();
        cfg.tray = false;
        let back = Config::from_toml(&cfg.to_toml().unwrap()).unwrap();
        assert!(!back.tray);
        assert_eq!(cfg, back);
    }

    /// `input_mode` defaults to digital and the `[analog]` table is optional, so
    /// an existing (pre-analog) config still loads unchanged.
    #[test]
    fn input_mode_defaults_to_digital_and_analog_optional() {
        let src = r#"
            default_profile = "default"
            [profiles.default]
            name = "default"
        "#;
        let cfg = Config::from_toml(src).expect("config without analog fields must load");
        assert_eq!(cfg.input_mode, InputMode::Digital);
        assert!(cfg.analog.keys.is_empty());
        assert_eq!(cfg.analog.poll_interval_ms, 50);
    }

    /// A full `[analog]` section + `input_mode = "analog"` parses into the typed
    /// model, including per-key thresholds and a dual-step entry.
    #[test]
    fn analog_section_parses() {
        let src = r#"
            default_profile = "default"
            input_mode = "analog"

            [analog]
            poll_interval_ms = 20

            [analog.keys.5]
            key = "KEY_Q"
            actuation_point = 0x60
            release_point = 0x50

            [analog.keys.6]
            key = "KEY_W"
            actuation_point = 0x40
            release_point = 0x30
            second_actuation_point = 0xC0
            second_action = "KEY_LEFTSHIFT"

            [profiles.default]
            name = "default"
        "#;
        let cfg = Config::from_toml(src).expect("analog config must parse");
        assert_eq!(cfg.input_mode, InputMode::Analog);
        assert_eq!(cfg.analog.poll_interval_ms, 20);

        let k5 = &cfg.analog.keys["5"];
        assert_eq!(k5.key, "KEY_Q");
        assert_eq!(k5.actuation_point, 0x60);
        assert_eq!(k5.release_point, 0x50);
        assert!(k5.second_actuation_point.is_none());

        let k6 = &cfg.analog.keys["6"];
        assert_eq!(k6.second_actuation_point, Some(0xC0));
        assert_eq!(k6.second_action.as_deref(), Some("KEY_LEFTSHIFT"));
    }

    /// An analog key entry may omit the thresholds — they default to the
    /// documented constants — so the minimal form is just `key = "..."`.
    #[test]
    fn analog_key_thresholds_default() {
        let src = r#"
            default_profile = "default"
            input_mode = "analog"
            [analog.keys.0]
            key = "KEY_Z"
            [profiles.default]
            name = "default"
        "#;
        let cfg = Config::from_toml(src).expect("minimal analog key must load");
        let k = &cfg.analog.keys["0"];
        assert_eq!(k.actuation_point, DEFAULT_ACTUATION_POINT);
        assert_eq!(k.release_point, DEFAULT_ACTUATION_POINT - DEFAULT_HYSTERESIS);
    }

    /// The full analog config round-trips through TOML unchanged (the daemon
    /// rewrites the file from the in-memory model on every mutating IPC request,
    /// so a lossy serialize would corrupt the analog map).
    #[test]
    fn analog_config_roundtrips_through_toml() {
        let mut cfg = Config::default_single();
        cfg.input_mode = InputMode::Analog;
        cfg.analog.poll_interval_ms = 33;
        cfg.analog.keys.insert(
            "7".to_string(),
            AnalogKey {
                key: "KEY_E".into(),
                actuation_point: 0x70,
                release_point: 0x55,
                second_actuation_point: Some(0xD0),
                second_action: Some("KEY_LEFTCTRL".into()),
            },
        );
        let text = cfg.to_toml().unwrap();
        let back = Config::from_toml(&text).unwrap();
        assert_eq!(cfg, back);
    }
}

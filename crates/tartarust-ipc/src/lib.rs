//! `tartarust-ipc` — the stable contract between the daemon and its clients.
//!
//! Messages are exchanged as newline-delimited JSON over a Unix domain socket.
//! This crate is intentionally dependency-light (serde only) and transport-free
//! so the UI, CLI, and daemon teams can build against a frozen schema in
//! parallel. Grow the `Request`/`Response` enums additively as milestones land;
//! bump `PROTOCOL_VERSION` on breaking changes.
//!
//! ## Domain types live here
//!
//! The shared domain model — profiles, binds, lighting, macros, device info —
//! is defined in [`model`] and re-exported at the crate root. These are the
//! types the daemon owns at runtime and the UI/CLI render; they are deliberately
//! free of any device/Linux dependency (keys are plain `KEY_*` / `BTN_*`
//! strings, not `evdev::KeyCode`), so the contract crate stays portable. The
//! daemon translates strings to evdev codes at its boundary.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(feature = "bundle")]
pub mod bundle;
pub mod model;

pub use model::{
    BindAction, DeviceInfo, Effect, Layer, LightingState, Macro, MacroStep, PROFILE_EXPORT_FORMAT,
    PROFILE_EXPORT_VERSION, Profile, ProfileExport, Rgb, ThumbAxisMode, WaveDirection,
    referenced_macro_names,
};

/// IPC schema version. Bump on any breaking change to `Request`/`Response`.
pub const PROTOCOL_VERSION: u32 = 0;

/// Default control-socket path, under `$XDG_RUNTIME_DIR` (falls back to `/tmp`).
pub fn default_socket_path() -> PathBuf {
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    runtime.join("tartarust.sock")
}

/// The control-socket path every client resolves through: [`default_socket_path`],
/// overridable via `TARTARUST_SOCKET` (tests / multiple instances). The
/// daemon, CLI, and UI all use this one function so they can never disagree.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("TARTARUST_SOCKET") {
        Some(p) => PathBuf::from(p),
        None => default_socket_path(),
    }
}

/// A client → daemon request.
///
/// Newline-delimited JSON, tagged by `kind`. Variants are additive: clients and
/// daemons of different (but same-`PROTOCOL_VERSION`) builds interoperate, with
/// unknown requests answered by [`Response::Error`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Liveness probe.
    Ping,
    /// Report daemon + connected-device status.
    GetStatus,

    // ---- device info -------------------------------------------------------
    /// Read firmware version, serial, and connection state from the device.
    GetDeviceInfo,

    // ---- profiles ----------------------------------------------------------
    /// List all profiles (names + metadata) and which is active.
    ListProfiles,
    /// Fetch one profile in full (binds, lighting, linked app).
    GetProfile {
        /// Profile name.
        name: String,
    },
    /// Create a new, empty profile.
    CreateProfile {
        /// Name for the new profile (must be unique).
        name: String,
    },
    /// Duplicate an existing profile: a deep copy (binds, lighting, thumb-axis
    /// mode) under a new name. The app link is NOT copied — two profiles linked
    /// to the same app would make auto-switch ambiguous.
    DuplicateProfile {
        /// Profile to copy.
        from: String,
        /// Name for the copy (must be unique).
        to: String,
    },
    /// Rename an existing profile.
    RenameProfile {
        /// Current name.
        from: String,
        /// New name.
        to: String,
    },
    /// Delete a profile. The active/last profile cannot be deleted.
    DeleteProfile {
        /// Profile to delete.
        name: String,
    },
    /// Make a profile active (applies its lighting + binds to the device).
    SwitchProfile {
        /// Profile to activate.
        name: String,
    },
    /// Set (or clear) the application a profile auto-activates for (M4).
    SetProfileLinkedApp {
        /// Profile to edit.
        name: String,
        /// App identifier to link; `None` clears the link.
        linked_app: Option<String>,
    },
    /// Set the profile that is active by default (when no app match applies).
    SetDefaultProfile {
        /// Profile to make the default.
        name: String,
    },
    /// Commit all pending session changes to disk.
    ///
    /// Mutating edits (binds, lighting, brightness, thumb-axis mode, actuation,
    /// profile create/rename/delete, macros, auto-switch) apply LIVE to the
    /// running device but are NOT persisted until this request — so the user can
    /// try changes for the session and decide whether to keep them. This flushes
    /// the daemon's entire in-memory config to the config file (whole-file
    /// persistence) and clears the unsaved-changes flag. `name` is advisory (the
    /// active profile by convention); the write always commits the full config.
    SaveProfile {
        /// The profile the user pressed Save on; `None` = the active profile.
        /// Advisory only — persistence is whole-file.
        name: Option<String>,
    },
    /// Set how a profile's thumb stick quantizes direction (eight/four-way).
    SetThumbAxisMode {
        /// Profile to edit.
        profile: String,
        /// New thumb-axis mode.
        mode: ThumbAxisMode,
    },
    /// Export one profile as a portable bundle: the profile plus the macros its
    /// binds reference (see [`ProfileExport`]). Read-only.
    ExportProfile {
        /// Profile to export.
        name: String,
    },
    /// Import a profile bundle (as produced by [`Request::ExportProfile`]).
    ///
    /// Atomic: on any rejection (name collision without `overwrite`, a
    /// same-name macro with different steps, two conflicting same-name macros
    /// within the bundle, or an unknown bind key name) nothing is changed.
    /// Bundled macros identical to existing ones import silently; bind keys are
    /// validated and canonicalized like `SetBind`; a `linked_app` another
    /// profile already claims is cleared (auto-switch must stay unambiguous).
    /// Like every other edit this applies live and is persisted only on
    /// [`Request::SaveProfile`]. If the imported name is the active profile
    /// (an in-place overwrite), its binds/lighting re-apply immediately.
    ImportProfile {
        /// The profile to add.
        profile: Profile,
        /// Macros bundled with it (normally those its binds reference).
        macros: Vec<Macro>,
        /// Import under this name instead of `profile.name`.
        rename: Option<String>,
        /// Replace an existing same-name profile / conflicting macros instead
        /// of rejecting.
        overwrite: bool,
    },

    // ---- binds -------------------------------------------------------------
    /// Set (or clear) the action bound to one physical key in one layer.
    SetBind {
        /// Profile to edit.
        profile: String,
        /// Which layer (base or Hypershift).
        layer: Layer,
        /// Physical key, by evdev name (`KEY_*`).
        key: String,
        /// New action; `None` removes the bind (falls back to passthrough).
        action: Option<BindAction>,
    },

    // ---- lighting ----------------------------------------------------------
    /// Read the active (or a named) profile's lighting state.
    GetLighting {
        /// Profile to read; `None` = active profile.
        profile: Option<String>,
    },
    /// Set a profile's lighting and apply it to the device if active.
    SetLighting {
        /// Profile to edit; `None` = active profile.
        profile: Option<String>,
        /// The new lighting state.
        lighting: LightingState,
    },
    /// Set the device brightness (0–255), applied immediately.
    SetBrightness {
        /// Brightness level.
        level: u8,
    },

    // ---- macros ------------------------------------------------------------
    /// List all macro names. (Names only — use `GetMacro` for a full body.)
    ListMacros,
    /// Fetch one macro's full body (its ordered steps).
    GetMacro {
        /// Macro name.
        name: String,
    },
    /// Create or replace a named macro.
    SaveMacro {
        /// The macro (its `name` field is the key).
        r#macro: Macro,
    },
    /// Delete a named macro.
    DeleteMacro {
        /// Macro name.
        name: String,
    },

    // ---- auto-switch (M4) --------------------------------------------------
    /// Enable or disable per-application auto-switch.
    SetAutoSwitch {
        /// `true` to enable auto-switching profiles by focused app.
        enabled: bool,
    },

    // ---- analog actuation --------------------------------------------------
    /// Read the live analog state: whether analog input is active, the latest
    /// per-key pressures, and the current per-key actuation config. Drives the
    /// calibration UI's live pressure view + threshold editors.
    GetAnalogState,
    /// Update one analog key's actuation settings. Applies to the running
    /// actuator immediately (live, session-only) and marks the config dirty; like
    /// every other edit it is written to disk only on [`Request::SaveProfile`].
    /// The `index` is the analog byte index (`0..23`); an out-of-range index is
    /// rejected with an error.
    SetActuation {
        /// Analog byte index to configure.
        index: u8,
        /// Pressure at/above which the key registers as DOWN.
        actuation_point: u8,
        /// Pressure below which a held key registers as UP (hysteresis: keep it
        /// under `actuation_point`).
        release_point: u8,
        /// Optional deeper actuation point for a dual-step ("second function")
        /// key. When set, `second_action` must also be set.
        second_actuation_point: Option<u8>,
        /// The deeper step's key, as an evdev `KEY_*` name — NOT a literal action
        /// payload. Like the primary key, the actuator simply emits this key when
        /// pressure crosses `second_actuation_point`; whatever bind is on it
        /// (a plain key, a chord, a macro, …) then resolves through the engine.
        /// Required iff `second_actuation_point` is set.
        second_action: Option<String>,
    },
}

/// A daemon → client response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// Reply to [`Request::GetStatus`].
    Status(DaemonStatus),
    /// Generic success with no payload (mutations: create/rename/delete/set/...).
    Ack,
    /// Reply to [`Request::GetDeviceInfo`].
    DeviceInfo(DeviceInfo),
    /// Reply to [`Request::ListProfiles`].
    Profiles(ProfileList),
    /// Reply to [`Request::GetProfile`].
    Profile(Profile),
    /// Reply to [`Request::ExportProfile`]. Safe as a newtype variant under
    /// internal tagging — `ProfileExport` is a struct (serializes as a map).
    ProfileExport(ProfileExport),
    /// Reply to [`Request::GetLighting`].
    Lighting(LightingState),
    /// Reply to [`Request::ListMacros`].
    ///
    /// A struct variant (not `Macros(Vec<String>)`): under `#[serde(tag = "kind")]`
    /// (internal tagging) a newtype variant wrapping a sequence cannot be
    /// serialized — serde requires each internally-tagged variant to serialize
    /// as a map. Wrapping the list in a named field fixes that.
    Macros {
        /// Macro names.
        names: Vec<String>,
    },
    /// Reply to [`Request::GetMacro`] — a single macro's full body. Safe as a
    /// newtype variant under internal tagging because `Macro` is a struct (it
    /// serializes as a map), unlike the `Macros` names list which had to be a
    /// struct variant.
    Macro(Macro),
    /// Reply to [`Request::GetAnalogState`]. Safe as a newtype variant under
    /// internal tagging — `AnalogState` is a struct (serializes as a map).
    AnalogState(AnalogState),
    /// A request could not be served.
    Error {
        /// Human-readable failure reason.
        message: String,
    },
}

/// Live analog input state for the calibration UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalogState {
    /// Whether the daemon is in analog mode with the source running (DRIVER mode
    /// held). NOT "a frame arrived recently" — the device goes silent when keys
    /// are steady, so an untouched-but-active keypad still reports `true`.
    pub analog_active: bool,
    /// The latest per-key pressures (one byte per analog key, 0..255). The last
    /// frame is held while the device is idle (it only emits on change), so a
    /// held key keeps its pressure here. Empty when not in analog mode.
    pub pressures: Vec<u8>,
    /// The current per-key actuation config, one entry per configured analog key.
    pub keys: Vec<AnalogKeyConfig>,
}

/// One analog key's actuation configuration, as surfaced to clients.
///
/// `index` is the analog byte index this key occupies in a pressure frame. The
/// `key` / `second_action` are evdev names the daemon's engine resolves binds by
/// (the same string form as a [`BindAction::Key`] entry) — analog keys are a
/// drop-in for the grabbed keyboard, so Hypershift/macros/profiles apply on top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalogKeyConfig {
    /// Analog byte index (`0..23`).
    pub index: u8,
    /// The evdev key this index maps to (`"KEY_Q"`).
    pub key: String,
    /// Pressure at/above which the key registers as DOWN.
    pub actuation_point: u8,
    /// Pressure below which a held key registers as UP.
    pub release_point: u8,
    /// Optional deeper second-step actuation point.
    pub second_actuation_point: Option<u8>,
    /// The deeper step's key, as an evdev `KEY_*` name — NOT a literal action
    /// payload. The actuator emits this key at `second_actuation_point` and its
    /// own bind (key/chord/macro) resolves through the engine, exactly like the
    /// primary `key`. Present iff a second step is configured.
    pub second_action: Option<String>,
}

/// Default actuation point: the pressure at/above which an analog key registers
/// DOWN. Lives in the contract crate so the daemon's config defaults and the
/// UI's "reset to default" / custom-value indicator can never disagree.
pub const DEFAULT_ACTUATION_POINT: u8 = 0x60;
/// Default release point: the default actuation point minus the default
/// hysteresis margin (0x10).
pub const DEFAULT_RELEASE_POINT: u8 = 0x50;

impl AnalogKeyConfig {
    /// Whether this key deviates from the default actuation settings — custom
    /// thresholds or a configured dual-step.
    pub fn is_custom(&self) -> bool {
        self.actuation_point != DEFAULT_ACTUATION_POINT
            || self.release_point != DEFAULT_RELEASE_POINT
            || self.second_actuation_point.is_some()
            || self.second_action.is_some()
    }
}

/// Snapshot of daemon + device state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// IPC schema version the daemon speaks.
    pub protocol_version: u32,
    /// Daemon crate version.
    pub daemon_version: String,
    /// `true` when a Tartarus Pro is currently claimed by the daemon.
    pub device_connected: bool,
    /// Name of the currently active profile.
    pub active_profile: String,
    /// Name of the configured default profile (active when no app match applies).
    pub default_profile: String,
    /// Whether per-application auto-switch is currently enabled.
    pub auto_switch: bool,
    /// Whether the daemon holds session changes not yet written to disk. The UI
    /// shows a Save affordance when this is `true` (see [`Request::SaveProfile`]).
    #[serde(default)]
    pub unsaved_changes: bool,
}

/// Result of [`Request::ListProfiles`]: profile summaries + the active one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileList {
    /// One entry per profile, in stable order.
    pub profiles: Vec<ProfileSummary>,
    /// Name of the active profile.
    pub active: String,
}

/// A lightweight profile listing entry (no binds/lighting payload).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSummary {
    /// Profile name.
    pub name: String,
    /// Application this profile auto-activates for (M4; data-only for now).
    pub linked_app: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Ping`/`GetStatus`/`Pong`/`Status`/`Error` must keep their exact wire
    /// form — clients built against the M0 schema depend on it.
    #[test]
    fn frozen_variants_have_stable_wire_form() {
        assert_eq!(
            serde_json::to_string(&Request::Ping).unwrap(),
            r#"{"kind":"ping"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::GetStatus).unwrap(),
            r#"{"kind":"get_status"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Pong).unwrap(),
            r#"{"kind":"pong"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Error {
                message: "x".into()
            })
            .unwrap(),
            r#"{"kind":"error","message":"x"}"#
        );
    }

    /// One sample of EVERY `Request` variant. The exhaustive `match` below makes
    /// this fail to compile if a variant is added without a sample here.
    fn sample_requests() -> Vec<Request> {
        // Compile-time guard: list every variant. If you add a Request variant,
        // add it here AND to the vec below.
        fn _exhaustive(r: &Request) {
            match r {
                Request::Ping
                | Request::GetStatus
                | Request::GetDeviceInfo
                | Request::ListProfiles
                | Request::GetProfile { .. }
                | Request::CreateProfile { .. }
                | Request::DuplicateProfile { .. }
                | Request::RenameProfile { .. }
                | Request::DeleteProfile { .. }
                | Request::SwitchProfile { .. }
                | Request::SetProfileLinkedApp { .. }
                | Request::SetDefaultProfile { .. }
                | Request::SaveProfile { .. }
                | Request::SetThumbAxisMode { .. }
                | Request::ExportProfile { .. }
                | Request::ImportProfile { .. }
                | Request::SetBind { .. }
                | Request::GetLighting { .. }
                | Request::SetLighting { .. }
                | Request::SetBrightness { .. }
                | Request::ListMacros
                | Request::GetMacro { .. }
                | Request::SaveMacro { .. }
                | Request::DeleteMacro { .. }
                | Request::SetAutoSwitch { .. }
                | Request::GetAnalogState
                | Request::SetActuation { .. } => {}
            }
        }
        vec![
            Request::Ping,
            Request::GetStatus,
            Request::GetDeviceInfo,
            Request::ListProfiles,
            Request::GetProfile { name: "fps".into() },
            Request::CreateProfile { name: "fps".into() },
            Request::DuplicateProfile {
                from: "fps".into(),
                to: "fps (copy)".into(),
            },
            Request::RenameProfile {
                from: "a".into(),
                to: "b".into(),
            },
            Request::DeleteProfile { name: "fps".into() },
            Request::SwitchProfile { name: "fps".into() },
            Request::SetProfileLinkedApp {
                name: "fps".into(),
                linked_app: Some("steam".into()),
            },
            Request::SetDefaultProfile { name: "fps".into() },
            Request::SaveProfile {
                name: Some("fps".into()),
            },
            Request::SetThumbAxisMode {
                profile: "fps".into(),
                mode: ThumbAxisMode::Four,
            },
            Request::ExportProfile { name: "fps".into() },
            Request::ImportProfile {
                profile: Profile::new("fps"),
                macros: vec![Macro {
                    name: "m".into(),
                    steps: vec![MacroStep::Delay { ms: 10 }],
                }],
                rename: Some("fps (imported)".into()),
                overwrite: true,
            },
            Request::SetBind {
                profile: "default".into(),
                layer: Layer::Hypershift,
                key: "KEY_A".into(),
                action: Some(BindAction::Key {
                    keys: vec!["KEY_LEFTCTRL".into(), "KEY_C".into()],
                }),
            },
            Request::GetLighting { profile: None },
            Request::SetLighting {
                profile: Some("fps".into()),
                lighting: LightingState::default(),
            },
            Request::SetBrightness { level: 128 },
            Request::ListMacros,
            Request::GetMacro { name: "m".into() },
            Request::SaveMacro {
                r#macro: Macro {
                    name: "m".into(),
                    steps: vec![MacroStep::Press {
                        key: "KEY_A".into(),
                    }],
                },
            },
            Request::DeleteMacro { name: "m".into() },
            Request::SetAutoSwitch { enabled: false },
            Request::GetAnalogState,
            Request::SetActuation {
                index: 5,
                actuation_point: 0x60,
                release_point: 0x50,
                second_actuation_point: Some(0xC0),
                second_action: Some("KEY_LEFTSHIFT".into()),
            },
        ]
    }

    /// One sample of EVERY `Response` variant.
    fn sample_responses() -> Vec<Response> {
        fn _exhaustive(r: &Response) {
            match r {
                Response::Pong
                | Response::Status(_)
                | Response::Ack
                | Response::DeviceInfo(_)
                | Response::Profiles(_)
                | Response::Profile(_)
                | Response::ProfileExport(_)
                | Response::Lighting(_)
                | Response::Macros { .. }
                | Response::Macro(_)
                | Response::AnalogState(_)
                | Response::Error { .. } => {}
            }
        }
        vec![
            Response::Pong,
            Response::Status(DaemonStatus {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "0.0.0".into(),
                device_connected: true,
                active_profile: "default".into(),
                default_profile: "default".into(),
                auto_switch: true,
                unsaved_changes: false,
            }),
            Response::Ack,
            Response::DeviceInfo(DeviceInfo {
                firmware: Some("1.5".into()),
                serial: Some("SN123".into()),
                connected: true,
            }),
            Response::Profiles(ProfileList {
                profiles: vec![ProfileSummary {
                    name: "default".into(),
                    linked_app: Some("brave-browser".into()),
                }],
                active: "default".into(),
            }),
            Response::Profile(Profile::new("default")),
            Response::ProfileExport(ProfileExport::new(
                Profile::new("fps"),
                vec![Macro {
                    name: "m".into(),
                    steps: vec![MacroStep::Press {
                        key: "KEY_A".into(),
                    }],
                }],
            )),
            Response::Lighting(LightingState::default()),
            Response::Macros {
                names: vec!["m1".into(), "m2".into()],
            },
            Response::Macro(Macro {
                name: "m1".into(),
                steps: vec![MacroStep::Press {
                    key: "KEY_A".into(),
                }],
            }),
            Response::AnalogState(AnalogState {
                analog_active: true,
                pressures: vec![0, 0, 0x60, 0, 0xC0],
                keys: vec![AnalogKeyConfig {
                    index: 2,
                    key: "KEY_Q".into(),
                    actuation_point: 0x60,
                    release_point: 0x50,
                    second_actuation_point: Some(0xC0),
                    second_action: Some("KEY_LEFTSHIFT".into()),
                }],
            }),
            Response::Error {
                message: "x".into(),
            },
        ]
    }

    /// Every Request and Response variant must SERIALIZE (the Macros bug was a
    /// serialize-time failure — a newtype variant wrapping a `Vec` is illegal
    /// under `#[serde(tag = "kind")]`), round-trip back equal, and contain no
    /// embedded newline (the wire framing delimiter).
    #[test]
    fn every_variant_serializes_roundtrips_and_is_newline_free() {
        for req in sample_requests() {
            let s = serde_json::to_string(&req)
                .unwrap_or_else(|e| panic!("Request {req:?} failed to serialize: {e}"));
            assert!(
                !s.contains('\n'),
                "Request {req:?} serialized with a newline"
            );
            let back: Request = serde_json::from_str(&s).unwrap();
            assert_eq!(req, back, "Request roundtrip mismatch: {s}");
        }
        for resp in sample_responses() {
            let s = serde_json::to_string(&resp)
                .unwrap_or_else(|e| panic!("Response {resp:?} failed to serialize: {e}"));
            assert!(
                !s.contains('\n'),
                "Response {resp:?} serialized with a newline"
            );
            let back: Response = serde_json::from_str(&s).unwrap();
            assert_eq!(resp, back, "Response roundtrip mismatch: {s}");
        }
    }

    /// Explicit regression for the fixed bug: `Response::Macros` must serialize.
    #[test]
    fn macros_response_serializes() {
        let r = Response::Macros {
            names: vec!["a".into(), "b".into()],
        };
        let s = serde_json::to_string(&r).expect("Macros must serialize");
        assert_eq!(s, r#"{"kind":"macros","names":["a","b"]}"#);
        assert_eq!(serde_json::from_str::<Response>(&s).unwrap(), r);
    }
}

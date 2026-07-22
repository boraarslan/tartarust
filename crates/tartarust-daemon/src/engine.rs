//! The engine: the single state-authority actor.
//!
//! One `Engine` lives on one thread and is the sole owner of all mutable daemon
//! state — the profiles, the active profile, the macros, and the live key
//! press/refcount bookkeeping. Two entry points mutate it, both on that thread
//! so no lock is needed:
//!
//!   * [`Engine::handle_key`] — a physical key event from the grabbed device,
//!     producing output events + side-effect [`Command`]s.
//!   * [`Engine::apply`] — an IPC [`Request`], producing a [`Response`] and any
//!     [`DeviceCmd`]s the daemon must forward to the device thread.
//!
//! The config authority is the shared `tartarust-ipc` model directly (profiles
//! keyed by evdev name strings); there is no second internal representation to
//! keep in sync. Per incoming event we convert `KeyCode -> name` for the lookup
//! and parse `name -> KeyCode` when emitting.
//!
//! Correctness rules (the "no stuck keys" guarantees):
//!
//!   * **Resolve on press, replay the inverse on release.** A press resolves its
//!     action once and records exactly which output keys it pressed; the release
//!     replays those, never re-resolving — so switching profile or toggling
//!     Hypershift mid-hold can't strand a key in the wrong layer.
//!   * **Refcount emitted keycodes.** Two physical keys both pressing
//!     `KEY_LEFTCTRL` only release it once the last holder lets go.

use std::collections::{BTreeMap, HashMap};

use evdev::{InputEvent, KeyCode, KeyEvent};

use tartarust_ipc::model::{BindAction, Layer, LightingState, Macro, Profile, ThumbAxisMode};
use tartarust_ipc::{
    DaemonStatus, DeviceInfo, PROTOCOL_VERSION, ProfileList, ProfileSummary, Request, Response,
};

use crate::device::DeviceCmd;
use crate::state::{AnalogConfig, AnalogKey, Config, InputMode};

/// A side effect the engine cannot perform itself (no device / process access).
/// Produced by [`Engine::handle_key`]; the daemon executes these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Play the named macro once.
    RunMacro(String),
    /// Apply this profile's lighting to the device (after a profile switch).
    DeviceCmd(DeviceCmd),
    /// Launch this program (argv; first element is the executable).
    LaunchProgram(Vec<String>),
}

/// The result of feeding one physical event into the engine.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Input events to emit through the virtual device, in order. uinput
    /// appends a trailing `SYN_REPORT`, so callers must NOT add one.
    pub events: Vec<InputEvent>,
    /// Higher-level side effects for the daemon to run.
    pub commands: Vec<Command>,
}

fn key_event(code: KeyCode, down: bool) -> InputEvent {
    *KeyEvent::new(code, i32::from(down))
}

/// Which output keycodes a single physical-key press is currently holding.
type HeldOutput = Vec<KeyCode>;

/// The stateful engine / state authority.
pub struct Engine {
    /// All profiles, keyed by name (the config authority).
    profiles: std::collections::BTreeMap<String, Profile>,
    /// All macros, keyed by name.
    macros: std::collections::BTreeMap<String, Macro>,
    /// The profile to activate at startup — persisted as `default_profile`. This
    /// is stable across runtime switches (manual or auto); `active_profile` is
    /// the live one. Keeping them separate stops auto-switch / manual switches
    /// from churning the on-disk startup default.
    configured_default: String,
    /// Active profile name (live; not necessarily the configured default).
    active_profile: String,
    /// Whether per-application auto-switch is enabled (persisted).
    auto_switch: bool,
    /// Whether the daemon shows a system-tray icon. Pass-through state the engine
    /// never mutates — carried here ONLY so [`Engine::config_snapshot`] writes it
    /// back faithfully (same rationale as `input_mode`/`analog`). Without it,
    /// persisting after any mutating IPC request would silently reset a user's
    /// `tray = false` to the default `true`.
    tray: bool,
    /// The configured input path. Pass-through state the engine never mutates —
    /// it is carried here ONLY so [`Engine::config_snapshot`] can write it back
    /// faithfully. Without this, persisting after any mutating IPC request would
    /// silently reset the file to `Digital` and drop the analog table.
    input_mode: InputMode,
    /// Analog actuation settings. Pass-through state (same rationale as
    /// `input_mode`): never mutated by the engine, preserved across snapshots.
    analog: AnalogConfig,
    /// The config as of the last successful disk write (startup load, then
    /// refreshed by [`Engine::mark_saved`] after each persist). Mutating IPC
    /// requests apply LIVE (device commands fire, the analog actuator updates)
    /// but the daemon does NOT persist them; only an explicit
    /// [`Request::SaveProfile`] flushes the config. The unsaved-changes state is
    /// DERIVED by diffing the live config against this snapshot — not a sticky
    /// flag — so an edit that manually reverts state back to the saved value
    /// reads as clean again (no phantom Save affordance).
    saved: Config,

    // ---- live state (orthogonal to config; never touched by edits) ----
    /// Physical keys currently holding the Hypershift layer. The layer is active
    /// while this is non-empty. A set (not a bool) so out-of-order release of two
    /// Hypershift-bound keys behaves correctly. The trigger is resolved on press
    /// and recorded here; release just removes the holder — it never re-resolves,
    /// so a profile switch mid-hold can't strand the layer active.
    hypershift_held_by: std::collections::HashSet<KeyCode>,
    held: HashMap<KeyCode, HeldOutput>,
    output_refcount: HashMap<KeyCode, u32>,

    // ---- device-info cache (filled by the daemon from the device thread) ----
    device_info: DeviceInfo,

    // ---- analog IPC handles (set by the daemon in analog mode only) ----
    /// Live pressure/active snapshot the analog source writes; read by
    /// `GetAnalogState`. `None` in digital mode → `analog_active:false`, no
    /// pressures.
    analog_snapshot: Option<std::sync::Arc<crate::analog_source::AnalogSnapshot>>,
    /// Channel pushing live `SetActuation` edits to the running actuator. `None`
    /// in digital mode → `SetActuation` persists config only (no live source).
    analog_update_tx: Option<std::sync::mpsc::Sender<crate::analog_source::ActuationUpdate>>,
}

impl Engine {
    /// Build an engine from a loaded [`Config`], starting on its default profile.
    pub fn new(config: Config) -> Self {
        let saved = config.clone();
        Engine {
            active_profile: config.default_profile.clone(),
            configured_default: config.default_profile,
            auto_switch: config.auto_switch,
            tray: config.tray,
            input_mode: config.input_mode,
            analog: config.analog,
            saved,
            profiles: config.profiles,
            macros: config.macros,
            hypershift_held_by: std::collections::HashSet::new(),
            held: HashMap::new(),
            output_refcount: HashMap::new(),
            device_info: DeviceInfo {
                firmware: None,
                serial: None,
                connected: false,
            },
            analog_snapshot: None,
            analog_update_tx: None,
        }
    }

    /// Wire the analog IPC handles. Called by the daemon (before the engine
    /// thread starts) ONLY in analog mode, so `GetAnalogState` can read live
    /// pressures and `SetActuation` can reach the running actuator. In digital
    /// mode these stay `None`.
    pub fn set_analog_ipc(
        &mut self,
        snapshot: std::sync::Arc<crate::analog_source::AnalogSnapshot>,
        update_tx: std::sync::mpsc::Sender<crate::analog_source::ActuationUpdate>,
    ) {
        self.analog_snapshot = Some(snapshot);
        self.analog_update_tx = Some(update_tx);
    }

    /// The active profile's name.
    pub fn active_profile(&self) -> &str {
        &self.active_profile
    }

    /// The active profile's thumbstick axis mode (`Eight` if the profile is
    /// somehow missing). Read fresh on every press so a runtime `SetThumbAxisMode`
    /// (or profile switch) takes effect immediately.
    pub fn active_profile_thumb_axis_mode(&self) -> ThumbAxisMode {
        self.profiles
            .get(&self.active_profile)
            .map(|p| p.thumb_axis_mode)
            .unwrap_or_default()
    }

    /// Whether Hypershift is currently held (any trigger key down).
    pub fn hypershift_active(&self) -> bool {
        !self.hypershift_held_by.is_empty()
    }

    /// The macro steps for `name`, if defined (used by the daemon to play it).
    pub fn macro_steps(&self, name: &str) -> Option<&Macro> {
        self.macros.get(name)
    }

    /// Update the cached device info (called when the device thread reports it).
    pub fn set_device_info(&mut self, info: DeviceInfo) {
        self.device_info = info;
    }

    /// Snapshot the full config (for persistence by the daemon loop). Uses the
    /// *configured* default — NOT the live `active_profile` — so runtime switches
    /// (manual or auto) don't churn the on-disk startup profile.
    pub fn config_snapshot(&self) -> Config {
        Config {
            default_profile: self.configured_default.clone(),
            auto_switch: self.auto_switch,
            tray: self.tray,
            input_mode: self.input_mode,
            analog: self.analog.clone(),
            profiles: self.profiles.clone(),
            macros: self.macros.clone(),
        }
    }

    /// Record the current config as the on-disk state (the daemon loop calls
    /// this after a successful persist).
    pub fn mark_saved(&mut self) {
        self.saved = self.config_snapshot();
    }

    /// Whether the in-memory config differs from the last-saved one. A live
    /// field-by-field diff (no snapshot clone), matching [`Engine::config_snapshot`]
    /// field for field.
    pub fn is_dirty(&self) -> bool {
        self.configured_default != self.saved.default_profile
            || self.auto_switch != self.saved.auto_switch
            || self.tray != self.saved.tray
            || self.input_mode != self.saved.input_mode
            || self.analog != self.saved.analog
            || self.profiles != self.saved.profiles
            || self.macros != self.saved.macros
    }

    /// The active profile's lighting as a [`DeviceCmd`] to apply it.
    pub fn active_lighting_cmd(&self) -> DeviceCmd {
        let lighting = self
            .profiles
            .get(&self.active_profile)
            .map(|p| p.lighting.clone())
            .unwrap_or_default();
        DeviceCmd::ApplyLighting(lighting)
    }

    // ===================== physical input path ==========================

    /// Feed one physical key event. `value`: 1 = down, 0 = up, 2 = autorepeat.
    pub fn handle_key(&mut self, code: KeyCode, value: i32) -> Outcome {
        match value {
            1 => self.on_press(code),
            0 => self.on_release(code),
            // Autorepeat: swallow. Never re-press a chord or re-fire a macro.
            _ => Outcome::default(),
        }
    }

    fn on_press(&mut self, code: KeyCode) -> Outcome {
        if self.held.contains_key(&code) {
            return Outcome::default(); // already held; ignore (defensive)
        }

        let mut outcome = Outcome::default();

        // Four-way thumbstick mode: a diagonal physical key has no axis of its
        // own, so synthesize the two adjacent cardinals' actions instead of
        // resolving the diagonal's own bind. Each cardinal resolves against the
        // active profile and refcounts its outputs through `apply_press_action`,
        // so a diagonal and a real cardinal pressing the same output hold it
        // jointly (Synapse's behavior). Eight-way mode skips this and falls
        // through to the normal single-key path.
        //
        // Known limitation: a cardinal here records its action under the
        // cardinal's KeyCode, but `held`/`on_release` key off the diagonal's
        // code. So binding a thumb cardinal (KEY_UP/DOWN/LEFT/RIGHT) to
        // Hypershift would strand the layer when reached via a diagonal. That's
        // an implausible binding (Hypershift on a movement key) and outside the
        // contract's cardinal-expansion intent, so it's accepted, not handled.
        if self.active_profile_thumb_axis_mode() == ThumbAxisMode::Four {
            let name = key_name(code);
            if let Some([a, b]) = tartarust_ipc::model::thumb_diagonal_cardinals(&name) {
                let mut pressed = HeldOutput::new();
                for cardinal in [a, b] {
                    if let Some(kc) = parse_key(cardinal) {
                        let act = self.resolve(kc);
                        self.apply_press_action(act, kc, &mut outcome, &mut pressed);
                    }
                }
                self.held.insert(code, pressed);
                return outcome;
            }
        }

        // Resolve against the layer active *now* (before this press changes it).
        let action = self.resolve(code);
        let mut pressed: HeldOutput = Vec::new();
        self.apply_press_action(action, code, &mut outcome, &mut pressed);

        self.held.insert(code, pressed);
        outcome
    }

    /// Apply one resolved bind action for a single physical key: emit/refcount
    /// any output keys (recording them into `pressed` for the release path),
    /// activate Hypershift, or queue side-effect commands. `original_code` is
    /// the physical key, emitted verbatim on Passthrough / no-bind.
    ///
    /// Shared by the normal single-key press path and the four-way diagonal
    /// expansion (where it's called once per adjacent cardinal). Behavior for
    /// the single-key path is identical to the previous inline match.
    fn apply_press_action(
        &mut self,
        action: Option<BindAction>,
        original_code: KeyCode,
        outcome: &mut Outcome,
        pressed: &mut HeldOutput,
    ) {
        match action {
            Some(BindAction::Hypershift) => {
                // Activate the secondary layer while this key is held. Record it
                // as a holder; emit nothing. The empty `held` record means the
                // release path is a no-op for output but still runs the
                // holder-removal below.
                self.hypershift_held_by.insert(original_code);
            }
            Some(BindAction::Key { keys }) => {
                for name in &keys {
                    if let Some(kc) = parse_key(name) {
                        self.press_output(kc, &mut outcome.events);
                        pressed.push(kc);
                    } else {
                        tracing::warn!(%name, "bind references unknown key name; skipping");
                    }
                }
            }
            Some(BindAction::MouseButton { button }) => {
                if let Some(kc) = parse_key(&button) {
                    self.press_output(kc, &mut outcome.events);
                    pressed.push(kc);
                } else {
                    tracing::warn!(%button, "bind references unknown button; skipping");
                }
            }
            Some(BindAction::Macro { name }) => {
                outcome.commands.push(Command::RunMacro(name));
            }
            Some(BindAction::SwitchProfile { name }) => {
                for cmd in self.activate_profile(&name) {
                    outcome.commands.push(Command::DeviceCmd(cmd));
                }
            }
            Some(BindAction::LaunchProgram { command }) => {
                outcome.commands.push(Command::LaunchProgram(command));
            }
            Some(BindAction::Disable) => {
                // Consume entirely.
            }
            // Passthrough, or no bind at all: emit the original key.
            Some(BindAction::Passthrough) | None => {
                self.press_output(original_code, &mut outcome.events);
                pressed.push(original_code);
            }
        }
    }

    fn on_release(&mut self, code: KeyCode) -> Outcome {
        // If this key was holding the Hypershift layer, drop it — never
        // re-resolve (the active profile may have changed since the press, e.g.
        // via a SwitchProfile bound in the hypershift layer).
        self.hypershift_held_by.remove(&code);

        let mut outcome = Outcome::default();
        if let Some(pressed) = self.held.remove(&code) {
            for out_code in pressed.into_iter().rev() {
                self.release_output(out_code, &mut outcome.events);
            }
        }
        outcome
    }

    /// Release EVERY output key currently held and clear all live key state.
    /// Called when a reader thread exits (device unplugged/revoked) so a key
    /// that was held at that moment doesn't stay stuck-down on the virtual
    /// device. Emits exactly one key-up per still-held output code (the refcount
    /// is irrelevant here — we are tearing down all physical state).
    pub fn release_all(&mut self) -> Outcome {
        let mut outcome = Outcome::default();
        // One key-up per output code that has any holders.
        let held_outputs: Vec<KeyCode> = self.output_refcount.keys().copied().collect();
        for code in held_outputs {
            outcome.events.push(key_event(code, false));
        }
        self.output_refcount.clear();
        self.held.clear();
        self.hypershift_held_by.clear();
        outcome
    }

    /// Resolve the action for `code` in the layer active right now. `None` means
    /// "no bind" (caller treats it as passthrough).
    fn resolve(&self, code: KeyCode) -> Option<BindAction> {
        let profile = self.profiles.get(&self.active_profile)?;
        let name = key_name(code);
        if self.hypershift_active()
            && let Some(action) = profile.hypershift.get(&name)
        {
            return Some(action.clone());
        }
        profile.base.get(&name).cloned()
    }

    fn press_output(&mut self, code: KeyCode, events: &mut Vec<InputEvent>) {
        let count = self.output_refcount.entry(code).or_insert(0);
        if *count == 0 {
            events.push(key_event(code, true));
        }
        *count += 1;
    }

    fn release_output(&mut self, code: KeyCode, events: &mut Vec<InputEvent>) {
        if let Some(count) = self.output_refcount.get_mut(&code) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                events.push(key_event(code, false));
                self.output_refcount.remove(&code);
            }
        }
    }

    /// Switch the active profile. Returns the device commands to apply the new
    /// profile's lighting. This is the single path that a bound `SwitchProfile`
    /// key, `Request::SwitchProfile`, AND auto-switch all route through, so they
    /// never diverge.
    ///
    /// Switching to the already-active profile is a no-op (returns no commands)
    /// — important under auto-switch, where the focus stream emits on every
    /// window change and would otherwise spam the device thread with redundant
    /// lighting re-applies.
    fn activate_profile(&mut self, name: &str) -> Vec<DeviceCmd> {
        if !self.profiles.contains_key(name) || name == self.active_profile {
            return Vec::new();
        }
        self.active_profile = name.to_string();
        vec![self.active_lighting_cmd()]
    }

    // ===================== auto-switch (M4) ==========================

    /// Whether per-application auto-switch is enabled.
    pub fn auto_switch_enabled(&self) -> bool {
        self.auto_switch
    }

    /// The profile linked to a window class, if any. First profile (BTreeMap
    /// order) whose `linked_app` matches `class` exactly wins. Pure — the
    /// matching test surface for auto-switch. An empty class matches nothing.
    pub fn profile_for_app(&self, class: &str) -> Option<String> {
        if class.is_empty() {
            return None;
        }
        self.profiles
            .values()
            .find(|p| p.linked_app.as_deref() == Some(class))
            .map(|p| p.name.clone())
    }

    /// Handle a focused-application change (from the window-focus watcher).
    /// If auto-switch is on and the focused app's class is linked to a profile,
    /// activate it. Focusing an unlinked app does nothing (manual switches stay
    /// sticky until a linked app is focused). Returns lighting device commands.
    ///
    /// Note: this is deliberately NOT persisted by the daemon — auto-switch must
    /// not churn the on-disk startup profile with whatever was last focused.
    pub fn on_focused_app(&mut self, class: &str) -> Vec<DeviceCmd> {
        if !self.auto_switch {
            return Vec::new();
        }
        match self.profile_for_app(class) {
            Some(target) => {
                let switched = target != self.active_profile;
                let cmds = self.activate_profile(&target);
                if switched {
                    tracing::info!(%class, profile = %target, "auto-switched profile for focused app");
                }
                cmds
            }
            None => Vec::new(),
        }
    }

    // ===================== IPC path ==========================

    /// Apply an IPC request to the state. Returns the response plus any device
    /// commands the daemon must forward to the device thread.
    ///
    /// This is intentionally I/O-free and disk-free: mutating requests update
    /// in-memory state and return device commands, but the daemon loop only
    /// writes the config to disk on an explicit `SaveProfile` (it marks the
    /// engine dirty otherwise). Pure device reads (`GetDeviceInfo`) are routed
    /// straight to the device thread without calling `apply`.
    pub fn apply(&mut self, request: Request, daemon_version: &str) -> (Response, Vec<DeviceCmd>) {
        match request {
            Request::Ping => (Response::Pong, vec![]),
            Request::GetStatus => (
                Response::Status(DaemonStatus {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_version: daemon_version.to_string(),
                    device_connected: self.device_info.connected,
                    active_profile: self.active_profile.clone(),
                    default_profile: self.configured_default.clone(),
                    auto_switch: self.auto_switch,
                    unsaved_changes: self.is_dirty(),
                }),
                vec![],
            ),
            Request::GetDeviceInfo => {
                // Answer from the cache. The daemon loop refreshes this cache
                // with a live device probe (a fresh `query_info` -> `set_device_info`)
                // immediately before routing a `GetDeviceInfo` here, so the cache
                // is current rather than stuck at the startup value.
                (Response::DeviceInfo(self.device_info.clone()), vec![])
            }
            Request::ListProfiles => {
                let profiles = self
                    .profiles
                    .values()
                    .map(|p| ProfileSummary {
                        name: p.name.clone(),
                        linked_app: p.linked_app.clone(),
                    })
                    .collect();
                (
                    Response::Profiles(ProfileList {
                        profiles,
                        active: self.active_profile.clone(),
                    }),
                    vec![],
                )
            }
            Request::GetProfile { name } => match self.profiles.get(&name) {
                Some(p) => (Response::Profile(p.clone()), vec![]),
                None => (err(format!("no such profile: {name}")), vec![]),
            },
            Request::CreateProfile { name } => self.create_profile(name),
            Request::DuplicateProfile { from, to } => self.duplicate_profile(from, to),
            Request::RenameProfile { from, to } => self.rename_profile(from, to),
            Request::DeleteProfile { name } => self.delete_profile(name),
            Request::SwitchProfile { name } => self.switch_profile(name),
            Request::SetProfileLinkedApp { name, linked_app } => {
                self.set_profile_linked_app(name, linked_app)
            }
            Request::SetDefaultProfile { name } => self.set_default_profile(name),
            // SaveProfile is the explicit "commit session to disk" trigger. The
            // engine just acks; the daemon loop does the actual atomic write,
            // clears `dirty` only on a successful write, and REPLACES this Ack with
            // an Error if the write fails (so the client learns the outcome — see
            // `pipeline::spawn_engine`).
            Request::SaveProfile { .. } => (Response::Ack, vec![]),
            Request::SetThumbAxisMode { profile, mode } => self.set_thumb_axis_mode(profile, mode),
            Request::ExportProfile { name } => self.export_profile(name),
            Request::ImportProfile {
                profile,
                macros,
                rename,
                overwrite,
            } => self.import_profile(profile, macros, rename, overwrite),
            Request::SetBind {
                profile,
                layer,
                key,
                action,
            } => self.set_bind(profile, layer, key, action),
            Request::GetLighting { profile } => self.get_lighting(profile),
            Request::SetLighting { profile, lighting } => self.set_lighting(profile, lighting),
            Request::SetBrightness { level } => self.set_brightness(level),
            Request::ListMacros => (
                Response::Macros {
                    names: self.macros.keys().cloned().collect(),
                },
                vec![],
            ),
            Request::GetMacro { name } => match self.macros.get(&name) {
                Some(m) => (Response::Macro(m.clone()), vec![]),
                None => (err(format!("no such macro: {name}")), vec![]),
            },
            Request::SaveMacro { r#macro } => {
                self.macros.insert(r#macro.name.clone(), r#macro);
                (Response::Ack, vec![])
            }
            Request::DeleteMacro { name } => {
                if self.macros.remove(&name).is_some() {
                    (Response::Ack, vec![])
                } else {
                    (err(format!("no such macro: {name}")), vec![])
                }
            }
            Request::SetAutoSwitch { enabled } => {
                self.auto_switch = enabled;
                (Response::Ack, vec![])
            }
            Request::GetAnalogState => (self.analog_state(), vec![]),
            Request::SetActuation {
                index,
                actuation_point,
                release_point,
                second_actuation_point,
                second_action,
            } => self.set_actuation(
                index,
                actuation_point,
                release_point,
                second_actuation_point,
                second_action,
            ),
        }
    }

    /// Build the live analog state for `GetAnalogState`: the source's active flag
    /// plus latest pressures (from the shared snapshot, if in analog mode), and
    /// the current per-key actuation config (always from `self.analog`).
    fn analog_state(&self) -> Response {
        let (analog_active, pressures) = match &self.analog_snapshot {
            Some(snap) if snap.is_active() => (true, snap.pressures()),
            // No snapshot (digital mode) or source not running: inactive, and the
            // pressures vec is empty per the IPC contract.
            _ => (false, Vec::new()),
        };
        let keys = self
            .analog
            .keys
            .iter()
            .filter_map(|(index_str, k)| {
                // The on-disk map is keyed by the byte index as a string; surface
                // a numeric index, skipping any malformed key (same tolerance as
                // the actuator build).
                let index = index_str.parse::<u8>().ok()?;
                Some(tartarust_ipc::AnalogKeyConfig {
                    index,
                    key: k.key.clone(),
                    actuation_point: k.actuation_point,
                    release_point: k.release_point,
                    second_actuation_point: k.second_actuation_point,
                    second_action: k.second_action.clone(),
                })
            })
            .collect();
        Response::AnalogState(tartarust_ipc::AnalogState {
            analog_active,
            pressures,
            keys,
        })
    }

    /// Apply a `SetActuation` edit. Validates the byte index and the key names,
    /// updates the in-memory `self.analog` map (so a later `SaveProfile` writes it
    /// via `config_snapshot`), AND — when an analog source is running — pushes the
    /// edit to the live actuator over the update channel. In digital mode the
    /// in-memory update is the only effect (it takes effect next time analog mode
    /// runs, once saved).
    fn set_actuation(
        &mut self,
        index: u8,
        actuation_point: u8,
        release_point: u8,
        second_actuation_point: Option<u8>,
        second_action: Option<String>,
    ) -> (Response, Vec<DeviceCmd>) {
        if (index as usize) >= tartarust_protocol::ANALOG_KEY_COUNT {
            return (
                err(format!(
                    "analog index {index} out of range (0..{})",
                    tartarust_protocol::ANALOG_KEY_COUNT
                )),
                vec![],
            );
        }
        // SetActuation edits an EXISTING analog key's thresholds; the primary key
        // identity comes from the current config (the UI edits a mapped key, it
        // does not introduce a new mapping — that's a config-file concern until
        // the byte->key map is finalized on hardware).
        let key_name = match self.analog.keys.get(&index.to_string()) {
            Some(existing) => existing.key.clone(),
            None => {
                return (
                    err(format!(
                        "analog index {index} is not configured; map it in the config first"
                    )),
                    vec![],
                );
            }
        };
        // second_actuation_point and second_action are all-or-nothing.
        if second_actuation_point.is_some() != second_action.is_some() {
            return (
                err("second_actuation_point and second_action must be set together".into()),
                vec![],
            );
        }
        // Validate the second_action evdev name at the boundary (like SetBind),
        // so a typo is a clear error not a silently-dropped second step.
        if let Some(name) = &second_action
            && parse_key(name).is_none()
        {
            return (err(format!("unknown second_action key: {name}")), vec![]);
        }

        let new_key = AnalogKey {
            key: key_name,
            actuation_point,
            release_point,
            second_actuation_point,
            second_action,
        };
        // Persist into the config map (string-keyed by byte index).
        self.analog.keys.insert(index.to_string(), new_key.clone());
        // Push to the live actuator if a source is running. The source forwards
        // any orphaned-held-key releases itself; nothing for the engine to emit.
        if let Some(tx) = &self.analog_update_tx {
            let _ = tx.send(crate::analog_source::ActuationUpdate {
                index: index as usize,
                key: new_key,
            });
        }
        (Response::Ack, vec![])
    }

    fn create_profile(&mut self, name: String) -> (Response, Vec<DeviceCmd>) {
        if self.profiles.contains_key(&name) {
            return (err(format!("profile already exists: {name}")), vec![]);
        }
        self.profiles.insert(name.clone(), Profile::new(name));
        (Response::Ack, vec![])
    }

    /// Deep-copy an existing profile (binds, lighting, thumb-axis mode) under a
    /// new name. The app link is deliberately NOT copied: two profiles linked to
    /// the same app would make auto-switch ambiguous.
    fn duplicate_profile(&mut self, from: String, to: String) -> (Response, Vec<DeviceCmd>) {
        let Some(src) = self.profiles.get(&from) else {
            return (err(format!("no such profile: {from}")), vec![]);
        };
        if self.profiles.contains_key(&to) {
            return (err(format!("profile already exists: {to}")), vec![]);
        }
        let mut copy = src.clone();
        copy.name = to.clone();
        copy.linked_app = None;
        self.profiles.insert(to, copy);
        (Response::Ack, vec![])
    }

    /// Build a portable [`ProfileExport`] bundle: the profile plus the macros
    /// its binds reference (only those that exist — a dangling macro bind is
    /// exported as-is, matching how it behaves here).
    fn export_profile(&self, name: String) -> (Response, Vec<DeviceCmd>) {
        let Some(profile) = self.profiles.get(&name) else {
            return (err(format!("no such profile: {name}")), vec![]);
        };
        let macros = tartarust_ipc::referenced_macro_names(profile)
            .iter()
            .filter_map(|n| {
                // The map key is what binds reference and what runtime lookup
                // uses; force it onto the bundled macro so a hand-edited config
                // with a mismatched inline `name` can't export a dangling ref.
                self.macros.get(n).cloned().map(|mut m| {
                    m.name = n.clone();
                    m
                })
            })
            .collect();
        (
            Response::ProfileExport(tartarust_ipc::ProfileExport::new(profile.clone(), macros)),
            vec![],
        )
    }

    /// Import a profile bundle. Atomic: every rejection happens before any
    /// state changes. Bundled macros identical to existing ones are a no-op;
    /// a same-name macro with different steps is a conflict unless `overwrite`
    /// (two conflicting same-name macros WITHIN the bundle are malformed and
    /// always rejected). Bind keys get the same validate-and-canonicalize
    /// treatment as [`Engine::set_bind`] — an unknown key name is a clear error,
    /// not a silently dead bind. A `linked_app` another profile already claims
    /// is cleared (the same ambiguity guard as `duplicate_profile`). Overwriting
    /// the ACTIVE profile re-applies its lighting so the change is live.
    fn import_profile(
        &mut self,
        mut profile: Profile,
        macros: Vec<Macro>,
        rename: Option<String>,
        overwrite: bool,
    ) -> (Response, Vec<DeviceCmd>) {
        let target = rename.unwrap_or_else(|| profile.name.clone());
        if target.is_empty() {
            return (err("imported profile has an empty name".into()), vec![]);
        }
        // Same-name-different-steps WITHIN the bundle: malformed regardless of
        // `overwrite` — last-one-wins would silently drop a definition.
        {
            let mut seen: BTreeMap<&str, &Macro> = BTreeMap::new();
            for m in &macros {
                if let Some(prev) = seen.insert(m.name.as_str(), m)
                    && *prev != *m
                {
                    return (
                        err(format!(
                            "bundle contains two macros named {} with different steps",
                            m.name
                        )),
                        vec![],
                    );
                }
            }
        }
        if !overwrite {
            if self.profiles.contains_key(&target) {
                return (
                    err(format!(
                        "profile already exists: {target} (re-import with overwrite, or rename)"
                    )),
                    vec![],
                );
            }
            if let Some(conflict) = macros
                .iter()
                .find(|m| self.macros.get(&m.name).is_some_and(|have| *have != **m))
            {
                return (
                    err(format!(
                        "macro already exists with different steps: {} (re-import with overwrite)",
                        conflict.name
                    )),
                    vec![],
                );
            }
        }
        // Validate + canonicalize the bind keys of both layers, mirroring
        // `set_bind`'s boundary rule: reject unknown names, store under the
        // `key_name(code)` spelling `resolve` looks up by.
        for layer in [Layer::Base, Layer::Hypershift] {
            let mut canonical = BTreeMap::new();
            for (key, action) in std::mem::take(profile.layer_mut(layer)) {
                let Some(code) = parse_key(&key) else {
                    return (err(format!("unknown key name in bundle: {key}")), vec![]);
                };
                canonical.insert(key_name(code), action);
            }
            *profile.layer_mut(layer) = canonical;
        }
        for m in macros {
            self.macros.insert(m.name.clone(), m);
        }
        // An app link another profile already claims would make auto-switch
        // ambiguous (first match wins) — clear it, like `duplicate_profile`.
        if let Some(app) = profile.linked_app.clone()
            && self
                .profiles
                .values()
                .any(|p| p.name != target && p.linked_app.as_deref() == Some(app.as_str()))
        {
            profile.linked_app = None;
        }
        profile.name = target.clone();
        self.profiles.insert(target.clone(), profile);
        if self.active_profile == target {
            // In-place overwrite of the ACTIVE profile. Binds resolve fresh on
            // every key event, but lighting is push-on-change — re-apply it.
            // (Not `activate_profile`: that no-ops when the name is already
            // active, which is exactly this case.)
            return (Response::Ack, vec![self.active_lighting_cmd()]);
        }
        (Response::Ack, vec![])
    }

    fn rename_profile(&mut self, from: String, to: String) -> (Response, Vec<DeviceCmd>) {
        if !self.profiles.contains_key(&from) {
            return (err(format!("no such profile: {from}")), vec![]);
        }
        if self.profiles.contains_key(&to) {
            return (err(format!("profile already exists: {to}")), vec![]);
        }
        let mut profile = self.profiles.remove(&from).expect("checked above");
        profile.name = to.clone();
        self.profiles.insert(to.clone(), profile);
        if self.active_profile == from {
            self.active_profile = to.clone();
        }
        if self.configured_default == from {
            self.configured_default = to;
        }
        (Response::Ack, vec![])
    }

    fn delete_profile(&mut self, name: String) -> (Response, Vec<DeviceCmd>) {
        if !self.profiles.contains_key(&name) {
            return (err(format!("no such profile: {name}")), vec![]);
        }
        if self.active_profile == name {
            return (err("cannot delete the active profile".into()), vec![]);
        }
        if self.configured_default == name {
            return (
                err("cannot delete the startup (default) profile".into()),
                vec![],
            );
        }
        if self.profiles.len() == 1 {
            return (err("cannot delete the last profile".into()), vec![]);
        }
        self.profiles.remove(&name);
        (Response::Ack, vec![])
    }

    fn switch_profile(&mut self, name: String) -> (Response, Vec<DeviceCmd>) {
        if !self.profiles.contains_key(&name) {
            return (err(format!("no such profile: {name}")), vec![]);
        }
        let cmds = self.activate_profile(&name);
        (Response::Ack, cmds)
    }

    /// Set (or clear, with `None`) a profile's linked application class. The
    /// daemon persists the snapshot afterwards, so the new link survives restart
    /// and feeds `profile_for_app` / auto-switch on the next focus change.
    fn set_profile_linked_app(
        &mut self,
        name: String,
        linked_app: Option<String>,
    ) -> (Response, Vec<DeviceCmd>) {
        let Some(p) = self.profiles.get_mut(&name) else {
            return (err(format!("no such profile: {name}")), vec![]);
        };
        p.linked_app = linked_app;
        (Response::Ack, vec![])
    }

    /// Set the configured startup (default) profile — the one activated when no
    /// app match applies. Validates it exists; persisted via `config_snapshot`
    /// (which writes `configured_default` as the on-disk `default_profile`). Does
    /// NOT change the live `active_profile`.
    fn set_default_profile(&mut self, name: String) -> (Response, Vec<DeviceCmd>) {
        if !self.profiles.contains_key(&name) {
            return (err(format!("no such profile: {name}")), vec![]);
        }
        self.configured_default = name;
        (Response::Ack, vec![])
    }

    /// Set a profile's thumbstick axis mode (four- vs eight-way). Persisted by
    /// the daemon snapshot. If it's the active profile, the change is live: the
    /// physical-input path reads `active_profile_thumb_axis_mode()` fresh on every
    /// press, so no separate engine copy needs syncing.
    fn set_thumb_axis_mode(
        &mut self,
        profile: String,
        mode: ThumbAxisMode,
    ) -> (Response, Vec<DeviceCmd>) {
        let Some(p) = self.profiles.get_mut(&profile) else {
            return (err(format!("no such profile: {profile}")), vec![]);
        };
        p.thumb_axis_mode = mode;
        (Response::Ack, vec![])
    }

    fn set_bind(
        &mut self,
        profile: String,
        layer: Layer,
        key: String,
        action: Option<BindAction>,
    ) -> (Response, Vec<DeviceCmd>) {
        // Validate the key name at the boundary AND canonicalize it: store the
        // bind under the same `key_name(code)` form that `resolve` looks up by,
        // so an aliased name (e.g. a different valid spelling that parses to the
        // same code) still resolves at runtime instead of binding into a dead
        // key that never matches.
        let Some(code) = parse_key(&key) else {
            return (err(format!("unknown key name: {key}")), vec![]);
        };
        let canonical = key_name(code);
        let Some(p) = self.profiles.get_mut(&profile) else {
            return (err(format!("no such profile: {profile}")), vec![]);
        };
        match action {
            Some(a) => {
                p.layer_mut(layer).insert(canonical, a);
            }
            None => {
                p.layer_mut(layer).remove(&canonical);
            }
        }
        (Response::Ack, vec![])
    }

    fn get_lighting(&self, profile: Option<String>) -> (Response, Vec<DeviceCmd>) {
        let name = profile.as_deref().unwrap_or(&self.active_profile);
        match self.profiles.get(name) {
            Some(p) => (Response::Lighting(p.lighting.clone()), vec![]),
            None => (err(format!("no such profile: {name}")), vec![]),
        }
    }

    fn set_lighting(
        &mut self,
        profile: Option<String>,
        lighting: LightingState,
    ) -> (Response, Vec<DeviceCmd>) {
        let name = profile.unwrap_or_else(|| self.active_profile.clone());
        let Some(p) = self.profiles.get_mut(&name) else {
            return (err(format!("no such profile: {name}")), vec![]);
        };
        p.lighting = lighting.clone();
        // Only push to the device if the edited profile is the active one.
        let cmds = if name == self.active_profile {
            vec![DeviceCmd::ApplyLighting(lighting)]
        } else {
            vec![]
        };
        (Response::Ack, cmds)
    }

    fn set_brightness(&mut self, level: u8) -> (Response, Vec<DeviceCmd>) {
        // Brightness lives in the active profile's lighting state.
        if let Some(p) = self.profiles.get_mut(&self.active_profile) {
            p.lighting.brightness = level;
        }
        (Response::Ack, vec![DeviceCmd::SetBrightness(level)])
    }
}

/// The canonical evdev name for a key code (`"KEY_A"`, `"BTN_LEFT"`).
fn key_name(code: KeyCode) -> String {
    format!("{code:?}")
}

/// Parse an evdev name into a key code; `None` if unknown. The single parse
/// authority for the crate: macro steps and analog key names validate exactly
/// like bind names.
pub(crate) fn parse_key(name: &str) -> Option<KeyCode> {
    name.parse::<KeyCode>().ok()
}

fn err(message: String) -> Response {
    Response::Error { message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::EventType;
    use std::collections::BTreeMap;
    use tartarust_ipc::model::{Effect, LightingState, MacroStep, Rgb};

    /// Build a single-profile config from base + hypershift binds. To set a
    /// Hypershift trigger, include `("KEY_X", BindAction::Hypershift)` in `base`.
    fn cfg(base: Vec<(&str, BindAction)>, hyper: Vec<(&str, BindAction)>) -> Config {
        let mut profile = Profile::new("default");
        for (key, action) in base {
            profile.base.insert(key.to_string(), action);
        }
        for (key, action) in hyper {
            profile.hypershift.insert(key.to_string(), action);
        }
        let mut profiles = BTreeMap::new();
        profiles.insert("default".to_string(), profile);
        Config {
            default_profile: "default".to_string(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles,
            macros: BTreeMap::new(),
        }
    }

    fn key_action(names: &[&str]) -> BindAction {
        BindAction::Key {
            keys: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Emitted KEY (code, value) pairs.
    fn keys(o: &Outcome) -> Vec<(u16, i32)> {
        o.events
            .iter()
            .filter(|e| e.event_type() == EventType::KEY)
            .map(|e| (e.code(), e.value()))
            .collect()
    }

    #[test]
    fn key_event_resolves_matching_name_bind() {
        // The KeyCode->name->bind path: a KEY_A event resolves a "KEY_A" bind.
        let mut e = Engine::new(cfg(vec![("KEY_A", key_action(&["KEY_F1"]))], vec![]));
        let down = e.handle_key(KeyCode::KEY_A, 1);
        assert_eq!(keys(&down), vec![(KeyCode::KEY_F1.0, 1)]);
    }

    #[test]
    fn passthrough_for_unbound_key() {
        let mut e = Engine::new(cfg(vec![], vec![]));
        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 1)),
            vec![(KeyCode::KEY_A.0, 1)]
        );
        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 0)),
            vec![(KeyCode::KEY_A.0, 0)]
        );
    }

    #[test]
    fn chord_presses_in_order_releases_in_reverse() {
        let mut e = Engine::new(cfg(
            vec![("KEY_A", key_action(&["KEY_LEFTCTRL", "KEY_C"]))],
            vec![],
        ));
        let down = e.handle_key(KeyCode::KEY_A, 1);
        assert_eq!(
            keys(&down),
            vec![(KeyCode::KEY_LEFTCTRL.0, 1), (KeyCode::KEY_C.0, 1)]
        );
        let up = e.handle_key(KeyCode::KEY_A, 0);
        assert_eq!(
            keys(&up),
            vec![(KeyCode::KEY_C.0, 0), (KeyCode::KEY_LEFTCTRL.0, 0)]
        );
    }

    #[test]
    fn disable_consumes_key() {
        let mut e = Engine::new(cfg(vec![("KEY_A", BindAction::Disable)], vec![]));
        assert!(keys(&e.handle_key(KeyCode::KEY_A, 1)).is_empty());
        assert!(keys(&e.handle_key(KeyCode::KEY_A, 0)).is_empty());
    }

    #[test]
    fn autorepeat_swallowed() {
        let mut e = Engine::new(cfg(vec![], vec![]));
        e.handle_key(KeyCode::KEY_A, 1);
        assert!(e.handle_key(KeyCode::KEY_A, 2).events.is_empty());
    }

    #[test]
    fn hypershift_bound_key_selects_secondary_layer() {
        // KEY_LEFTALT is bound to Hypershift in the BASE layer — that's the
        // trigger. Holding it activates the layer and emits nothing.
        let mut e = Engine::new(cfg(
            vec![
                ("KEY_LEFTALT", BindAction::Hypershift),
                ("KEY_A", key_action(&["KEY_1"])),
            ],
            vec![("KEY_A", key_action(&["KEY_F1"]))],
        ));
        assert!(keys(&e.handle_key(KeyCode::KEY_LEFTALT, 1)).is_empty());
        assert!(e.hypershift_active());
        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 1)),
            vec![(KeyCode::KEY_F1.0, 1)]
        );
        // Releasing the trigger deactivates the layer.
        e.handle_key(KeyCode::KEY_LEFTALT, 0);
        assert!(!e.hypershift_active());
    }

    #[test]
    fn hypershift_released_mid_hold_releases_layer_action() {
        let mut e = Engine::new(cfg(
            vec![
                ("KEY_LEFTALT", BindAction::Hypershift),
                ("KEY_A", key_action(&["KEY_1"])),
            ],
            vec![("KEY_A", key_action(&["KEY_F1"]))],
        ));
        e.handle_key(KeyCode::KEY_LEFTALT, 1);
        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 1)),
            vec![(KeyCode::KEY_F1.0, 1)]
        );
        e.handle_key(KeyCode::KEY_LEFTALT, 0);
        // Release must undo KEY_F1, not KEY_1.
        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 0)),
            vec![(KeyCode::KEY_F1.0, 0)]
        );
    }

    #[test]
    fn hypershift_trigger_survives_profile_switch_mid_hold() {
        // The discriminating test for "record-on-press, don't re-resolve": hold
        // a Hypershift trigger, then a hypershift-layer key switches profile
        // (where the trigger key is NOT bound to Hypershift), then release the
        // trigger. It must still deactivate — re-resolving in the new profile
        // would leave hypershift stuck active forever.
        let mut p1 = Profile::new("p1");
        p1.base.insert("KEY_LEFTALT".into(), BindAction::Hypershift);
        // In the hypershift layer, KEY_S switches to p2.
        p1.hypershift.insert(
            "KEY_S".into(),
            BindAction::SwitchProfile { name: "p2".into() },
        );
        // p2 does NOT bind KEY_LEFTALT to Hypershift at all.
        let p2 = Profile::new("p2");
        let mut profiles = BTreeMap::new();
        profiles.insert("p1".into(), p1);
        profiles.insert("p2".into(), p2);
        let mut e = Engine::new(Config {
            default_profile: "p1".into(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles,
            macros: BTreeMap::new(),
        });

        e.handle_key(KeyCode::KEY_LEFTALT, 1); // trigger down -> layer active
        assert!(e.hypershift_active());
        e.handle_key(KeyCode::KEY_S, 1); // hypershift-layer switch -> p2
        assert_eq!(e.active_profile(), "p2");
        e.handle_key(KeyCode::KEY_S, 0);
        // Release the trigger: must deactivate despite p2 not binding it.
        e.handle_key(KeyCode::KEY_LEFTALT, 0);
        assert!(
            !e.hypershift_active(),
            "trigger release must deactivate even after a mid-hold profile switch"
        );
    }

    #[test]
    fn shared_modifier_refcounted() {
        let mut e = Engine::new(cfg(
            vec![
                ("KEY_Q", key_action(&["KEY_LEFTCTRL", "KEY_A"])),
                ("KEY_W", key_action(&["KEY_LEFTCTRL", "KEY_B"])),
            ],
            vec![],
        ));
        e.handle_key(KeyCode::KEY_Q, 1);
        let dw = e.handle_key(KeyCode::KEY_W, 1);
        assert_eq!(keys(&dw), vec![(KeyCode::KEY_B.0, 1)]); // ctrl already down
        let uq = e.handle_key(KeyCode::KEY_Q, 0);
        assert_eq!(keys(&uq), vec![(KeyCode::KEY_A.0, 0)]); // ctrl still held by W
        let uw = e.handle_key(KeyCode::KEY_W, 0);
        assert_eq!(
            keys(&uw),
            vec![(KeyCode::KEY_B.0, 0), (KeyCode::KEY_LEFTCTRL.0, 0)]
        );
    }

    #[test]
    fn release_all_emits_key_up_for_every_held_output() {
        // The stuck-key fix (#2): on unplug the reader calls release_all(), which
        // must emit a key-up for every output key currently held so nothing stays
        // stuck-down on the virtual device.
        let mut e = Engine::new(cfg(
            vec![("KEY_A", key_action(&["KEY_LEFTCTRL", "KEY_C"]))],
            vec![],
        ));
        // Hold the chord; KEY_A never gets a key-up (simulating unplug mid-hold).
        let down = e.handle_key(KeyCode::KEY_A, 1);
        assert_eq!(
            keys(&down),
            vec![(KeyCode::KEY_LEFTCTRL.0, 1), (KeyCode::KEY_C.0, 1)]
        );

        let released = e.release_all();
        let mut got = keys(&released);
        got.sort();
        // Exactly one key-up per held output (order-independent).
        assert_eq!(
            got,
            vec![(KeyCode::KEY_LEFTCTRL.0, 0), (KeyCode::KEY_C.0, 0)]
        );
        // State is fully cleared: a second release_all emits nothing.
        assert!(keys(&e.release_all()).is_empty());
    }

    #[test]
    fn release_all_emits_one_key_up_per_refcounted_output() {
        // A shared modifier held by two physical keys must release exactly once.
        let mut e = Engine::new(cfg(
            vec![
                ("KEY_Q", key_action(&["KEY_LEFTCTRL", "KEY_A"])),
                ("KEY_W", key_action(&["KEY_LEFTCTRL", "KEY_B"])),
            ],
            vec![],
        ));
        e.handle_key(KeyCode::KEY_Q, 1);
        e.handle_key(KeyCode::KEY_W, 1);
        let mut got = keys(&e.release_all());
        got.sort();
        // LEFTCTRL (held twice) appears once, plus A and B. Expected list is in
        // ascending key-code order to match the sorted `got` (LEFTCTRL=29 < A=30
        // < B=48).
        let mut expected = vec![
            (KeyCode::KEY_LEFTCTRL.0, 0),
            (KeyCode::KEY_A.0, 0),
            (KeyCode::KEY_B.0, 0),
        ];
        expected.sort();
        assert_eq!(got, expected);
    }

    #[test]
    fn switch_profile_mid_hold_releases_old_layer_action() {
        // Generalizes the hypershift-mid-hold guarantee to profile switches:
        // hold KEY_A (emits KEY_1), switch profile (where KEY_A=KEY_2), release
        // KEY_A — must release KEY_1 (what was pressed), not KEY_2.
        let mut p1 = Profile::new("p1");
        p1.base.insert("KEY_A".into(), key_action(&["KEY_1"]));
        let mut p2 = Profile::new("p2");
        p2.base.insert("KEY_A".into(), key_action(&["KEY_2"]));
        let mut profiles = BTreeMap::new();
        profiles.insert("p1".into(), p1);
        profiles.insert("p2".into(), p2);
        let config = Config {
            default_profile: "p1".into(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles,
            macros: BTreeMap::new(),
        };
        let mut e = Engine::new(config);

        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 1)),
            vec![(KeyCode::KEY_1.0, 1)]
        );
        let (resp, _) = e.apply(Request::SwitchProfile { name: "p2".into() }, "test");
        assert_eq!(resp, Response::Ack);
        assert_eq!(e.active_profile(), "p2");
        // Release replays the record: KEY_1, not KEY_2.
        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 0)),
            vec![(KeyCode::KEY_1.0, 0)]
        );
    }

    #[test]
    fn bound_switch_profile_emits_lighting_cmd() {
        let mut p1 = Profile::new("p1");
        p1.base.insert(
            "KEY_A".into(),
            BindAction::SwitchProfile { name: "p2".into() },
        );
        let mut p2 = Profile::new("p2");
        p2.lighting = LightingState {
            effect: Effect::Static {
                color: Rgb::new(255, 0, 0),
            },
            brightness: 100,
        };
        let mut profiles = BTreeMap::new();
        profiles.insert("p1".into(), p1);
        profiles.insert("p2".into(), p2);
        let mut e = Engine::new(Config {
            default_profile: "p1".into(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles,
            macros: BTreeMap::new(),
        });
        let out = e.handle_key(KeyCode::KEY_A, 1);
        assert_eq!(e.active_profile(), "p2");
        // A DeviceCmd applying p2's lighting must be among the commands.
        assert!(out.commands.iter().any(|c| matches!(
            c,
            Command::DeviceCmd(DeviceCmd::ApplyLighting(l)) if l.brightness == 100
        )));
    }

    // ---- four-way thumbstick diagonal expansion ----

    /// A single-profile engine whose profile has `thumb_axis_mode = mode` and the
    /// given base binds. The four diagonal physical keys (KEY_HOME/PAGEUP/END/
    /// PAGEDOWN) are left UNBOUND here so the cardinals they expand to resolve to
    /// passthrough unless the test binds them.
    fn thumb_engine(mode: ThumbAxisMode, base: Vec<(&str, BindAction)>) -> Engine {
        let mut profile = Profile::new("default");
        profile.thumb_axis_mode = mode;
        for (key, action) in base {
            profile.base.insert(key.to_string(), action);
        }
        let mut profiles = BTreeMap::new();
        profiles.insert("default".to_string(), profile);
        Engine::new(Config {
            default_profile: "default".into(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles,
            macros: BTreeMap::new(),
        })
    }

    #[test]
    fn four_mode_diagonal_press_emits_both_adjacent_cardinals() {
        // KEY_HOME is the up-left diagonal; in four-way mode pressing it must
        // emit BOTH KEY_UP and KEY_LEFT (their resolved binds — here remapped).
        let mut e = thumb_engine(
            ThumbAxisMode::Four,
            vec![
                ("KEY_UP", key_action(&["KEY_W"])),
                ("KEY_LEFT", key_action(&["KEY_A"])),
            ],
        );
        let down = e.handle_key(KeyCode::KEY_HOME, 1);
        assert_eq!(
            keys(&down),
            vec![(KeyCode::KEY_W.0, 1), (KeyCode::KEY_A.0, 1)]
        );
    }

    #[test]
    fn four_mode_diagonal_release_releases_both_cardinals() {
        let mut e = thumb_engine(
            ThumbAxisMode::Four,
            vec![
                ("KEY_UP", key_action(&["KEY_W"])),
                ("KEY_LEFT", key_action(&["KEY_A"])),
            ],
        );
        e.handle_key(KeyCode::KEY_HOME, 1);
        let up = e.handle_key(KeyCode::KEY_HOME, 0);
        // Released in reverse press order: A (from KEY_LEFT) then W (from KEY_UP).
        assert_eq!(
            keys(&up),
            vec![(KeyCode::KEY_A.0, 0), (KeyCode::KEY_W.0, 0)]
        );
    }

    #[test]
    fn four_mode_diagonal_and_real_cardinal_refcount_shared_output() {
        // KEY_HOME expands to KEY_UP + KEY_LEFT. With KEY_UP passthrough, both
        // the diagonal's up-component AND a real KEY_UP press hold KEY_UP. The
        // shared output stays down until BOTH release (refcount), matching the
        // refcount guarantee for normal chords.
        let mut e = thumb_engine(ThumbAxisMode::Four, vec![]);
        // Diagonal down: emits KEY_UP (passthrough) + KEY_LEFT (passthrough).
        let d = e.handle_key(KeyCode::KEY_HOME, 1);
        assert_eq!(
            keys(&d),
            vec![(KeyCode::KEY_UP.0, 1), (KeyCode::KEY_LEFT.0, 1)]
        );
        // Real cardinal KEY_UP down: already held by the diagonal -> no new event.
        let up_press = e.handle_key(KeyCode::KEY_UP, 1);
        assert!(
            keys(&up_press).is_empty(),
            "KEY_UP already held by diagonal"
        );
        // Release the diagonal: KEY_LEFT goes up, but KEY_UP stays (real cardinal).
        let diag_up = e.handle_key(KeyCode::KEY_HOME, 0);
        assert_eq!(keys(&diag_up), vec![(KeyCode::KEY_LEFT.0, 0)]);
        // Release the real cardinal: now KEY_UP finally goes up.
        let up_rel = e.handle_key(KeyCode::KEY_UP, 0);
        assert_eq!(keys(&up_rel), vec![(KeyCode::KEY_UP.0, 0)]);
    }

    #[test]
    fn eight_mode_diagonal_resolves_its_own_bind() {
        // In eight-way mode the diagonal is a normal key: it resolves ITS OWN
        // bind (here remapped to KEY_F5), NOT the adjacent cardinals.
        let mut e = thumb_engine(
            ThumbAxisMode::Eight,
            vec![
                ("KEY_HOME", key_action(&["KEY_F5"])),
                ("KEY_UP", key_action(&["KEY_W"])),
                ("KEY_LEFT", key_action(&["KEY_A"])),
            ],
        );
        let down = e.handle_key(KeyCode::KEY_HOME, 1);
        assert_eq!(keys(&down), vec![(KeyCode::KEY_F5.0, 1)]);
        let up = e.handle_key(KeyCode::KEY_HOME, 0);
        assert_eq!(keys(&up), vec![(KeyCode::KEY_F5.0, 0)]);
    }

    #[test]
    fn set_thumb_axis_mode_persists_and_is_live() {
        // SetThumbAxisMode flips a profile's mode; the snapshot persists it and
        // the active profile honors it immediately on the next press.
        let mut e = thumb_engine(ThumbAxisMode::Eight, vec![]);
        assert_eq!(e.active_profile_thumb_axis_mode(), ThumbAxisMode::Eight);
        // Eight-way: the diagonal passes through as itself.
        let d = e.handle_key(KeyCode::KEY_HOME, 1);
        assert_eq!(keys(&d), vec![(KeyCode::KEY_HOME.0, 1)]);
        e.handle_key(KeyCode::KEY_HOME, 0);

        let (resp, _) = e.apply(
            Request::SetThumbAxisMode {
                profile: "default".into(),
                mode: ThumbAxisMode::Four,
            },
            "t",
        );
        assert_eq!(resp, Response::Ack);
        assert_eq!(e.active_profile_thumb_axis_mode(), ThumbAxisMode::Four);
        assert_eq!(
            e.config_snapshot().profiles["default"].thumb_axis_mode,
            ThumbAxisMode::Four
        );
        // Now four-way: the diagonal expands to its two cardinals (passthrough).
        let d2 = e.handle_key(KeyCode::KEY_HOME, 1);
        assert_eq!(
            keys(&d2),
            vec![(KeyCode::KEY_UP.0, 1), (KeyCode::KEY_LEFT.0, 1)]
        );
    }

    #[test]
    fn set_profile_linked_app_sets_and_clears() {
        let mut e = two_profile_engine();
        e.apply(Request::CreateProfile { name: "fps".into() }, "t");
        let (resp, _) = e.apply(
            Request::SetProfileLinkedApp {
                name: "fps".into(),
                linked_app: Some("game".into()),
            },
            "t",
        );
        assert_eq!(resp, Response::Ack);
        assert_eq!(e.profile_for_app("game"), Some("fps".to_string()));
        // Clearing it (None) unlinks.
        let (resp2, _) = e.apply(
            Request::SetProfileLinkedApp {
                name: "fps".into(),
                linked_app: None,
            },
            "t",
        );
        assert_eq!(resp2, Response::Ack);
        assert_eq!(e.profile_for_app("game"), None);
        // Unknown profile -> error.
        assert!(matches!(
            e.apply(
                Request::SetProfileLinkedApp {
                    name: "ghost".into(),
                    linked_app: None,
                },
                "t",
            )
            .0,
            Response::Error { .. }
        ));
    }

    #[test]
    fn set_default_profile_validates_and_persists() {
        let mut e = two_profile_engine();
        e.apply(Request::CreateProfile { name: "fps".into() }, "t");
        // Switching the configured default does not move the live active profile.
        let (resp, _) = e.apply(Request::SetDefaultProfile { name: "fps".into() }, "t");
        assert_eq!(resp, Response::Ack);
        assert_eq!(
            e.active_profile(),
            "default",
            "default change must not switch live"
        );
        // Persisted as the on-disk startup default.
        assert_eq!(e.config_snapshot().default_profile, "fps");
        // Unknown profile -> error.
        assert!(matches!(
            e.apply(
                Request::SetDefaultProfile {
                    name: "ghost".into()
                },
                "t"
            )
            .0,
            Response::Error { .. }
        ));
    }

    // ---- IPC apply() tests ----

    fn two_profile_engine() -> Engine {
        let mut profiles = BTreeMap::new();
        profiles.insert("default".to_string(), Profile::new("default"));
        Engine::new(Config {
            default_profile: "default".into(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles,
            macros: BTreeMap::new(),
        })
    }

    #[test]
    fn create_rename_delete_profile() {
        let mut e = two_profile_engine();
        assert_eq!(
            e.apply(Request::CreateProfile { name: "fps".into() }, "t")
                .0,
            Response::Ack
        );
        // duplicate rejected
        assert!(matches!(
            e.apply(Request::CreateProfile { name: "fps".into() }, "t")
                .0,
            Response::Error { .. }
        ));
        assert_eq!(
            e.apply(
                Request::RenameProfile {
                    from: "fps".into(),
                    to: "shooter".into()
                },
                "t"
            )
            .0,
            Response::Ack
        );
        assert_eq!(
            e.apply(
                Request::DeleteProfile {
                    name: "shooter".into()
                },
                "t"
            )
            .0,
            Response::Ack
        );
        // cannot delete active
        assert!(matches!(
            e.apply(
                Request::DeleteProfile {
                    name: "default".into()
                },
                "t"
            )
            .0,
            Response::Error { .. }
        ));
    }

    #[test]
    fn set_bind_validates_and_mutates() {
        let mut e = two_profile_engine();
        let ok = e.apply(
            Request::SetBind {
                profile: "default".into(),
                layer: Layer::Base,
                key: "KEY_A".into(),
                action: Some(key_action(&["KEY_F1"])),
            },
            "t",
        );
        assert_eq!(ok.0, Response::Ack);
        // It actually took effect.
        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 1)),
            vec![(KeyCode::KEY_F1.0, 1)]
        );
        // Unknown key name rejected.
        assert!(matches!(
            e.apply(
                Request::SetBind {
                    profile: "default".into(),
                    layer: Layer::Base,
                    key: "KEY_NOPE".into(),
                    action: None,
                },
                "t",
            )
            .0,
            Response::Error { .. }
        ));
    }

    #[test]
    fn set_bind_stores_canonical_key_name() {
        // Regression (#6): the stored bind key must be the canonical
        // `key_name(code)` form that `resolve` looks up by — not the raw user
        // string — so the bind actually resolves at runtime. We assert the
        // profile map is keyed by the canonical name.
        let mut e = two_profile_engine();
        let key_str = "KEY_A".to_string();
        let canonical = key_name(KeyCode::KEY_A); // what resolve() looks up
        e.apply(
            Request::SetBind {
                profile: "default".into(),
                layer: Layer::Base,
                key: key_str,
                action: Some(key_action(&["KEY_F1"])),
            },
            "t",
        );
        let profile = e.profiles.get("default").unwrap();
        assert!(
            profile.base.contains_key(&canonical),
            "bind must be stored under the canonical key name `{canonical}`"
        );
        // And it resolves (the lookup path that the bug broke).
        assert_eq!(
            keys(&e.handle_key(KeyCode::KEY_A, 1)),
            vec![(KeyCode::KEY_F1.0, 1)]
        );
    }

    #[test]
    fn set_lighting_active_emits_devicecmd_inactive_does_not() {
        let mut e = two_profile_engine();
        e.apply(
            Request::CreateProfile {
                name: "other".into(),
            },
            "t",
        );
        let light = LightingState {
            effect: Effect::Spectrum,
            brightness: 200,
        };
        // active profile -> device cmd
        let (r, cmds) = e.apply(
            Request::SetLighting {
                profile: Some("default".into()),
                lighting: light.clone(),
            },
            "t",
        );
        assert_eq!(r, Response::Ack);
        assert_eq!(cmds, vec![DeviceCmd::ApplyLighting(light.clone())]);
        // inactive profile -> no device cmd, but state is stored
        let (r2, cmds2) = e.apply(
            Request::SetLighting {
                profile: Some("other".into()),
                lighting: light,
            },
            "t",
        );
        assert_eq!(r2, Response::Ack);
        assert!(cmds2.is_empty());
    }

    #[test]
    fn set_brightness_emits_devicecmd_and_stores() {
        let mut e = two_profile_engine();
        let (r, cmds) = e.apply(Request::SetBrightness { level: 42 }, "t");
        assert_eq!(r, Response::Ack);
        assert_eq!(cmds, vec![DeviceCmd::SetBrightness(42)]);
        // stored in active profile lighting
        if let (Response::Lighting(l), _) = e.apply(Request::GetLighting { profile: None }, "t") {
            assert_eq!(l.brightness, 42);
        } else {
            panic!("expected lighting");
        }
    }

    #[test]
    fn macro_save_get_list_delete() {
        let mut e = two_profile_engine();
        let m = Macro {
            name: "m".into(),
            steps: vec![MacroStep::Press {
                key: "KEY_A".into(),
            }],
        };
        assert_eq!(
            e.apply(Request::SaveMacro { r#macro: m.clone() }, "t").0,
            Response::Ack
        );
        if let (Response::Macros { names }, _) = e.apply(Request::ListMacros, "t") {
            assert_eq!(names, vec!["m".to_string()]);
        } else {
            panic!("expected macros list");
        }
        // GetMacro returns the full body (steps), not just the name.
        assert_eq!(
            e.apply(Request::GetMacro { name: "m".into() }, "t").0,
            Response::Macro(m)
        );
        // Unknown macro -> Error.
        assert!(matches!(
            e.apply(
                Request::GetMacro {
                    name: "nope".into()
                },
                "t"
            )
            .0,
            Response::Error { .. }
        ));
        assert_eq!(
            e.apply(Request::DeleteMacro { name: "m".into() }, "t").0,
            Response::Ack
        );
        assert!(matches!(
            e.apply(Request::DeleteMacro { name: "m".into() }, "t").0,
            Response::Error { .. }
        ));
    }

    // ---- auto-switch (M4) ----

    /// Engine with profiles "default" (no link) and "fps" (linked to "game").
    fn auto_switch_engine(auto_switch: bool) -> Engine {
        let mut default = Profile::new("default");
        default.linked_app = None;
        let mut fps = Profile::new("fps");
        fps.linked_app = Some("game".to_string());
        let mut profiles = BTreeMap::new();
        profiles.insert("default".into(), default);
        profiles.insert("fps".into(), fps);
        Engine::new(Config {
            default_profile: "default".into(),
            auto_switch,
            tray: true,
            input_mode: InputMode::Digital,
            analog: AnalogConfig::default(),
            profiles,
            macros: BTreeMap::new(),
        })
    }

    #[test]
    fn profile_for_app_matches_linked_app_exactly() {
        let e = auto_switch_engine(true);
        assert_eq!(e.profile_for_app("game"), Some("fps".to_string()));
        assert_eq!(e.profile_for_app("other"), None);
        assert_eq!(e.profile_for_app(""), None); // empty class never matches
        assert_eq!(e.profile_for_app("Game"), None); // case-sensitive
    }

    #[test]
    fn focusing_linked_app_switches_profile() {
        let mut e = auto_switch_engine(true);
        assert_eq!(e.active_profile(), "default");
        let cmds = e.on_focused_app("game");
        assert_eq!(e.active_profile(), "fps");
        // Switching emits the new profile's lighting.
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn focusing_unlinked_app_does_not_revert() {
        // A manual switch stays sticky; focusing an unlinked app is a no-op.
        let mut e = auto_switch_engine(true);
        e.apply(Request::SwitchProfile { name: "fps".into() }, "t");
        assert_eq!(e.active_profile(), "fps");
        let cmds = e.on_focused_app("some-terminal"); // not linked anywhere
        assert_eq!(e.active_profile(), "fps", "must not revert to default");
        assert!(cmds.is_empty());
    }

    #[test]
    fn refocusing_same_linked_app_is_a_noop() {
        let mut e = auto_switch_engine(true);
        e.on_focused_app("game"); // -> fps
        let cmds = e.on_focused_app("game"); // already fps; no churn
        assert!(cmds.is_empty(), "re-focus must not re-emit lighting");
    }

    #[test]
    fn auto_switch_disabled_does_nothing() {
        let mut e = auto_switch_engine(false);
        let cmds = e.on_focused_app("game");
        assert_eq!(e.active_profile(), "default");
        assert!(cmds.is_empty());
    }

    #[test]
    fn set_auto_switch_toggles_and_persists_in_snapshot() {
        let mut e = auto_switch_engine(true);
        assert!(e.auto_switch_enabled());
        let (resp, _) = e.apply(Request::SetAutoSwitch { enabled: false }, "t");
        assert_eq!(resp, Response::Ack);
        assert!(!e.auto_switch_enabled());
        // It is reflected in the persisted snapshot and in status.
        assert!(!e.config_snapshot().auto_switch);
        // And now focusing a linked app does nothing.
        assert!(e.on_focused_app("game").is_empty());
    }

    #[test]
    fn snapshot_keeps_configured_default_not_active() {
        // After an auto-switch, the persisted default_profile must remain the
        // configured one, not the live active profile.
        let mut e = auto_switch_engine(true);
        e.on_focused_app("game");
        assert_eq!(e.active_profile(), "fps");
        assert_eq!(
            e.config_snapshot().default_profile,
            "default",
            "auto-switch must not change the persisted startup profile"
        );
    }

    #[test]
    fn snapshot_preserves_input_mode_and_analog_through_mutation() {
        // Critical persistence guard: `input_mode` and the `[analog]` table are
        // pass-through state the Engine doesn't act on, but the daemon persists a
        // `config_snapshot()` after EVERY mutating IPC request. If the snapshot
        // dropped them, the first SetBind/SetBrightness/... in analog mode would
        // rewrite config.toml back to digital with the analog map gone — silently
        // destroying the user's setup. So after a mutation they must survive
        // verbatim.
        let mut analog = AnalogConfig {
            poll_interval_ms: 25,
            keys: BTreeMap::new(),
        };
        analog.keys.insert(
            "5".to_string(),
            crate::state::AnalogKey {
                key: "KEY_Q".into(),
                actuation_point: 0x60,
                release_point: 0x50,
                second_actuation_point: Some(0xC0),
                second_action: Some("KEY_LEFTSHIFT".into()),
            },
        );
        let mut profiles = BTreeMap::new();
        profiles.insert("default".to_string(), Profile::new("default"));
        let mut e = Engine::new(Config {
            default_profile: "default".into(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Analog,
            analog: analog.clone(),
            profiles,
            macros: BTreeMap::new(),
        });

        // A mutating request that does NOT touch input_mode/analog.
        let (resp, _) = e.apply(
            Request::SetBind {
                profile: "default".into(),
                layer: Layer::Base,
                key: "KEY_A".into(),
                action: Some(key_action(&["KEY_F1"])),
            },
            "t",
        );
        assert_eq!(resp, Response::Ack);

        // The snapshot the daemon would persist must still carry both verbatim.
        let snap = e.config_snapshot();
        assert_eq!(snap.input_mode, InputMode::Analog);
        assert_eq!(
            snap.analog, analog,
            "analog map must survive a mutating edit"
        );
    }

    // ---- analog IPC (GetAnalogState / SetActuation) ----

    /// An analog-mode engine with byte index 5 mapped to KEY_Q.
    fn analog_engine() -> Engine {
        let mut analog = AnalogConfig {
            poll_interval_ms: 50,
            keys: BTreeMap::new(),
        };
        analog.keys.insert(
            "5".to_string(),
            AnalogKey {
                key: "KEY_Q".into(),
                actuation_point: 0x60,
                release_point: 0x50,
                second_actuation_point: None,
                second_action: None,
            },
        );
        let mut profiles = BTreeMap::new();
        profiles.insert("default".to_string(), Profile::new("default"));
        Engine::new(Config {
            default_profile: "default".into(),
            auto_switch: true,
            tray: true,
            input_mode: InputMode::Analog,
            analog,
            profiles,
            macros: BTreeMap::new(),
        })
    }

    #[test]
    fn set_actuation_updates_config_and_persists() {
        let mut e = analog_engine();
        let (resp, cmds) = e.apply(
            Request::SetActuation {
                index: 5,
                actuation_point: 0x40,
                release_point: 0x30,
                second_actuation_point: Some(0xC0),
                second_action: Some("KEY_LEFTSHIFT".into()),
            },
            "t",
        );
        assert_eq!(resp, Response::Ack);
        assert!(cmds.is_empty());
        // The persisted snapshot reflects the new thresholds + second step.
        let k = &e.config_snapshot().analog.keys["5"];
        assert_eq!(k.actuation_point, 0x40);
        assert_eq!(k.release_point, 0x30);
        assert_eq!(k.second_actuation_point, Some(0xC0));
        assert_eq!(k.second_action.as_deref(), Some("KEY_LEFTSHIFT"));
        // The primary key identity is preserved (SetActuation edits thresholds).
        assert_eq!(k.key, "KEY_Q");
    }

    #[test]
    fn set_actuation_rejects_unconfigured_index() {
        let mut e = analog_engine();
        // Index 6 is not mapped — the UI must map it in the config first.
        assert!(matches!(
            e.apply(
                Request::SetActuation {
                    index: 6,
                    actuation_point: 0x60,
                    release_point: 0x50,
                    second_actuation_point: None,
                    second_action: None,
                },
                "t",
            )
            .0,
            Response::Error { .. }
        ));
    }

    #[test]
    fn set_actuation_rejects_out_of_range_index_and_bad_second() {
        let mut e = analog_engine();
        // Out-of-range index (>= ANALOG_KEY_COUNT).
        assert!(matches!(
            e.apply(
                Request::SetActuation {
                    index: 99,
                    actuation_point: 0x60,
                    release_point: 0x50,
                    second_actuation_point: None,
                    second_action: None,
                },
                "t",
            )
            .0,
            Response::Error { .. }
        ));
        // second_actuation_point without second_action.
        assert!(matches!(
            e.apply(
                Request::SetActuation {
                    index: 5,
                    actuation_point: 0x60,
                    release_point: 0x50,
                    second_actuation_point: Some(0xC0),
                    second_action: None,
                },
                "t",
            )
            .0,
            Response::Error { .. }
        ));
        // unknown second_action key name.
        assert!(matches!(
            e.apply(
                Request::SetActuation {
                    index: 5,
                    actuation_point: 0x60,
                    release_point: 0x50,
                    second_actuation_point: Some(0xC0),
                    second_action: Some("KEY_NOPE".into()),
                },
                "t",
            )
            .0,
            Response::Error { .. }
        ));
    }

    #[test]
    fn get_analog_state_without_source_is_inactive_with_config() {
        // No analog source wired (analog_snapshot is None): inactive, empty
        // pressures, but the configured keys are still surfaced.
        let mut e = analog_engine();
        let (resp, _) = e.apply(Request::GetAnalogState, "t");
        let tartarust_ipc::Response::AnalogState(state) = resp else {
            panic!("expected AnalogState");
        };
        assert!(!state.analog_active);
        assert!(state.pressures.is_empty());
        assert_eq!(state.keys.len(), 1);
        assert_eq!(state.keys[0].index, 5);
        assert_eq!(state.keys[0].key, "KEY_Q");
    }

    #[test]
    fn get_analog_state_with_active_snapshot_reports_pressures() {
        use crate::analog_source::AnalogSnapshot;
        let mut e = analog_engine();
        // Wire a live snapshot + update channel as the daemon would in analog mode.
        let snapshot = AnalogSnapshot::new();
        let (update_tx, _update_rx) = std::sync::mpsc::channel();
        e.set_analog_ipc(snapshot.clone(), update_tx);

        // Inactive until the source marks it active.
        let (resp, _) = e.apply(Request::GetAnalogState, "t");
        if let tartarust_ipc::Response::AnalogState(s) = resp {
            assert!(!s.analog_active);
            assert!(s.pressures.is_empty());
        } else {
            panic!("expected AnalogState");
        }

        // Simulate the source marking active + publishing a frame.
        snapshot.mark_active_for_test();
        snapshot.store_frame_for_test(5, 0x7F);

        let (resp, _) = e.apply(Request::GetAnalogState, "t");
        let tartarust_ipc::Response::AnalogState(s) = resp else {
            panic!("expected AnalogState");
        };
        assert!(s.analog_active);
        assert_eq!(s.pressures.len(), tartarust_protocol::ANALOG_KEY_COUNT);
        assert_eq!(s.pressures[5], 0x7F);
    }

    #[test]
    fn set_actuation_pushes_to_live_actuator_channel() {
        use crate::analog_source::AnalogSnapshot;
        let mut e = analog_engine();
        let snapshot = AnalogSnapshot::new();
        let (update_tx, update_rx) = std::sync::mpsc::channel();
        e.set_analog_ipc(snapshot, update_tx);

        let (resp, _) = e.apply(
            Request::SetActuation {
                index: 5,
                actuation_point: 0x40,
                release_point: 0x30,
                second_actuation_point: None,
                second_action: None,
            },
            "t",
        );
        assert_eq!(resp, Response::Ack);
        // The edit was pushed to the live actuator channel.
        let update = update_rx.try_recv().expect("an ActuationUpdate was sent");
        assert_eq!(update.index, 5);
        assert_eq!(update.key.actuation_point, 0x40);
        assert_eq!(update.key.key, "KEY_Q");
    }

    /// Save semantics: unsaved state is a live DIFF against the last-saved
    /// config (not a sticky flag), so a mutating edit lights it, a manual revert
    /// back to the saved value clears it, and `SaveProfile` is a pure ack (the
    /// daemon loop does the disk write and calls `mark_saved` — `apply` itself
    /// never touches the saved snapshot).
    #[test]
    fn unsaved_is_a_saved_config_diff_and_manual_revert_clears_it() {
        let mut e = Engine::new(cfg(vec![], vec![]));
        let bind = |action: Option<BindAction>| Request::SetBind {
            profile: "default".into(),
            layer: Layer::Base,
            key: "KEY_A".into(),
            action,
        };

        // Fresh engine: clean, status reports no unsaved changes.
        assert!(!e.is_dirty());
        let (Response::Status(s), _) = e.apply(Request::GetStatus, "v") else {
            panic!("expected status");
        };
        assert!(!s.unsaved_changes);

        // A mutating edit diverges the config from the saved snapshot.
        e.apply(bind(Some(BindAction::Disable)), "v");
        assert!(e.is_dirty());
        let (Response::Status(s), _) = e.apply(Request::GetStatus, "v") else {
            panic!("expected status");
        };
        assert!(s.unsaved_changes, "status must surface pending edits");

        // Manually reverting the edit restores the saved state: clean again.
        e.apply(bind(None), "v");
        assert!(!e.is_dirty(), "a reverted edit must not read as unsaved");

        // SaveProfile acks with no device commands and does NOT itself record
        // the save — the loop calls mark_saved only after a successful persist.
        e.apply(bind(Some(BindAction::Disable)), "v");
        let (resp, cmds) = e.apply(Request::SaveProfile { name: None }, "v");
        assert_eq!(resp, Response::Ack);
        assert!(cmds.is_empty());
        assert!(e.is_dirty());

        e.mark_saved();
        assert!(!e.is_dirty());
    }

    /// `DuplicateProfile` deep-copies binds/lighting/thumb mode under the new
    /// name, drops the app link (auto-switch must stay unambiguous), and rejects
    /// a missing source or an existing target.
    #[test]
    fn duplicate_profile_deep_copies_but_not_the_app_link() {
        let mut config = cfg(vec![("KEY_A", BindAction::Disable)], vec![]);
        let p = config.profiles.get_mut("default").unwrap();
        p.linked_app = Some("game.exe".into());
        p.thumb_axis_mode = ThumbAxisMode::Four;
        let mut e = Engine::new(config);

        let (resp, cmds) = e.apply(
            Request::DuplicateProfile {
                from: "default".into(),
                to: "copy".into(),
            },
            "v",
        );
        assert_eq!(resp, Response::Ack);
        assert!(cmds.is_empty());

        let (Response::Profile(copy), _) = e.apply(
            Request::GetProfile {
                name: "copy".into(),
            },
            "v",
        ) else {
            panic!("expected the copied profile");
        };
        assert_eq!(copy.name, "copy");
        assert_eq!(copy.base.get("KEY_A"), Some(&BindAction::Disable));
        assert_eq!(copy.thumb_axis_mode, ThumbAxisMode::Four);
        assert_eq!(copy.linked_app, None, "app link must not be copied");

        // Source untouched.
        let (Response::Profile(src), _) = e.apply(
            Request::GetProfile {
                name: "default".into(),
            },
            "v",
        ) else {
            panic!("expected the source profile");
        };
        assert_eq!(src.linked_app.as_deref(), Some("game.exe"));

        // Missing source / existing target are rejected.
        assert!(matches!(
            e.apply(
                Request::DuplicateProfile {
                    from: "ghost".into(),
                    to: "x".into()
                },
                "v"
            )
            .0,
            Response::Error { .. }
        ));
        assert!(matches!(
            e.apply(
                Request::DuplicateProfile {
                    from: "default".into(),
                    to: "copy".into()
                },
                "v"
            )
            .0,
            Response::Error { .. }
        ));
    }

    #[test]
    fn export_bundles_the_profile_with_only_its_referenced_macros() {
        let mut config = cfg(
            vec![(
                "KEY_A",
                BindAction::Macro {
                    name: "used".into(),
                },
            )],
            vec![(
                "KEY_B",
                BindAction::Macro {
                    name: "ghost".into(),
                },
            )],
        );
        config.macros.insert(
            "used".into(),
            Macro {
                name: "used".into(),
                steps: vec![MacroStep::Delay { ms: 5 }],
            },
        );
        config.macros.insert(
            "unused".into(),
            Macro {
                name: "unused".into(),
                steps: vec![],
            },
        );
        let mut e = Engine::new(config);

        let (Response::ProfileExport(bundle), cmds) = e.apply(
            Request::ExportProfile {
                name: "default".into(),
            },
            "v",
        ) else {
            panic!("expected a profile bundle");
        };
        assert!(cmds.is_empty());
        assert_eq!(bundle.format, tartarust_ipc::PROFILE_EXPORT_FORMAT);
        assert_eq!(bundle.version, tartarust_ipc::PROFILE_EXPORT_VERSION);
        assert_eq!(bundle.profile.name, "default");
        // `unused` is not referenced; the hypershift bind's `ghost` does not
        // exist (the bind itself is carried, dangling, inside the profile).
        assert_eq!(
            bundle
                .macros
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            vec!["used"]
        );

        assert!(matches!(
            e.apply(
                Request::ExportProfile {
                    name: "ghost".into()
                },
                "v"
            )
            .0,
            Response::Error { .. }
        ));
    }

    #[test]
    fn import_roundtrips_an_exported_bundle_into_another_engine() {
        let mut config = cfg(
            vec![(
                "KEY_A",
                BindAction::Macro {
                    name: "burst".into(),
                },
            )],
            vec![],
        );
        config.macros.insert(
            "burst".into(),
            Macro {
                name: "burst".into(),
                steps: vec![MacroStep::Press {
                    key: "KEY_1".into(),
                }],
            },
        );
        let mut src = Engine::new(config);
        let (Response::ProfileExport(bundle), _) = src.apply(
            Request::ExportProfile {
                name: "default".into(),
            },
            "v",
        ) else {
            panic!("expected a profile bundle");
        };

        // A fresh engine has its own "default": import under a new name.
        let mut dst = Engine::new(cfg(vec![], vec![]));
        assert!(!dst.is_dirty());
        let (resp, cmds) = dst.apply(
            Request::ImportProfile {
                profile: bundle.profile.clone(),
                macros: bundle.macros.clone(),
                rename: Some("imported".into()),
                overwrite: false,
            },
            "v",
        );
        assert_eq!(resp, Response::Ack);
        assert!(
            cmds.is_empty(),
            "importing an inactive profile is device-quiet"
        );
        assert!(dst.is_dirty(), "an import is an unsaved edit");

        let (Response::Profile(got), _) = dst.apply(
            Request::GetProfile {
                name: "imported".into(),
            },
            "v",
        ) else {
            panic!("expected the imported profile");
        };
        let mut want = bundle.profile.clone();
        want.name = "imported".into();
        assert_eq!(got, want);
        let (Response::Macro(m), _) = dst.apply(
            Request::GetMacro {
                name: "burst".into(),
            },
            "v",
        ) else {
            panic!("expected the imported macro");
        };
        assert_eq!(m, bundle.macros[0]);
    }

    #[test]
    fn import_rejects_collisions_atomically() {
        let mut config = cfg(vec![], vec![]);
        config.macros.insert(
            "m".into(),
            Macro {
                name: "m".into(),
                steps: vec![MacroStep::Delay { ms: 1 }],
            },
        );
        let mut e = Engine::new(config);

        // Existing profile name without overwrite.
        assert!(matches!(
            e.apply(
                Request::ImportProfile {
                    profile: Profile::new("default"),
                    macros: vec![],
                    rename: None,
                    overwrite: false,
                },
                "v"
            )
            .0,
            Response::Error { .. }
        ));

        // A same-name macro with DIFFERENT steps is a conflict — and the
        // rejection must leave nothing behind.
        let (resp, _) = e.apply(
            Request::ImportProfile {
                profile: Profile::new("fresh"),
                macros: vec![Macro {
                    name: "m".into(),
                    steps: vec![MacroStep::Delay { ms: 99 }],
                }],
                rename: None,
                overwrite: false,
            },
            "v",
        );
        assert!(matches!(resp, Response::Error { .. }));
        assert!(
            matches!(
                e.apply(
                    Request::GetProfile {
                        name: "fresh".into()
                    },
                    "v"
                )
                .0,
                Response::Error { .. }
            ),
            "rejected import must not insert the profile"
        );
        let (Response::Macro(m), _) = e.apply(Request::GetMacro { name: "m".into() }, "v") else {
            panic!("expected the original macro");
        };
        assert_eq!(m.steps, vec![MacroStep::Delay { ms: 1 }]);
        assert!(!e.is_dirty(), "a rejected import must not dirty the config");

        // An IDENTICAL bundled macro is not a conflict.
        let (resp, _) = e.apply(
            Request::ImportProfile {
                profile: Profile::new("fresh"),
                macros: vec![Macro {
                    name: "m".into(),
                    steps: vec![MacroStep::Delay { ms: 1 }],
                }],
                rename: None,
                overwrite: false,
            },
            "v",
        );
        assert_eq!(resp, Response::Ack);
    }

    #[test]
    fn import_overwrite_replaces_and_relights_the_active_profile() {
        let mut e = Engine::new(cfg(vec![("KEY_A", BindAction::Disable)], vec![]));
        let mut incoming = Profile::new("default");
        incoming
            .base
            .insert("KEY_A".into(), key_action(&["KEY_F5"]));
        incoming.lighting = LightingState {
            effect: Effect::Static {
                color: Rgb::new(1, 2, 3),
            },
            brightness: 9,
        };

        let (resp, cmds) = e.apply(
            Request::ImportProfile {
                profile: incoming,
                macros: vec![],
                rename: None,
                overwrite: true,
            },
            "v",
        );
        assert_eq!(resp, Response::Ack);
        assert_eq!(
            cmds,
            vec![DeviceCmd::ApplyLighting(LightingState {
                effect: Effect::Static {
                    color: Rgb::new(1, 2, 3),
                },
                brightness: 9,
            })],
            "overwriting the ACTIVE profile must re-apply its lighting"
        );

        // The overwrite is live: the imported bind resolves on the next press.
        let down = e.handle_key(KeyCode::KEY_A, 1);
        assert_eq!(keys(&down), vec![(KeyCode::KEY_F5.0, 1)]);
    }

    /// Two same-name-different-steps macros WITHIN one bundle are malformed:
    /// rejected even with overwrite (last-one-wins would silently drop one).
    #[test]
    fn import_rejects_intra_bundle_macro_conflicts() {
        let mut e = Engine::new(cfg(vec![], vec![]));
        let twins = vec![
            Macro {
                name: "burst".into(),
                steps: vec![MacroStep::Delay { ms: 1 }],
            },
            Macro {
                name: "burst".into(),
                steps: vec![MacroStep::Delay { ms: 2 }],
            },
        ];
        for overwrite in [false, true] {
            let (resp, _) = e.apply(
                Request::ImportProfile {
                    profile: Profile::new("fresh"),
                    macros: twins.clone(),
                    rename: None,
                    overwrite,
                },
                "v",
            );
            assert!(matches!(resp, Response::Error { .. }));
        }
        // Nothing landed.
        assert!(matches!(
            e.apply(
                Request::GetMacro {
                    name: "burst".into()
                },
                "v"
            )
            .0,
            Response::Error { .. }
        ));
        assert!(!e.is_dirty());
    }

    /// An imported `linked_app` another profile already claims is cleared (the
    /// duplicate_profile ambiguity guard); a unique link is kept.
    #[test]
    fn import_clears_a_colliding_app_link_but_keeps_a_unique_one() {
        let mut config = cfg(vec![], vec![]);
        config.profiles.get_mut("default").unwrap().linked_app = Some("game".into());
        let mut e = Engine::new(config);

        let mut colliding = Profile::new("backup");
        colliding.linked_app = Some("game".into());
        assert_eq!(
            e.apply(
                Request::ImportProfile {
                    profile: colliding,
                    macros: vec![],
                    rename: None,
                    overwrite: false,
                },
                "v",
            )
            .0,
            Response::Ack
        );
        let (Response::Profile(got), _) = e.apply(
            Request::GetProfile {
                name: "backup".into(),
            },
            "v",
        ) else {
            panic!("expected the imported profile");
        };
        assert_eq!(got.linked_app, None, "colliding app link must be cleared");

        let mut unique = Profile::new("solo");
        unique.linked_app = Some("editor".into());
        e.apply(
            Request::ImportProfile {
                profile: unique,
                macros: vec![],
                rename: None,
                overwrite: false,
            },
            "v",
        );
        let (Response::Profile(got), _) = e.apply(
            Request::GetProfile {
                name: "solo".into(),
            },
            "v",
        ) else {
            panic!("expected the imported profile");
        };
        assert_eq!(got.linked_app.as_deref(), Some("editor"));
    }

    /// Bind keys get set_bind's boundary treatment: an unknown key name rejects
    /// the whole import (atomically), valid names import intact.
    #[test]
    fn import_validates_bind_key_names() {
        let mut e = Engine::new(cfg(vec![], vec![]));
        let mut bad = Profile::new("fresh");
        bad.hypershift
            .insert("KEY_BOGUS".into(), BindAction::Disable);
        let (resp, _) = e.apply(
            Request::ImportProfile {
                profile: bad,
                macros: vec![],
                rename: None,
                overwrite: false,
            },
            "v",
        );
        let Response::Error { message } = resp else {
            panic!("an unknown bind key must reject the import");
        };
        assert!(message.contains("KEY_BOGUS"));
        assert!(
            matches!(
                e.apply(
                    Request::GetProfile {
                        name: "fresh".into()
                    },
                    "v"
                )
                .0,
                Response::Error { .. }
            ),
            "rejected import must not insert the profile"
        );

        let mut good = Profile::new("fresh");
        good.base.insert("KEY_A".into(), key_action(&["KEY_F5"]));
        assert_eq!(
            e.apply(
                Request::ImportProfile {
                    profile: good,
                    macros: vec![],
                    rename: None,
                    overwrite: false,
                },
                "v",
            )
            .0,
            Response::Ack
        );
        let (Response::Profile(got), _) = e.apply(
            Request::GetProfile {
                name: "fresh".into(),
            },
            "v",
        ) else {
            panic!("expected the imported profile");
        };
        assert!(got.base.contains_key("KEY_A"));
    }
}

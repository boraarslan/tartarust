//! UI state model.
//!
//! The shared domain types (`Profile`, `BindAction`, `LightingState`, `Effect`,
//! `Rgb`, `Macro`, `MacroStep`, `Layer`, `DeviceInfo`) now come from
//! [`tartarust_ipc`] — they are the canonical, daemon-owned, on-the-wire schema.
//! This module re-exports them and adds the UI-only concerns the contract crate
//! deliberately doesn't carry:
//!
//!   * **Keypad geometry** — `KeyId` / `ThumbDir` / `WheelDir` describe the
//!     physical layout we render. The IPC schema keys binds by evdev name
//!     (`"KEY_W"`), so we keep a [`KeyId`]<->evdev-name map ([`KeyId::evdev`]).
//!   * [`AppState`] — a UI superset that holds the live (or last-known/mock)
//!     `Profile`, device + daemon status, an `offline` flag, and transient view
//!     state (selected key, edit layer, selected macro).

use std::collections::BTreeMap;

// Canonical shared types — these are the source of truth (tartarust-ipc).
pub use tartarust_ipc::model::{
    BindAction, DeviceInfo, Effect, Layer, LightingState, MAX_MACRO_DELAY_MS, Macro, MacroStep,
    Profile, Rgb, ThumbAxisMode, WaveDirection,
};
pub use tartarust_ipc::{DaemonStatus, ProfileSummary};

// ---------------------------------------------------------------------------
// Keypad geometry (UI-only) — what we draw and how it maps to evdev key names.
// ---------------------------------------------------------------------------

/// A physical input on the Tartarus Pro, by position. This is a *rendering*
/// identity; binds are stored against the evdev name from [`KeyId::evdev`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyId {
    /// Main grid key, 0-indexed row-major across the 19 keys in rows of 5/5/5/4.
    Grid(u8),
    /// One of the 8 thumbpad directions.
    Thumb(ThumbDir),
    /// Scroll wheel, up or down.
    Wheel(WheelDir),
    /// The large thumb / spacebar actuator.
    Space,
    /// The thumb button (emits `KEY_LEFTALT`); digital-only, no analog pressure byte.
    ThumbButton,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThumbDir {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl ThumbDir {
    pub const ALL: [ThumbDir; 8] = [
        ThumbDir::N,
        ThumbDir::NE,
        ThumbDir::E,
        ThumbDir::SE,
        ThumbDir::S,
        ThumbDir::SW,
        ThumbDir::W,
        ThumbDir::NW,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ThumbDir::N => "N",
            ThumbDir::NE => "NE",
            ThumbDir::E => "E",
            ThumbDir::SE => "SE",
            ThumbDir::S => "S",
            ThumbDir::SW => "SW",
            ThumbDir::W => "W",
            ThumbDir::NW => "NW",
        }
    }

    /// True for the four diagonal directions (NW/NE/SW/SE), which a `Four`-way
    /// [`ThumbAxisMode`] collapses away.
    pub fn is_diagonal(&self) -> bool {
        matches!(
            self,
            ThumbDir::NW | ThumbDir::NE | ThumbDir::SW | ThumbDir::SE
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WheelDir {
    Up,
    Down,
}

impl KeyId {
    /// The evdev key name this position emits on the stock device, or `None`
    /// when the input is not a bindable `KEY_*` (see the scroll wheel).
    ///
    /// TODO(integration): PLACEHOLDER, UNCONFIRMED against `discover-keys` on
    /// real hardware. The daemon ships an empty default profile, so there is no
    /// canonical map to mirror. remap-daemon will send the real position->name
    /// table once the user runs discover-keys; only this function changes then.
    /// Per remap-daemon's cautions (2026-06-21):
    ///   * GRID (19 keys): hardware-confirmed analog byte map (byte i == grid i).
    ///   * THUMBPAD (8 dirs): keycodes UNCERTAIN (arrows are a guess; could be
    ///     keypad or custom) — treat as especially provisional.
    ///   * SCROLL WHEEL: almost certainly emits `REL_WHEEL` (a relative axis on
    ///     the mouse interface), NOT a key — so it is likely NOT bindable as a
    ///     `KEY_*` in M1. We return `None` rather than fabricate a name.
    ///   * THUMB/SPACE: `KEY_SPACE` (high confidence).
    pub fn evdev(self) -> Option<&'static str> {
        let name = match self {
            KeyId::Grid(i) => GRID_EVDEV.get(i as usize).copied().unwrap_or("KEY_UNKNOWN"),
            KeyId::Thumb(d) => match d {
                ThumbDir::N => "KEY_UP",
                ThumbDir::S => "KEY_DOWN",
                ThumbDir::E => "KEY_RIGHT",
                ThumbDir::W => "KEY_LEFT",
                ThumbDir::NE => "KEY_PAGEUP",
                ThumbDir::NW => "KEY_HOME",
                ThumbDir::SE => "KEY_PAGEDOWN",
                ThumbDir::SW => "KEY_END",
            },
            // Scroll wheel: REL_WHEEL, not a key — not bindable as KEY_* (M1).
            KeyId::Wheel(_) => return None,
            KeyId::Space => "KEY_SPACE",
            KeyId::ThumbButton => "KEY_LEFTALT",
        };
        Some(name)
    }

    /// Whether this input can be remapped as a key bind (false for the wheel).
    pub fn is_bindable(self) -> bool {
        self.evdev().is_some()
    }

    /// Short label shown on the rendered key cap (the device legend, not the bind).
    pub fn cap(self) -> String {
        match self {
            KeyId::Grid(i) => GRID_LEGENDS
                .get(i as usize)
                .copied()
                .unwrap_or("•")
                .to_string(),
            KeyId::Thumb(d) => d.label().to_string(),
            KeyId::Wheel(WheelDir::Up) => "▲".to_string(),
            KeyId::Wheel(WheelDir::Down) => "▼".to_string(),
            KeyId::Space => "SPACE".to_string(),
            KeyId::ThumbButton => "ALT".to_string(),
        }
    }
}

/// Default per-key device legends (left-keyboard mirror). Display only.
/// 19 keys in rows of 5/5/5/4; numbers are the TOP ROW (no F-keys on hardware).
pub const GRID_LEGENDS: [&str; 19] = [
    "1", "2", "3", "4", "5", //
    "TAB", "Q", "W", "E", "R", //
    "CAPS", "A", "S", "D", "F", //
    "SHIFT", "Z", "X", "C",
];

/// Default evdev name each grid position emits. Matches the hardware-confirmed
/// analog byte map exactly: byte i == grid i for 0..18.
pub const GRID_EVDEV: [&str; 19] = [
    "KEY_1",
    "KEY_2",
    "KEY_3",
    "KEY_4",
    "KEY_5", //
    "KEY_TAB",
    "KEY_Q",
    "KEY_W",
    "KEY_E",
    "KEY_R", //
    "KEY_CAPSLOCK",
    "KEY_A",
    "KEY_S",
    "KEY_D",
    "KEY_F", //
    "KEY_LEFTSHIFT",
    "KEY_Z",
    "KEY_X",
    "KEY_C",
];

/// Per-row cap counts for the keypad renderer (rows of 5/5/5/4). Total = 19.
pub const GRID_ROWS: [usize; 4] = [5, 5, 5, 4];

// ---------------------------------------------------------------------------
// Bind presentation helpers — render `BindAction` on caps + in the editor.
// ---------------------------------------------------------------------------

/// Human-readable value for a bind, shown on the cap + in the editor.
pub fn bind_value_label(action: Option<&BindAction>) -> String {
    match action {
        None | Some(BindAction::Passthrough) => "Default".into(),
        Some(BindAction::Key { keys }) => keys
            .iter()
            .map(|k| key_label(k))
            .collect::<Vec<_>>()
            .join(" + "),
        Some(BindAction::MouseButton { button }) => key_label(button),
        Some(BindAction::Macro { name }) => name.clone(),
        Some(BindAction::SwitchProfile { name }) => name.clone(),
        Some(BindAction::LaunchProgram { command }) => command.first().cloned().unwrap_or_default(),
        Some(BindAction::Hypershift) => "Hypershift".into(),
        Some(BindAction::Disable) => "Disabled".into(),
    }
}

/// The label a Customize cap shows for its CURRENT function — always reflecting
/// what the key does now, so the cap is self-describing:
///   * bound (non-passthrough) → the bound value (`bind_value_label`),
///   * an unbound thumb CARDINAL → its default arrow glyph (the thumb stick
///     defaults to the arrow keys), so N/E/S/W read as ↑/→/↓/←,
///   * any other unbound key → empty (its default IS its legend, so the cap's
///     own legend already says it — no clutter).
pub fn cap_function(key: KeyId, action: Option<&BindAction>) -> String {
    match action {
        Some(a) if !matches!(a, BindAction::Passthrough) => bind_value_label(Some(a)),
        _ => match key {
            KeyId::Thumb(ThumbDir::N) => "↑".into(),
            KeyId::Thumb(ThumbDir::E) => "→".into(),
            KeyId::Thumb(ThumbDir::S) => "↓".into(),
            KeyId::Thumb(ThumbDir::W) => "←".into(),
            _ => String::new(),
        },
    }
}

/// Strip the `KEY_`/`BTN_` prefix and title-case for display.
pub fn pretty_key(name: &str) -> String {
    let stripped = name
        .strip_prefix("KEY_")
        .or_else(|| name.strip_prefix("BTN_"))
        .unwrap_or(name);
    let mut chars = stripped.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => stripped.to_string(),
    }
}

// ---------------------------------------------------------------------------
// evdev key catalog — single source of truth for the key picker AND for the
// human-readable labels shown on caps / in the editor.
//
// Every `evdev` string below parses via `evdev` 0.13 `KeyCode::from_str`. The
// catalog drives three things: the categorized picker UI, the `evdev -> label`
// lookup (`key_label`), and the picker's fuzzy filter (`search_index`).
// ---------------------------------------------------------------------------

/// One bindable input: its canonical evdev name + a glyph-forward US label.
pub struct KeyEntry {
    pub evdev: &'static str,
    pub label: &'static str,
}

/// A named group of [`KeyEntry`]s, as rendered in the picker.
pub struct KeyCategory {
    pub name: &'static str,
    pub keys: &'static [KeyEntry],
}

/// Shorthand for a `KeyEntry` literal.
const fn ke(evdev: &'static str, label: &'static str) -> KeyEntry {
    KeyEntry { evdev, label }
}

static LETTERS: &[KeyEntry] = &[
    ke("KEY_A", "A"),
    ke("KEY_B", "B"),
    ke("KEY_C", "C"),
    ke("KEY_D", "D"),
    ke("KEY_E", "E"),
    ke("KEY_F", "F"),
    ke("KEY_G", "G"),
    ke("KEY_H", "H"),
    ke("KEY_I", "I"),
    ke("KEY_J", "J"),
    ke("KEY_K", "K"),
    ke("KEY_L", "L"),
    ke("KEY_M", "M"),
    ke("KEY_N", "N"),
    ke("KEY_O", "O"),
    ke("KEY_P", "P"),
    ke("KEY_Q", "Q"),
    ke("KEY_R", "R"),
    ke("KEY_S", "S"),
    ke("KEY_T", "T"),
    ke("KEY_U", "U"),
    ke("KEY_V", "V"),
    ke("KEY_W", "W"),
    ke("KEY_X", "X"),
    ke("KEY_Y", "Y"),
    ke("KEY_Z", "Z"),
];

static NUMBERS: &[KeyEntry] = &[
    ke("KEY_1", "1"),
    ke("KEY_2", "2"),
    ke("KEY_3", "3"),
    ke("KEY_4", "4"),
    ke("KEY_5", "5"),
    ke("KEY_6", "6"),
    ke("KEY_7", "7"),
    ke("KEY_8", "8"),
    ke("KEY_9", "9"),
    ke("KEY_0", "0"),
];

static FUNCTION: &[KeyEntry] = &[
    ke("KEY_F1", "F1"),
    ke("KEY_F2", "F2"),
    ke("KEY_F3", "F3"),
    ke("KEY_F4", "F4"),
    ke("KEY_F5", "F5"),
    ke("KEY_F6", "F6"),
    ke("KEY_F7", "F7"),
    ke("KEY_F8", "F8"),
    ke("KEY_F9", "F9"),
    ke("KEY_F10", "F10"),
    ke("KEY_F11", "F11"),
    ke("KEY_F12", "F12"),
    ke("KEY_F13", "F13"),
    ke("KEY_F14", "F14"),
    ke("KEY_F15", "F15"),
    ke("KEY_F16", "F16"),
    ke("KEY_F17", "F17"),
    ke("KEY_F18", "F18"),
    ke("KEY_F19", "F19"),
    ke("KEY_F20", "F20"),
    ke("KEY_F21", "F21"),
    ke("KEY_F22", "F22"),
    ke("KEY_F23", "F23"),
    ke("KEY_F24", "F24"),
];

static MODIFIERS: &[KeyEntry] = &[
    ke("KEY_LEFTCTRL", "Left Ctrl"),
    ke("KEY_RIGHTCTRL", "Right Ctrl"),
    ke("KEY_LEFTSHIFT", "Left Shift"),
    ke("KEY_RIGHTSHIFT", "Right Shift"),
    ke("KEY_LEFTALT", "Left Alt"),
    ke("KEY_RIGHTALT", "Right Alt (AltGr)"),
    ke("KEY_LEFTMETA", "Left Super (Win)"),
    ke("KEY_RIGHTMETA", "Right Super (Win)"),
    ke("KEY_CAPSLOCK", "Caps Lock"),
    ke("KEY_FN", "Fn"),
];

static NAVIGATION: &[KeyEntry] = &[
    ke("KEY_UP", "Up Arrow"),
    ke("KEY_DOWN", "Down Arrow"),
    ke("KEY_LEFT", "Left Arrow"),
    ke("KEY_RIGHT", "Right Arrow"),
];

static EDITING: &[KeyEntry] = &[
    ke("KEY_HOME", "Home"),
    ke("KEY_END", "End"),
    ke("KEY_PAGEUP", "Page Up"),
    ke("KEY_PAGEDOWN", "Page Down"),
    ke("KEY_INSERT", "Insert"),
    ke("KEY_DELETE", "Delete"),
    ke("KEY_ESC", "Esc"),
    ke("KEY_TAB", "Tab"),
    ke("KEY_ENTER", "Enter"),
    ke("KEY_SPACE", "Space"),
    ke("KEY_BACKSPACE", "Backspace"),
    ke("KEY_SYSRQ", "Print Screen"),
    ke("KEY_SCROLLLOCK", "Scroll Lock"),
    ke("KEY_PAUSE", "Pause / Break"),
    ke("KEY_COMPOSE", "Menu (Context)"),
    ke("KEY_MENU", "Menu"),
];

static SYMBOLS: &[KeyEntry] = &[
    ke("KEY_MINUS", "- Minus"),
    ke("KEY_EQUAL", "= Equals"),
    ke("KEY_LEFTBRACE", "[ Left Bracket"),
    ke("KEY_RIGHTBRACE", "] Right Bracket"),
    ke("KEY_BACKSLASH", "\\ Backslash"),
    ke("KEY_SEMICOLON", "; Semicolon"),
    ke("KEY_APOSTROPHE", "' Apostrophe"),
    ke("KEY_GRAVE", "` Backtick"),
    ke("KEY_COMMA", ", Comma"),
    ke("KEY_DOT", ". Period"),
    ke("KEY_SLASH", "/ Slash"),
    ke("KEY_102ND", "< > (ISO key)"),
];

static NUMPAD: &[KeyEntry] = &[
    ke("KEY_NUMLOCK", "Num Lock"),
    ke("KEY_KP0", "Numpad 0"),
    ke("KEY_KP1", "Numpad 1"),
    ke("KEY_KP2", "Numpad 2"),
    ke("KEY_KP3", "Numpad 3"),
    ke("KEY_KP4", "Numpad 4"),
    ke("KEY_KP5", "Numpad 5"),
    ke("KEY_KP6", "Numpad 6"),
    ke("KEY_KP7", "Numpad 7"),
    ke("KEY_KP8", "Numpad 8"),
    ke("KEY_KP9", "Numpad 9"),
    ke("KEY_KPDOT", "Numpad ."),
    ke("KEY_KPENTER", "Numpad Enter"),
    ke("KEY_KPPLUS", "Numpad +"),
    ke("KEY_KPMINUS", "Numpad -"),
    ke("KEY_KPASTERISK", "Numpad *"),
    ke("KEY_KPSLASH", "Numpad /"),
    ke("KEY_KPEQUAL", "Numpad ="),
    ke("KEY_KPCOMMA", "Numpad ,"),
];

static MEDIA: &[KeyEntry] = &[
    ke("KEY_MUTE", "Mute"),
    ke("KEY_VOLUMEUP", "Volume Up"),
    ke("KEY_VOLUMEDOWN", "Volume Down"),
    ke("KEY_PLAYPAUSE", "Play/Pause"),
    ke("KEY_STOPCD", "Stop"),
    ke("KEY_NEXTSONG", "Next Track"),
    ke("KEY_PREVIOUSSONG", "Previous Track"),
    ke("KEY_PLAYCD", "Play"),
    ke("KEY_PAUSECD", "Pause (Media)"),
    ke("KEY_RECORD", "Record"),
    ke("KEY_REWIND", "Rewind"),
    ke("KEY_FASTFORWARD", "Fast Forward"),
    ke("KEY_EJECTCD", "Eject"),
];

static SYSTEM: &[KeyEntry] = &[
    ke("KEY_POWER", "Power"),
    ke("KEY_SLEEP", "Sleep"),
    ke("KEY_WAKEUP", "Wake"),
    ke("KEY_BRIGHTNESSUP", "Brightness Up"),
    ke("KEY_BRIGHTNESSDOWN", "Brightness Down"),
    ke("KEY_CALC", "Calculator"),
    ke("KEY_MAIL", "Mail"),
    ke("KEY_HOMEPAGE", "Browser Home"),
    ke("KEY_WWW", "Browser / Web"),
    ke("KEY_SEARCH", "Search"),
    ke("KEY_FILE", "File Manager"),
    ke("KEY_CONFIG", "Settings"),
    ke("KEY_PRINT", "Print"),
    ke("KEY_COFFEE", "Lock Screen"),
    ke("KEY_CUT", "Cut"),
    ke("KEY_COPY", "Copy"),
    ke("KEY_PASTE", "Paste"),
    ke("KEY_UNDO", "Undo"),
    ke("KEY_REDO", "Redo"),
    ke("KEY_FIND", "Find"),
    ke("KEY_AGAIN", "Again / Repeat"),
    ke("KEY_HELP", "Help"),
];

static MOUSE: &[KeyEntry] = &[
    ke("BTN_LEFT", "Left Click"),
    ke("BTN_RIGHT", "Right Click"),
    ke("BTN_MIDDLE", "Middle Click"),
    ke("BTN_SIDE", "Mouse Side"),
    ke("BTN_EXTRA", "Mouse Extra"),
    ke("BTN_FORWARD", "Mouse Forward"),
    ke("BTN_BACK", "Mouse Back"),
    ke("BTN_TASK", "Mouse Task"),
];

/// The full catalog of bindable inputs, grouped for the picker. This is the
/// single source of truth: [`key_label`] and the picker's [`search_index`] both
/// derive from it.
pub static KEY_CATALOG: &[KeyCategory] = &[
    KeyCategory {
        name: "Letters",
        keys: LETTERS,
    },
    KeyCategory {
        name: "Numbers",
        keys: NUMBERS,
    },
    KeyCategory {
        name: "Function",
        keys: FUNCTION,
    },
    KeyCategory {
        name: "Modifiers",
        keys: MODIFIERS,
    },
    KeyCategory {
        name: "Navigation",
        keys: NAVIGATION,
    },
    KeyCategory {
        name: "Editing",
        keys: EDITING,
    },
    KeyCategory {
        name: "Symbols",
        keys: SYMBOLS,
    },
    KeyCategory {
        name: "Numpad",
        keys: NUMPAD,
    },
    KeyCategory {
        name: "Media",
        keys: MEDIA,
    },
    KeyCategory {
        name: "System",
        keys: SYSTEM,
    },
    KeyCategory {
        name: "Mouse",
        keys: MOUSE,
    },
];

/// `evdev name -> label`, built once from [`KEY_CATALOG`].
fn label_map() -> &'static std::collections::HashMap<&'static str, &'static str> {
    static MAP: std::sync::OnceLock<std::collections::HashMap<&'static str, &'static str>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        KEY_CATALOG
            .iter()
            .flat_map(|c| c.keys.iter())
            .map(|e| (e.evdev, e.label))
            .collect()
    })
}

/// Human-readable label for an evdev name. Falls back to [`pretty_key`] for
/// names not in the catalog (e.g. raw `discover-keys` output).
pub fn key_label(name: &str) -> String {
    label_map()
        .get(name)
        .map_or_else(|| pretty_key(name), |l| l.to_string())
}

/// Lowercased search haystacks for the picker filter, built once. Each tuple is
/// `(haystack, entry, category name)` where the haystack is
/// `lowercase(label + " " + evdev-without-KEY_/BTN_-prefix)`.
pub(crate) fn search_index() -> &'static Vec<(String, &'static KeyEntry, &'static str)> {
    static INDEX: std::sync::OnceLock<Vec<(String, &'static KeyEntry, &'static str)>> =
        std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        KEY_CATALOG
            .iter()
            .flat_map(|c| c.keys.iter().map(move |e| (e, c.name)))
            .map(|(e, cat)| {
                let bare = e
                    .evdev
                    .strip_prefix("KEY_")
                    .or_else(|| e.evdev.strip_prefix("BTN_"))
                    .unwrap_or(e.evdev);
                let haystack = format!("{} {}", e.label, bare).to_lowercase();
                (haystack, e, cat)
            })
            .collect()
    })
}

// ---------------------------------------------------------------------------
// Analog calibration note.
//
// Live analog state (per-key pressures + actuation config) is NOT held in
// `AppState`: the Calibration screen polls `GetAnalogState` on a short interval
// into its own local signal (see `screens/calibration.rs`), so a ~100ms pressure
// tick can't repaint the whole shell. The shared wire types live in `tartarust_ipc`
// (`AnalogState` / `AnalogKeyConfig`) — the device reports RAW pressure 0..255
// (there is no pressure->mm curve), and offline the screen shows an honest
// "daemon offline" state rather than fabricated bars.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// AppState — UI superset over the live/last-known daemon state.
// ---------------------------------------------------------------------------

/// The single global UI store, provided as `Signal<AppState>` at the root.
///
/// Holds the active `Profile` (binds + lighting live inside it), the list of
/// profile summaries, device/daemon status, every macro, an `offline` flag, and
/// transient view state. When the daemon is reachable the IPC client overlays
/// live state; when it isn't, the mock defaults render and `offline` is set.
#[derive(Clone, Debug, PartialEq)]
pub struct AppState {
    /// True when the daemon socket is missing/unreachable. Drives the badge and
    /// makes mutations no-ops (they'd fail anyway).
    pub offline: bool,
    /// Last status from `GetStatus` (protocol/daemon version, active profile).
    /// `device_connected` here is a *startup cache*, not live hotplug state.
    pub status: Option<DaemonStatus>,
    /// Last `GetDeviceInfo`.
    pub device: DeviceInfo,

    /// Profile summaries from `ListProfiles` (for the picker + Profiles list).
    pub profiles: Vec<ProfileSummary>,
    /// Index into `profiles` of the active one.
    pub active_profile: usize,
    /// The fully-loaded active profile (binds for both layers + lighting).
    pub profile: Profile,

    /// All macros (`ListMacros` + per-name fetch; mocked offline).
    pub macros: Vec<Macro>,

    // ---- transient view state ----
    /// Currently edited layer on the Customize screen.
    pub layer: Layer,
    /// Selected key, if any (drives the bind editor + calibration panels).
    pub selected: Option<KeyId>,
    /// Index of the macro open in the editor.
    pub selected_macro: usize,

    /// True while a profile switch + reload is in flight. Gates bind writes so a
    /// `SetBind` can't fire against the stale active-profile name mid-switch.
    pub switching: bool,
    /// Generation counter for profile switches: bumped when a switch starts, and
    /// each switch task captures the value it bumped to. Only the task whose
    /// generation is still current may clear `switching` or apply its
    /// `GetProfile` result, so an older overlapping switch finishing late can't
    /// re-open the bind gate or overwrite the newer switch's state.
    pub switch_gen: u64,
    /// True when there are session edits the daemon hasn't written to disk
    /// (mirrors `DaemonStatus::unsaved_changes`). Set optimistically on any
    /// dirtying mutation, cleared on a successful `SaveProfile`, and re-synced
    /// from the daemon on every state reload. Drives the Save affordance.
    pub unsaved: bool,
    /// Names of macros whose full body has been loaded from the daemon (or are
    /// local). Distinguishes a name-only stub from a genuinely-empty macro, so
    /// the editor can block edits until a live stub's body arrives (review #5).
    pub macros_loaded: std::collections::BTreeSet<String>,
}

impl AppState {
    /// The bind for a key on a layer, looked up by evdev name. `None` = unbound
    /// (or not a bindable key, e.g. the scroll wheel).
    pub fn bind_for(&self, key: KeyId, layer: Layer) -> Option<BindAction> {
        let name = key.evdev()?;
        self.profile.layer(layer).get(name).cloned()
    }

    /// Replace (or clear) a key's bind on the current edit layer, in-place.
    /// No-op for inputs that aren't bindable keys (the scroll wheel).
    pub fn set_bind(&mut self, key: KeyId, action: Option<BindAction>) {
        let Some(name) = key.evdev() else { return };
        let layer = self.layer;
        let map = self.profile.layer_mut(layer);
        match action {
            Some(a) => {
                map.insert(name.to_string(), a);
            }
            None => {
                map.remove(name);
            }
        }
    }

    /// Active profile name, for `SetBind`/`SetLighting` requests.
    pub fn active_profile_name(&self) -> String {
        self.profile.name.clone()
    }
}

// ---------------------------------------------------------------------------
// Mock state — the offline default. Repurposed from the M3 mock so the cage
// harness (which runs with NO daemon) renders believable screens.
// ---------------------------------------------------------------------------

/// Realistic offline state. Overlaid by live daemon state when reachable.
///
/// The `default` profile ships EMPTY — every key at its hardware default, no
/// binds and no second actions. Feature showcases belong in a separate `test`
/// profile (see `docs/examples/config.toml` and the live config), never in
/// `default`. Offline the mock only fully-loads the active profile's body, so
/// `test` appears in the picker but its demo binds live in those config files.
pub fn mock_state() -> AppState {
    let profile = Profile {
        name: "default".into(),
        linked_app: None,
        base: BTreeMap::new(),
        hypershift: BTreeMap::new(),
        lighting: LightingState {
            effect: Effect::Spectrum,
            brightness: 191, // ~75% of 255
        },
        thumb_axis_mode: ThumbAxisMode::default(),
    };

    let macros = vec![
        Macro {
            name: "Reload + Aim".into(),
            steps: vec![
                MacroStep::Press {
                    key: "KEY_R".into(),
                },
                MacroStep::Delay { ms: 40 },
                MacroStep::Release {
                    key: "KEY_R".into(),
                },
                MacroStep::Delay { ms: 120 },
                MacroStep::Press {
                    key: "BTN_RIGHT".into(),
                },
            ],
        },
        Macro {
            name: "Build Wall".into(),
            steps: vec![
                MacroStep::Press {
                    key: "KEY_Q".into(),
                },
                MacroStep::Delay { ms: 20 },
                MacroStep::Release {
                    key: "KEY_Q".into(),
                },
                MacroStep::Press {
                    key: "BTN_LEFT".into(),
                },
                MacroStep::Delay { ms: 20 },
                MacroStep::Release {
                    key: "BTN_LEFT".into(),
                },
            ],
        },
        Macro {
            name: "Spam F".into(),
            steps: vec![
                MacroStep::Press {
                    key: "KEY_F".into(),
                },
                MacroStep::Delay { ms: 30 },
                MacroStep::Release {
                    key: "KEY_F".into(),
                },
            ],
        },
    ];

    let profiles = vec![
        ProfileSummary {
            name: "default".into(),
            linked_app: None,
        },
        ProfileSummary {
            name: "test".into(),
            linked_app: Some("cs2.exe".into()),
        },
        ProfileSummary {
            name: "MMO Grind".into(),
            linked_app: Some("wow.exe".into()),
        },
        ProfileSummary {
            name: "Desktop".into(),
            linked_app: None,
        },
    ];

    // Mock macros are local/fully-loaded — mark them all as having bodies.
    let macros_loaded = macros.iter().map(|m| m.name.clone()).collect();

    AppState {
        offline: true,
        status: None,
        device: DeviceInfo {
            firmware: Some("1.05".into()),
            serial: Some("PM2148E000000".into()),
            connected: false,
        },
        profiles,
        active_profile: 0,
        profile,
        macros,
        layer: Layer::Base,
        selected: None,
        selected_macro: 0,
        switching: false,
        switch_gen: 0,
        unsaved: false,
        macros_loaded,
    }
}

/// Three-case index fix-up after `list.remove(removed)` for an index tracking a
/// position in that list: a removal BELOW the index shifts it down by one (an
/// overflow-only clamp would leave it pointing at the wrong, shifted item), and
/// an index past the new end clamps to the last item (0 when the list empties).
pub fn adjust_index_after_remove(index: &mut usize, removed: usize, new_len: usize) {
    if removed < *index {
        *index -= 1;
    } else if *index >= new_len {
        *index = new_len.saturating_sub(1);
    }
}

/// Pick a name that doesn't collide with `taken`: `base` if free, else
/// `"{base} 2"`, `"{base} 3"`, … until unique. Profiles and macros are both
/// addressed BY NAME on the daemon, so colliding names make the name-keyed
/// operations (rename / delete / the loaded-set) ambiguous — de-dupe on create.
pub fn unique_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| base.to_string())
}

/// Expand a leading `~/` (or bare `~`) to `$HOME`. The export/import dialogs
/// take typed paths, and `~` is how people write them; everything else passes
/// through untouched. With no `$HOME` the path is returned as-is.
pub fn expand_tilde(path: &str) -> std::path::PathBuf {
    // A set-but-empty $HOME would silently produce cwd-relative paths.
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        if path == "~" {
            return std::path::PathBuf::from(home);
        }
        if let Some(rest) = path.strip_prefix("~/") {
            // Strip extra slashes: `~//x` must stay under $HOME — a leading
            // `/` in `rest` would make `join` DISCARD the base entirely.
            return std::path::Path::new(&home).join(rest.trim_start_matches('/'));
        }
    }
    std::path::PathBuf::from(path)
}

/// The suggested destination for exporting `profile_name`:
/// `~/{name}.tartarust.toml`, with `/` (a path separator) made safe. Spaces
/// are legal in filenames and profile names commonly have them — kept.
pub fn default_export_path(profile_name: &str) -> String {
    let safe = profile_name.replace('/', "-");
    let stem = safe.trim();
    if stem.is_empty() {
        "~/profile.tartarust.toml".to_string()
    } else {
        format!("~/{stem}.tartarust.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_expands_home_prefix_only() {
        // The suite runs with $HOME set (dev shells / CI both have it).
        let home = std::env::var("HOME").expect("tests need $HOME");
        assert_eq!(expand_tilde("~"), std::path::PathBuf::from(&home));
        assert_eq!(
            expand_tilde("~/x/y.toml"),
            std::path::Path::new(&home).join("x/y.toml")
        );
        // A doubled slash must not escape $HOME (Path::join treats an absolute
        // right-hand side as a full replacement).
        assert_eq!(
            expand_tilde("~//x/y.toml"),
            std::path::Path::new(&home).join("x/y.toml")
        );
        // No expansion mid-path or for other prefixes.
        assert_eq!(
            expand_tilde("/tmp/~/x.toml"),
            std::path::PathBuf::from("/tmp/~/x.toml")
        );
        assert_eq!(
            expand_tilde("~user/x.toml"),
            std::path::PathBuf::from("~user/x.toml")
        );
    }

    #[test]
    fn default_export_path_is_home_relative_and_separator_safe() {
        assert_eq!(
            default_export_path("MMO Grind"),
            "~/MMO Grind.tartarust.toml"
        );
        assert_eq!(default_export_path("a/b"), "~/a-b.tartarust.toml");
        assert_eq!(default_export_path("  "), "~/profile.tartarust.toml");
    }
}

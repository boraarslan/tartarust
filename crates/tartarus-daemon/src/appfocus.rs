//! Per-application focus watcher (M4 auto-switch source).
//!
//! Watches the focused window's application class and feeds changes to the
//! engine as [`EngineMsg::FocusedApp`]. The engine decides whether to
//! auto-switch profiles (matching the class against each profile's
//! `linked_app`); this module is just the detection I/O.
//!
//! ## Design: a pull-based [`FocusSource`] trait
//!
//! Backends are swappable behind [`FocusSource`], a blocking pull interface:
//! `next()` returns the next focused-app class (an empty string means "no
//! window focused"), or `None` when the source is permanently done. One consume
//! loop ([`run_loop`]) does dedup + debounce + emit, so that logic is written
//! and tested once regardless of backend. This is what makes the matching logic
//! testable with a [`MockSource`] — no compositor required.
//!
//!   * [`HyprlandSource`] (when `HYPRLAND_INSTANCE_SIGNATURE` is set): reads the
//!     compositor's event socket (`.socket2.sock`) as plain newline text — no
//!     libwayland / hyprland-ipc crate. Yields the current focus once on connect
//!     (socket2 only delivers *changes*), then streams `activewindow>>` events,
//!     reconnecting internally if the socket drops (compositor restart).
//!   * [`X11Source`] (fallback): polls the active window's `WM_CLASS` via
//!     `xprop` once a second. Best effort; under pure-Wayland the X11 root may
//!     not track `_NET_ACTIVE_WINDOW`.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::ipc_server::EngineMsg;

/// Trailing-coalesce window for rapid focus changes. Short enough that a profile
/// switch still feels instant; long enough to swallow a fast tab-through.
const DEBOUNCE: Duration = Duration::from_millis(75);

/// A pull source of focused-app classes. `next` blocks until the focus changes
/// (or, for pollers, until the poll interval elapses) and returns the new app
/// class. An empty string means "no window focused". `None` means the source is
/// permanently finished (the consume loop then stops).
pub trait FocusSource {
    /// Block until the next focus state is known; return its app class.
    fn next(&mut self) -> Option<String>;
}

/// Which focus backend is available in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Hyprland event socket.
    Hyprland,
    /// X11 active-window polling.
    X11,
    /// No usable backend detected.
    None,
}

/// Detect the focus backend from the environment.
pub fn detect_backend() -> Backend {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        Backend::Hyprland
    } else if std::env::var_os("DISPLAY").is_some() {
        Backend::X11
    } else {
        Backend::None
    }
}

/// Spawn the focus watcher on a dedicated thread, picking the backend by
/// availability. Runs until the engine channel closes. A missing compositor is
/// logged, not fatal.
pub fn spawn(engine_tx: Sender<EngineMsg>) {
    let backend = detect_backend();
    tracing::info!(?backend, "app-focus watcher backend");
    std::thread::Builder::new()
        .name("tartarus-appfocus".into())
        .spawn(move || match backend {
            Backend::Hyprland => run_loop(HyprlandSource::new(), &engine_tx),
            Backend::X11 => run_loop(X11Source::default(), &engine_tx),
            Backend::None => {} // nothing to watch
        })
        .expect("spawning app-focus thread");
}

/// The single consume loop: pull classes from `source`, dedup (ignore unchanged
/// focus), debounce (trailing-coalesce rapid changes), and emit
/// [`EngineMsg::FocusedApp`]. Stops when the source ends or the engine channel
/// closes. This is the only place focus-event policy lives — tested via
/// `MockSource`.
///
/// Debounce: the (blocking) source is drained on a helper thread into a channel
/// so the loop can trailing-coalesce — after the first change it waits up to
/// [`DEBOUNCE`] for a rapidly-following change and keeps only the latest, so a
/// fast tab-through emits once instead of once per intermediate window.
pub fn run_loop<S: FocusSource + Send + 'static>(source: S, engine_tx: &Sender<EngineMsg>) {
    let rx = drain_source(source);
    let mut last_emitted: Option<String> = None;

    while let Ok(mut class) = rx.recv() {
        // Trailing-coalesce: swallow any further changes that arrive within the
        // debounce window, keeping the most recent.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(newer) => class = newer, // a newer change; keep coalescing
                Err(_) => break,            // timeout or source done
            }
        }

        if last_emitted.as_deref() == Some(class.as_str()) {
            continue; // unchanged focus
        }
        last_emitted = Some(class.clone());
        if engine_tx.send(EngineMsg::FocusedApp(class)).is_err() {
            return; // engine gone
        }
    }
}

/// Drain a blocking [`FocusSource`] into a channel on a helper thread, so the
/// consume loop can use `recv_timeout` for debounce without the source's
/// blocking `next()` stalling the window.
fn drain_source<S: FocusSource + Send + 'static>(
    mut source: S,
) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("tartarus-appfocus-src".into())
        .spawn(move || {
            while let Some(class) = source.next() {
                if tx.send(class).is_err() {
                    return; // consumer gone
                }
            }
        })
        .expect("spawning focus-source thread");
    rx
}

// ============================ Hyprland ============================

/// The Hyprland event-socket source.
pub struct HyprlandSource {
    /// Pending classes already parsed but not yet returned (the initial sample
    /// plus any line burst).
    reader: Option<BufReader<UnixStream>>,
    /// Whether we still owe the one-shot initial-focus sample after connecting.
    need_initial: bool,
}

impl HyprlandSource {
    /// Create an unconnected source; the first `next()` connects.
    pub fn new() -> Self {
        HyprlandSource {
            reader: None,
            need_initial: true,
        }
    }

    fn socket_path() -> Option<std::path::PathBuf> {
        let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
        Some(
            std::path::Path::new(&runtime)
                .join("hypr")
                .join(sig)
                .join(".socket2.sock"),
        )
    }

    /// (Re)connect, blocking with backoff until the socket is available.
    fn ensure_connected(&mut self) {
        while self.reader.is_none() {
            let Some(path) = Self::socket_path() else {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            match UnixStream::connect(&path) {
                Ok(stream) => {
                    tracing::debug!(path = %path.display(), "connected to Hyprland event socket");
                    self.reader = Some(BufReader::new(stream));
                    self.need_initial = true; // re-sample current focus on (re)connect
                }
                Err(err) => {
                    tracing::debug!(?err, "Hyprland socket connect failed; retrying");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }
}

impl Default for HyprlandSource {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusSource for HyprlandSource {
    fn next(&mut self) -> Option<String> {
        loop {
            self.ensure_connected();

            // Owe the initial sample? socket2 only delivers *changes*, so on
            // (re)connect we read the current focus once via hyprctl, so a daemon
            // that starts with a linked app focused switches immediately.
            if self.need_initial {
                self.need_initial = false;
                if let Some(class) = hyprctl_current_class() {
                    return Some(class);
                }
                // else fall through to read socket events
            }

            let reader = self.reader.as_mut().expect("connected above");
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    // EOF: compositor closed the socket (e.g. restart). Drop and
                    // reconnect on the next loop — do NOT return None (that would
                    // permanently kill auto-switch for the session).
                    tracing::debug!("Hyprland socket closed; reconnecting");
                    self.reader = None;
                    continue;
                }
                Ok(_) => {
                    if let Some(parsed) = parse_hyprland_line(line.trim_end()) {
                        return Some(parsed.unwrap_or_default());
                    }
                    // not an activewindow line; keep reading
                }
                Err(err) => {
                    tracing::debug!(?err, "Hyprland socket read error; reconnecting");
                    self.reader = None;
                    continue;
                }
            }
        }
    }
}

/// Parse one Hyprland `socket2` line into a focused-app class, if it is an
/// `activewindow` event. Returns `Some(Some(class))` on a focus change,
/// `Some(None)` when focus is cleared (`activewindow>>,`), or `None` for lines
/// we ignore. Line form: `activewindow>>CLASS,TITLE` — the class never contains
/// a comma (the title can), so we split on the FIRST comma. The
/// `activewindowv2>>ADDRESS` variant carries an address, not a class, and is
/// ignored (it doesn't match the `activewindow>>` prefix).
pub fn parse_hyprland_line(line: &str) -> Option<Option<String>> {
    let rest = line.strip_prefix("activewindow>>")?;
    let class = rest.split(',').next().unwrap_or("");
    if class.is_empty() {
        Some(None)
    } else {
        Some(Some(class.to_string()))
    }
}

/// The currently-focused window's class via `hyprctl -j activewindow`. `None`
/// if no window is focused or hyprctl fails.
fn hyprctl_current_class() -> Option<String> {
    let out = std::process::Command::new("hyprctl")
        .args(["-j", "activewindow"])
        .output()
        .ok()?;
    parse_hyprctl_class(&String::from_utf8_lossy(&out.stdout))
}

/// Extract the `"class"` value from `hyprctl -j activewindow` JSON (small string
/// scan — no serde_json dep needed here).
///
/// Takes the full quoted value, NOT splitting on `:`. A window class can contain
/// a colon (e.g. `foo:bar`); splitting on `:` would truncate it to `foo`,
/// disagreeing with the socket2 stream parser ([`parse_hyprland_line`], which
/// keeps the whole class). Both must agree or the initial-sample switch and the
/// streamed switches would target different profiles for the same window.
pub fn parse_hyprctl_class(json: &str) -> Option<String> {
    let after_key = json.split("\"class\"").nth(1)?;
    // Skip to the opening quote of the value (past the `:` and any whitespace),
    // then take everything up to the closing quote.
    let open = after_key.find('"')?;
    let rest = &after_key[open + 1..];
    let close = rest.find('"')?;
    let value = &rest[..close];
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

// ============================ X11 ============================

/// The X11 polling source: re-reads the active window's class each interval.
pub struct X11Source {
    interval: Duration,
    first: bool,
}

impl Default for X11Source {
    fn default() -> Self {
        X11Source {
            interval: Duration::from_secs(1),
            first: true,
        }
    }
}

impl FocusSource for X11Source {
    fn next(&mut self) -> Option<String> {
        // Sample immediately on the first call (so startup focus is seen), then
        // sleep between subsequent polls.
        if self.first {
            self.first = false;
        } else {
            std::thread::sleep(self.interval);
        }
        Some(x11_active_class().unwrap_or_default())
    }
}

/// Read the active window's class via `xprop`: `_NET_ACTIVE_WINDOW` -> the
/// window's `WM_CLASS` instance name (the field apps match on, mirroring
/// Hyprland's `class`).
fn x11_active_class() -> Option<String> {
    use std::process::Command;
    let out = Command::new("xprop")
        .args(["-root", "_NET_ACTIVE_WINDOW"])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&out.stdout);
    let id = line.rsplit(' ').next()?.trim();
    if id.is_empty() || id == "0x0" {
        return None;
    }
    let out = Command::new("xprop")
        .args(["-id", id, "WM_CLASS"])
        .output()
        .ok()?;
    parse_xprop_wm_class(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `WM_CLASS(STRING) = "instance", "Class"` -> the instance string.
pub fn parse_xprop_wm_class(s: &str) -> Option<String> {
    let after = s.split('=').nth(1)?;
    let first = after.split('"').nth(1)?;
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    // ---- pure parser coverage (kept from the original; real format checks) ----

    #[test]
    fn parses_hyprland_activewindow_class() {
        assert_eq!(
            parse_hyprland_line("activewindow>>brave-browser,Some Title - Brave"),
            Some(Some("brave-browser".to_string()))
        );
    }

    #[test]
    fn title_with_commas_does_not_break_class() {
        assert_eq!(
            parse_hyprland_line("activewindow>>code,file.rs, project, branch"),
            Some(Some("code".to_string()))
        );
    }

    #[test]
    fn empty_focus_clears() {
        assert_eq!(parse_hyprland_line("activewindow>>,"), Some(None));
    }

    #[test]
    fn ignores_non_activewindow_lines() {
        assert_eq!(parse_hyprland_line("workspace>>2"), None);
        assert_eq!(parse_hyprland_line("focusedmon>>DP-3,3"), None);
        assert_eq!(parse_hyprland_line("activewindowv2>>564139d5b0d0"), None);
    }

    #[test]
    fn parses_xprop_wm_class_instance() {
        assert_eq!(
            parse_xprop_wm_class(r#"WM_CLASS(STRING) = "brave-browser", "Brave-browser""#),
            Some("brave-browser".to_string())
        );
        assert_eq!(parse_xprop_wm_class("WM_CLASS:  not found."), None);
    }

    #[test]
    fn parses_hyprctl_json_class() {
        let json = r#"{"address":"0x1","class":"brave-browser","title":"x, y","focusHistoryID":0}"#;
        assert_eq!(parse_hyprctl_class(json), Some("brave-browser".to_string()));
        assert_eq!(parse_hyprctl_class(r#"{"class":"","title":""}"#), None);
        assert_eq!(parse_hyprctl_class("{}"), None);
    }

    #[test]
    fn hyprctl_and_socket2_agree_on_a_class_with_a_colon() {
        // Regression (#8): a class containing a ':' must NOT be truncated by the
        // hyprctl one-shot parser, so it matches what the socket2 stream parser
        // yields for the same window. Otherwise the initial-sample switch and the
        // streamed switches would target different profiles.
        let class = "org.kde.foo:bar";
        let json = format!(r#"{{"class":"{class}","title":"t"}}"#);
        let from_hyprctl = parse_hyprctl_class(&json);
        let from_socket2 =
            parse_hyprland_line(&format!("activewindow>>{class},Some Title")).flatten();
        assert_eq!(from_hyprctl.as_deref(), Some(class));
        assert_eq!(from_socket2.as_deref(), Some(class));
        assert_eq!(from_hyprctl, from_socket2, "parsers must agree");
    }

    // ---- consume-loop policy via a MOCK source (no compositor) ----

    /// A scripted focus source. `gap` is the delay returned before each item; a
    /// gap longer than `DEBOUNCE` makes each item a distinct settled focus (so
    /// dedup/matching is exercised), while a near-zero gap feeds a burst (so
    /// debounce coalescing is exercised).
    struct MockSource {
        items: std::vec::IntoIter<String>,
        gap: Duration,
    }

    impl MockSource {
        fn new(items: Vec<&str>, gap: Duration) -> Self {
            MockSource {
                items: items
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .into_iter(),
                gap,
            }
        }
    }

    impl FocusSource for MockSource {
        fn next(&mut self) -> Option<String> {
            std::thread::sleep(self.gap);
            self.items.next()
        }
    }

    /// Collect the `FocusedApp` classes emitted for a scripted focus sequence,
    /// with each item settled well past the debounce window.
    fn emitted_settled(script: Vec<&str>) -> Vec<String> {
        emitted(script, DEBOUNCE * 3)
    }

    fn emitted(script: Vec<&str>, gap: Duration) -> Vec<String> {
        let (tx, rx) = mpsc::channel::<EngineMsg>();
        // run_loop drains the source on a helper thread; run it on its own thread
        // and collect until the channel closes.
        let collector = std::thread::spawn(move || {
            rx.into_iter()
                .filter_map(|m| match m {
                    EngineMsg::FocusedApp(c) => Some(c),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        run_loop(MockSource::new(script, gap), &tx);
        drop(tx);
        collector.join().unwrap()
    }

    #[test]
    fn loop_dedups_consecutive_same_class() {
        // game,game,game collapses to one emit; term is distinct.
        let got = emitted_settled(vec!["game", "game", "game", "term"]);
        assert_eq!(got, vec!["game".to_string(), "term".to_string()]);
    }

    #[test]
    fn loop_emits_each_distinct_settled_change() {
        let got = emitted_settled(vec!["a", "b", "a", "c"]);
        assert_eq!(
            got,
            vec![
                "a".to_string(),
                "b".to_string(),
                "a".to_string(),
                "c".to_string()
            ]
        );
    }

    #[test]
    fn loop_propagates_empty_class_as_no_focus() {
        // Empty string ("no window focused") is a distinct state and is emitted;
        // the engine treats it as "no match" (no switch).
        let got = emitted_settled(vec!["game", "", "game"]);
        assert_eq!(
            got,
            vec!["game".to_string(), String::new(), "game".to_string()]
        );
    }

    #[test]
    fn loop_stops_when_source_ends() {
        assert!(emitted_settled(vec![]).is_empty());
    }

    #[test]
    fn loop_debounces_a_rapid_burst_to_the_last() {
        // A burst fed faster than the debounce window coalesces to the final
        // value. Generous tolerance: the gap (~0) is far below DEBOUNCE, so the
        // trailing-coalesce keeps only "final". (Not a tight-timing assertion —
        // it relies only on near-zero << 75ms.)
        let got = emitted(vec!["a", "b", "c", "final"], Duration::from_millis(1));
        assert_eq!(got, vec!["final".to_string()]);
    }
}

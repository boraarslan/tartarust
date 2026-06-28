//! Optional system-tray icon (StatusNotifierItem over D-Bus).
//!
//! While the daemon runs, this shows a tray icon whose menu can launch the
//! separate `tartarus-ui` binary ("Open UI") or shut the daemon down ("Quit").
//! Left-clicking the icon also launches the UI.
//!
//! Design constraints, because this daemon owns a grabbed USB device:
//!   * The tray is **best-effort**. [`spawn`] returns `None` (after logging a
//!     warning) if the D-Bus session bus is unreachable; a missing tray must
//!     never stop the daemon from running. We pass `assume_sni_available(true)`
//!     so that "no StatusNotifierWatcher yet" is a *soft* error — the daemon may
//!     start before the compositor, and the icon appears once a host registers
//!     (and survives a host restart, since `watcher_offline` defaults to `true`).
//!   * It runs on ksni's own zbus/tokio tasks; it never touches the device grab,
//!     the engine/reader/output threads, or the IPC server.
//!   * **Quit** does NOT call `process::exit` — it pokes the daemon's existing
//!     clean-shutdown path (NORMAL-mode restore + grab release) via a channel, so
//!     the keypad is never left grabbed or stuck in DRIVER mode.
//!   * **Open UI** launches `tartarus-ui` as a detached child. The daemon itself
//!     never opens a window.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ksni::{Tray, TrayMethods};
use tokio::sync::mpsc::UnboundedSender;

/// Name of the UI executable we launch, both as a sibling of the daemon exe and
/// as a bare `PATH` lookup fallback.
const UI_BINARY_NAME: &str = "tartarus-ui";

/// The tray model handed to ksni. Holds only the channel used to request the
/// daemon's clean shutdown; everything else is static.
pub struct TartarusTray {
    /// Sender into the daemon's shutdown select. "Quit" sends `()` here, which
    /// unblocks the same path SIGINT/SIGTERM take (NORMAL restore + grab release).
    quit_tx: UnboundedSender<()>,
}

impl Tray for TartarusTray {
    fn id(&self) -> String {
        "rusty-tartarus".into()
    }

    fn title(&self) -> String {
        "rusty-tartarus".into()
    }

    /// A themed icon name (freedesktop icon spec) — no embedded bytes. Gaming
    /// input device; falls back to a generic icon if the theme lacks it.
    fn icon_name(&self) -> String {
        "input-gaming".into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "rusty-tartarus".into(),
            description: "Razer Tartarus Pro daemon".into(),
            icon_name: "input-gaming".into(),
            ..Default::default()
        }
    }

    /// Left-click (Activate): launch the UI. The daemon opens no window itself.
    fn activate(&mut self, _x: i32, _y: i32) {
        launch_ui();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Open UI".into(),
                icon_name: "input-gaming".into(),
                activate: Box::new(|_tray: &mut Self| launch_ui()),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    // Drive the daemon's existing clean shutdown. If the receiver
                    // is already gone (daemon shutting down), ignore the error.
                    let _ = tray.quit_tx.send(());
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Spawn the tray service in the background. Returns the ksni [`Handle`] on
/// success (the caller must keep it alive for the daemon's lifetime), or `None`
/// if the tray could not be started — in which case the daemon keeps running
/// headless. Never panics on a tray error.
///
/// Must be called from within a tokio runtime (ksni's default `tokio` feature
/// spawns its service onto the ambient runtime).
///
/// [`Handle`]: ksni::Handle
pub async fn spawn(quit_tx: UnboundedSender<()>) -> Option<ksni::Handle<TartarusTray>> {
    let tray = TartarusTray { quit_tx };
    // `assume_sni_available(true)`: treat "no StatusNotifierWatcher" as a soft
    // error routed to `watcher_offline` (which keeps the service alive) instead
    // of failing the spawn — so the icon still appears if the daemon starts
    // before the desktop's tray host. A truly dead session bus still errors here.
    match tray.assume_sni_available(true).spawn().await {
        Ok(handle) => {
            tracing::info!("system tray registered (StatusNotifierItem)");
            Some(handle)
        }
        Err(err) => {
            tracing::warn!(
                ?err,
                "could not start the system tray (no D-Bus session bus?); continuing headless"
            );
            None
        }
    }
}

/// Launch `tartarus-ui` as a detached child. Best-effort: logs on failure and
/// never blocks or waits on the child.
fn launch_ui() {
    let bin = ui_binary_path();
    let mut cmd = Command::new(&bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Put the UI in its own process group so a signal sent to the daemon's group
    // (e.g. Ctrl-C in a shared terminal) does not also kill the window we just
    // launched. Safe wrapper — no `unsafe` (the crate forbids it).
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);

    match cmd.spawn() {
        Ok(child) => {
            tracing::info!(pid = child.id(), bin = %bin.display(), "launched tartarus-ui from tray");
            // Detached: never waited on. Dropping the handle leaves it running;
            // on daemon exit the child is reparented to init.
            drop(child);
        }
        Err(err) => {
            tracing::error!(?err, bin = %bin.display(), "failed to launch tartarus-ui from tray");
        }
    }
}

/// Resolve the `tartarus-ui` binary path: prefer a sibling of the running daemon
/// executable, else fall back to a bare name for a `PATH` lookup.
fn ui_binary_path() -> PathBuf {
    resolve_ui_binary(std::env::current_exe().ok().as_deref(), |p| p.is_file())
}

/// Pure resolver behind [`ui_binary_path`], split out so it is testable without
/// touching the real process or filesystem.
///
/// `current_exe` is the daemon's own path (if known); `exists` reports whether a
/// candidate file is present. Returns the sibling `tartarus-ui` next to the
/// daemon when it exists, otherwise the bare [`UI_BINARY_NAME`] (resolved against
/// `PATH` by [`Command`]).
fn resolve_ui_binary(current_exe: Option<&Path>, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if let Some(exe) = current_exe
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(UI_BINARY_NAME);
        if exists(&sibling) {
            return sibling;
        }
    }
    PathBuf::from(UI_BINARY_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_sibling_when_present() {
        let exe = Path::new("/opt/rusty-tartarus/bin/tartarus-daemon");
        let got = resolve_ui_binary(Some(exe), |p| {
            p == Path::new("/opt/rusty-tartarus/bin/tartarus-ui")
        });
        assert_eq!(got, PathBuf::from("/opt/rusty-tartarus/bin/tartarus-ui"));
    }

    #[test]
    fn falls_back_to_bare_name_when_sibling_absent() {
        let exe = Path::new("/usr/bin/tartarus-daemon");
        let got = resolve_ui_binary(Some(exe), |_| false);
        assert_eq!(got, PathBuf::from(UI_BINARY_NAME));
    }

    #[test]
    fn falls_back_to_bare_name_when_exe_unknown() {
        let got = resolve_ui_binary(None, |_| true);
        assert_eq!(got, PathBuf::from(UI_BINARY_NAME));
    }
}

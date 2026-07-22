//! Desktop integration for the *installed* binary: XDG desktop-entry + icon
//! install, and best-effort daemon autostart.
//!
//! Everything here is fire-and-forget infrastructure that runs on the main
//! thread before the Dioxus launch. The invariant: **no failure in this module
//! may ever block or kill the UI** — a read-only home, a missing daemon
//! binary, an unset `$HOME` all degrade to a `tracing` warning and the app's
//! honest offline mode.
//!
//! Two env gates (value must be exactly `1`):
//! * `TARTARUST_UI_NO_DESKTOP_INSTALL=1` — skip the entry/icon install.
//! * `TARTARUST_UI_NO_AUTOSTART=1` — skip the daemon probe/spawn. The headless
//!   cage screenshot harness sets BOTH: it runs the UI with no daemon on
//!   purpose, and auto-spawning one there would EVIOCGRAB the real keypad
//!   while the user types.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// Product id: `Icon=` name (and icon filename), state subdir (daemon.log).
const APP_ID: &str = "tartarust";

/// The window class the running app actually gets — the **binary name**, not
/// the window title. dioxus-desktop/tao pass no app id to GTK, so the Wayland
/// `app_id` / X11 `WM_CLASS` falls back to `g_get_prgname()` = argv[0]'s
/// basename. Desktop environments associate a window with its `.desktop` file
/// by `app_id == entry basename` (and `StartupWMClass`), so both MUST be this
/// exact string or the installed entry never matches the running window
/// (generic gear icon, broken dock grouping/pinning).
const WM_CLASS: &str = "tartarust-ui";

/// The daemon's binary name, for sibling/`PATH` resolution. Kept as a local
/// constant on purpose: the UI must not depend on daemon code (see
/// `tartarust-daemon/src/tray.rs`, which mirrors this for the reverse spawn).
const DAEMON_BIN_NAME: &str = "tartarust-daemon";

/// The scalable icon, embedded so the installed binary is self-contained.
const ICON_SVG: &str = include_str!("../assets/tartarust.svg");

const NO_DESKTOP_INSTALL_ENV: &str = "TARTARUST_UI_NO_DESKTOP_INSTALL";
const NO_AUTOSTART_ENV: &str = "TARTARUST_UI_NO_AUTOSTART";

/// `true` when the gate variable is set to exactly `1` (the documented form).
fn gated(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|v| v == "1")
}

// ---------------------------------------------------------------------------
// Desktop entry + icon
// ---------------------------------------------------------------------------

/// What [`write_if_changed`] did — surfaced so the `install-desktop` CLI can
/// report honestly and tests can count writes by outcome instead of mtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    Unchanged,
}

/// Install/refresh the desktop entry + icon. Runs on every UI launch; never
/// fatal (warnings only) and skipped when [`NO_DESKTOP_INSTALL_ENV`]`=1`.
pub fn ensure_desktop_integration() {
    if gated(NO_DESKTOP_INSTALL_ENV) {
        return;
    }
    match install_inputs() {
        Ok((base, exe)) => {
            if let Err(err) = refresh_desktop_files_at(&base, &exe) {
                tracing::warn!(%err, "desktop entry/icon install failed; continuing without");
            }
        }
        Err(why) => tracing::warn!(why, "skipping desktop entry install"),
    }
}

/// The explicit CLI form (`tartarust-ui install-desktop`): run the install —
/// the env gate deliberately does NOT apply, an explicit ask wins — print the
/// two target paths with what happened, and exit non-zero on failure. Never
/// opens a window (the caller returns before the Dioxus launch).
pub fn install_desktop_cli() {
    let (base, exe) = match install_inputs() {
        Ok(v) => v,
        Err(why) => {
            eprintln!("install-desktop: {why}");
            std::process::exit(1);
        }
    };
    match install_desktop_files_at(&base, &exe) {
        Ok(results) => {
            for (path, outcome) in results {
                let what = match outcome {
                    WriteOutcome::Written => "written",
                    WriteOutcome::Unchanged => "unchanged",
                };
                println!("{} ({what})", path.display());
            }
        }
        Err(err) => {
            eprintln!("install-desktop failed: {err}");
            std::process::exit(1);
        }
    }
}

/// The two runtime inputs the testable core is parameterised over: the XDG
/// data dir and our own absolute executable path (baked into `Exec=`).
fn install_inputs() -> Result<(PathBuf, PathBuf), &'static str> {
    let base = data_dir().ok_or("neither XDG_DATA_HOME nor HOME is set")?;
    let exe = std::env::current_exe().map_err(|_| "cannot resolve current_exe")?;
    Ok((base, exe))
}

/// Entry basename = [`WM_CLASS`], not [`APP_ID`] — that equality is what lets
/// GNOME/KDE match the running window to this file (see [`WM_CLASS`]).
fn entry_path(base: &Path) -> PathBuf {
    base.join("applications")
        .join(format!("{WM_CLASS}.desktop"))
}

fn icon_path(base: &Path) -> PathBuf {
    base.join("icons/hicolor/scalable/apps")
        .join(format!("{APP_ID}.svg"))
}

/// Testable core: write the desktop entry and icon under `base`
/// (`$XDG_DATA_HOME`-shaped), with `exe` as the `Exec=` target. Returns the
/// two `(path, outcome)` pairs in entry-then-icon order. Unconditional — this
/// is the explicit-CLI semantics; the launch path goes through
/// [`refresh_desktop_files_at`].
fn install_desktop_files_at(base: &Path, exe: &Path) -> io::Result<[(PathBuf, WriteOutcome); 2]> {
    let entry_path = entry_path(base);
    let icon_path = icon_path(base);
    let entry = write_if_changed(&entry_path, desktop_entry(exe).as_bytes())?;
    let icon = write_if_changed(&icon_path, ICON_SVG.as_bytes())?;
    Ok([(entry_path, entry), (icon_path, icon)])
}

/// Launch-time variant of [`install_desktop_files_at`]: the icon write is
/// unconditional (byte-compare no-op), but an entry whose `Exec=` points at a
/// DIFFERENT binary that still exists is left alone. Every ungated
/// `tartarust-ui` process runs this — including a `target/debug` build launched
/// by a dev daemon's tray — and blindly baking our own `current_exe()` into
/// `Exec=` would repoint the user's menu entry at a path that dies on the next
/// `cargo clean`. So we rewrite only when the entry is missing/unparseable
/// (claim it), already ours (format refreshes stay possible), or its target is
/// gone (the moved-binary self-heal this feature exists for). The explicit
/// `install-desktop` CLI keeps the unconditional semantics: an explicit ask wins.
fn refresh_desktop_files_at(base: &Path, exe: &Path) -> io::Result<()> {
    let entry = entry_path(base);
    if let Some(target) = existing_exec_target(&entry)
        && target != exe
        && target.is_file()
    {
        tracing::info!(
            entry = %entry.display(),
            target = %target.display(),
            "desktop entry points at another live install; not rewriting Exec"
        );
        write_if_changed(&icon_path(base), ICON_SVG.as_bytes())?;
        return Ok(());
    }
    install_desktop_files_at(base, exe)?;
    Ok(())
}

/// The program our previously-written entry launches: the `Exec=` line pushed
/// back through the inverse of [`exec_quote`]'s two layers. `None` for a
/// missing file or anything outside that grammar — nobody else writes this
/// basename, so unparseable means corrupt, and the caller reclaims it.
fn existing_exec_target(entry_path: &Path) -> Option<PathBuf> {
    let body = fs::read_to_string(entry_path).ok()?;
    let raw = body.lines().find_map(|l| l.strip_prefix("Exec="))?;
    exec_unquote(raw)
}

/// Write `contents` only when the on-disk bytes differ (or the file is
/// missing); creates parent dirs on the write path. The cheap no-op matters:
/// this runs on every launch, and gratuitous rewrites would churn mtimes and
/// make desktop environments rescan their menu/icon caches.
fn write_if_changed(path: &Path, contents: &[u8]) -> io::Result<WriteOutcome> {
    if fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(WriteOutcome::Unchanged);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(WriteOutcome::Written)
}

/// The `.desktop` file body. `Exec=` embeds the absolute path of the running
/// binary, so a moved/reinstalled binary self-heals on next launch (the
/// byte-compare in [`write_if_changed`] catches the change).
fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Tartarust\n\
         GenericName=Keypad configuration\n\
         Comment=Configure the Razer Tartarus Pro keypad\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Settings;HardwareSettings;\n\
         StartupWMClass={WM_CLASS}\n",
        exec = exec_quote(exe),
    )
}

/// Quote a program path for the `Exec=` key per the Desktop Entry spec.
///
/// Two escaping layers stack in that spec, applied here innermost-first:
/// 1. *Exec* layer — an argument containing whitespace or a reserved char
///    must be double-quoted, with `"`, `` ` ``, `$`, `\` backslash-escaped
///    inside the quotes; a literal `%` is a field-code introducer and becomes
///    `%%` (quoted or not).
/// 2. *Keyfile string* layer — the whole value is unescaped once as a keyfile
///    string, so every backslash from layer 1 must be doubled on disk, and
///    raw control chars become `\n`/`\t`/`\r` sequences.
///
/// A `cargo install` path is plain and passes through untouched; the layers
/// exist so a home dir with a space (or worse) still round-trips.
fn exec_quote(path: &Path) -> String {
    let raw = path.to_string_lossy();
    // The spec's reserved set: presence of any of these forces quoting.
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '\r', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#',
        '(', ')', '`',
    ];
    let mut exec_arg = String::with_capacity(raw.len() + 2);
    if raw.chars().any(|c| RESERVED.contains(&c)) {
        exec_arg.push('"');
        for c in raw.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                exec_arg.push('\\');
            }
            if c == '%' {
                exec_arg.push('%');
            }
            exec_arg.push(c);
        }
        exec_arg.push('"');
    } else {
        for c in raw.chars() {
            if c == '%' {
                exec_arg.push('%');
            }
            exec_arg.push(c);
        }
    }
    // Keyfile-string layer on top of the Exec-quoted form.
    let mut on_disk = String::with_capacity(exec_arg.len());
    for c in exec_arg.chars() {
        match c {
            '\\' => on_disk.push_str("\\\\"),
            '\n' => on_disk.push_str("\\n"),
            '\t' => on_disk.push_str("\\t"),
            '\r' => on_disk.push_str("\\r"),
            _ => on_disk.push(c),
        }
    }
    on_disk
}

/// Inverse of [`exec_quote`], for reading our own entry back (the foreign-
/// entry guard in [`refresh_desktop_files_at`]). Peels the same two layers in
/// reverse — keyfile unescape, then Exec unquote — and only accepts the
/// single-program grammar we emit: `None` on a dangling escape, an unpaired
/// quote, or a real `%X` field code (we only ever write `%%`).
fn exec_unquote(on_disk: &str) -> Option<PathBuf> {
    // Keyfile layer: `\\`/`\n`/`\t`/`\r` (+ `\s`, legal though we never emit it).
    let mut keyfile = String::with_capacity(on_disk.len());
    let mut chars = on_disk.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            keyfile.push(c);
            continue;
        }
        keyfile.push(match chars.next()? {
            '\\' => '\\',
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            's' => ' ',
            _ => return None,
        });
    }
    // Exec layer: one argument, double-quoted iff it contains reserved chars.
    let quoted = keyfile.starts_with('"');
    let inner = if quoted {
        keyfile.strip_prefix('"')?.strip_suffix('"')?
    } else {
        keyfile.as_str()
    };
    let mut path = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quoted => path.push(chars.next()?),
            '%' => match chars.next()? {
                '%' => path.push('%'),
                _ => return None,
            },
            _ => path.push(c),
        }
    }
    Some(PathBuf::from(path))
}

/// `$XDG_DATA_HOME`, else `~/.local/share`. `None` only when `HOME` is unset
/// too. Per the basedir spec a relative (or empty) value is invalid → fallback.
fn data_dir() -> Option<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_STATE_HOME`, else `~/.local/state` (daemon autostart log).
fn state_dir() -> Option<PathBuf> {
    xdg_dir("XDG_STATE_HOME", ".local/state")
}

fn xdg_dir(var: &str, home_suffix: &str) -> Option<PathBuf> {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => Some(p),
        _ => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home_suffix)),
    }
}

// ---------------------------------------------------------------------------
// Daemon autostart
// ---------------------------------------------------------------------------

/// Set once when THIS process spawned the daemon. Read by the app's initial
/// state load ([`crate::App`]) so only a launch we caused gets the retry
/// patience — when nothing was spawned (daemon deliberately absent, e.g. the
/// cage harness) the single probe keeps first render instant.
static DAEMON_SPAWNED: AtomicBool = AtomicBool::new(false);

pub fn daemon_was_spawned() -> bool {
    DAEMON_SPAWNED.load(Ordering::Relaxed)
}

/// If no daemon answers on the control socket, spawn one detached. Best-effort
/// throughout: a missing binary or failed spawn logs a warning and leaves the
/// UI in its honest offline mode. Skipped when [`NO_AUTOSTART_ENV`]`=1`.
pub fn ensure_daemon_running() {
    if gated(NO_AUTOSTART_ENV) {
        return;
    }
    if daemon_reachable() {
        return;
    }
    spawn_daemon_detached();
}

/// Sync liveness probe, run once on the main thread before the Dioxus launch.
/// No timeout machinery: a Unix-socket connect is local and immediate —
/// success requires a live listener, and a missing/stale socket fails with
/// ENOENT/ECONNREFUSED right away (nothing network-like ever blocks here).
fn daemon_reachable() -> bool {
    std::os::unix::net::UnixStream::connect(tartarust_ipc::socket_path()).is_ok()
}

fn spawn_daemon_detached() {
    let bin = resolve_daemon_bin(std::env::current_exe().ok().as_deref(), |p| p.is_file());

    let mut cmd = Command::new(&bin);
    cmd.arg("run").stdin(Stdio::null());

    // Daemon stdout+stderr append to a persistent log so a crash long after
    // the UI closed is still diagnosable. Two independent O_APPEND fds on the
    // same file interleave safely (atomic appends) — no fd cloning needed. If
    // the log can't be opened, spawn anyway with null IO: autostart beats logs.
    let log_path = state_dir().map(|d| d.join(APP_ID).join("daemon.log"));
    let log_dest = match log_path
        .as_deref()
        .and_then(|p| open_append(p).zip(open_append(p)))
    {
        Some((out, err)) => {
            cmd.stdout(out).stderr(err);
            log_path
                .as_deref()
                .map_or_else(|| "-".into(), |p| p.display().to_string())
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
            "(none: state dir unavailable)".to_string()
        }
    };

    // Own process group: the daemon must survive the UI exiting and must not
    // receive signals aimed at the UI's group (e.g. Ctrl-C in a shared
    // terminal). Safe wrapper, no `unsafe` — same convention as the daemon
    // tray's reverse spawn (tray.rs::launch_ui).
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);

    match cmd.spawn() {
        Ok(mut child) => {
            DAEMON_SPAWNED.store(true, Ordering::Relaxed);
            tracing::info!(
                pid = child.id(),
                bin = %bin.display(),
                log = %log_dest,
                "daemon was not reachable — spawned it detached"
            );
            // Reap, don't leak: its own process group or not, the daemon is
            // still OUR child, so an exit without a wait() leaves a zombie in
            // the UI's process table for the whole session — and exits are
            // expected (flock-race loser when a systemd unit was mid-start,
            // failed device grab without the udev rule, a later crash). A
            // detached thread parked in wait() reaps it the moment it dies;
            // no SIGCHLD machinery, and a healthy daemon just keeps the
            // thread blocked until the UI exits.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => {
            tracing::warn!(
                %err,
                bin = %bin.display(),
                "could not autostart the daemon; UI stays in offline mode"
            );
        }
    }
}

/// Open `path` for appending, creating parent dirs. `None` on any failure
/// (the caller degrades to null IO).
fn open_append(path: &Path) -> Option<fs::File> {
    fs::create_dir_all(path.parent()?).ok()?;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// Pure resolver behind [`spawn_daemon_detached`], split out for tests.
/// Mirrors the daemon tray's `resolve_ui_binary` convention (deliberately not
/// shared code — the UI must not import the daemon): prefer the sibling
/// `tartarust-daemon` next to our own executable (`cargo install --path .`
/// puts all the bins in one dir), else return the bare name and let
/// [`Command`] do the `PATH` lookup.
fn resolve_daemon_bin(current_exe: Option<&Path>, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if let Some(exe) = current_exe
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(DAEMON_BIN_NAME);
        if exists(&sibling) {
            return sibling;
        }
    }
    PathBuf::from(DAEMON_BIN_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// Minimal scoped tempdir (no `tempfile` dep): unique per pid+counter so
    /// parallel tests never collide, removed on drop. No env-var mutation
    /// anywhere in these tests — the `_at` core takes the base explicitly.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "tartarust-ui-desktop-test-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fresh_install_writes_entry_and_icon() {
        let tmp = TempDir::new();
        let exe = Path::new("/opt/bin/tartarust-ui");
        let [(entry_path, entry), (icon_path, icon)] =
            install_desktop_files_at(tmp.path(), exe).unwrap();
        assert_eq!(entry, WriteOutcome::Written);
        assert_eq!(icon, WriteOutcome::Written);
        // Basename + WMClass must equal the binary name (the real app_id);
        // the icon keeps the product id.
        assert_eq!(
            entry_path,
            tmp.path().join("applications/tartarust-ui.desktop")
        );
        assert_eq!(
            icon_path,
            tmp.path().join("icons/hicolor/scalable/apps/tartarust.svg")
        );
        let body = fs::read_to_string(&entry_path).unwrap();
        assert!(body.starts_with("[Desktop Entry]\n"));
        assert!(body.contains("Exec=/opt/bin/tartarust-ui\n"));
        assert!(body.contains("Icon=tartarust\n"));
        assert!(body.contains("StartupWMClass=tartarust-ui\n"));
        assert_eq!(fs::read_to_string(&icon_path).unwrap(), ICON_SVG);
    }

    #[test]
    fn second_call_is_a_no_op() {
        let tmp = TempDir::new();
        let exe = Path::new("/opt/bin/tartarust-ui");
        install_desktop_files_at(tmp.path(), exe).unwrap();
        let [(_, entry), (_, icon)] = install_desktop_files_at(tmp.path(), exe).unwrap();
        assert_eq!(entry, WriteOutcome::Unchanged);
        assert_eq!(icon, WriteOutcome::Unchanged);
    }

    #[test]
    fn changed_exec_rewrites_entry_only() {
        let tmp = TempDir::new();
        install_desktop_files_at(tmp.path(), Path::new("/old/tartarust-ui")).unwrap();
        let [(entry_path, entry), (_, icon)] =
            install_desktop_files_at(tmp.path(), Path::new("/new/tartarust-ui")).unwrap();
        assert_eq!(entry, WriteOutcome::Written);
        assert_eq!(icon, WriteOutcome::Unchanged);
        let body = fs::read_to_string(&entry_path).unwrap();
        assert!(body.contains("Exec=/new/tartarust-ui\n"));
        assert!(!body.contains("/old/"));
    }

    #[test]
    fn unwritable_base_errors_without_panicking() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new();
        let base = tmp.path().join("readonly");
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o555)).unwrap();
        let result = install_desktop_files_at(&base, Path::new("/opt/bin/tartarust-ui"));
        // Restore before asserting so TempDir::drop can clean up either way.
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn exec_quote_plain_path_is_untouched() {
        assert_eq!(
            exec_quote(Path::new("/home/user/.cargo/bin/tartarust-ui")),
            "/home/user/.cargo/bin/tartarust-ui"
        );
    }

    #[test]
    fn exec_quote_space_forces_quotes() {
        assert_eq!(
            exec_quote(Path::new("/opt/my apps/tartarust-ui")),
            "\"/opt/my apps/tartarust-ui\""
        );
    }

    #[test]
    fn exec_quote_escapes_reserved_chars_through_both_layers() {
        // `$` inside quotes: Exec layer -> `\$`, keyfile layer doubles the
        // backslash -> `\\$` on disk. A literal `%` becomes `%%`.
        assert_eq!(exec_quote(Path::new("/a dir/$x")), "\"/a dir/\\\\$x\"");
        assert_eq!(exec_quote(Path::new("/plain/100%cpu")), "/plain/100%%cpu");
    }

    #[test]
    fn exec_unquote_inverts_exec_quote() {
        for raw in [
            "/home/user/.cargo/bin/tartarust-ui",
            "/opt/my apps/tartarust-ui",
            "/a dir/$x",
            "/plain/100%cpu",
            "/weird/\"quo`te\"/bin\\x",
        ] {
            let path = Path::new(raw);
            assert_eq!(
                exec_unquote(&exec_quote(path)).as_deref(),
                Some(path),
                "round-trip failed for {raw:?}"
            );
        }
        // Not our grammar: a real field code, a dangling escape.
        assert_eq!(exec_unquote("/usr/bin/foo %f"), None);
        assert_eq!(exec_unquote("/usr/bin/foo\\"), None);
    }

    #[test]
    fn refresh_keeps_entry_owned_by_another_live_binary() {
        let tmp = TempDir::new();
        // "Installed" binary that exists on disk; the entry points at it.
        let installed = tmp.path().join("cargo-bin/tartarust-ui");
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, b"").unwrap();
        install_desktop_files_at(tmp.path(), &installed).unwrap();
        // A different (dev) build refreshing must NOT steal Exec=.
        refresh_desktop_files_at(tmp.path(), Path::new("/repo/target/debug/tartarust-ui")).unwrap();
        let body = fs::read_to_string(entry_path(tmp.path())).unwrap();
        assert!(body.contains("Exec="));
        assert!(!body.contains("target/debug"));
        assert_eq!(
            existing_exec_target(&entry_path(tmp.path())).as_deref(),
            Some(&*installed)
        );
    }

    #[test]
    fn refresh_self_heals_when_exec_target_is_gone() {
        let tmp = TempDir::new();
        // Entry points at a path that does not exist (binary moved/removed).
        install_desktop_files_at(tmp.path(), Path::new("/old/gone/tartarust-ui")).unwrap();
        let new_exe = tmp.path().join("bin/tartarust-ui");
        fs::create_dir_all(new_exe.parent().unwrap()).unwrap();
        fs::write(&new_exe, b"").unwrap();
        refresh_desktop_files_at(tmp.path(), &new_exe).unwrap();
        assert_eq!(
            existing_exec_target(&entry_path(tmp.path())).as_deref(),
            Some(&*new_exe)
        );
    }

    #[test]
    fn refresh_rewrites_own_entry_and_claims_missing_or_corrupt() {
        let tmp = TempDir::new();
        let exe = tmp.path().join("bin/tartarust-ui");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();
        // Missing entry -> written.
        refresh_desktop_files_at(tmp.path(), &exe).unwrap();
        assert_eq!(
            existing_exec_target(&entry_path(tmp.path())).as_deref(),
            Some(&*exe)
        );
        // Same exe but stale body (older format) -> refreshed, not skipped.
        fs::write(
            entry_path(tmp.path()),
            format!("[Desktop Entry]\nExec={}\n", exe.display()),
        )
        .unwrap();
        refresh_desktop_files_at(tmp.path(), &exe).unwrap();
        let body = fs::read_to_string(entry_path(tmp.path())).unwrap();
        assert!(body.contains("StartupWMClass=tartarust-ui\n"));
        // Unparseable Exec -> claimed (only we write this basename).
        fs::write(entry_path(tmp.path()), "[Desktop Entry]\nExec=broken %f\n").unwrap();
        refresh_desktop_files_at(tmp.path(), &exe).unwrap();
        assert_eq!(
            existing_exec_target(&entry_path(tmp.path())).as_deref(),
            Some(&*exe)
        );
    }

    #[test]
    fn resolve_daemon_prefers_existing_sibling() {
        let got = resolve_daemon_bin(Some(Path::new("/opt/bin/tartarust-ui")), |p| {
            p == Path::new("/opt/bin/tartarust-daemon")
        });
        assert_eq!(got, PathBuf::from("/opt/bin/tartarust-daemon"));
    }

    #[test]
    fn resolve_daemon_falls_back_to_path_lookup() {
        // Sibling missing -> bare name (Command resolves it against PATH).
        let got = resolve_daemon_bin(Some(Path::new("/opt/bin/tartarust-ui")), |_| false);
        assert_eq!(got, PathBuf::from(DAEMON_BIN_NAME));
        // No current_exe at all -> same fallback.
        let got = resolve_daemon_bin(None, |_| true);
        assert_eq!(got, PathBuf::from(DAEMON_BIN_NAME));
    }
}

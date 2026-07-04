//! `tartarus-daemon` binary — the single device owner + IPC server.
//!
//! Subcommands:
//!   * `run` — discover + grab the device, load config, run the remap engine
//!     AND the IPC server (the normal daemon mode).
//!   * `discover` — list the Tartarus input nodes (no grab; safe).
//!   * `discover-keys` — grab + print key events for building a config.
//!
//! Concurrency model: one tokio runtime hosts the IPC server and the signal
//! handler. The engine is the single state authority on its own OS thread,
//! consuming an `EngineMsg` channel fed by the input reader threads and the IPC
//! tasks. The uinput output device and the hidraw device each have their own
//! owner thread. Nothing shares the device behind a lock.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use evdev::{Device, EventSummary};

use tartarus_daemon::analog_source;
use tartarus_daemon::appfocus;
use tartarus_daemon::device;
use tartarus_daemon::engine::Engine;
use tartarus_daemon::input::{self, NodeKind};
use tartarus_daemon::ipc_server::{self, EngineMsg};
use tartarus_daemon::output;
use tartarus_daemon::pipeline;
use tartarus_daemon::state::{Config, InputMode};
use tartarus_daemon::tray;

const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set true while the device is in DRIVER (analog) mode. Read by the grab-safety
/// panic hook to decide whether it must send a best-effort NORMAL command before
/// `abort()` — `abort()` skips Drop, and unlike a grab, fd-close does NOT take
/// the device out of DRIVER mode (only an explicit NORMAL command does), so a
/// panic in analog mode would otherwise strand the keypad mute until replug.
static ANALOG_ARMED: AtomicBool = AtomicBool::new(false);

/// Razer Tartarus Pro remap + control daemon.
#[derive(Debug, Parser)]
#[command(name = "tartarus-daemon", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the remap engine + IPC server (the normal daemon mode).
    Run {
        /// Path to the config file. Defaults to the XDG config location.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// List the Tartarus Pro input nodes (does not grab; safe while in use).
    Discover,
    /// Grab the device and print each key event so you can map physical keys.
    DiscoverKeys,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Cmd::Run { config } => run(config),
        Cmd::Discover => discover(),
        Cmd::DiscoverKeys => discover_keys(),
    }
}

// ============================ config path ==============================

fn default_config_path() -> Result<PathBuf> {
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(xdg) if !xdg.is_empty() => PathBuf::from(xdg),
        _ => {
            let home = std::env::var("HOME").context("HOME not set; pass --config")?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("rusty-tartarus").join("config.toml"))
}

// ============================ run (daemon) =============================

fn run(config_path: Option<PathBuf>) -> Result<()> {
    // Grab-safety: while the device is grabbed, a panic on ANY thread (reader,
    // engine, output) must tear down the whole process, not just that thread —
    // otherwise the keypad stays grabbed (EVIOCGRAB held by the surviving
    // process) and the user is locked out. Abort closes all fds, releasing the
    // grab and destroying uinput. Installed only for the daemon mode.
    install_grab_safety_panic_hook();

    let config_path = match config_path {
        Some(p) => p,
        None => default_config_path()?,
    };
    let config = Config::load(&config_path).context("loading config")?;
    tracing::info!(path = %config_path.display(), profile = %config.default_profile, "config loaded");

    // Do ALL fallible setup that can fail with an early `Err(?)` return BEFORE
    // we grab the device or spawn the engine. Otherwise, a failure here would
    // drop `output` whose `Drop` joins the output thread — and the engine thread
    // holds an `out_tx` clone, so that join would DEADLOCK while still holding
    // the EVIOCGRAB, locking the user out of the keypad. By front-loading the
    // tokio-runtime build and the socket bind, any failure happens while nothing
    // is grabbed and no engine thread exists, so the handles drop cleanly.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    let socket = tartarus_ipc::socket_path();
    // `_socket_lock` is the single-instance startup lock; it must stay alive for
    // the daemon's whole lifetime (the kernel releases it on process exit).
    let (listener, _socket_lock) = rt
        .block_on(ipc_server::bind(&socket))
        .context("binding IPC socket")?;

    // Build the output (uinput) and device (hidraw) owner threads, so the grab
    // below never drops physical events with nowhere to send them.
    let output = output::spawn().context("starting output device")?;
    let out_tx = output.sender();
    let device = device::spawn();

    // The engine channel: input readers + IPC tasks both feed it.
    let (engine_tx, engine_rx) = mpsc::channel::<EngineMsg>();

    // Acquire the physical input source. The two modes are mutually exclusive:
    // DRIVER (analog) mode disables the normal HID keyboard, so there is nothing
    // to grab — analog REPLACES the evdev grab. Both feed the SAME engine channel
    // with `EngineMsg::Input`, so the engine, binds, Hypershift, and macros are
    // identical on top of either path. The handle is kept alive for the daemon's
    // lifetime and torn down explicitly before exit (see the shutdown block).
    //
    // `analog_ipc` carries the analog mode's live IPC wiring (pressure snapshot +
    // SetActuation update channel) so the engine can serve `GetAnalogState` /
    // `SetActuation`; `None` in digital mode.
    let mut analog_ipc = None;
    let analog = match config.input_mode {
        InputMode::Digital => {
            // Discover + grab the physical nodes. From here on, an early `?`
            // return that drops `output`/`device` could deadlock-on-join while
            // grabbed — so there are deliberately NO `?` early-returns past this
            // point until the engine is spawned. `discover_required` runs before
            // any grab; `grab` failures abort the loop, but we have not yet handed
            // `out_tx` to an engine thread, so the output thread's channel closes
            // when `output` drops and its join completes.
            let nodes = input::discover_required()?;
            // Grab ONLY the keyboard node(s). The engine remaps key events and
            // nothing else, so the keyboard node is the only one whose events we
            // consume. Grabbing the Pointer node would suppress the thumbpad's
            // mouse movement/scroll, and grabbing the Other (consumer-control)
            // node would swallow its keys (volume etc.) — both with no benefit,
            // since neither is read. Leaving them ungrabbed lets those events
            // reach the OS normally. We grab the narrowest surface that does the
            // job, and `input::grab` re-verifies the USB id is exactly the
            // Tartarus Pro right before EVIOCGRAB (defense in depth).
            // The Tartarus exposes TWO keyboard interfaces: a 6-key BOOT keyboard
            // (interface 0, 6-key rollover) and the real 22-slot array keyboard
            // (interface 1, full N-key rollover). Reading the boot node caps
            // simultaneous presses at 6 AND double-emits keys that also surface on
            // interface 1. So when a CONFIRMED NKRO node is present, skip the boot
            // node and read only the NKRO one. If no confirmed non-boot keyboard
            // node can be found (sysfs unreadable / unexpected topology), fall back
            // to grabbing whatever keyboard node exists and warn that rollover may
            // be capped. The selection is factored into `input::plan_grabs` (pure +
            // unit-tested with hand-built `(kind, interface)` tuples); here we only
            // consume its index-aligned decision over the owned nodes.
            let plan = input::plan_grabs(
                &nodes
                    .iter()
                    .map(|n| (n.kind, n.interface))
                    .collect::<Vec<_>>(),
            );
            if plan.warn_rollover {
                tracing::warn!(
                    "no confirmed N-key-rollover (interface 1) keyboard node found \
                     for the Tartarus Pro; simultaneous key presses may be capped \
                     at 6 (grabbing the available keyboard node(s) as a fallback)"
                );
            }
            let mut grabbed_any = false;
            for (mut node, do_grab) in nodes.into_iter().zip(plan.grab) {
                if node.kind != NodeKind::Keyboard {
                    tracing::debug!(
                        path = %node.path.display(),
                        kind = ?node.kind,
                        "not a keyboard node; leaving ungrabbed (events pass through to the OS)",
                    );
                    continue;
                }
                // Skip the 6KRO boot keyboard when the NKRO keyboard is available
                // (the plan already decided this; a keyboard node not marked for
                // grab is the skipped boot node).
                if !do_grab {
                    tracing::info!(
                        path = %node.path.display(),
                        "skipping the 6KRO boot keyboard node (interface 0); reading \
                         the N-key-rollover keyboard (interface 1) instead",
                    );
                    continue;
                }
                input::grab(&mut node).context("grabbing device node")?;
                // Reader thread forwards key events as EngineMsg::Input.
                pipeline::spawn_reader(node, engine_tx.clone());
                grabbed_any = true;
            }
            if !grabbed_any {
                tracing::warn!(
                    "found the Tartarus Pro but no keyboard node to grab; remap is inactive"
                );
            }
            None
        }
        InputMode::Analog => {
            // Spawn the analog source: it owns the AnalogReader (entering DRIVER
            // mode) and derives key-down/up from raw pressure, forwarding the same
            // `EngineMsg::Input` events as the digital readers. `ANALOG_ARMED`
            // arms the panic hook's NORMAL restore. The `?` here is safe: it
            // happens before the engine thread takes `out_tx`, so `output`/`device`
            // drop without a deadlock (nothing is grabbed in analog mode anyway).
            tracing::info!("input_mode = analog: enabling DRIVER mode (evdev grab skipped)");
            // Shared analog IPC plumbing: the snapshot the source writes + the
            // channel SetActuation edits flow down. The engine gets clones below.
            let snapshot = analog_source::AnalogSnapshot::new();
            let (update_tx, update_rx) = mpsc::channel::<analog_source::ActuationUpdate>();
            let source = analog_source::spawn(
                config.analog.clone(),
                engine_tx.clone(),
                // Hand the source the SAME global the panic hook reads, so
                // arming/disarming DRIVER mode is visible to it from any thread.
                &ANALOG_ARMED,
                snapshot.clone(),
                update_rx,
            )
            .context("starting analog input source (is the device plugged in and accessible?)")?;
            analog_ipc = Some((snapshot, update_tx));
            Some(source)
        }
    };

    // Prime the device-info cache (connection state) before serving.
    let info = device.query_info();
    let _ = engine_tx.send(EngineMsg::DeviceInfo(info));

    // Read the tray opt-out before `config` is moved into the engine below.
    let tray_enabled = config.tray;

    // The engine runs on its own thread, owning all state + the output/device
    // handles. It applies the startup profile's lighting once up. The handle is
    // intentionally not joined (see the shutdown comment below).
    let mut engine = Engine::new(config);
    // In analog mode, wire the live IPC handles so GetAnalogState/SetActuation
    // reach the running source. Digital mode leaves them unset.
    if let Some((snapshot, update_tx)) = analog_ipc.take() {
        engine.set_analog_ipc(snapshot, update_tx);
    }
    let _engine_thread =
        pipeline::spawn_engine(engine, engine_rx, out_tx, device, config_path, DAEMON_VERSION);

    // Run the pre-bound IPC server + signal handling on the runtime built above.
    rt.block_on(async {
        let server_tx = engine_tx.clone();
        tokio::spawn(async move { ipc_server::serve(listener, server_tx).await });

        // Per-application auto-switch: watch the focused window's app class and
        // feed changes to the engine (which decides whether to switch profiles).
        appfocus::spawn(engine_tx.clone());

        // Optional system tray. Spawned AFTER the device is grabbed and the IPC
        // server is up. Its "Quit" item shuts the daemon down by sending on
        // `quit_tx`, which feeds the SAME select as SIGINT/SIGTERM below — so the
        // tray takes the existing NORMAL-restore + grab-release shutdown path,
        // never a bare `process::exit`. Keep `_tray_handle` bound for the rest of
        // the daemon's life: dropping it would tear the tray service down.
        let (quit_tx, mut quit_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _tray_handle = if tray_enabled {
            tray::spawn(quit_tx).await
        } else {
            tracing::info!("system tray disabled by config (tray = false)");
            None
        };

        wait_for_shutdown_signal(&mut quit_rx).await;
    });

    // Shutdown. In ANALOG mode we MUST restore NORMAL before `process::exit`
    // (which skips Drop). The analog source's reader is owned by its worker
    // thread, so cooperatively joining that thread runs the reader's Drop on its
    // own stack — sending NORMAL with the proven re-enumeration recovery. The
    // analog `poll()` has a timeout, so unlike the digital reader (which blocks
    // forever in `fetch_events`) this thread IS joinable: `stop_and_join` signals
    // it and waits. This is the deterministic happy-path NORMAL restore; the
    // panic hook covers the abort path.
    tracing::info!("signal received; shutting down");
    if let Some(source) = analog {
        tracing::info!("restoring device to NORMAL mode (joining analog source)");
        source.stop_and_join();
    }

    // Exit the process directly. We cannot cooperatively join the ENGINE thread:
    // in digital mode the reader threads block forever in `fetch_events()` on the
    // idle grabbed device, holding `engine_tx` clones, so `engine_rx` never
    // closes. Exiting closes those fds — the kernel releases the EVIOCGRAB and
    // destroys the uinput device — so the only thing needing manual cleanup is
    // the socket. (In analog mode the source is already joined above; the engine
    // thread is still detached for the same uniform-teardown reason.)
    ipc_server::cleanup(&socket);
    std::process::exit(0);
}

/// Install a panic hook that aborts the whole process on any thread panic, after
/// restoring device safety invariants that `abort()` would otherwise skip.
///
/// With the device grabbed, a panicking worker thread that merely unwinds would
/// leave the rest of the process (and its `EVIOCGRAB`) alive — stranding the
/// keypad. Chaining to the default hook (so the panic is still logged) then
/// `abort()`ing guarantees fd close, which releases the grab and destroys the
/// virtual device.
///
/// `abort()` also skips every `Drop`, including the analog `DriverModeGuard`'s
/// NORMAL restore. For a grab, fd-close is enough (the kernel drops the grab);
/// for DRIVER mode it is NOT — only an explicit NORMAL command exits it. So when
/// [`ANALOG_ARMED`] is set we send a best-effort NORMAL here, before `abort()`,
/// or a panic in analog mode would leave the keypad mute until replug.
fn install_grab_safety_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        if ANALOG_ARMED.load(Ordering::SeqCst) {
            eprintln!(
                "tartarus-daemon: panic in analog mode — sending NORMAL before abort \
                 (else the keypad stays mute until replug)"
            );
            let restored = tartarus_hid::restore_normal_best_effort();
            if !restored {
                eprintln!(
                    "tartarus-daemon: could NOT confirm NORMAL after panic; \
                     if the keypad does not type, UNPLUG and REPLUG it"
                );
            }
        }
        eprintln!("tartarus-daemon: thread panic — aborting to release grab / virtual device");
        std::process::abort();
    }));
}

/// Await SIGINT, SIGTERM, SIGHUP, or a tray "Quit" request — whichever comes
/// first. SIGHUP matters: closing the terminal the daemon was launched from
/// must take this same shutdown path — at the default disposition it would kill
/// the process outright, skipping the NORMAL-mode restore and stranding an
/// analog-mode keypad in DRIVER mode (mute) until replug.
async fn wait_for_shutdown_signal(quit_rx: &mut tokio::sync::mpsc::UnboundedReceiver<()>) {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut hup = signal(SignalKind::hangup()).expect("SIGHUP handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = hup.recv() => {}
        // Tray "Quit". `Some(())` (NOT `None`) so that a closed channel — the
        // tray was disabled or failed to start, dropping the only sender —
        // *disables* this branch rather than firing it, leaving us waiting on the
        // signals above. Without this guard the daemon would shut down on startup.
        Some(()) = quit_rx.recv() => {}
    }
}

// ============================ discover ================================

fn discover() -> Result<()> {
    let nodes = input::discover_required()?;
    println!("Found {} Tartarus Pro node(s):", nodes.len());
    for node in &nodes {
        let name = node.device.name().unwrap_or("<unnamed>");
        println!("  {:<20} {:?}  \"{}\"", node.path.display(), node.kind, name);
    }
    Ok(())
}

fn discover_keys() -> Result<()> {
    let nodes = input::discover_required()?;
    // Same narrow grab surface as the daemon's run mode: keyboard nodes only,
    // and skip the 6KRO boot keyboard when the NKRO node is present — both
    // interfaces emit each keypress, so grabbing both prints every key twice.
    let plan = input::plan_grabs(
        &nodes
            .iter()
            .map(|n| (n.kind, n.interface))
            .collect::<Vec<_>>(),
    );
    let grab_count = nodes
        .iter()
        .zip(&plan.grab)
        .filter(|(n, g)| n.kind == NodeKind::Keyboard && **g)
        .count();
    println!("Grabbing {grab_count} Tartarus node(s). Press keys on the keypad.");
    println!("Each line is: <down|up|repeat>  <KEY_NAME>  (code <n>)");
    println!("Ctrl-C in this terminal to stop.\n");

    let mut handles = Vec::new();
    for (mut node, do_grab) in nodes.into_iter().zip(plan.grab) {
        if node.kind != NodeKind::Keyboard || !do_grab {
            continue;
        }
        input::grab(&mut node).context("grabbing device for discovery")?;
        handles.push(thread::spawn(move || print_keys(node.device)));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

fn print_keys(mut device: Device) {
    loop {
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if let EventSummary::Key(_, key, value) = event.destructure() {
                        let edge = match value {
                            1 => "down",
                            0 => "up",
                            2 => "repeat",
                            other => {
                                println!("?{other}   {key:?}  (code {})", key.0);
                                continue;
                            }
                        };
                        println!("{edge:<6} {key:?}  (code {})", key.0);
                    }
                }
            }
            Err(err) => {
                eprintln!("read error: {err}");
                return;
            }
        }
    }
}

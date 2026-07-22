//! The remap pipeline wiring: reader threads + the engine actor loop.
//!
//! This is the glue that turns the building blocks (grabbed input nodes, the
//! engine, the uinput output thread, the hidraw device thread) into a running
//! remap loop. It lives in the library — not `main.rs` — so the synthetic
//! loopback integration test exercises the EXACT same read → resolve → emit code
//! the daemon runs, rather than a re-implementation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use evdev::{EventSummary, KeyCode};

use crate::analog_source::ClaimedKeys;
use crate::device::DeviceHandle;
use crate::engine::{Command, Engine};
use crate::input::DeviceNode;
use crate::ipc_server::EngineMsg;
use crate::output::{self, OutputCmd};
use crate::state::Config;

/// Spawn a reader thread for one grabbed node, forwarding key events to the
/// engine as [`EngineMsg::Input`]. The thread exits cleanly (no hot-spin) when
/// the device read errors — e.g. the device is unplugged — or when the engine
/// channel closes.
///
/// `drop_keys` is the per-reader claimed-key filter: a key-DOWN whose code is in
/// the set is dropped before it is forwarded, and that key's later up/repeat
/// edges follow the same decision. WHY: in analog mode the keys claimed by
/// `[analog.keys]` are synthesized by the analog source from pressure; a digital
/// duplicate of the same key arriving on a grabbed node (the spacebar is the
/// known candidate — analog byte 19 AND possibly a digital boot-report key) must
/// not double-feed the engine, where e.g. a macro bind would fire twice per
/// press. The set is LIVE — `SetActuation` can change or remove a
/// `second_action` key name, and the analog source republishes its claimed codes
/// on every such edit (see [`ClaimedKeys`]) — which is exactly why the decision
/// latches on the down edge: filtering each edge independently against a moving
/// set could drop the UP of a forwarded DOWN (stuck key on the sink) or forward
/// an UP the engine never saw go down. Digital mode passes an empty, never-
/// written set (behavior unchanged; the engine ignores unheld UPs and swallows
/// repeats either way).
pub fn spawn_reader(
    mut node: DeviceNode,
    tx: Sender<EngineMsg>,
    drop_keys: ClaimedKeys,
) -> JoinHandle<()> {
    let path = node.path.clone();
    thread::Builder::new()
        .name("tartarust-reader".into())
        .spawn(move || {
            // Keys THIS reader forwarded DOWN and has not yet forwarded UP.
            // Carries each press's drop decision to its up/repeat edges, so a
            // live `drop_keys` change mid-hold cannot orphan or duplicate edges.
            let mut forwarded_down: HashSet<u16> = HashSet::new();
            loop {
                match node.device.fetch_events() {
                    Ok(events) => {
                        for event in events {
                            let EventSummary::Key(_, key, value) = event.destructure() else {
                                continue;
                            };
                            let forward = match value {
                                // Down: consult the live drop set (sole lock site).
                                1 => {
                                    let claimed = drop_keys
                                        .read()
                                        .map(|set| set.contains(&key.0))
                                        .unwrap_or(false);
                                    if !claimed {
                                        forwarded_down.insert(key.0);
                                    }
                                    !claimed
                                }
                                // Up: follow the press's decision.
                                0 => forwarded_down.remove(&key.0),
                                // Autorepeat (or any exotic value): ditto.
                                _ => forwarded_down.contains(&key.0),
                            };
                            if forward && tx.send(EngineMsg::Input(key.0, value)).is_err() {
                                return; // engine gone
                            }
                        }
                    }
                    Err(err) => {
                        // A read error means the node went away (unplug) or was
                        // revoked. Stop this reader; do not spin. The daemon keeps
                        // serving IPC and its other readers. First, ask the engine to
                        // release any keys held at this instant, so a key that was
                        // down when the device vanished doesn't stay stuck on the
                        // virtual device (no key-up would ever arrive for it).
                        tracing::error!(path = %path.display(), ?err, "read error; reader stopping");
                        let _ = tx.send(EngineMsg::ReleaseAll);
                        return;
                    }
                }
            }
        })
        .expect("spawning reader thread")
}

/// Spawn the engine actor thread: the sole owner of all state. Consumes
/// [`EngineMsg`]s, drives the output/device threads, and persists config after
/// mutating IPC requests. Applies the startup profile's lighting once on entry.
///
/// `daemon_version` is threaded through to `GetStatus` replies. `config_path` is
/// where mutating IPC requests are persisted (atomic write).
pub fn spawn_engine(
    mut engine: Engine,
    rx: std::sync::mpsc::Receiver<EngineMsg>,
    out_tx: Sender<OutputCmd>,
    device: DeviceHandle,
    config_path: PathBuf,
    daemon_version: &'static str,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("tartarust-engine".into())
        .spawn(move || {
            // Apply the startup profile's lighting once.
            device.send(engine.active_lighting_cmd());

            for msg in rx {
                match msg {
                    EngineMsg::Input(code, value) => {
                        let outcome = engine.handle_key(KeyCode(code), value);
                        emit_and_dispatch(
                            &engine,
                            outcome.events,
                            outcome.commands,
                            &out_tx,
                            &device,
                        );
                    }
                    EngineMsg::Ipc(request, reply) => {
                        // For requests that expose device connection state, do a
                        // LIVE device probe first so the answer reflects the
                        // current state (e.g. connected:false after unplug)
                        // rather than the stale startup cache.
                        if matches!(
                            request,
                            tartarust_ipc::Request::GetDeviceInfo
                                | tartarust_ipc::Request::GetStatus
                        ) {
                            engine.set_device_info(device.query_info());
                        }
                        // Save semantics: mutating edits apply LIVE (device
                        // commands fire below) but are NOT written to disk. An
                        // explicit `SaveProfile` is the sole disk write. The
                        // unsaved-changes state needs no bookkeeping here — the
                        // engine derives it by diffing the live config against
                        // its last-saved snapshot.
                        let is_save = matches!(request, tartarust_ipc::Request::SaveProfile { .. });
                        let (response, cmds) = engine.apply(request, daemon_version);
                        for cmd in cmds {
                            device.send(cmd);
                        }
                        if is_save {
                            // Persist BEFORE replying so the Save reply reflects
                            // the on-disk outcome: a failed write returns an Error
                            // (and leaves the saved snapshot stale, so the state
                            // stays unsaved and the user can retry) instead of a
                            // misleading Ack that would mask silent data loss.
                            let reply_msg = if persist(&engine, &config_path) {
                                engine.mark_saved();
                                response
                            } else {
                                tartarust_ipc::Response::Error {
                                    message: format!(
                                        "failed to write config to {}",
                                        config_path.display()
                                    ),
                                }
                            };
                            let _ = reply.send(reply_msg);
                        } else {
                            let _ = reply.send(response);
                        }
                    }
                    EngineMsg::DeviceInfo(info) => engine.set_device_info(info),
                    EngineMsg::FocusedApp(class) => {
                        // Auto-switch by focused app. Deliberately NOT persisted —
                        // it must not churn the on-disk startup profile.
                        for cmd in engine.on_focused_app(&class) {
                            device.send(cmd);
                        }
                    }
                    EngineMsg::ReleaseAll => {
                        // A reader exited (unplug). Release any keys held at that
                        // instant so none stay stuck-down on the virtual device.
                        let events = engine.release_all().events;
                        if !events.is_empty() {
                            let _ = out_tx.send(OutputCmd::Emit(events));
                        }
                    }
                }
            }
            tracing::debug!("engine thread exiting");
        })
        .expect("spawning engine thread")
}

/// Emit output events and run the side-effect commands from one input outcome.
fn emit_and_dispatch(
    engine: &Engine,
    events: Vec<evdev::InputEvent>,
    commands: Vec<Command>,
    out_tx: &Sender<OutputCmd>,
    device: &DeviceHandle,
) {
    if !events.is_empty() {
        let _ = out_tx.send(OutputCmd::Emit(events));
    }
    for command in commands {
        match command {
            Command::RunMacro(name) => match engine.macro_steps(&name) {
                Some(m) => output::run_macro(out_tx, &m.steps),
                None => tracing::warn!(%name, "macro not found at dispatch"),
            },
            Command::DeviceCmd(cmd) => device.send(cmd),
            Command::LaunchProgram(argv) => launch_program(argv),
        }
    }
}

/// Persist the engine's current config to disk (atomic). Returns `true` on a
/// successful write (so the caller can record the saved snapshot), `false` on
/// error (the snapshot stays stale, so the state reads unsaved and the user can
/// retry Save).
fn persist(engine: &Engine, path: &Path) -> bool {
    let config: Config = engine.config_snapshot();
    match config.save(path) {
        Ok(()) => true,
        Err(err) => {
            tracing::error!(?err, path = %path.display(), "failed to persist config");
            false
        }
    }
}

fn launch_program(argv: Vec<String>) {
    let Some((program, args)) = argv.split_first() else {
        tracing::warn!("launch_program with empty command");
        return;
    };
    tracing::info!(?argv, "launching program");
    match ProcCommand::new(program).args(args).spawn() {
        Ok(mut child) => {
            // std does not reap on drop: without a wait() the child would stay
            // a zombie in the process table for the daemon's whole lifetime.
            let reaper = thread::Builder::new()
                .name("tartarust-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                });
            if let Err(err) = reaper {
                tracing::error!(?err, "failed to spawn reaper thread");
            }
        }
        Err(err) => tracing::error!(?err, ?argv, "failed to launch program"),
    }
}

// NOTE: the hybrid claimed-key drop set is derived from the analog actuator
// ([`crate::analog_source::Actuator::claimed_codes`]) and tested there — the
// actuator is the single authority for what analog can synthesize, so the set
// inherits its validity checks by construction. Unsaved-changes semantics (a
// live diff against the last-saved config) are tested on the engine.

//! The remap pipeline wiring: reader threads + the engine actor loop.
//!
//! This is the glue that turns the building blocks (grabbed input nodes, the
//! engine, the uinput output thread, the hidraw device thread) into a running
//! remap loop. It lives in the library — not `main.rs` — so the synthetic
//! loopback integration test exercises the EXACT same read → resolve → emit code
//! the daemon runs, rather than a re-implementation.

use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use evdev::{EventSummary, KeyCode};

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
pub fn spawn_reader(mut node: DeviceNode, tx: Sender<EngineMsg>) -> JoinHandle<()> {
    let path = node.path.clone();
    thread::Builder::new()
        .name("tartarus-reader".into())
        .spawn(move || loop {
            match node.device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        if let EventSummary::Key(_, key, value) = event.destructure()
                            && tx.send(EngineMsg::Input(key.0, value)).is_err()
                        {
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
        .name("tartarus-engine".into())
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
                            tartarus_ipc::Request::GetDeviceInfo
                                | tartarus_ipc::Request::GetStatus
                        ) {
                            engine.set_device_info(device.query_info());
                        }
                        // Save semantics: mutating edits apply LIVE (device
                        // commands fire below) but are NOT written to disk — they
                        // only mark the engine dirty. An explicit `SaveProfile`
                        // is the sole disk write.
                        let is_save = matches!(request, tartarus_ipc::Request::SaveProfile { .. });
                        let dirties = marks_dirty(&request);
                        let (response, cmds) = engine.apply(request, daemon_version);
                        for cmd in cmds {
                            device.send(cmd);
                        }
                        if is_save {
                            // Persist BEFORE replying so the Save reply reflects
                            // the on-disk outcome: a failed write returns an Error
                            // (and keeps dirty set, so the user can retry) instead
                            // of a misleading Ack that would mask silent data loss.
                            let reply_msg = if persist(&engine, &config_path) {
                                engine.clear_dirty();
                                response
                            } else {
                                tartarus_ipc::Response::Error {
                                    message: format!(
                                        "failed to write config to {}",
                                        config_path.display()
                                    ),
                                }
                            };
                            let _ = reply.send(reply_msg);
                        } else {
                            // Mark dirty only when the edit actually changed state.
                            // A REJECTED mutation returns `Response::Error` and
                            // mutates nothing (every handler validates before it
                            // writes), so it must not light the unsaved indicator.
                            let mark = mark_dirty_after(dirties, &response);
                            let _ = reply.send(response);
                            if mark {
                                engine.mark_dirty();
                            }
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

/// Whether a request changes the persisted config, so it should mark the engine
/// dirty (a pending, unsaved session edit).
///
/// Excludes `SwitchProfile` (it changes only the live active profile, which is
/// never persisted — `config_snapshot` writes `configured_default`) and
/// `SaveProfile` (the save trigger itself, handled separately).
pub fn marks_dirty(req: &tartarus_ipc::Request) -> bool {
    use tartarus_ipc::Request::*;
    matches!(
        req,
        CreateProfile { .. }
            | RenameProfile { .. }
            | DeleteProfile { .. }
            | SetProfileLinkedApp { .. }
            | SetDefaultProfile { .. }
            | SetThumbAxisMode { .. }
            | SetBind { .. }
            | SetLighting { .. }
            | SetBrightness { .. }
            | SaveMacro { .. }
            | DeleteMacro { .. }
            | SetAutoSwitch { .. }
            | SetActuation { .. }
    )
}

/// Whether an applied non-save request should mark the engine dirty: it must be
/// a config-changing request (`dirties`) AND the engine must have ACCEPTED it. A
/// rejected request returns [`tartarus_ipc::Response::Error`] and mutates nothing
/// (every `apply` handler validates before it writes), so it leaves no pending
/// change and must not surface the Save affordance.
fn mark_dirty_after(dirties: bool, response: &tartarus_ipc::Response) -> bool {
    dirties && !matches!(response, tartarus_ipc::Response::Error { .. })
}

/// Persist the engine's current config to disk (atomic). Returns `true` on a
/// successful write (so the caller can clear the dirty flag), `false` on error
/// (dirty stays set, so the user can retry Save).
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
                .name("tartarus-reap".into())
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

#[cfg(test)]
mod tests {
    use super::{mark_dirty_after, marks_dirty};
    use tartarus_ipc::{Layer, Request, Response};

    /// The dirty mark must depend on BOTH the request type and the apply outcome:
    /// a dirtying request the engine REJECTED (Error) changed no state, so it must
    /// not light the unsaved indicator (review finding).
    #[test]
    fn rejected_mutation_does_not_mark_dirty() {
        let edit = Request::SetBind {
            profile: "p".into(),
            layer: Layer::Base,
            key: "KEY_A".into(),
            action: None,
        };
        let dirties = marks_dirty(&edit);
        // Accepted (Ack) dirtying edit -> mark dirty.
        assert!(mark_dirty_after(dirties, &Response::Ack));
        // Rejected (Error) dirtying edit -> do NOT mark dirty (no state change).
        assert!(!mark_dirty_after(dirties, &Response::Error { message: "x".into() }));
        // A non-dirtying request never marks dirty, whatever the response.
        assert!(!mark_dirty_after(marks_dirty(&Request::GetStatus), &Response::Ack));
    }

    /// `marks_dirty` is the daemon's "this edit leaves unsaved state" predicate.
    /// It must cover every config-changing request, but NOT `SwitchProfile` (it
    /// changes only the live active profile, which is never persisted), NOT
    /// `SaveProfile` (the commit trigger itself), and NOT any read.
    #[test]
    fn marks_dirty_covers_edits_excludes_switch_save_and_reads() {
        // Content edits.
        assert!(marks_dirty(&Request::SetBind {
            profile: "p".into(),
            layer: Layer::Base,
            key: "KEY_A".into(),
            action: None,
        }));
        assert!(marks_dirty(&Request::SetActuation {
            index: 0,
            actuation_point: 0x60,
            release_point: 0x50,
            second_actuation_point: None,
            second_action: None,
        }));
        assert!(marks_dirty(&Request::SetLighting {
            profile: None,
            lighting: tartarus_ipc::LightingState::default(),
        }));
        // Structural / management edits.
        assert!(marks_dirty(&Request::CreateProfile { name: "p".into() }));
        assert!(marks_dirty(&Request::DeleteProfile { name: "p".into() }));
        assert!(marks_dirty(&Request::SetDefaultProfile { name: "p".into() }));
        assert!(marks_dirty(&Request::DeleteMacro { name: "m".into() }));
        assert!(marks_dirty(&Request::SetAutoSwitch { enabled: true }));

        // Excluded: switching active profile persists nothing.
        assert!(!marks_dirty(&Request::SwitchProfile { name: "p".into() }));
        // Excluded: Save is the commit, handled separately.
        assert!(!marks_dirty(&Request::SaveProfile { name: None }));
        // Excluded: reads never dirty.
        assert!(!marks_dirty(&Request::GetStatus));
        assert!(!marks_dirty(&Request::ListProfiles));
        assert!(!marks_dirty(&Request::GetAnalogState));
    }
}

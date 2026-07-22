//! The virtual output device and the thread that owns it.
//!
//! A single thread owns the uinput [`VirtualDevice`] and is the *only* writer
//! to it. The engine thread (and any macro request) talks to it over a channel
//! of [`OutputCmd`]. This keeps the device write path serialized and — crucial
//! for macros — means a macro's inter-step sleeps never block the engine thread
//! that is reading physical events; at worst they queue behind the macro on the
//! output thread, which is the desired "macro plays out atomically" behavior.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use anyhow::Context;
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};

use tartarust_ipc::model::MacroStep;

use crate::macros::{MacroAction, lower};

/// A command for the output thread.
pub enum OutputCmd {
    /// Emit a batch of events immediately (uinput appends one `SYN_REPORT`).
    Emit(Vec<InputEvent>),
    /// Play a macro (already-lowered) once, honoring its delays.
    RunMacro(Vec<MacroAction>),
}

/// Handle to the output thread. Drop it (or call [`OutputHandle::shutdown`]) to
/// stop the thread and destroy the virtual device.
pub struct OutputHandle {
    tx: Sender<OutputCmd>,
    join: Option<JoinHandle<()>>,
    /// The `/dev/input/eventN` node(s) the kernel created for our virtual sink.
    /// Used by the loopback integration test to read what the engine emits;
    /// empty if the kernel had not populated sysfs yet at spawn time.
    node_paths: Vec<std::path::PathBuf>,
}

impl OutputHandle {
    /// A cloneable sender for the output thread. Hand one to anything that needs
    /// to emit (the engine loop) so it can post events/macros.
    pub fn sender(&self) -> Sender<OutputCmd> {
        self.tx.clone()
    }

    /// The `/dev/input/eventN` paths the kernel assigned to the virtual sink.
    /// Primarily for tests that need to read the emitted (remapped) events.
    pub fn node_paths(&self) -> &[std::path::PathBuf] {
        &self.node_paths
    }

    /// Stop the output thread and wait for it to exit (dropping the device).
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        // Dropping every sender closes the channel; the loop then ends. We hold
        // one (`self.tx`); replace it with a fresh dead channel to drop it.
        let (dead_tx, _dead_rx) = mpsc::channel();
        self.tx = dead_tx;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for OutputHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Build the virtual device (declaring all the capabilities we might emit) and
/// spawn the thread that owns it. Returns a handle for posting commands.
pub fn spawn() -> anyhow::Result<OutputHandle> {
    let mut device = build_virtual_device().context("building uinput virtual device")?;
    // Resolve the kernel-assigned event node(s). Right after `build()` the
    // kernel may not have populated sysfs yet, so retry briefly. Non-fatal: the
    // daemon doesn't need these (only the loopback test does).
    let node_paths = resolve_node_paths(&mut device);
    let (tx, rx) = mpsc::channel::<OutputCmd>();
    let join = thread::Builder::new()
        .name("tartarust-output".into())
        .spawn(move || output_loop(device, rx))
        .context("spawning output thread")?;
    Ok(OutputHandle {
        tx,
        join: Some(join),
        node_paths,
    })
}

/// Best-effort resolution of a virtual device's `/dev/input/eventN` nodes,
/// retrying for the brief window before the kernel populates sysfs.
fn resolve_node_paths(device: &mut VirtualDevice) -> Vec<std::path::PathBuf> {
    for _ in 0..50 {
        if let Ok(nodes) = device.enumerate_dev_nodes_blocking() {
            let paths: Vec<_> = nodes.flatten().collect();
            if !paths.is_empty() {
                return paths;
            }
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    tracing::debug!("virtual device event node did not appear in sysfs in time");
    Vec::new()
}

/// The output thread body: drain the channel, emitting / playing macros.
///
/// `engine_held` mirrors the keys the engine currently holds down on the sink
/// (its stream is already refcount-deduped: one press per first holder, one
/// release per last). A playing macro must not touch those keys: its Release
/// would silently lift a modifier a physical bind is still holding — the
/// engine's later release is then a no-op the kernel filters — and its Press is
/// a no-op anyway (the key is already down). Skipping both keeps the macro's
/// own press/release pairing consistent without tracking macro-held keys.
fn output_loop(mut device: VirtualDevice, rx: Receiver<OutputCmd>) {
    let mut engine_held: HashSet<u16> = HashSet::new();
    for cmd in rx {
        match cmd {
            OutputCmd::Emit(events) => {
                for event in &events {
                    if event.event_type() == EventType::KEY {
                        match event.value() {
                            1 => {
                                engine_held.insert(event.code());
                            }
                            0 => {
                                engine_held.remove(&event.code());
                            }
                            _ => {}
                        }
                    }
                }
                if let Err(err) = device.emit(&events) {
                    tracing::warn!(?err, "failed to emit events");
                }
            }
            OutputCmd::RunMacro(actions) => {
                for action in actions {
                    match action {
                        MacroAction::Emit(events) => {
                            let events: Vec<InputEvent> = events
                                .into_iter()
                                .filter(|event| {
                                    !(event.event_type() == EventType::KEY
                                        && matches!(event.value(), 0 | 1)
                                        && engine_held.contains(&event.code()))
                                })
                                .collect();
                            if events.is_empty() {
                                continue;
                            }
                            if let Err(err) = device.emit(&events) {
                                tracing::warn!(?err, "failed to emit macro event");
                            }
                        }
                        MacroAction::Delay(d) => thread::sleep(d),
                    }
                }
            }
        }
    }
    tracing::debug!("output thread exiting; virtual device will be destroyed");
}

/// Helper to lower a config macro and post it to the output thread in one step.
pub fn run_macro(tx: &Sender<OutputCmd>, steps: &[MacroStep]) {
    let actions = lower(steps);
    let _ = tx.send(OutputCmd::RunMacro(actions));
}

/// Construct the virtual device, declaring a broad set of keys, mouse buttons,
/// and relative axes. The kernel silently drops events for any code not
/// declared here, so we over-declare: every keyboard key, the common mouse
/// buttons, and movement/wheel axes. Extra declarations are harmless.
fn build_virtual_device() -> std::io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    // Declare the whole standard key range. evdev key codes run roughly
    // 0..0x300; declaring all of them means any KEY_* / BTN_* a user binds is
    // already enabled. Codes that aren't real keys are inert.
    for code in 0u16..0x300 {
        keys.insert(KeyCode(code));
    }

    let mut axes = AttributeSet::<RelativeAxisCode>::new();
    axes.insert(RelativeAxisCode::REL_X);
    axes.insert(RelativeAxisCode::REL_Y);
    axes.insert(RelativeAxisCode::REL_WHEEL);
    axes.insert(RelativeAxisCode::REL_HWHEEL);
    axes.insert(RelativeAxisCode::REL_WHEEL_HI_RES);
    axes.insert(RelativeAxisCode::REL_HWHEEL_HI_RES);

    VirtualDevice::builder()?
        .name("tartarust")
        .with_keys(&keys)?
        .with_relative_axes(&axes)?
        .build()
}

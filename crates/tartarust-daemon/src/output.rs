//! The virtual output device and the thread that owns it.
//!
//! A single thread owns the uinput [`VirtualDevice`] and is the *only* writer
//! to it. The engine thread (and any macro request) talks to it over a channel
//! of [`OutputCmd`]. This keeps the device write path serialized and — crucial
//! for macros — means a macro's inter-step sleeps never block the engine thread
//! that is reading physical events; at worst they queue behind the macro on the
//! output thread, which is the desired "macro plays out atomically" behavior.
//!
//! That queue is unbounded, so macro REQUESTS are rate-limited instead: only one
//! macro at a time is in flight, and a request that arrives while one is still
//! queued or playing is dropped. See [`run_macro`].

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    RunMacro {
        actions: Vec<MacroAction>,
        /// The poster's in-flight guard. It was set before this command was
        /// queued; the output thread clears it once playback ends, which is what
        /// lets the poster send the next macro. See [`run_macro`].
        in_flight: Arc<AtomicBool>,
    },
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
            OutputCmd::RunMacro { actions, in_flight } => {
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
                // Playback (sleeps included) is over: release the in-flight slot
                // so the next request is accepted. Cleared HERE and not when the
                // command was dequeued, so the guard covers the whole playback.
                in_flight.store(false, Ordering::SeqCst);
            }
        }
    }
    tracing::debug!("output thread exiting; virtual device will be destroyed");
}

thread_local! {
    /// This thread's macro in-flight guard: `true` while a macro it posted is
    /// queued or playing on the output thread. Created on first use, then
    /// reused. See [`run_macro`] for why it lives in a thread-local.
    static IN_FLIGHT: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Helper to lower a config macro and post it to the output thread in one step.
///
/// At most ONE macro is in flight (queued or playing) per posting thread: a
/// request that arrives while the previous one has not finished is DROPPED, not
/// queued. WHY: playback runs inline on the output thread and sleeps for every
/// `Delay` step, so a macro-bound key tapped during a long macro would otherwise
/// pile up copies of the lowered macro in the unbounded channel — kilobytes per
/// copy — and keep replaying them long after the user stopped tapping. The
/// engine already swallows autorepeat for macro binds; this covers discrete
/// taps. Dropping the NEW request (rather than the playing one) keeps the
/// "a macro plays out atomically" contract.
///
/// The guard is an `Arc<AtomicBool>` shared with the output thread: claimed here
/// before the send, carried with the macro, cleared there when playback ends.
/// The poster's half lives in a thread-local because a poster holds nothing but
/// a cloned `Sender`, which carries no identity to hang per-channel state off
/// of. Macros are posted from exactly one thread — the engine actor, whose
/// dispatch is the sole caller — so per-thread IS per-output-thread here; a
/// second posting thread would just get its own one-macro budget, which still
/// bounds the queue.
pub fn run_macro(tx: &Sender<OutputCmd>, steps: &[MacroStep]) {
    IN_FLIGHT.with_borrow_mut(|slot| {
        let in_flight = slot.get_or_insert_with(|| Arc::new(AtomicBool::new(false)));
        // false -> true claims the slot. Err means a macro is still in flight
        // (the compare_exchange also keeps two posters from both claiming it).
        if in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::debug!("a macro is already queued or playing; dropping this request");
            return;
        }
        let actions = lower(steps);
        let cmd = OutputCmd::RunMacro {
            actions,
            in_flight: Arc::clone(in_flight),
        };
        if tx.send(cmd).is_err() {
            // The output thread is gone, so nothing will ever clear the guard.
            // Release it here; otherwise this thread could never post again.
            in_flight.store(false, Ordering::SeqCst);
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard is thread-local and libtest may reuse a thread between tests,
    /// so each test starts from a known-clear guard.
    fn reset_guard() {
        IN_FLIGHT.with_borrow_mut(|slot| *slot = None);
    }

    fn steps() -> Vec<MacroStep> {
        vec![
            MacroStep::Press {
                key: "KEY_A".into(),
            },
            MacroStep::Release {
                key: "KEY_A".into(),
            },
        ]
    }

    /// A macro-bound key tapped again while the first macro is still queued or
    /// playing must NOT enqueue a second copy (that is the unbounded-growth bug).
    #[test]
    fn macro_request_dropped_while_one_is_in_flight() {
        reset_guard();
        let (tx, rx) = mpsc::channel::<OutputCmd>();

        run_macro(&tx, &steps());
        run_macro(&tx, &steps());
        run_macro(&tx, &steps());
        let queued: Vec<_> = rx.try_iter().collect();
        assert_eq!(queued.len(), 1, "only the first macro may be queued");

        // Clear the guard the way the output thread does when playback ends.
        let OutputCmd::RunMacro { in_flight, .. } = &queued[0] else {
            panic!("expected a RunMacro command");
        };
        in_flight.store(false, Ordering::SeqCst);

        run_macro(&tx, &steps());
        assert_eq!(
            rx.try_iter().count(),
            1,
            "a macro posted after playback ended must be queued"
        );
    }

    /// A send onto a dead channel (output thread gone) must not leave the guard
    /// claimed forever — nothing else would ever clear it.
    #[test]
    fn failed_send_releases_the_guard() {
        reset_guard();
        let (dead_tx, dead_rx) = mpsc::channel::<OutputCmd>();
        drop(dead_rx);
        run_macro(&dead_tx, &steps());

        let (tx, rx) = mpsc::channel::<OutputCmd>();
        run_macro(&tx, &steps());
        assert_eq!(rx.try_iter().count(), 1);
    }
}

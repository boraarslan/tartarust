//! Analog input reader for the Tartarus Pro: enable DRIVER mode and read live
//! per-key pressure frames from interface 1.
//!
//! This is the reusable building block behind the `analog_read` / `analog_map`
//! examples and (next) the daemon's analog-actuation engine. It wraps two
//! invariants:
//!
//!   1. **Enable:** on construction it opens the control interface (interface 2)
//!      and sends `set_device_mode(DRIVER)`, which is what makes the device emit
//!      the raw pressure reports (`docs/research/08-analog-handshake.md`).
//!   2. **Restore:** a [`DriverModeGuard`] owns the control transport and sends
//!      `set_device_mode(NORMAL)` on `Drop`. In DRIVER mode the keypad sends no
//!      normal keystrokes, so leaving it there makes it mute until replug — the
//!      guard guarantees restoration on every drop/unwind path. Hardware-proven
//!      safe, including the dead-fd reopen path if NORMAL re-enumerates the Pro.
//!
//! Signal handling is **not** in this module on purpose: a process-global SIGINT
//! handler is the application's job (the examples install one; the daemon uses
//! its own async signal plumbing and drops the reader on shutdown). The library
//! contract is only "Drop restores NORMAL".

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use nix::poll::{PollFd, PollFlags, PollTimeout};
use tartarus_protocol::device::{self, DEVICE_MODE_DRIVER, DEVICE_MODE_NORMAL};
use tartarus_protocol::AnalogFrame;

use crate::discover::{discover_control_node, discover_node_for_interface, INPUT_INTERFACE};
use crate::error::HidError;
use crate::transport::Transport;

/// Read buffer size for an interface-1 HID input report. Frames are
/// [`ANALOG_FRAME_LEN`] (24) bytes; the slack tolerates any larger report the
/// device might send without a reallocation.
const READ_BUF_LEN: usize = 64;

/// How long [`restore_normal_via_reopen`] waits for the control node to reappear
/// after a NORMAL send that may have re-enumerated the Pro.
const REOPEN_DEADLINE: Duration = Duration::from_secs(4);
/// Poll interval while waiting for the control node to reappear.
const REOPEN_POLL: Duration = Duration::from_millis(250);

/// Pause before retrying a control command that hit a transient EPIPE stall
/// (the value the `tartarus-hidraw-framing` recon settled on). See
/// [`command_with_epipe_retry`].
const EPIPE_RETRY_DELAY: Duration = Duration::from_millis(20);

/// Run a control command, retrying once after a short pause if it hits a
/// transient EPIPE (USB STALL) on the `HIDIOCSFEATURE` send phase.
///
/// `Transport::command` deliberately does **not** retry EPIPE (its docs put that
/// on the caller); we are that caller. Whole-command retry is safe for the
/// device-mode commands here because they are **idempotent** — re-sending the
/// same `set_device_mode`, or re-reading `get_device_mode`, has no cumulative
/// effect. Other (non-idempotent) commands must not blindly use this.
fn command_with_epipe_retry(
    transport: &Transport,
    request: &tartarus_protocol::RazerReport,
) -> Result<tartarus_protocol::RazerReport, HidError> {
    match transport.command(request) {
        Err(e) if e.is_epipe_stall() => {
            tracing::debug!("control command hit transient EPIPE; retrying once after 20ms");
            sleep(EPIPE_RETRY_DELAY);
            transport.command(request)
        }
        other => other,
    }
}

/// Owns the open control transport and guarantees the device returns to NORMAL
/// mode on drop. Construct this **before** sending DRIVER so there is never a
/// window where DRIVER is set without an armed restore.
pub struct DriverModeGuard {
    transport: Transport,
}

impl DriverModeGuard {
    /// Open and take ownership of the control interface (interface 2, discovered
    /// dynamically). Does **not** change the device mode yet — call
    /// [`DriverModeGuard::enter_driver`] for that.
    pub fn open() -> Result<Self, HidError> {
        let node = discover_control_node()?;
        let transport = Transport::open_path(&node)?;
        tracing::debug!(node = %node.display(), "armed driver-mode guard on control interface");
        Ok(Self { transport })
    }

    /// Send `set_device_mode(DRIVER)` to enable analog reporting. After this the
    /// keypad stops sending normal keystrokes until NORMAL is restored (on drop).
    pub fn enter_driver(&self) -> Result<(), HidError> {
        command_with_epipe_retry(&self.transport, &device::set_device_mode(DEVICE_MODE_DRIVER))?;
        tracing::debug!("device entered DRIVER mode (analog enabled)");
        Ok(())
    }

    /// Read back `get_device_mode` arg[0] (the current mode byte). Useful to
    /// confirm a mode switch took (DRIVER = 0x03, NORMAL = 0x00).
    pub fn read_mode(&self) -> Result<u8, HidError> {
        let resp = command_with_epipe_retry(&self.transport, &device::get_device_mode())?;
        Ok(resp.arguments[0])
    }
}

impl Drop for DriverModeGuard {
    fn drop(&mut self) {
        // Restore NORMAL on every exit path. Never panic in Drop. The Pro may
        // re-enumerate on NORMAL, killing this fd mid-send; treat that as
        // recoverable by re-discovering and reopening the control node.
        match command_with_epipe_retry(&self.transport, &device::set_device_mode(DEVICE_MODE_NORMAL))
        {
            Ok(_) => match self.read_mode() {
                Ok(DEVICE_MODE_NORMAL) => {
                    tracing::info!("restored device mode: NORMAL (keypad typing OK)");
                    return;
                }
                Ok(other) => tracing::warn!(
                    mode = other,
                    "after NORMAL send, device reports a non-NORMAL mode"
                ),
                Err(e) => tracing::warn!(error = %e, "NORMAL sent but mode read-back failed; re-checking"),
            },
            Err(e) => tracing::warn!(
                error = %e,
                "NORMAL send on existing fd failed; device may have re-enumerated, reopening"
            ),
        }

        // Recovery: the fd may be dead because the device re-enumerated. Wait for
        // the node to reappear, then re-discover, reopen, resend, and confirm.
        if restore_normal_via_reopen() {
            return;
        }

        // Last resort: could not confirm NORMAL by any path. Make it loud — this
        // is the one outcome we must never leave silent.
        tracing::error!(
            "could not confirm the Tartarus Pro is back in NORMAL mode; \
             if the keypad does not type, UNPLUG and REPLUG it to restore"
        );
    }
}

/// Best-effort, allocation-light restore of NORMAL mode for the **abort path**.
///
/// The normal restore is the [`DriverModeGuard`]'s `Drop`, which a clean drop or
/// a cooperative thread join runs. But `std::process::abort()` (the daemon's
/// grab-safety panic hook) skips `Drop` entirely — and unlike a grab, closing
/// the fd does NOT take the device out of DRIVER mode; only an explicit NORMAL
/// command does. So a panic in analog mode would strand the keypad mute.
///
/// This is the escape hatch the daemon's panic hook calls before `abort()`: it
/// opens a fresh control transport (the panicking thread's guard is unreachable)
/// and sends NORMAL, confirming once. It is deliberately quick and signal-/panic-
/// safe in spirit (no unwinding, no locks, short bounded retry) since it runs
/// while the process is already tearing down. Returns whether NORMAL was
/// confirmed; the caller logs and aborts regardless.
pub fn restore_normal_best_effort() -> bool {
    let Ok(node) = discover_control_node() else {
        return false;
    };
    let Ok(transport) = Transport::open_path(&node) else {
        return false;
    };
    let _ = transport.command(&device::set_device_mode(DEVICE_MODE_NORMAL));
    matches!(
        transport.command(&device::get_device_mode()),
        Ok(resp) if resp.arguments[0] == DEVICE_MODE_NORMAL
    )
}

/// Re-discover and reopen the control node, send NORMAL, and confirm arg[0]==0.
/// Returns true only when NORMAL is confirmed. A reset that lands the device
/// back in NORMAL (node reappears reading 0x00) counts as success.
fn restore_normal_via_reopen() -> bool {
    let deadline = Instant::now() + REOPEN_DEADLINE;
    while Instant::now() < deadline {
        sleep(REOPEN_POLL);
        let Ok(node) = discover_control_node() else {
            continue; // node not back yet
        };
        let Ok(transport) = Transport::open_path(&node) else {
            continue; // reappeared but not yet openable
        };
        // Best-effort resend (a fresh reset already lands in NORMAL, but assert it).
        let _ = transport.command(&device::set_device_mode(DEVICE_MODE_NORMAL));
        if let Ok(resp) = transport.command(&device::get_device_mode())
            && resp.arguments[0] == DEVICE_MODE_NORMAL
        {
            tracing::info!(node = %node.display(), "restored device mode: NORMAL after reopen");
            return true;
        }
    }
    false
}

/// Reads live per-key analog pressure frames from the Tartarus Pro.
///
/// On construction it opens the analog input node (interface 1), arms a
/// [`DriverModeGuard`], and sends DRIVER — so simply holding an `AnalogReader`
/// keeps the device in analog mode, and dropping it restores NORMAL. Use
/// [`AnalogReader::poll`] in an event loop to pull frames.
pub struct AnalogReader {
    /// Restores NORMAL on drop (the safety-critical step). The `guard` and
    /// `input` fds are independent — one is the control interface, the other the
    /// read-only input node — so drop order between them does not affect the
    /// restore; the guard's Drop runs on every unwind path regardless.
    guard: DriverModeGuard,
    input: File,
    input_node: PathBuf,
    buf: [u8; READ_BUF_LEN],
}

impl AnalogReader {
    /// Open the analog input node and enable DRIVER mode.
    ///
    /// Construction order preserves the proven-safe sequence: open the input fd
    /// first (read-only, sends nothing to the device), then arm the guard (opens
    /// control, no mode change), then `enter_driver()`. If `enter_driver` fails,
    /// the guard's Drop still sends NORMAL (harmless), so there is never a window
    /// where DRIVER is set without an armed restore.
    pub fn open() -> Result<Self, HidError> {
        let input_node = discover_node_for_interface(INPUT_INTERFACE)?;
        let input = OpenOptions::new()
            .read(true)
            .open(&input_node)
            .map_err(|source| HidError::Io {
                path: input_node.clone(),
                source,
            })?;

        let guard = DriverModeGuard::open()?;
        guard.enter_driver()?;
        tracing::debug!(node = %input_node.display(), "analog reader open (DRIVER mode)");

        Ok(Self {
            guard,
            input,
            input_node,
            buf: [0u8; READ_BUF_LEN],
        })
    }

    /// The opened analog input node path.
    pub fn input_path(&self) -> &std::path::Path {
        &self.input_node
    }

    /// Read back the current device mode (DRIVER = 0x03 when active).
    pub fn device_mode(&self) -> Result<u8, HidError> {
        self.guard.read_mode()
    }

    /// Poll for one analog frame, waiting up to `timeout`.
    ///
    /// Returns:
    ///   - `Ok(Some(frame))` — a report-0x06 analog frame was read and decoded;
    ///   - `Ok(None)` — the timeout elapsed with no analog frame (idle, a
    ///     non-analog report such as NKRO, or a signal interrupted the wait).
    ///     Idle silence is normal: the device only emits on key-state change.
    ///   - `Err(_)` — an I/O error reading the node (e.g. device unplugged). The
    ///     caller must surface this and tear down rather than spin.
    ///
    /// `EINTR` (a signal interrupting `poll`/`read`, e.g. an app's SIGINT) maps
    /// to `Ok(None)` so an event loop simply re-checks its own stop condition and
    /// loops, unwinding through Drop on the next iteration. A signal is never an
    /// error here.
    pub fn poll(&mut self, timeout: Duration) -> Result<Option<AnalogFrame>, HidError> {
        let borrowed = self.input.as_fd();
        let mut fds = [PollFd::new(borrowed, PollFlags::POLLIN)];
        let millis = timeout.as_millis().min(u16::MAX as u128) as u16;
        match nix::poll::poll(&mut fds, PollTimeout::from(millis)) {
            Ok(0) => return Ok(None), // timed out, no data
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => return Ok(None), // signal; let caller re-check
            Err(source) => {
                // A poll() failure on the input fd (e.g. EBADF after unplug) is an
                // I/O failure on the node, not a feature-report ioctl.
                return Err(HidError::Io {
                    path: self.input_node.clone(),
                    source: std::io::Error::from_raw_os_error(source as i32),
                });
            }
        }

        if !fds[0]
            .revents()
            .map(|r| r.contains(PollFlags::POLLIN))
            .unwrap_or(false)
        {
            return Ok(None);
        }

        let n = match self.input.read(&mut self.buf) {
            Ok(0) => return Ok(None),
            Ok(n) => n,
            Err(e) if e.raw_os_error() == Some(nix::errno::Errno::EAGAIN as i32) => return Ok(None),
            Err(e) if e.raw_os_error() == Some(nix::errno::Errno::EINTR as i32) => return Ok(None),
            Err(source) => {
                return Err(HidError::Io {
                    path: self.input_node.clone(),
                    source,
                })
            }
        };

        // Decode only full report-0x06 analog frames; ignore everything else
        // (NKRO 0x01, the declared-but-silent 0x04/0x05, short reads). The read
        // buffer is sized to hold a full frame (asserted in tests).
        Ok(AnalogFrame::decode(&self.buf[..n]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tartarus_protocol::ANALOG_FRAME_LEN;

    // The frame-decode logic itself is unit-tested in `tartarus-protocol`
    // (`analog::tests`). Here we only sanity-check the buffer sizing invariant
    // the reader relies on; the rest of `AnalogReader` is hardware-bound.
    #[test]
    fn read_buffer_fits_a_full_frame() {
        const { assert!(ANALOG_FRAME_LEN <= READ_BUF_LEN) };
    }
}

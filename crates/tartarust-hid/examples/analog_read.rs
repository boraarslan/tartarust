//! Analog enable + read-path confirmation (uses the library [`AnalogReader`]).
//!
//! Puts the Tartarus Pro into **DRIVER mode** — the state Synapse uses to turn on
//! analog — then reads decoded per-key pressure frames from **interface 1** for a
//! fixed window. This is the fix for the long-standing "analog sees nothing": the
//! device only emits the raw pressure reports (id 0x06) once it's in DRIVER mode
//! (`docs/research/08-analog-handshake.md`).
//!
//! Run with:
//!     cargo run -p tartarust-hid --example analog_read
//!
//! Requires read/write access to BOTH the control node (interface 2, to send the
//! mode command) and the input node (interface 1, to read analog). With the udev
//! rule installed this is a normal-user run; otherwise it needs root.
//!
//! ## SAFETY — the keypad must never be left in DRIVER mode
//!
//! In DRIVER mode the device sends **no** normal keystrokes. The [`AnalogReader`]
//! owns a `DriverModeGuard` that restores NORMAL on `Drop` (every normal/error
//! path, hardware-proven, incl. dead-fd reopen). The one thing the library can't
//! do is turn a Ctrl-C into an unwind — a raw SIGINT default-kills the process
//! and skips `Drop`. So this example installs a SIGINT handler that only sets a
//! flag (never exits/aborts); the poll loop checks the flag, breaks, and unwinds
//! through the reader's `Drop`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use tartarust_hid::{AnalogReader, HidError};
use tartarust_protocol::ANALOG_KEY_COUNT;

/// How long to keep DRIVER mode set while polling interface 1. The Tartarus is a
/// secondary keypad (the user types on a separate keyboard), so the DRIVER "mute"
/// window is free — this is sized for a human to start the run and press keys.
const CAPTURE_SECS: u64 = 8;

/// Set by the SIGINT handler. The poll loop checks it and breaks so the reader's
/// `Drop` runs (a raw SIGINT default-kills the process and skips `Drop`).
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// SIGINT handler: flip the flag and return. Must do nothing else — no `exit`,
/// no `abort` — so the normal unwind path (and the guard's `Drop`) still runs.
extern "C" fn on_sigint(_sig: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Install the SIGINT handler. Localized `unsafe` for the `sigaction` call.
#[allow(unsafe_code)]
fn install_sigint_handler() -> Result<()> {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
    let action = SigAction::new(
        SigHandler::Handler(on_sigint),
        SaFlags::empty(), // no SA_RESTART: poll() returns EINTR so the loop notices the flag
        SigSet::empty(),
    );
    // SAFETY: `on_sigint` only stores to an atomic and returns — async-signal-safe.
    unsafe { sigaction(Signal::SIGINT, &action) }.map_err(|e| anyhow::anyhow!("sigaction: {e}"))?;
    Ok(())
}

/// Per-key-index statistics gathered over the capture window.
#[derive(Default, Clone)]
struct KeyStats {
    /// Number of frames where this index was non-zero.
    active_frames: u64,
    /// Peak pressure seen at this index.
    peak: u8,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    install_sigint_handler()?;

    // Open the reader: this opens interface 1, arms the restore guard, and sends
    // DRIVER. From here all exit paths unwind through the reader's Drop (NORMAL).
    let mut reader = match AnalogReader::open() {
        Ok(r) => r,
        Err(HidError::DeviceNotFound) => {
            eprintln!("Tartarus Pro not found (need both interface 1 and 2). Plugged in?");
            std::process::exit(2);
        }
        Err(e) if e.is_permission_denied() => {
            eprintln!("permission denied — udev rule not installed; run as root for a one-off.");
            std::process::exit(13);
        }
        Err(e) => return Err(e.into()),
    };
    println!("analog input interface: {}", reader.input_path().display());

    match reader.device_mode() {
        Ok(0x03) => println!("device mode: 0x03 (DRIVER) — analog enabled, control fd survived"),
        Ok(m) => println!("device mode: 0x{m:02x} (expected 0x03 DRIVER)"),
        Err(e) => eprintln!("note: could not read back device mode ({e})"),
    }

    println!("\n>>> Polling interface 1 for analog frames for {CAPTURE_SECS}s. <<<");
    println!(">>> Press keys on the KEYPAD (not this terminal) to see pressure values. <<<\n");

    let mut stats = vec![KeyStats::default(); ANALOG_KEY_COUNT];
    let mut frames = 0u64;
    let deadline = Instant::now() + Duration::from_secs(CAPTURE_SECS);

    while Instant::now() < deadline {
        if INTERRUPTED.load(Ordering::SeqCst) {
            println!("\n(interrupted — restoring NORMAL)");
            break;
        }
        match reader.poll(Duration::from_millis(200))? {
            Some(frame) => {
                frames += 1;
                for (i, &p) in frame.pressures.iter().enumerate() {
                    if p != 0 {
                        stats[i].active_frames += 1;
                        stats[i].peak = stats[i].peak.max(p);
                    }
                }
            }
            None => continue, // timeout / idle / signal — re-check deadline + flag
        }
    }

    report(frames, &stats);
    // `reader` drops here → restores NORMAL (logged at info level).
    Ok(())
}

/// Print the capture summary.
fn report(frames: u64, stats: &[KeyStats]) {
    println!("\n=== CAPTURE RESULT ===");
    println!("{frames} analog frame(s) (report 0x06) decoded.");
    if frames == 0 {
        println!("No analog frames seen. Either no keys were pressed, or reports only emit on");
        println!("key-state change (idle silence is NOT a failure). Re-run and press keypad keys.");
        return;
    }
    let any_active = stats.iter().any(|s| s.active_frames > 0);
    if !any_active {
        println!("Frames streamed but every byte stayed zero — no key registered pressure.");
        return;
    }
    println!("Per-key-index activity (byte index : frames active : peak pressure):");
    for (i, s) in stats.iter().enumerate() {
        if s.active_frames > 0 {
            println!(
                "  byte[{i:2}] : {:4} frames : peak 0x{:02X}",
                s.active_frames, s.peak
            );
        }
    }
    println!("\nIntermediate (non-0x00/0xFF) peaks confirm analog pressure tracking.");
}

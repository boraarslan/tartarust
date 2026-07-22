//! Map analog byte indices to physical keys.
//!
//! Runs the Tartarus Pro in DRIVER mode and, each time you press ONE key, prints
//! the byte index that lit up and its peak pressure — e.g. `byte[ 5] = 0xC3`.
//! Press the keypad keys in a known order and record the byte index for each to
//! build the byte-index -> physical-key table the actuation engine needs.
//!
//! Run with:
//!     cargo run -p tartarust-hid --example analog_map
//!
//! Runs until you press Ctrl-C. NORMAL mode is always restored on exit (the
//! [`AnalogReader`]'s guard handles it; a SIGINT handler turns Ctrl-C into a
//! clean unwind through `Drop`).
//!
//! Tips for a clean map:
//!   - Press ONE key at a time, fully, then release before the next.
//!   - A line is emitted on RELEASE, carrying the peak pressure (the true max).
//!   - Hold-and-release deliberately; brief taps still register.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use tartarust_hid::{AnalogReader, HidError};
use tartarust_protocol::ANALOG_KEY_COUNT;

/// Pressure above which a byte index counts as "pressed" (rising edge). Small
/// enough to catch light presses, high enough to ignore sensor noise / rest.
const PRESS_THRESHOLD: u8 = 0x10;
/// Pressure at/below which a pressed index counts as "released" (falling edge).
/// Hysteresis: lower than [`PRESS_THRESHOLD`] so a value hovering near the
/// threshold doesn't chatter press/release.
const RELEASE_THRESHOLD: u8 = 0x08;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_sig: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Install the SIGINT handler (flag-only; never exits, so `Drop` still runs).
#[allow(unsafe_code)]
fn install_sigint_handler() -> Result<()> {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
    let action = SigAction::new(
        SigHandler::Handler(on_sigint),
        SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: `on_sigint` only stores to an atomic and returns — async-signal-safe.
    unsafe { sigaction(Signal::SIGINT, &action) }.map_err(|e| anyhow::anyhow!("sigaction: {e}"))?;
    Ok(())
}

/// Per-index press tracking: whether the index is currently held, and the peak
/// pressure observed during the current press.
#[derive(Default, Clone, Copy)]
struct KeyTracker {
    pressed: bool,
    peak: u8,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    install_sigint_handler()?;

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

    match reader.device_mode() {
        Ok(0x03) => {}
        Ok(m) => eprintln!("warning: device mode is 0x{m:02x}, expected 0x03 (DRIVER)"),
        Err(e) => eprintln!("note: could not read back device mode ({e})"),
    }

    println!("DRIVER mode active on {}.", reader.input_path().display());
    println!("Press keypad keys ONE AT A TIME. Each release prints its byte index + peak.");
    println!("Press Ctrl-C when done (NORMAL mode is restored on exit).\n");

    let mut trackers = [KeyTracker::default(); ANALOG_KEY_COUNT];
    let mut presses = 0u64;

    while !INTERRUPTED.load(Ordering::SeqCst) {
        let frame = match reader.poll(Duration::from_millis(200))? {
            Some(f) => f,
            None => continue, // idle / timeout / signal — loop re-checks the flag
        };

        for (i, &pressure) in frame.pressures.iter().enumerate() {
            let t = &mut trackers[i];
            if t.pressed {
                t.peak = t.peak.max(pressure);
                if pressure <= RELEASE_THRESHOLD {
                    // Falling edge — emit the press with its peak, then reset.
                    presses += 1;
                    println!("  #{presses:<3} byte[{i:2}] = 0x{:02X}", t.peak);
                    t.pressed = false;
                    t.peak = 0;
                }
            } else if pressure >= PRESS_THRESHOLD {
                // Rising edge — start tracking this press.
                t.pressed = true;
                t.peak = pressure;
            }
        }
    }

    println!("\n(interrupted — restoring NORMAL)");
    println!("{presses} key press(es) detected.");
    // `reader` drops here → restores NORMAL.
    Ok(())
}

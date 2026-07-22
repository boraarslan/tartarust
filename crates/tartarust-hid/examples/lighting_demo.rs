//! Lighting hardware spot-check: send every Chroma effect + a custom frame to
//! the real device and report which the hardware accepts. De-risks the effect
//! argument layouts that were previously only unit-tested against OpenRazer
//! source.
//!
//! Run with:
//!     cargo run -p tartarust-hid --example lighting_demo
//!
//! Safety: only lighting (class 0x0F) and brightness writes — all reversible.
//! The run always ends by restoring spectrum + full brightness so the keypad is
//! left in a sane, visible state.
//!
//! NOTE on the earlier "EPIPE / must acquire driver control" theory: it did not
//! reproduce under controlled testing — effect and brightness writes succeed
//! directly with no acquire step (matching OpenRazer, which also has none). The
//! occasional EPIPE is a transient USB STALL (e.g. kernel-module contention on
//! the node), so each command here is retried once on a send-phase STALL.

use std::time::Duration;

use anyhow::Result;
use tartarust_hid::{HidError, Transport, discover_control_node};
use tartarust_protocol::lighting::{self, Rgb, WaveDirection};
use tartarust_protocol::{RazerReport, device};

/// How long to hold each effect so a human can glance at the keypad.
const DWELL: Duration = Duration::from_millis(900);

/// Outcome of one effect attempt.
enum Outcome {
    Ok(u8),        // accepted; carries the response status byte
    Stall,         // HIDIOCSFEATURE EPIPE (USB STALL), even after one retry
    Status(u8),    // device returned a non-success status after retries
    Mismatch,      // response header never echoed the request
    Other(String), // any other transport error
}

impl Outcome {
    fn label(&self) -> String {
        match self {
            Outcome::Ok(s) => format!("OK (status 0x{s:02x})"),
            Outcome::Stall => "STALL (EPIPE)".to_string(),
            Outcome::Status(s) => format!("device status 0x{s:02x}"),
            Outcome::Mismatch => "response mismatch".to_string(),
            Outcome::Other(e) => format!("error: {e}"),
        }
    }
    fn accepted(&self) -> bool {
        matches!(self, Outcome::Ok(_))
    }
}

/// Send one report, retrying once if the send phase STALLs (EPIPE).
fn send_one(t: &Transport, report: &RazerReport) -> Outcome {
    for attempt in 0..2 {
        match t.command(report) {
            Ok(resp) => return Outcome::Ok(resp.status),
            Err(e) if e.is_epipe_stall() => {
                if attempt == 0 {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                return Outcome::Stall;
            }
            Err(HidError::DeviceStatus { status, .. }) => return Outcome::Status(status),
            Err(HidError::ResponseMismatch { .. }) => return Outcome::Mismatch,
            Err(e) => return Outcome::Other(e.to_string()),
        }
    }
    Outcome::Stall
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let node = discover_control_node()?;
    let t = Transport::open_path(&node)?;
    println!("control interface: {}", node.display());

    let red = Rgb::new(0xFF, 0x00, 0x00);
    let green = Rgb::new(0x00, 0xFF, 0x00);
    let blue = Rgb::new(0x00, 0x00, 0xFF);

    // (label, report). Each is one extended-matrix effect command.
    let effects: Vec<(&str, RazerReport)> = vec![
        ("none", lighting::effect_none()),
        ("static(red)", lighting::effect_static(red)),
        ("spectrum", lighting::effect_spectrum()),
        ("wave(right)", lighting::effect_wave(WaveDirection::Right)),
        ("reactive(2,green)", lighting::effect_reactive(2, green)),
        (
            "breathing_single(blue)",
            lighting::effect_breathing_single(blue),
        ),
        (
            "breathing_dual(red,blue)",
            lighting::effect_breathing_dual(red, blue),
        ),
        ("breathing_random", lighting::effect_breathing_random()),
        ("starlight_random(2)", lighting::effect_starlight_random(2)),
        (
            "starlight_single(2,green)",
            lighting::effect_starlight_single(2, green),
        ),
        (
            "starlight_dual(2,red,blue)",
            lighting::effect_starlight_dual(2, red, blue),
        ),
    ];

    let mut results: Vec<(String, Outcome)> = Vec::new();

    for (label, report) in &effects {
        let outcome = send_one(&t, report);
        println!("  {label:<28} -> {}", outcome.label());
        results.push((label.to_string(), outcome));
        std::thread::sleep(DWELL);
    }

    // Custom frame: upload one full row of distinct colours across the 21
    // columns, then switch to custom-frame display mode (the mode switch is what
    // makes it visible).
    {
        let cols = lighting::MATRIX_COLS as u8;
        let colors: Vec<Rgb> = (0..cols)
            .map(|i| {
                // simple gradient so the row is visibly multicolour
                Rgb::new(
                    (255u16 * i as u16 / cols as u16) as u8,
                    (255u16 * (cols - i) as u16 / cols as u16) as u8,
                    0x40,
                )
            })
            .collect();
        match lighting::set_custom_frame(0, 0, cols - 1, &colors) {
            Ok(frame) => {
                let up = send_one(&t, &frame);
                println!("  {:<28} -> {}", "custom_frame upload", up.label());
                results.push(("custom_frame upload".into(), up));
                let disp = send_one(&t, &lighting::effect_custom_frame());
                println!("  {:<28} -> {}", "custom_frame display", disp.label());
                results.push(("custom_frame display".into(), disp));
                std::thread::sleep(DWELL);
            }
            Err(e) => println!("  custom_frame build failed: {e}"),
        }
    }

    // Brightness: prove a brightness *change* is accepted (the old EPIPE case).
    {
        let cur = match t.command(&device::get_brightness()) {
            Ok(r) => device::parse_brightness(&r),
            Err(_) => 255,
        };
        let target = if cur == 128 { 200 } else { 128 };
        let out = send_one(&t, &device::set_brightness(target));
        println!(
            "  {:<28} -> {}",
            format!("set_brightness({target})"),
            out.label()
        );
        results.push((format!("set_brightness({target})"), out));
    }

    // Unconditional restore: spectrum + full brightness.
    let _ = t.command(&lighting::effect_spectrum());
    let _ = t.command(&device::set_brightness(255));
    println!("restored: spectrum + brightness 255");

    // Summary.
    let accepted: Vec<&str> = results
        .iter()
        .filter(|(_, o)| o.accepted())
        .map(|(l, _)| l.as_str())
        .collect();
    let rejected: Vec<String> = results
        .iter()
        .filter(|(_, o)| !o.accepted())
        .map(|(l, o)| format!("{l} [{}]", o.label()))
        .collect();
    println!("\n=== SUMMARY ===");
    println!("accepted ({}/{}):", accepted.len(), results.len());
    for a in &accepted {
        println!("  + {a}");
    }
    if !rejected.is_empty() {
        println!("rejected:");
        for r in &rejected {
            println!("  - {r}");
        }
    }
    Ok(())
}

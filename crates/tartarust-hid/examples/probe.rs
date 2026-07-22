//! End-to-end device validation: discover the Tartarus Pro control interface,
//! then read and print firmware version + serial.
//!
//! Run with:
//!     cargo run -p tartarust-hid --example probe
//!
//! Requires read/write access to the control hidraw node (interface 2). Until
//! the udev rule is installed the node is root-owned, so a non-root run fails
//! with a permission error — that is expected and reported clearly, not a bug
//! in the transport.

use anyhow::Result;
use tartarust_hid::{HidError, Transport, discover_control_node};
use tartarust_protocol::device;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tartarust_hid=debug".into()),
        )
        .init();

    let node = match discover_control_node() {
        Ok(n) => n,
        Err(HidError::DeviceNotFound) => {
            eprintln!("Tartarus Pro (1532:0244) control interface not found.");
            eprintln!("Is the device plugged in?");
            std::process::exit(2);
        }
        Err(e) => return Err(e.into()),
    };
    println!("control interface: {}", node.display());

    let transport = match Transport::open_path(&node) {
        Ok(t) => t,
        Err(e) if e.is_permission_denied() => {
            eprintln!("permission denied opening {}", node.display());
            eprintln!("the udev rule for non-root access is not installed yet.");
            eprintln!("retry once it lands, or run as root for a one-off check.");
            std::process::exit(13);
        }
        Err(e) => return Err(e.into()),
    };

    // Firmware version.
    let fw_resp = transport.command(&device::get_firmware_version())?;
    let fw = device::parse_firmware_version(&fw_resp);
    println!("firmware version: {fw}");

    // Serial.
    let serial_resp = transport.command(&device::get_serial())?;
    let serial = device::parse_serial(&serial_resp)?;
    println!("serial: {serial}");

    Ok(())
}

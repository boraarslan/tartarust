//! Locate the Tartarus Pro's control hidraw node via sysfs.
//!
//! The device exposes several hidraw interfaces; control feature reports go to
//! **USB interface 2** (`docs/research/02-tartarus-pro-device.md`). The node
//! number is not stable across machines/reboots, so we discover it by matching:
//!
//!   - `/sys/class/hidraw/hidrawN/device/uevent` line
//!     `HID_ID=0003:00001532:00000244` (bus:vendor:product, uppercase hex), and
//!   - the ancestor USB-interface directory's `bInterfaceNumber` == `02`.
//!
//! The matching node maps to `/dev/hidrawN`.

use std::fs;
use std::path::{Path, PathBuf};

use tartarust_protocol::{PRODUCT_ID, VENDOR_ID};

use crate::error::HidError;

const SYS_HIDRAW: &str = "/sys/class/hidraw";

/// USB interface that carries the 90-byte vendor control feature report.
pub const CONTROL_INTERFACE: u8 = 0x02;
/// USB interface that carries keyboard input — including the analog-shaped
/// INPUT reports (IDs 0x04/0x05/0x06), per the hardware recon in
/// `docs/research/03-analog-actuation.md`.
pub const INPUT_INTERFACE: u8 = 0x01;

/// Find `/dev/hidrawN` for the Tartarus Pro control interface (interface 2).
///
/// Returns [`HidError::DeviceNotFound`] if no matching node exists (device
/// unplugged, or a kernel without hidraw sysfs).
pub fn discover_control_node() -> Result<PathBuf, HidError> {
    discover_node_for_interface(CONTROL_INTERFACE)
}

/// Find `/dev/hidrawN` for the Tartarus Pro USB interface `iface`, matching the
/// device by `HID_ID` and the ancestor `bInterfaceNumber`.
///
/// Read-only: this only walks sysfs. Opening the returned node and `read()`ing
/// it (e.g. interface 1 for analog input recon) sends nothing to the device.
pub fn discover_node_for_interface(iface: u8) -> Result<PathBuf, HidError> {
    let entries = match fs::read_dir(SYS_HIDRAW) {
        Ok(e) => e,
        Err(_) => return Err(HidError::DeviceNotFound),
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("hidraw") {
            continue;
        }
        let sys_node = entry.path(); // /sys/class/hidraw/hidrawN

        if !hid_id_matches(&sys_node, VENDOR_ID, PRODUCT_ID) {
            continue;
        }
        if interface_number(&sys_node) == Some(iface) {
            let dev = PathBuf::from("/dev").join(name.as_ref());
            tracing::debug!(node = %dev.display(), iface, "found Tartarus Pro interface");
            return Ok(dev);
        }
    }

    Err(HidError::DeviceNotFound)
}

/// Whether `hidrawN/device/uevent` carries `HID_ID=0003:0000VVVV:0000PPPP`.
fn hid_id_matches(sys_node: &Path, vendor: u16, product: u16) -> bool {
    let uevent = sys_node.join("device/uevent");
    let Ok(text) = fs::read_to_string(&uevent) else {
        return false;
    };
    // Format: HID_ID=BUS:VENDOR:PRODUCT, each field uppercase-hex, zero-padded.
    let expected = format!("0003:{:08X}:{:08X}", vendor, product);
    text.lines()
        .filter_map(|l| l.strip_prefix("HID_ID="))
        .any(|v| v.eq_ignore_ascii_case(&expected))
}

/// Read the USB `bInterfaceNumber` for this hidraw node by resolving its
/// `device` symlink and walking up to the nearest ancestor that has the file.
fn interface_number(sys_node: &Path) -> Option<u8> {
    let device = fs::canonicalize(sys_node.join("device")).ok()?;
    let mut dir: &Path = &device;
    // The USB interface dir (holding bInterfaceNumber) is the immediate parent
    // of the resolved HID device dir, but walk a few levels up to be robust.
    for _ in 0..6 {
        let candidate = dir.join("bInterfaceNumber");
        if let Ok(s) = fs::read_to_string(&candidate) {
            return u8::from_str_radix(s.trim(), 16).ok();
        }
        dir = dir.parent()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_id_format() {
        // Sanity-check the expected string against the real Tartarus value.
        let expected = format!("0003:{:08X}:{:08X}", VENDOR_ID, PRODUCT_ID);
        assert_eq!(expected, "0003:00001532:00000244");
    }
}

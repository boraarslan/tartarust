//! Device-info and brightness command builders, plus response parsers.
//!
//! Command bytes are pinned from `docs/research/02-tartarus-pro-device.md` and
//! the OpenRazer constructors in `razerchromacommon.c`:
//!
//! | Command            | tid  | class | id   | data_size | args                          |
//! |--------------------|------|-------|------|-----------|-------------------------------|
//! | get_firmware       | 0xFF | 0x00  | 0x81 | 0x02      | (none)                        |
//! | get_serial         | 0xFF | 0x00  | 0x82 | 0x16      | (none)                        |
//! | get_device_mode    | 0xFF | 0x00  | 0x84 | 0x02      | (none)                        |
//! | set_device_mode    | 0x1F | 0x00  | 0x04 | 0x02      | mode, 0x00                    |
//! | set_brightness     | 0x1F | 0x0F  | 0x04 | 0x03      | VARSTORE, ZERO_LED, level     |
//! | get_brightness     | 0x1F | 0x0F  | 0x84 | 0x03      | VARSTORE, ZERO_LED            |

use crate::{
    command::{Command, TransactionId},
    error::ProtocolError,
    lighting::{VARSTORE, ZERO_LED},
    report::RazerReport,
};

// ---- request builders -----------------------------------------------------

/// `get_firmware_version` — class 0x00, id 0x81. Response: arg[0]=major,
/// arg[1]=minor.
pub fn get_firmware_version() -> RazerReport {
    Command::new(TransactionId::Standard, 0x00, 0x81, 0x02, &[]).to_report()
}

/// `get_serial` — class 0x00, id 0x82. Response: arg[0..22] = ASCII serial.
pub fn get_serial() -> RazerReport {
    Command::new(TransactionId::Standard, 0x00, 0x82, 0x16, &[]).to_report()
}

/// Device-mode values for [`set_device_mode`] (`set_device_mode` arg[0]).
///
/// `NORMAL` (0x00) is the standard HID-keyboard mode the keypad powers on in.
/// `DRIVER` (0x03) stops normal keystroke HID and makes the device emit the raw
/// per-key analog pressure reports (IDs 0x04/0x05/0x06) on USB interface 1.
/// Source: `docs/research/08-analog-handshake.md` (decoded from Synapse's login).
pub const DEVICE_MODE_NORMAL: u8 = 0x00;
/// See [`DEVICE_MODE_NORMAL`]. 0x03 = DRIVER mode (raw analog reports).
pub const DEVICE_MODE_DRIVER: u8 = 0x03;

/// `get_device_mode` — class 0x00, id 0x84. Response: arg[0]=mode, arg[1]=param.
pub fn get_device_mode() -> RazerReport {
    Command::new(TransactionId::Standard, 0x00, 0x84, 0x02, &[]).to_report()
}

/// `set_device_mode` — class 0x00, id 0x04, args `[mode, 0x00]`. Use the
/// [`DEVICE_MODE_NORMAL`] / [`DEVICE_MODE_DRIVER`] constants for `mode`.
///
/// Transaction id: the Tartarus Pro shares **0x1F** for this command, per
/// OpenRazer's per-PID table (`razerkbd_driver.c` `razer_set_device_mode`:
/// `USB_DEVICE_ID_RAZER_TARTARUS_PRO → request.transaction_id.id = 0x1F`).
/// Hence [`TransactionId::Lighting`] (0x1F) here despite the non-lighting class.
/// (The tid is just an echo and the device tolerates others — Synapse's login
/// used 0x07 — but 0x1F is the production-proven value for product 0x0244.)
///
/// History: OpenRazer skips `set_device_mode(0x00,0x00)` **at module probe time**
/// for the Pro ("Tartarus Pro resets when it receives this command",
/// `razerkbd_driver.c:5787-5794`) because the device is already NORMAL on enumerate
/// and re-asserting it there re-enumerates the device. That caveat is probe-time
/// only: the same comment notes the daemon then routinely puts the device into
/// DRIVER mode, and the Synapse capture confirms DRIVER (0x03) is sent on every
/// login and the control fd survives. Callers that send DRIVER **must** restore
/// NORMAL on exit (DRIVER leaves the keypad mute); restoring NORMAL may itself
/// re-enumerate the Pro, which still lands it in NORMAL — handle a dropped fd by
/// re-discovering the control node (see `tartarus-hid`'s `analog_read` example).
pub fn set_device_mode(mode: u8) -> RazerReport {
    Command::new(TransactionId::Lighting, 0x00, 0x04, 0x02, &[mode, 0x00]).to_report()
}

/// `set_brightness` — class 0x0F, id 0x04. Targets `ZERO_LED` (0x00), **not**
/// the backlight LED used by effects. `level` is 0..=255.
pub fn set_brightness(level: u8) -> RazerReport {
    Command::new(
        TransactionId::Lighting,
        0x0F,
        0x04,
        0x03,
        &[VARSTORE, ZERO_LED, level],
    )
    .to_report()
}

/// `get_brightness` — class 0x0F, id 0x84. Response arg[2] = brightness.
pub fn get_brightness() -> RazerReport {
    Command::new(
        TransactionId::Lighting,
        0x0F,
        0x84,
        0x03,
        &[VARSTORE, ZERO_LED],
    )
    .to_report()
}

// ---- response parsers -----------------------------------------------------

/// Firmware version `(major, minor)`, as in OpenRazer's `%u.%02u` formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirmwareVersion {
    pub major: u8,
    pub minor: u8,
}

impl core::fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "v{}.{:02}", self.major, self.minor)
    }
}

/// Parse a `get_firmware_version` response: arg[0]=major, arg[1]=minor.
pub fn parse_firmware_version(resp: &RazerReport) -> FirmwareVersion {
    FirmwareVersion {
        major: resp.arguments[0],
        minor: resp.arguments[1],
    }
}

/// Parse a `get_serial` response: arg[0..22] is an ASCII serial, NUL-padded.
/// Trailing NULs and whitespace are trimmed.
pub fn parse_serial(resp: &RazerReport) -> Result<String, ProtocolError> {
    let raw = &resp.arguments[..22];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let s = core::str::from_utf8(&raw[..end])
        .map_err(|_| ProtocolError::BadResponse("serial is not valid ASCII/UTF-8"))?;
    Ok(s.trim().to_string())
}

/// Parse a `get_brightness` response: arg[2] = brightness level.
pub fn parse_brightness(resp: &RazerReport) -> u8 {
    resp.arguments[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_firmware_version_exact_bytes() {
        // tid 0xFF, ds 0x02, class 0x00, id 0x81, no args; crc = 0x83.
        let b = get_firmware_version().to_bytes().unwrap();
        assert_eq!(b[0], 0x00, "status");
        assert_eq!(b[1], 0xFF, "transaction_id");
        assert_eq!([b[2], b[3]], [0x00, 0x00], "remaining_packets");
        assert_eq!(b[4], 0x00, "protocol_type");
        assert_eq!(b[5], 0x02, "data_size");
        assert_eq!(b[6], 0x00, "command_class");
        assert_eq!(b[7], 0x81, "command_id");
        assert!(b[8..88].iter().all(|&x| x == 0), "args all zero");
        assert_eq!(b[88], 0x83, "crc");
        assert_eq!(b[89], 0x00, "reserved");
    }

    #[test]
    fn set_brightness_exact_bytes() {
        // tid 0x1F, ds 0x03, class 0x0F, id 0x04, args 01 00 <level>.
        // crc = 0x03 ^ 0x0F ^ 0x04 ^ 0x01 ^ 0x00 ^ level = 0x09 ^ level.
        let b = set_brightness(0x00).to_bytes().unwrap();
        assert_eq!(b[1], 0x1F);
        assert_eq!(b[5], 0x03);
        assert_eq!(b[6], 0x0F);
        assert_eq!(b[7], 0x04);
        assert_eq!(&b[8..11], &[0x01, 0x00, 0x00]);
        assert_eq!(b[88], 0x09, "crc for level 0x00");

        let b80 = set_brightness(0x80).to_bytes().unwrap();
        assert_eq!(b80[10], 0x80);
        assert_eq!(b80[88], 0x09 ^ 0x80, "crc tracks level");
    }

    #[test]
    fn set_device_mode_exact_bytes() {
        // tid 0x1F (Pro shares Lighting for set_device_mode), ds 0x02,
        // class 0x00, id 0x04, args [mode, 0x00].
        // crc covers bytes 2..=87 = ds ^ class ^ id ^ args
        //   = 0x02 ^ 0x00 ^ 0x04 ^ mode ^ 0x00 = 0x06 ^ mode.
        let b = set_device_mode(DEVICE_MODE_DRIVER).to_bytes().unwrap();
        assert_eq!(b[0], 0x00, "status");
        assert_eq!(b[1], 0x1F, "transaction_id");
        assert_eq!([b[2], b[3]], [0x00, 0x00], "remaining_packets");
        assert_eq!(b[4], 0x00, "protocol_type");
        assert_eq!(b[5], 0x02, "data_size");
        assert_eq!(b[6], 0x00, "command_class");
        assert_eq!(b[7], 0x04, "command_id");
        assert_eq!(&b[8..10], &[0x03, 0x00], "args = [DRIVER, 0x00]");
        assert!(b[10..88].iter().all(|&x| x == 0), "trailing args zero");
        // DRIVER: 0x06 ^ 0x03 = 0x05 (matches the verified login capture, frame 119).
        assert_eq!(b[88], 0x05, "crc for DRIVER mode");
        assert_eq!(b[89], 0x00, "reserved");

        // NORMAL: 0x06 ^ 0x00 = 0x06.
        let n = set_device_mode(DEVICE_MODE_NORMAL).to_bytes().unwrap();
        assert_eq!(&n[8..10], &[0x00, 0x00], "args = [NORMAL, 0x00]");
        assert_eq!(n[88], 0x06, "crc for NORMAL mode");
    }

    #[test]
    fn parse_firmware_and_serial() {
        let mut resp = RazerReport::new();
        resp.arguments[0] = 1;
        resp.arguments[1] = 5;
        let fw = parse_firmware_version(&resp);
        assert_eq!(fw, FirmwareVersion { major: 1, minor: 5 });
        assert_eq!(fw.to_string(), "v1.05");

        let serial = b"PM0000000001\0\0\0\0\0\0\0\0\0\0";
        resp.arguments[..22].copy_from_slice(serial);
        assert_eq!(parse_serial(&resp).unwrap(), "PM0000000001");
    }
}

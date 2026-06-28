//! The 90-byte Razer HID feature report (`struct razer_report`) and its XOR
//! checksum.
//!
//! Layout is pinned from OpenRazer `driver/razercommon.h:124-136` and
//! `docs/research/01-razer-protocol.md`:
//!
//! | Offset | Size | Field               | Endianness |
//! |-------:|-----:|---------------------|------------|
//! | 0      | 1    | `status`            | n/a        |
//! | 1      | 1    | `transaction_id`    | n/a        |
//! | 2      | 2    | `remaining_packets` | **big**    |
//! | 4      | 1    | `protocol_type`     | n/a        |
//! | 5      | 1    | `data_size`         | n/a        |
//! | 6      | 1    | `command_class`     | n/a        |
//! | 7      | 1    | `command_id`        | n/a        |
//! | 8      | 80   | `arguments`         | n/a        |
//! | 88     | 1    | `crc`               | n/a        |
//! | 89     | 1    | `reserved`          | n/a        |

use deku::prelude::*;

use crate::{error::ProtocolError, REPORT_SIZE};

/// Status byte values (`razercommon.h` `RAZER_CMD_*` macros).
///
/// `NewCommand` (0x00) is what we stamp on an outgoing request; the device never
/// returns it. `Busy` is **treated as success** by the OpenRazer driver, and we
/// follow suit (see `tartarus-hid`'s retry loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// 0x00 — value stamped on a freshly built request before sending.
    NewCommand = 0x00,
    /// 0x01 — `RAZER_CMD_BUSY`; some commands report this but did succeed.
    Busy = 0x01,
    /// 0x02 — `RAZER_CMD_SUCCESSFUL`.
    Successful = 0x02,
    /// 0x03 — `RAZER_CMD_FAILURE`.
    Failure = 0x03,
    /// 0x04 — `RAZER_CMD_TIMEOUT` (a.k.a. "No Response").
    Timeout = 0x04,
    /// 0x05 — `RAZER_CMD_NOT_SUPPORTED`.
    NotSupported = 0x05,
}

impl Status {
    /// Decode a raw status byte, or `None` for an unknown value.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x00 => Some(Self::NewCommand),
            0x01 => Some(Self::Busy),
            0x02 => Some(Self::Successful),
            0x03 => Some(Self::Failure),
            0x04 => Some(Self::Timeout),
            0x05 => Some(Self::NotSupported),
            _ => None,
        }
    }

    /// Whether this status counts as a successful response. The driver treats
    /// `Busy` as success (`razerkbd_driver.c:432-435`), so we do too.
    pub fn is_success(self) -> bool {
        matches!(self, Self::Successful | Self::Busy)
    }
}

/// Razer USB HID feature report — exactly 90 bytes on the wire.
///
/// Note: `crc` is **not** auto-maintained by `to_bytes()`. Call
/// [`RazerReport::update_crc`] after mutating any covered field (or use the
/// `command`/`device`/`lighting` builders, which set it for you).
#[derive(Debug, Clone, PartialEq, Eq, DekuRead, DekuWrite)]
pub struct RazerReport {
    /// Byte 0 — request/response status. See [`Status`].
    pub status: u8,
    /// Byte 1 — groups request/response. For the Tartarus Pro this is the
    /// per-command transaction id (0x1F / 0x3F / 0xFF).
    pub transaction_id: u8,
    /// Bytes 2-3 — remaining packets, **big-endian**. `0x0000` for the
    /// single-packet commands this device uses.
    #[deku(endian = "big")]
    pub remaining_packets: u16,
    /// Byte 4 — protocol type, always `0x00`.
    pub protocol_type: u8,
    /// Byte 5 — payload size in `arguments` (≤ 80).
    pub data_size: u8,
    /// Byte 6 — command class.
    pub command_class: u8,
    /// Byte 7 — command id; bit 7 set means a "get" (read) direction.
    pub command_id: u8,
    /// Bytes 8-87 — 80-byte argument payload.
    pub arguments: [u8; 80],
    /// Byte 88 — XOR checksum of bytes 2..=87. Maintained by
    /// [`RazerReport::update_crc`], not by `to_bytes()`.
    pub crc: u8,
    /// Byte 89 — reserved, always `0x00`.
    pub reserved: u8,
}

impl RazerReport {
    /// A zeroed request with `status = 0x00` (`NewCommand`) and all other fields
    /// at zero. Builders fill in the header, arguments, transaction id, and crc.
    pub fn new() -> Self {
        Self {
            status: 0x00,
            transaction_id: 0x00,
            remaining_packets: 0x0000,
            protocol_type: 0x00,
            data_size: 0x00,
            command_class: 0x00,
            command_id: 0x00,
            arguments: [0u8; 80],
            crc: 0x00,
            reserved: 0x00,
        }
    }

    /// XOR of bytes 2..=87 of the serialized report
    /// (`razer_calculate_crc`, `razercommon.c:111-124`).
    ///
    /// Covers: `remaining_packets` (2,3), `protocol_type` (4), `data_size` (5),
    /// `command_class` (6), `command_id` (7), `arguments` (8..=87).
    /// Excludes: `status` (0), `transaction_id` (1), `crc` (88), `reserved` (89).
    pub fn compute_crc(&self) -> u8 {
        let mut crc: u8 = 0;
        crc ^= (self.remaining_packets >> 8) as u8;
        crc ^= self.remaining_packets as u8;
        crc ^= self.protocol_type;
        crc ^= self.data_size;
        crc ^= self.command_class;
        crc ^= self.command_id;
        for b in &self.arguments {
            crc ^= b;
        }
        crc
    }

    /// Recompute and store the checksum in the `crc` field. Call after building
    /// or mutating a request, before serializing.
    pub fn update_crc(&mut self) {
        self.crc = self.compute_crc();
    }

    /// Whether the stored `crc` field matches a freshly computed checksum.
    pub fn verify_crc(&self) -> bool {
        self.crc == self.compute_crc()
    }

    /// Serialize to the exact 90-byte wire buffer. Does **not** recompute the
    /// crc — call [`RazerReport::update_crc`] first if a covered field changed.
    pub fn to_bytes(&self) -> Result<[u8; REPORT_SIZE], ProtocolError> {
        let v = DekuContainerWrite::to_bytes(self)?;
        // deku produces exactly REPORT_SIZE bytes for this fixed-size struct;
        // convert to a fixed array so callers get a compile-time size.
        <[u8; REPORT_SIZE]>::try_from(v.as_slice())
            .map_err(|_| ProtocolError::InvalidArgument("serialized report was not 90 bytes"))
    }

    /// Parse a 90-byte wire buffer into a [`RazerReport`].
    pub fn from_bytes(buf: &[u8; REPORT_SIZE]) -> Result<Self, ProtocolError> {
        let report = Self::try_from(buf.as_slice())?;
        Ok(report)
    }

    /// Decode the `status` byte into a [`Status`], if it is a known value.
    pub fn status(&self) -> Option<Status> {
        Status::from_u8(self.status)
    }
}

impl Default for RazerReport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report round-trips byte-for-byte through serialize/parse.
    #[test]
    fn round_trip() {
        let mut r = RazerReport::new();
        r.transaction_id = 0x1F;
        r.data_size = 0x03;
        r.command_class = 0x0F;
        r.command_id = 0x04;
        r.arguments[0] = 0x01;
        r.arguments[2] = 0x80;
        r.update_crc();

        let bytes = r.to_bytes().expect("serialize");
        assert_eq!(bytes.len(), REPORT_SIZE);
        let parsed = RazerReport::from_bytes(&bytes).expect("parse");
        assert_eq!(parsed, r);
        assert!(parsed.verify_crc());
    }

    /// Field offsets land where the spec says (big-endian remaining_packets,
    /// crc at 88, reserved at 89).
    #[test]
    fn field_offsets() {
        let mut r = RazerReport::new();
        r.status = 0xAA;
        r.transaction_id = 0xBB;
        r.remaining_packets = 0x1234; // big-endian -> 0x12 0x34
        r.protocol_type = 0xCC;
        r.data_size = 0x05;
        r.command_class = 0x0F;
        r.command_id = 0x84;
        r.arguments[0] = 0x11;
        r.arguments[79] = 0x22;
        r.update_crc();
        r.reserved = 0xDD;

        let b = r.to_bytes().unwrap();
        assert_eq!(b[0], 0xAA, "status");
        assert_eq!(b[1], 0xBB, "transaction_id");
        assert_eq!([b[2], b[3]], [0x12, 0x34], "remaining_packets big-endian");
        assert_eq!(b[4], 0xCC, "protocol_type");
        assert_eq!(b[5], 0x05, "data_size");
        assert_eq!(b[6], 0x0F, "command_class");
        assert_eq!(b[7], 0x84, "command_id");
        assert_eq!(b[8], 0x11, "arguments[0]");
        assert_eq!(b[87], 0x22, "arguments[79]");
        assert_eq!(b[88], r.crc, "crc at 88");
        assert_eq!(b[89], 0xDD, "reserved at 89");
    }

    /// CRC is the XOR of bytes 2..=87 — hand-computed against a known vector.
    #[test]
    fn crc_hand_vector() {
        // get_firmware_version: tid 0xFF, ds 0x02, class 0x00, id 0x81, no args.
        // crc = 0x00(rem hi) ^ 0x00(rem lo) ^ 0x00(proto) ^ 0x02(ds)
        //       ^ 0x00(class) ^ 0x81(id) ^ (all-zero args) = 0x02 ^ 0x81 = 0x83.
        let mut r = RazerReport::new();
        r.transaction_id = 0xFF;
        r.data_size = 0x02;
        r.command_class = 0x00;
        r.command_id = 0x81;
        assert_eq!(r.compute_crc(), 0x83);

        // Independent reference: fold XOR over the serialized bytes 2..88.
        r.update_crc();
        let b = r.to_bytes().unwrap();
        let reference = b[2..88].iter().fold(0u8, |acc, &x| acc ^ x);
        assert_eq!(b[88], reference);
    }

    #[test]
    fn status_decoding() {
        assert_eq!(Status::from_u8(0x02), Some(Status::Successful));
        assert!(Status::Busy.is_success());
        assert!(Status::Successful.is_success());
        assert!(!Status::Failure.is_success());
        assert_eq!(Status::from_u8(0x42), None);
    }
}

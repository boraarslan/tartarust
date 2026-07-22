//! Analog input frame decode for the Tartarus Pro's DRIVER-mode reports.
//!
//! In DRIVER mode (`device::set_device_mode(DEVICE_MODE_DRIVER)`) the keypad
//! stops sending normal keystroke HID and emits raw per-key pressure reports on
//! USB **interface 1**. Hardware-confirmed (2026-06-22): report id **0x06**
//! streams 24-byte frames — a 1-byte report id followed by **23** per-key
//! pressure bytes, each `0x00..=0xFF` (0 = released, 0xFF = fully actuated),
//! with intermediate values tracking press depth. See
//! `docs/research/03-analog-actuation.md` (descriptor recon: IDs 0x04/0x05/0x06
//! each `Usage 0x03, LogicalMax 255, Size 8, Count 23, INPUT`) and
//! `docs/research/08-analog-handshake.md` (the DRIVER-mode enable).
//!
//! This module is pure decode — it turns a raw HID frame into a fixed pressure
//! array. The byte-index → physical-key mapping is built separately (the
//! `analog_map` example in `tartarust-hid`) and lives above this layer.

/// HID report id carrying live per-key analog pressure (hardware-confirmed).
///
/// The descriptor also declares 0x04 and 0x05 with the same shape, but only 0x06
/// was observed streaming on hardware; decode targets 0x06 and ignores the rest.
pub const ANALOG_REPORT_ID: u8 = 0x06;

/// Number of per-key pressure bytes in an analog frame (descriptor `Count 23`).
pub const ANALOG_KEY_COUNT: usize = 23;

/// On-wire length of an analog frame: 1 report-id byte + [`ANALOG_KEY_COUNT`]
/// pressure bytes.
pub const ANALOG_FRAME_LEN: usize = 1 + ANALOG_KEY_COUNT;

/// A decoded analog input frame: one pressure byte per key (`0` = released,
/// `0xFF` = fully actuated). Index is the raw byte position; the byte → physical
/// key mapping is established separately and applied by a higher layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalogFrame {
    /// Per-key pressure, in raw byte-index order.
    pub pressures: [u8; ANALOG_KEY_COUNT],
}

impl AnalogFrame {
    /// Decode a raw HID input frame into an [`AnalogFrame`].
    ///
    /// Returns `None` for any frame that is not a full analog report: a report id
    /// other than [`ANALOG_REPORT_ID`] (e.g. standard NKRO 0x01, or the unused
    /// 0x04/0x05), or a frame shorter than [`ANALOG_FRAME_LEN`]. A longer buffer
    /// is accepted (only the first [`ANALOG_FRAME_LEN`] bytes are read), since a
    /// read into an oversized buffer reports the byte count separately.
    pub fn decode(frame: &[u8]) -> Option<Self> {
        if frame.len() < ANALOG_FRAME_LEN || frame[0] != ANALOG_REPORT_ID {
            return None;
        }
        let mut pressures = [0u8; ANALOG_KEY_COUNT];
        pressures.copy_from_slice(&frame[1..ANALOG_FRAME_LEN]);
        Some(Self { pressures })
    }

    /// Whether any key registers non-zero pressure (a frame worth acting on).
    pub fn is_active(&self) -> bool {
        self.pressures.iter().any(|&p| p != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_valid_0x06_frame() {
        // report id 0x06 + 23 ascending pressures.
        let mut raw = [0u8; ANALOG_FRAME_LEN];
        raw[0] = ANALOG_REPORT_ID;
        for (i, b) in raw[1..].iter_mut().enumerate() {
            *b = (i * 10) as u8;
        }
        let frame = AnalogFrame::decode(&raw).expect("0x06 frame decodes");
        assert_eq!(frame.pressures.len(), 23);
        assert_eq!(frame.pressures[0], 0);
        assert_eq!(frame.pressures[5], 50);
        assert_eq!(frame.pressures[22], 220);
        assert!(frame.is_active());
    }

    #[test]
    fn all_zero_frame_is_inactive() {
        let mut raw = [0u8; ANALOG_FRAME_LEN];
        raw[0] = ANALOG_REPORT_ID;
        let frame = AnalogFrame::decode(&raw).unwrap();
        assert!(!frame.is_active());
    }

    #[test]
    fn wrong_report_id_rejected() {
        // Standard NKRO (0x01) and the declared-but-unused 0x04/0x05 must not decode.
        for id in [0x01u8, 0x04, 0x05] {
            let mut raw = [0u8; ANALOG_FRAME_LEN];
            raw[0] = id;
            assert_eq!(AnalogFrame::decode(&raw), None, "id 0x{id:02x} must not decode");
        }
    }

    #[test]
    fn short_frame_rejected() {
        // A truncated frame (id present, payload cut off) must not decode with a
        // zero-filled tail.
        let short = [ANALOG_REPORT_ID, 0xFF, 0xC3];
        assert_eq!(AnalogFrame::decode(&short), None);
        // An empty buffer is also rejected (no panic).
        assert_eq!(AnalogFrame::decode(&[]), None);
    }

    #[test]
    fn oversized_frame_reads_first_24_bytes() {
        // A read into a 64-byte buffer that reports 24 bytes still decodes the
        // leading frame; trailing slop is ignored.
        let mut raw = [0u8; 64];
        raw[0] = ANALOG_REPORT_ID;
        raw[1] = 0xAB;
        raw[ANALOG_FRAME_LEN - 1] = 0xCD; // last pressure byte (index 22)
        raw[ANALOG_FRAME_LEN] = 0x99; // slop just past the frame — must be ignored
        let frame = AnalogFrame::decode(&raw).unwrap();
        assert_eq!(frame.pressures[0], 0xAB);
        assert_eq!(frame.pressures[22], 0xCD);
    }
}

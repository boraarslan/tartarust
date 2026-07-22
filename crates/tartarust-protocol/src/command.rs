//! Typed command construction: turn a `(transaction_id, class, id, data_size,
//! args)` tuple into a fully-formed, crc-stamped [`RazerReport`].
//!
//! The Tartarus Pro's per-command `transaction_id` is **not** derivable from the
//! wire `(class, id)` pair — breathing and the other extended-matrix effects all
//! share class `0x0F` / id `0x02` yet need different ids (0x3F vs 0x1F). So the
//! id is carried explicitly here, and the higher-level builders in `device` and
//! `lighting` pick the right [`TransactionId`] per semantic command.

use crate::report::RazerReport;

/// Per-device / per-command transaction id for product `0x0244`.
///
/// Source: `docs/research/02-tartarus-pro-device.md` and
/// `razerkbd_driver.c` (each effect handler stamps `request.transaction_id.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransactionId {
    /// 0x1F — lighting effects, brightness, custom frames (the Pro default).
    Lighting = 0x1F,
    /// 0x3F — breathing effect variants only.
    Breathing = 0x3F,
    /// 0xFF — device-agnostic standard commands: get_firmware, get_serial,
    /// get_device_mode.
    Standard = 0xFF,
}

impl From<TransactionId> for u8 {
    fn from(t: TransactionId) -> u8 {
        t as u8
    }
}

/// A device command before it is framed into a [`RazerReport`]. Holds the header
/// triplet and the argument payload (only the leading `data_size` bytes are
/// meaningful, but the whole 80-byte area is carried for fidelity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub transaction_id: TransactionId,
    pub command_class: u8,
    pub command_id: u8,
    pub data_size: u8,
    pub args: [u8; 80],
}

impl Command {
    /// Build a command from the header triplet and a slice of leading argument
    /// bytes (zero-padded to 80). `args` must fit in the 80-byte payload.
    ///
    /// Mirrors OpenRazer's `get_razer_report(class, id, data_size)` followed by
    /// filling `arguments[..]`.
    pub fn new(
        transaction_id: TransactionId,
        command_class: u8,
        command_id: u8,
        data_size: u8,
        args: &[u8],
    ) -> Self {
        debug_assert!(args.len() <= 80, "command args exceed 80-byte payload");
        let mut buf = [0u8; 80];
        let n = args.len().min(80);
        buf[..n].copy_from_slice(&args[..n]);
        Self {
            transaction_id,
            command_class,
            command_id,
            data_size,
            args: buf,
        }
    }

    /// Frame this command into a wire-ready [`RazerReport`] with the correct
    /// transaction id and a freshly computed crc. `status`/`protocol_type`/
    /// `remaining_packets`/`reserved` are left at their request defaults (0x00).
    pub fn to_report(&self) -> RazerReport {
        let mut report = RazerReport::new();
        report.transaction_id = self.transaction_id.into();
        report.data_size = self.data_size;
        report.command_class = self.command_class;
        report.command_id = self.command_id;
        report.arguments = self.args;
        report.update_crc();
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_report_sets_header_and_crc() {
        let cmd = Command::new(
            TransactionId::Lighting,
            0x0F,
            0x04,
            0x03,
            &[0x01, 0x00, 0x80],
        );
        let r = cmd.to_report();
        assert_eq!(r.transaction_id, 0x1F);
        assert_eq!(r.command_class, 0x0F);
        assert_eq!(r.command_id, 0x04);
        assert_eq!(r.data_size, 0x03);
        assert_eq!(&r.arguments[..3], &[0x01, 0x00, 0x80]);
        assert!(r.verify_crc());
    }

    #[test]
    fn transaction_id_values() {
        assert_eq!(u8::from(TransactionId::Lighting), 0x1F);
        assert_eq!(u8::from(TransactionId::Breathing), 0x3F);
        assert_eq!(u8::from(TransactionId::Standard), 0xFF);
    }
}

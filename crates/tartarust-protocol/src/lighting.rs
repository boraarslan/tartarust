//! Chroma lighting: extended-matrix effect setters and per-key custom frames.
//!
//! Every effect goes through the extended-matrix effect command (class `0x0F`,
//! id `0x02`); `set_custom_frame` uses class `0x0F`, id `0x03`. Arg layouts are
//! transcribed byte-for-byte from OpenRazer `razerchromacommon.c`
//! (`razer_chroma_extended_matrix_*`), cross-checked against
//! `docs/research/02-tartarus-pro-device.md`.
//!
//! The shared base (`razer_chroma_extended_matrix_effect_base`,
//! `razerchromacommon.c:481`) always writes:
//!   args[0] = variable_storage, args[1] = led_id, args[2] = effect_id
//! and sets `data_size` to the effect's `arg_size`. Effects then add their own
//! bytes on top.
//!
//! Transaction id: all effects use `Lighting` (0x1F) **except** the three
//! breathing variants, which use `Breathing` (0x3F)
//! (`docs/research/02-tartarus-pro-device.md`, exception 2).

use crate::{
    command::{Command, TransactionId},
    error::ProtocolError,
    report::RazerReport,
};

// ---- shared constants (razercommon.h:33-57) -------------------------------

/// NOSTORE — do not persist to onboard memory.
pub const NOSTORE: u8 = 0x00;
/// VARSTORE — persist to the active onboard storage slot.
pub const VARSTORE: u8 = 0x01;
/// ZERO_LED — LED id used by brightness set/get.
pub const ZERO_LED: u8 = 0x00;
/// BACKLIGHT_LED — LED id all extended-matrix effects target.
pub const BACKLIGHT_LED: u8 = 0x05;

// ---- effect ids (the `effect_id` written at args[2]) ----------------------

const EFFECT_NONE: u8 = 0x00;
const EFFECT_STATIC: u8 = 0x01;
const EFFECT_BREATHING: u8 = 0x02;
const EFFECT_SPECTRUM: u8 = 0x03;
const EFFECT_WAVE: u8 = 0x04;
const EFFECT_REACTIVE: u8 = 0x05;
const EFFECT_STARLIGHT: u8 = 0x07;
const EFFECT_CUSTOM_FRAME: u8 = 0x08;

// Extended-matrix command coordinates.
const EXT_MATRIX_CLASS: u8 = 0x0F;
const EXT_MATRIX_EFFECT_ID: u8 = 0x02; // command_id for effect setters
const EXT_MATRIX_FRAME_ID: u8 = 0x03; // command_id for set_custom_frame

/// Matrix geometry for the Tartarus Pro: 1 row × 21 columns.
pub const MATRIX_ROWS: usize = 1;
pub const MATRIX_COLS: usize = 21;

/// An 8-bit-per-channel RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Wave scroll direction. OpenRazer clamps to 0..=2; the device speed byte is
/// fixed at 0x28 (`razer_chroma_extended_matrix_effect_wave`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WaveDirection {
    /// 0x00 (some devices treat 0x00/0x01 as the two directions).
    Right = 0x00,
    Left = 0x01,
}

/// Build the shared effect base report: args[0]=storage, args[1]=led,
/// args[2]=effect; `data_size = arg_size`. Effect-specific bytes are layered on
/// by the caller after this returns.
fn effect_base(tid: TransactionId, arg_size: u8, effect_id: u8) -> RazerReport {
    Command::new(
        tid,
        EXT_MATRIX_CLASS,
        EXT_MATRIX_EFFECT_ID,
        arg_size,
        &[VARSTORE, BACKLIGHT_LED, effect_id],
    )
    .to_report()
}

/// Re-stamp a report's crc after the caller mutated `arguments`.
fn restamp(mut report: RazerReport) -> RazerReport {
    report.update_crc();
    report
}

// ---- effect setters -------------------------------------------------------

/// "None" — turn the backlight off. `base(0x06, ..., 0x00)`.
pub fn effect_none() -> RazerReport {
    effect_base(TransactionId::Lighting, 0x06, EFFECT_NONE)
}

/// "Spectrum" — cycle the full colour spectrum. `base(0x06, ..., 0x03)`.
pub fn effect_spectrum() -> RazerReport {
    effect_base(TransactionId::Lighting, 0x06, EFFECT_SPECTRUM)
}

/// "Static" — a single fixed colour. `base(0x09, ..., 0x01)` then
/// args[5]=0x01, args[6..9]=R,G,B.
pub fn effect_static(c: Rgb) -> RazerReport {
    let mut r = effect_base(TransactionId::Lighting, 0x09, EFFECT_STATIC);
    r.arguments[5] = 0x01;
    r.arguments[6] = c.r;
    r.arguments[7] = c.g;
    r.arguments[8] = c.b;
    restamp(r)
}

/// "Wave" — animated colour wave. `base(0x06, ..., 0x04)` then
/// args[3]=direction (clamped 0..=2), args[4]=0x28 (fixed speed).
pub fn effect_wave(direction: WaveDirection) -> RazerReport {
    let mut r = effect_base(TransactionId::Lighting, 0x06, EFFECT_WAVE);
    r.arguments[3] = direction as u8;
    r.arguments[4] = 0x28; // fixed speed; lower is faster (OpenRazer default)
    restamp(r)
}

/// "Reactive" — light up on keypress in a colour. `base(0x09, ..., 0x05)` then
/// args[4]=speed (clamped 1..=4), args[5]=0x01, args[6..9]=R,G,B.
pub fn effect_reactive(speed: u8, c: Rgb) -> RazerReport {
    let mut r = effect_base(TransactionId::Lighting, 0x09, EFFECT_REACTIVE);
    r.arguments[4] = speed.clamp(0x01, 0x04);
    r.arguments[5] = 0x01;
    r.arguments[6] = c.r;
    r.arguments[7] = c.g;
    r.arguments[8] = c.b;
    restamp(r)
}

/// "Breathing", single colour. `base(0x09, ..., 0x02)` then args[3]=0x01,
/// args[5]=0x01, args[6..9]=R,G,B. **Uses transaction id 0x3F.**
pub fn effect_breathing_single(c: Rgb) -> RazerReport {
    let mut r = effect_base(TransactionId::Breathing, 0x09, EFFECT_BREATHING);
    r.arguments[3] = 0x01;
    r.arguments[5] = 0x01;
    r.arguments[6] = c.r;
    r.arguments[7] = c.g;
    r.arguments[8] = c.b;
    restamp(r)
}

/// "Breathing", two colours. `base(0x0C, ..., 0x02)` then args[3]=0x02,
/// args[5]=0x02, args[6..9]=RGB1, args[9..12]=RGB2. **Uses transaction id 0x3F.**
pub fn effect_breathing_dual(c1: Rgb, c2: Rgb) -> RazerReport {
    let mut r = effect_base(TransactionId::Breathing, 0x0C, EFFECT_BREATHING);
    r.arguments[3] = 0x02;
    r.arguments[5] = 0x02;
    r.arguments[6] = c1.r;
    r.arguments[7] = c1.g;
    r.arguments[8] = c1.b;
    r.arguments[9] = c2.r;
    r.arguments[10] = c2.g;
    r.arguments[11] = c2.b;
    restamp(r)
}

/// "Breathing", random colours. `base(0x06, ..., 0x02)` with no extra bytes.
/// **Uses transaction id 0x3F.**
pub fn effect_breathing_random() -> RazerReport {
    effect_base(TransactionId::Breathing, 0x06, EFFECT_BREATHING)
}

/// "Starlight", random colours. `base(0x06, ..., 0x07)` then args[4]=speed
/// (clamped 1..=3).
pub fn effect_starlight_random(speed: u8) -> RazerReport {
    let mut r = effect_base(TransactionId::Lighting, 0x06, EFFECT_STARLIGHT);
    r.arguments[4] = speed.clamp(0x01, 0x03);
    restamp(r)
}

/// "Starlight", single colour. `base(0x09, ..., 0x07)` then args[4]=speed
/// (1..=3), args[5]=0x01, args[6..9]=R,G,B.
pub fn effect_starlight_single(speed: u8, c: Rgb) -> RazerReport {
    let mut r = effect_base(TransactionId::Lighting, 0x09, EFFECT_STARLIGHT);
    r.arguments[4] = speed.clamp(0x01, 0x03);
    r.arguments[5] = 0x01;
    r.arguments[6] = c.r;
    r.arguments[7] = c.g;
    r.arguments[8] = c.b;
    restamp(r)
}

/// "Starlight", two colours. `base(0x0C, ..., 0x07)` then args[4]=speed (1..=3),
/// args[5]=0x02, args[6..9]=RGB1, args[9..12]=RGB2.
pub fn effect_starlight_dual(speed: u8, c1: Rgb, c2: Rgb) -> RazerReport {
    let mut r = effect_base(TransactionId::Lighting, 0x0C, EFFECT_STARLIGHT);
    r.arguments[4] = speed.clamp(0x01, 0x03);
    r.arguments[5] = 0x02;
    r.arguments[6] = c1.r;
    r.arguments[7] = c1.g;
    r.arguments[8] = c1.b;
    r.arguments[9] = c2.r;
    r.arguments[10] = c2.g;
    r.arguments[11] = c2.b;
    restamp(r)
}

/// Switch the device into "custom frame" display mode so an uploaded frame
/// (see [`set_custom_frame`]) is shown. `base(0x0C, 0x00, 0x00, 0x08)` — note
/// storage and led id are both 0x00 here, unlike the other effects.
pub fn effect_custom_frame() -> RazerReport {
    Command::new(
        TransactionId::Lighting,
        EXT_MATRIX_CLASS,
        EXT_MATRIX_EFFECT_ID,
        0x0C,
        &[0x00, 0x00, EFFECT_CUSTOM_FRAME],
    )
    .to_report()
}

/// `data_size` forced for the Pro's custom-frame path (`packetLength` 0x47 in
/// `razer_chroma_extended_matrix_set_custom_frame2`).
const CUSTOM_FRAME_PACKET_LEN: u8 = 0x47;

/// Upload one row of per-key RGB. `class 0x0F / id 0x03`, `data_size 0x47`.
///
/// Args (`razer_chroma_extended_matrix_set_custom_frame2`):
///   args[2]=row_index, args[3]=start_col, args[4]=stop_col,
///   args[5..]=RGB triples for columns `start_col..=stop_col` (inclusive).
///
/// `colors` must have exactly `stop_col + 1 - start_col` entries, and the span
/// must fit the matrix and the 80-byte argument area. After uploading the
/// frame(s), call [`effect_custom_frame`] to display it.
pub fn set_custom_frame(
    row_index: u8,
    start_col: u8,
    stop_col: u8,
    colors: &[Rgb],
) -> Result<RazerReport, ProtocolError> {
    if stop_col < start_col {
        return Err(ProtocolError::InvalidArgument(
            "custom frame stop_col < start_col",
        ));
    }
    if (stop_col as usize) >= MATRIX_COLS {
        return Err(ProtocolError::InvalidArgument(
            "custom frame stop_col beyond matrix width",
        ));
    }
    let span = (stop_col - start_col) as usize + 1;
    if colors.len() != span {
        return Err(ProtocolError::InvalidArgument(
            "custom frame colour count != column span",
        ));
    }
    // RGB data starts at args[5]; must fit the 80-byte payload.
    if 5 + span * 3 > 80 {
        return Err(ProtocolError::InvalidArgument(
            "custom frame row too long for argument payload",
        ));
    }

    let mut args = [0u8; 80];
    args[2] = row_index;
    args[3] = start_col;
    args[4] = stop_col;
    for (i, c) in colors.iter().enumerate() {
        let base = 5 + i * 3;
        args[base] = c.r;
        args[base + 1] = c.g;
        args[base + 2] = c.b;
    }

    Ok(Command::new(
        TransactionId::Lighting,
        EXT_MATRIX_CLASS,
        EXT_MATRIX_FRAME_ID,
        CUSTOM_FRAME_PACKET_LEN,
        &args,
    )
    .to_report())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: serialized header + first N args.
    fn hdr(r: &RazerReport) -> (u8, u8, u8, u8) {
        (r.transaction_id, r.data_size, r.command_class, r.command_id)
    }

    #[test]
    fn none_and_spectrum() {
        let none = effect_none();
        assert_eq!(hdr(&none), (0x1F, 0x06, 0x0F, 0x02));
        assert_eq!(&none.arguments[..3], &[VARSTORE, BACKLIGHT_LED, 0x00]);
        assert!(none.verify_crc());

        let spec = effect_spectrum();
        assert_eq!(&spec.arguments[..3], &[VARSTORE, BACKLIGHT_LED, 0x03]);
        assert_eq!(spec.data_size, 0x06);
    }

    #[test]
    fn static_layout() {
        let r = effect_static(Rgb::new(0xAA, 0xBB, 0xCC));
        assert_eq!(hdr(&r), (0x1F, 0x09, 0x0F, 0x02));
        // args: [VARSTORE, BACKLIGHT_LED, 0x01, 0, 0, 0x01, R, G, B]
        assert_eq!(
            &r.arguments[..9],
            &[0x01, 0x05, 0x01, 0x00, 0x00, 0x01, 0xAA, 0xBB, 0xCC]
        );
        assert!(r.verify_crc());
    }

    #[test]
    fn wave_layout() {
        let r = effect_wave(WaveDirection::Left);
        assert_eq!(r.data_size, 0x06);
        assert_eq!(&r.arguments[..5], &[0x01, 0x05, 0x04, 0x01, 0x28]);
    }

    #[test]
    fn reactive_clamps_speed() {
        let r = effect_reactive(0xFF, Rgb::new(1, 2, 3));
        assert_eq!(r.data_size, 0x09);
        // args[4]=speed clamped to 4, args[5]=1, args[6..9]=RGB
        assert_eq!(&r.arguments[..9], &[0x01, 0x05, 0x05, 0x00, 0x04, 0x01, 1, 2, 3]);
    }

    #[test]
    fn breathing_uses_3f_transaction() {
        let single = effect_breathing_single(Rgb::new(9, 8, 7));
        assert_eq!(single.transaction_id, 0x3F, "breathing single tid");
        assert_eq!(single.data_size, 0x09);
        assert_eq!(&single.arguments[..9], &[0x01, 0x05, 0x02, 0x01, 0x00, 0x01, 9, 8, 7]);

        let dual = effect_breathing_dual(Rgb::new(1, 2, 3), Rgb::new(4, 5, 6));
        assert_eq!(dual.transaction_id, 0x3F, "breathing dual tid");
        assert_eq!(dual.data_size, 0x0C);
        assert_eq!(
            &dual.arguments[..12],
            &[0x01, 0x05, 0x02, 0x02, 0x00, 0x02, 1, 2, 3, 4, 5, 6]
        );

        let rand = effect_breathing_random();
        assert_eq!(rand.transaction_id, 0x3F, "breathing random tid");
        assert_eq!(rand.data_size, 0x06);
        assert_eq!(&rand.arguments[..3], &[0x01, 0x05, 0x02]);
    }

    #[test]
    fn starlight_variants() {
        let rnd = effect_starlight_random(0xFF);
        assert_eq!(rnd.transaction_id, 0x1F);
        assert_eq!(rnd.data_size, 0x06);
        assert_eq!(&rnd.arguments[..5], &[0x01, 0x05, 0x07, 0x00, 0x03]);

        let single = effect_starlight_single(2, Rgb::new(10, 20, 30));
        assert_eq!(single.data_size, 0x09);
        assert_eq!(
            &single.arguments[..9],
            &[0x01, 0x05, 0x07, 0x00, 0x02, 0x01, 10, 20, 30]
        );

        let dual = effect_starlight_dual(1, Rgb::new(1, 2, 3), Rgb::new(4, 5, 6));
        assert_eq!(dual.data_size, 0x0C);
        assert_eq!(
            &dual.arguments[..12],
            &[0x01, 0x05, 0x07, 0x00, 0x01, 0x02, 1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn custom_frame_mode_switch() {
        let r = effect_custom_frame();
        assert_eq!(hdr(&r), (0x1F, 0x0C, 0x0F, 0x02));
        assert_eq!(&r.arguments[..3], &[0x00, 0x00, 0x08]);
    }

    #[test]
    fn set_custom_frame_layout() {
        let colors = [Rgb::new(0xFF, 0x00, 0x00), Rgb::new(0x00, 0xFF, 0x00)];
        let r = set_custom_frame(0, 3, 4, &colors).unwrap();
        assert_eq!(hdr(&r), (0x1F, 0x47, 0x0F, 0x03));
        // args[2]=row, args[3]=start, args[4]=stop, then RGB triples from args[5].
        assert_eq!(
            &r.arguments[..11],
            &[0x00, 0x00, 0x00, 0x03, 0x04, 0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00]
        );
        assert!(r.verify_crc());
    }

    #[test]
    fn set_custom_frame_full_row() {
        // 21 columns -> 0..=20, 21 colours, 5 + 63 = 68 bytes ≤ 80.
        let colors: Vec<Rgb> = (0..MATRIX_COLS as u8).map(|i| Rgb::new(i, i, i)).collect();
        let r = set_custom_frame(0, 0, (MATRIX_COLS - 1) as u8, &colors).unwrap();
        assert_eq!(r.arguments[4], 20);
        assert_eq!(&r.arguments[5..8], &[0, 0, 0]);
        assert_eq!(&r.arguments[5 + 60..5 + 63], &[20, 20, 20]);
    }

    #[test]
    fn set_custom_frame_rejects_bad_spans() {
        assert!(set_custom_frame(0, 5, 4, &[]).is_err(), "stop < start");
        assert!(
            set_custom_frame(0, 0, 21, &[]).is_err(),
            "stop beyond matrix width"
        );
        assert!(
            set_custom_frame(0, 0, 1, &[Rgb::new(0, 0, 0)]).is_err(),
            "colour count mismatch"
        );
    }
}

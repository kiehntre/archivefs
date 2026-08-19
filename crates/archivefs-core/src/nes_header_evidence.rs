//! Pure, read-only NES/Famicom `iNES`/`NES 2.0` header field decoding -
//! deeper than [`crate::header_normalization`]'s magic-only recognition
//! (which only answers "does this carry the iNES header, and can it be
//! reversibly stripped"). This module decodes the header's actual fields:
//! mapper number, NES 2.0 vs iNES 1.0, PRG/CHR ROM size, and the trainer/
//! battery/mirroring/four-screen/console-type flags.
//!
//! # Format verified, not assumed
//!
//! Verified against the NESdev wiki's iNES page
//! (`https://www.nesdev.org/wiki/INES`), the community-maintained primary
//! reference for this format (the same page every mainstream NES emulator's
//! `.nes` loader is written against):
//!
//! ```text
//! [0..4]  magic         "NES\x1A"
//! [4]     prg_rom_units PRG ROM size, 16 KiB units
//! [5]     chr_rom_units CHR ROM size, 8 KiB units (0 = CHR RAM)
//! [6]     flags6        mapper low nibble (bits 4-7), four-screen (bit 3),
//!                        trainer (bit 2), battery (bit 1), mirroring (bit 0)
//! [7]     flags7        mapper high nibble (bits 4-7), NES 2.0 identifier
//!                        (bits 2-3 == %10), PlayChoice-10 (bit 1),
//!                        VS Unisystem (bit 0)
//! [8]     flags8        iNES 1.0: PRG-RAM size (8 KiB units, informal).
//!                        NES 2.0: mapper bits 8-11 (bits 0-3), submapper
//!                        (bits 4-7)
//! [9]     flags9        NES 2.0 only: PRG ROM size MSB nibble (bits 0-3),
//!                        CHR ROM size MSB nibble (bits 4-7)
//! ```
//!
//! NES 2.0 detection: `flags7 & 0x0C == 0x08` (bits 3-2 equal `%10`) -
//! exactly the NESdev wiki's own stated rule.
//!
//! # What this module does not decode
//!
//! NES 2.0's exponent-multiplier PRG/CHR size encoding (used only when the
//! MSB nibble read from byte 9 is itself `0xF`, an intentionally rare case
//! for unusually large or oddly-sized ROMs) is not decoded here - this
//! module reports the plain 12-bit combined size in that case rather than
//! computing the exponent form, which is real, extra decode complexity for
//! a genuinely uncommon input this milestone does not need to chase. PRG-
//! RAM/EEPROM/CHR-RAM size fields (NES 2.0 bytes 10-11) and the
//! timing/vs-system/misc-ROM/expansion-device bytes (12-15) are likewise
//! out of scope for this pass - this module covers the fields real DAT/
//! catalogue identity work actually needs (mapper, ROM size, flags), not
//! every NES 2.0 byte.
//!
//! # Physical header facts vs. normalized payload - kept separate
//!
//! Like every other header in this crate, an iNES header can be a generator-
//! attached convenience, not preservation truth - the same reason
//! [`crate::header_normalization`] treats it as a *reversible* strip, never
//! an assumed-correct source of fact about the payload. This module reports
//! only what the header bytes themselves say; it never reads or infers
//! anything about the payload PRG/CHR data that follows.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const INES_HEADER_BYTES: usize = 16;
const INES_MAGIC: &[u8; 4] = b"NES\x1a";

const PRG_ROM_UNITS_OFFSET: usize = 4;
const CHR_ROM_UNITS_OFFSET: usize = 5;
const FLAGS6_OFFSET: usize = 6;
const FLAGS7_OFFSET: usize = 7;
const FLAGS8_OFFSET: usize = 8;
const FLAGS9_OFFSET: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NesMirroring {
    Vertical,
    Horizontal,
    /// Four-screen VRAM layout overrides the mirroring bit entirely - the
    /// NESdev wiki's own documented precedence.
    FourScreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NesConsoleType {
    Nes,
    VsUnisystem,
    PlayChoice10,
}

/// What a parsed iNES/NES 2.0 header directly states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InesHeaderFact {
    pub is_nes20: bool,
    /// PRG ROM size in 16 KiB units - the plain 8-bit iNES 1.0 value, or the
    /// NES 2.0 12-bit combined value (see the module documentation for the
    /// one case - the exponent-multiplier form - this does not decode).
    pub prg_rom_16k_units: u16,
    /// CHR ROM size in 8 KiB units (0 = CHR RAM).
    pub chr_rom_8k_units: u16,
    /// Combined mapper number - up to 8 bits (iNES 1.0) or up to 12 bits
    /// (NES 2.0).
    pub mapper: u16,
    /// NES 2.0 only.
    pub submapper: Option<u8>,
    pub mirroring: NesMirroring,
    pub battery: bool,
    pub trainer: bool,
    pub console_type: NesConsoleType,
}

/// Parses `bytes` (must be at least [`INES_HEADER_BYTES`] long, beginning
/// with the iNES magic) into an [`InesHeaderFact`]. Fails closed (`None`) on
/// a short buffer or wrong magic - never a partial struct.
pub fn parse_ines_header(bytes: &[u8]) -> Option<InesHeaderFact> {
    if bytes.len() < INES_HEADER_BYTES || &bytes[0..4] != INES_MAGIC.as_slice() {
        return None;
    }
    let flags6 = bytes[FLAGS6_OFFSET];
    let flags7 = bytes[FLAGS7_OFFSET];
    let flags8 = bytes[FLAGS8_OFFSET];
    let flags9 = bytes[FLAGS9_OFFSET];

    let is_nes20 = flags7 & 0x0C == 0x08;

    let mapper_low = (flags6 & 0xF0) >> 4;
    let mapper_mid = flags7 & 0xF0;
    let (mapper, submapper, prg_rom_16k_units, chr_rom_8k_units) = if is_nes20 {
        let mapper_high = flags8 & 0x0F;
        let mapper = (u16::from(mapper_high) << 8) | u16::from(mapper_mid) | u16::from(mapper_low);
        let submapper = (flags8 & 0xF0) >> 4;
        let prg_msb = flags9 & 0x0F;
        let chr_msb = (flags9 & 0xF0) >> 4;
        let prg = (u16::from(prg_msb) << 8) | u16::from(bytes[PRG_ROM_UNITS_OFFSET]);
        let chr = (u16::from(chr_msb) << 8) | u16::from(bytes[CHR_ROM_UNITS_OFFSET]);
        (mapper, Some(submapper), prg, chr)
    } else {
        let mapper = u16::from(mapper_mid) | u16::from(mapper_low);
        (
            mapper,
            None,
            u16::from(bytes[PRG_ROM_UNITS_OFFSET]),
            u16::from(bytes[CHR_ROM_UNITS_OFFSET]),
        )
    };

    let mirroring = if flags6 & 0x08 != 0 {
        NesMirroring::FourScreen
    } else if flags6 & 0x01 != 0 {
        NesMirroring::Horizontal
    } else {
        NesMirroring::Vertical
    };

    let console_type = if flags7 & 0x02 != 0 {
        NesConsoleType::PlayChoice10
    } else if flags7 & 0x01 != 0 {
        NesConsoleType::VsUnisystem
    } else {
        NesConsoleType::Nes
    };

    Some(InesHeaderFact {
        is_nes20,
        prg_rom_16k_units,
        chr_rom_8k_units,
        mapper,
        submapper,
        mirroring,
        battery: flags6 & 0x02 != 0,
        trainer: flags6 & 0x04 != 0,
        console_type,
    })
}

/// Neutral evidence for a parsed iNES/NES 2.0 header: `Strong`
/// `ContentSignature` for the format identifier (`"NES 2.0"` or `"iNES"`),
/// carrying the decoded mapper number in the detail string. Mapper/PRG-CHR
/// size/flags are real structural facts, not merely a magic byte, so this
/// stays at `Strong` - consistent with
/// [`crate::header_normalization::HeaderNormalizationKind::INes16`]'s own
/// `Strong` rating for the same magic.
pub fn observe_ines_evidence(fact: &InesHeaderFact) -> Vec<ContentEvidence> {
    let format = if fact.is_nes20 { "NES 2.0" } else { "iNES" };
    vec![ContentEvidence::new(
        ContentEvidenceKind::ContentSignature,
        format,
        ContentEvidenceConfidence::Strong,
        format!(
            "{format} header parsed: mapper {}{}, PRG {} x16KiB, CHR {} x8KiB",
            fact.mapper,
            fact.submapper
                .map(|sub| format!(".{sub}"))
                .unwrap_or_default(),
            fact.prg_rom_16k_units,
            fact.chr_rom_8k_units
        ),
    )]
}

/// A [`ContentDetector`] wrapping [`parse_ines_header`]/[`observe_ines_evidence`]
/// for use by multi-detector callers such as
/// [`crate::archive_member_content_evidence`].
pub struct NesHeaderDetector;

impl ContentDetector for NesHeaderDetector {
    fn id(&self) -> &'static str {
        "nes_ines_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match parse_ines_header(data) {
            Some(fact) => ContentDetectionOutcome::Recognized {
                evidence: observe_ines_evidence(&fact),
            },
            None => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_header() -> [u8; INES_HEADER_BYTES] {
        let mut header = [0u8; INES_HEADER_BYTES];
        header[0..4].copy_from_slice(INES_MAGIC);
        header
    }

    #[test]
    fn wrong_magic_fails_closed() {
        assert_eq!(parse_ines_header(&[0u8; INES_HEADER_BYTES]), None);
    }

    #[test]
    fn truncated_header_fails_closed() {
        assert_eq!(parse_ines_header(&base_header()[..8]), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_ines_header(&[]), None);
    }

    #[test]
    fn plain_ines1_header_is_not_nes20() {
        let header = base_header();
        let fact = parse_ines_header(&header).unwrap();
        assert!(!fact.is_nes20);
        assert_eq!(fact.submapper, None);
    }

    #[test]
    fn nes20_identifier_bits_are_detected() {
        let mut header = base_header();
        header[FLAGS7_OFFSET] = 0x08; // bits 3-2 = %10
        let fact = parse_ines_header(&header).unwrap();
        assert!(fact.is_nes20);
    }

    #[test]
    fn nes20_identifier_requires_exact_bit_pattern() {
        // 0x0C has bits 3-2 = %11, not %10 - not NES 2.0.
        let mut header = base_header();
        header[FLAGS7_OFFSET] = 0x0C;
        let fact = parse_ines_header(&header).unwrap();
        assert!(!fact.is_nes20);
    }

    #[test]
    fn prg_chr_sizes_are_read_from_ines1_bytes() {
        let mut header = base_header();
        header[PRG_ROM_UNITS_OFFSET] = 4;
        header[CHR_ROM_UNITS_OFFSET] = 2;
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.prg_rom_16k_units, 4);
        assert_eq!(fact.chr_rom_8k_units, 2);
    }

    #[test]
    fn chr_zero_means_chr_ram() {
        let header = base_header();
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.chr_rom_8k_units, 0);
    }

    #[test]
    fn mapper_number_combines_both_nibbles() {
        let mut header = base_header();
        header[FLAGS6_OFFSET] = 0x40; // mapper low nibble = 4
        header[FLAGS7_OFFSET] = 0x10; // mapper high nibble = 1 -> mapper 0x14 = 20
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.mapper, 20);
    }

    #[test]
    fn nes20_extends_mapper_and_reports_submapper() {
        let mut header = base_header();
        header[FLAGS6_OFFSET] = 0x00;
        header[FLAGS7_OFFSET] = 0x08; // NES2.0 identifier, mapper high nibble 0
        header[FLAGS8_OFFSET] = 0x53; // mapper bits8-11=3, submapper=5
        let fact = parse_ines_header(&header).unwrap();
        assert!(fact.is_nes20);
        assert_eq!(fact.mapper, 0x300);
        assert_eq!(fact.submapper, Some(5));
    }

    #[test]
    fn nes20_extends_prg_chr_size_msb_nibbles() {
        let mut header = base_header();
        header[FLAGS7_OFFSET] = 0x08;
        header[PRG_ROM_UNITS_OFFSET] = 0x01;
        header[CHR_ROM_UNITS_OFFSET] = 0x02;
        header[FLAGS9_OFFSET] = 0x21; // PRG MSB nibble=1, CHR MSB nibble=2
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.prg_rom_16k_units, 0x101);
        assert_eq!(fact.chr_rom_8k_units, 0x202);
    }

    #[test]
    fn mirroring_bit_zero_is_vertical() {
        let header = base_header();
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.mirroring, NesMirroring::Vertical);
    }

    #[test]
    fn mirroring_bit_one_is_horizontal() {
        let mut header = base_header();
        header[FLAGS6_OFFSET] = 0x01;
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.mirroring, NesMirroring::Horizontal);
    }

    #[test]
    fn four_screen_bit_overrides_mirroring_bit() {
        let mut header = base_header();
        header[FLAGS6_OFFSET] = 0x08 | 0x01; // four-screen + horizontal bit set
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.mirroring, NesMirroring::FourScreen);
    }

    #[test]
    fn battery_and_trainer_flags_are_read() {
        let mut header = base_header();
        header[FLAGS6_OFFSET] = 0x02 | 0x04;
        let fact = parse_ines_header(&header).unwrap();
        assert!(fact.battery);
        assert!(fact.trainer);
    }

    #[test]
    fn console_type_defaults_to_nes() {
        let header = base_header();
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.console_type, NesConsoleType::Nes);
    }

    #[test]
    fn console_type_vs_unisystem_bit() {
        let mut header = base_header();
        header[FLAGS7_OFFSET] = 0x01;
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.console_type, NesConsoleType::VsUnisystem);
    }

    #[test]
    fn console_type_playchoice10_bit() {
        let mut header = base_header();
        header[FLAGS7_OFFSET] = 0x02;
        let fact = parse_ines_header(&header).unwrap();
        assert_eq!(fact.console_type, NesConsoleType::PlayChoice10);
    }

    #[test]
    fn evidence_reports_nes20_format_label() {
        let mut header = base_header();
        header[FLAGS7_OFFSET] = 0x08;
        let fact = parse_ines_header(&header).unwrap();
        let evidence = observe_ines_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].value, "NES 2.0");
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn evidence_reports_ines_format_label() {
        let header = base_header();
        let fact = parse_ines_header(&header).unwrap();
        let evidence = observe_ines_evidence(&fact);
        assert_eq!(evidence[0].value, "iNES");
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = base_header();
        let fact = parse_ines_header(&header).unwrap();
        for item in observe_ines_evidence(&fact) {
            assert_eq!(item.kind, ContentEvidenceKind::ContentSignature);
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = base_header();
        assert_eq!(parse_ines_header(&header), parse_ines_header(&header));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = base_header();
        let before = header;
        let _ = parse_ines_header(&header);
        assert_eq!(header, before);
    }

    #[test]
    fn matches_existing_header_normalization_recognition() {
        use crate::header_normalization::recognize_ines;
        let header = base_header();
        assert!(recognize_ines(&header));
        assert!(parse_ines_header(&header).is_some());
    }
}

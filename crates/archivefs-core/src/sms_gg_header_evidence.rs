//! Pure, read-only Sega Master System / Game Gear `"TMR SEGA"` header
//! evidence.
//!
//! # Format verified, not assumed
//!
//! Verified against SMS Power!'s ROM Header development page
//! (`https://www.smspower.org/Development/ROMHeader`), the primary
//! community reference this format is documented in, cross-checked against
//! an independent secondary summary confirming the identical byte layout:
//!
//! ```text
//! Relative to the header's own start offset (see [`HEADER_CANDIDATE_OFFSETS`]):
//! [0x0..0x8]  "TMR SEGA"           8-byte magic
//! [0x8..0xA]  reserved             2 bytes (commonly 0x0000/0x2020/0xFFFF)
//! [0xA..0xC]  checksum             2 bytes, little-endian (declared, not
//!                                  independently validated - see below)
//! [0xC..0xE]  product_code_bcd     2 bytes, packed BCD (raw, not decoded -
//!                                  see [`TmrSegaHeaderFact::product_code_bcd`])
//! [0xE]       version_byte         high nibble = version; low nibble =
//!                                  5th BCD digit of the product code
//! [0xF]       region_checksum_byte high nibble = region/system code;
//!                                  low nibble = checksum-range code
//! ```
//!
//! Header location: the BIOS checks, in order, absolute file offsets
//! `0x7FF0`, `0x3FF0`, then `0x1FF0` for the `"TMR SEGA"` magic - the same
//! order [`find_tmr_sega_header`] tries them in. Real cartridges
//! overwhelmingly use `0x7FF0` (the 32 KiB-ROM-sized location); the smaller
//! offsets exist for smaller ROMs whose highest bank never reaches `0x7FF0`.
//!
//! # What this module does not decode
//!
//! - **The checksum is not independently validated.** SMS/Game Gear
//!   checksum coverage depends on a size-dependent range selected by the
//!   low nibble of the region/checksum byte, with several distinct ranges
//!   documented for different ROM sizes up to 256 KiB - real, genuine
//!   complexity this pass does not chase (matching
//!   [`crate::megadrive_header_evidence`]'s "whole-ROM checksum work stays
//!   opt-in, and only where it can be verified correctly" discipline, taken
//!   one step further here: not even an opt-in function is added, since the
//!   range-selection rules themselves were not independently corroborated
//!   to this crate's own two-source standard). `checksum` is exposed as a
//!   plain declared fact.
//! - **The product code is not decoded to decimal.** Its BCD nibble
//!   ordering was not independently corroborated by a second source in this
//!   research pass, so [`TmrSegaHeaderFact::product_code_bcd`] exposes the
//!   raw packed bytes rather than presenting an unverified digit order as
//!   fact.
//!
//! # `TMR SEGA` distinguishes neither Master System nor Game Gear alone
//!
//! The magic and most fields are identical between the two systems; only
//! the region/system nibble (high nibble of the last byte) differs. This
//! module reports [`SmsGgSystem`] from that nibble but never collapses it
//! into a platform decision - see [`observe_tmr_sega_evidence`].

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const TMR_SEGA_MAGIC: &[u8; 8] = b"TMR SEGA";
pub const TMR_SEGA_HEADER_BYTES: usize = 16;

/// The absolute file offsets the real BIOS checks, in the same priority
/// order.
pub const HEADER_CANDIDATE_OFFSETS: [usize; 3] = [0x7FF0, 0x3FF0, 0x1FF0];

const RESERVED_OFFSET: usize = 0x8;
const CHECKSUM_OFFSET: usize = 0xA;
const PRODUCT_CODE_OFFSET: usize = 0xC;
const VERSION_BYTE_OFFSET: usize = 0xE;
const REGION_CHECKSUM_BYTE_OFFSET: usize = 0xF;

/// The system/region nibble read from the high nibble of the header's last
/// byte - the one field that actually distinguishes Master System from Game
/// Gear (never the magic, which both share).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsGgSystem {
    SmsJapan,
    SmsExport,
    GgJapan,
    GgExport,
    GgInternational,
    /// A nibble value this module does not recognise - reported honestly
    /// rather than guessed at.
    Unknown(u8),
}

impl SmsGgSystem {
    fn from_nibble(nibble: u8) -> Self {
        match nibble {
            3 => Self::SmsExport,
            4 => Self::SmsJapan,
            5 => Self::GgJapan,
            6 => Self::GgExport,
            7 => Self::GgInternational,
            other => Self::Unknown(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SmsJapan => "Master System (Japan)",
            Self::SmsExport => "Master System (Export)",
            Self::GgJapan => "Game Gear (Japan)",
            Self::GgExport => "Game Gear (Export)",
            Self::GgInternational => "Game Gear (International)",
            Self::Unknown(_) => "Unknown region/system nibble",
        }
    }
}

/// What a parsed `TMR SEGA` header directly states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmrSegaHeaderFact {
    /// The absolute file offset this header was found at - one of
    /// [`HEADER_CANDIDATE_OFFSETS`].
    pub header_offset: usize,
    /// Declared checksum, as read - not independently validated. See the
    /// module documentation.
    pub checksum: u16,
    /// Raw packed BCD product-code bytes, as read - not decoded to decimal.
    /// See the module documentation.
    pub product_code_bcd: [u8; 2],
    pub version: u8,
    pub system: SmsGgSystem,
}

/// Checks whether `bytes[offset..offset+8]` matches [`TMR_SEGA_MAGIC`].
pub fn looks_like_tmr_sega_at(bytes: &[u8], offset: usize) -> bool {
    bytes
        .get(offset..offset + TMR_SEGA_MAGIC.len())
        .is_some_and(|slice| slice == TMR_SEGA_MAGIC.as_slice())
}

/// Tries every [`HEADER_CANDIDATE_OFFSETS`] entry, in order, and returns the
/// first offset within `bytes` whose magic matches. `None` if none do.
pub fn find_tmr_sega_header(bytes: &[u8]) -> Option<usize> {
    HEADER_CANDIDATE_OFFSETS
        .into_iter()
        .find(|&offset| looks_like_tmr_sega_at(bytes, offset))
}

/// Parses the [`TMR_SEGA_HEADER_BYTES`]-byte header at `header_offset`
/// within `bytes`. `None` if the magic does not match there, or the region
/// does not fit - fails closed, never a partial struct. A caller should
/// obtain `header_offset` from [`find_tmr_sega_header`] rather than guessing
/// one.
pub fn parse_tmr_sega_header(bytes: &[u8], header_offset: usize) -> Option<TmrSegaHeaderFact> {
    if !looks_like_tmr_sega_at(bytes, header_offset) {
        return None;
    }
    let region = bytes.get(header_offset..header_offset + TMR_SEGA_HEADER_BYTES)?;
    let checksum = u16::from_le_bytes(
        region[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    let product_code_bcd = [region[PRODUCT_CODE_OFFSET], region[PRODUCT_CODE_OFFSET + 1]];
    let version_byte = region[VERSION_BYTE_OFFSET];
    let region_checksum_byte = region[REGION_CHECKSUM_BYTE_OFFSET];

    let _ = RESERVED_OFFSET; // documented, not surfaced as its own field

    Some(TmrSegaHeaderFact {
        header_offset,
        checksum,
        product_code_bcd,
        version: version_byte >> 4,
        system: SmsGgSystem::from_nibble(region_checksum_byte >> 4),
    })
}

/// Neutral evidence for a parsed [`TmrSegaHeaderFact`]: `Strong`
/// `BootStructure` for the `TMR SEGA` magic match itself (a specific,
/// documented 8-byte signature, matching this crate's precedent for other
/// magic-string boot signatures), plus the [`SmsGgSystem`] variant reported
/// as its own `Corroborated` fact - a real signal, but this module never
/// resolves it into "this is a Master System ROM" or "this is a Game Gear
/// ROM" itself.
pub fn observe_tmr_sega_evidence(fact: &TmrSegaHeaderFact) -> Vec<ContentEvidence> {
    vec![
        ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "TMR SEGA",
            ContentEvidenceConfidence::Strong,
            format!(
                "TMR SEGA header magic matched at file offset {:#06x}",
                fact.header_offset
            ),
        ),
        ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            fact.system.label(),
            ContentEvidenceConfidence::Corroborated,
            "system/region nibble read from the TMR SEGA header - Master System and Game Gear \
             share the identical header format, so this alone never decides between them",
        ),
    ]
}

/// A [`ContentDetector`] wrapping [`find_tmr_sega_header`]/
/// [`parse_tmr_sega_header`]/[`observe_tmr_sega_evidence`].
pub struct TmrSegaHeaderDetector;

impl ContentDetector for TmrSegaHeaderDetector {
    fn id(&self) -> &'static str {
        "sms_gg_tmr_sega_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match find_tmr_sega_header(data).and_then(|offset| parse_tmr_sega_header(data, offset)) {
            Some(fact) => ContentDetectionOutcome::Recognized {
                evidence: observe_tmr_sega_evidence(&fact),
            },
            None => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_rom(header_offset: usize, region_nibble: u8) -> Vec<u8> {
        let mut rom = vec![0u8; header_offset + TMR_SEGA_HEADER_BYTES];
        rom[header_offset..header_offset + TMR_SEGA_MAGIC.len()].copy_from_slice(TMR_SEGA_MAGIC);
        rom[header_offset + CHECKSUM_OFFSET..header_offset + CHECKSUM_OFFSET + 2]
            .copy_from_slice(&0x1234u16.to_le_bytes());
        rom[header_offset + PRODUCT_CODE_OFFSET] = 0x56;
        rom[header_offset + PRODUCT_CODE_OFFSET + 1] = 0x78;
        rom[header_offset + VERSION_BYTE_OFFSET] = 0x10; // version=1
        rom[header_offset + REGION_CHECKSUM_BYTE_OFFSET] = region_nibble << 4;
        rom
    }

    // ------------------------------------------------------------------
    // Header location
    // ------------------------------------------------------------------

    #[test]
    fn finds_header_at_0x7ff0() {
        let rom = synthetic_rom(0x7FF0, 6);
        assert_eq!(find_tmr_sega_header(&rom), Some(0x7FF0));
    }

    #[test]
    fn finds_header_at_0x3ff0_when_0x7ff0_absent() {
        let rom = synthetic_rom(0x3FF0, 6);
        assert_eq!(find_tmr_sega_header(&rom), Some(0x3FF0));
    }

    #[test]
    fn finds_header_at_0x1ff0_when_others_absent() {
        let rom = synthetic_rom(0x1FF0, 6);
        assert_eq!(find_tmr_sega_header(&rom), Some(0x1FF0));
    }

    #[test]
    fn prefers_0x7ff0_over_smaller_offsets_when_both_present() {
        let mut rom = synthetic_rom(0x7FF0, 6);
        // Also plant a header at 0x1FF0.
        let smaller = synthetic_rom(0x1FF0, 3);
        rom[0x1FF0..0x1FF0 + TMR_SEGA_HEADER_BYTES]
            .copy_from_slice(&smaller[0x1FF0..0x1FF0 + TMR_SEGA_HEADER_BYTES]);
        assert_eq!(find_tmr_sega_header(&rom), Some(0x7FF0));
    }

    #[test]
    fn no_magic_anywhere_is_not_found() {
        let rom = vec![0u8; 0x8000];
        assert_eq!(find_tmr_sega_header(&rom), None);
    }

    #[test]
    fn empty_rom_is_not_found_not_panic() {
        assert_eq!(find_tmr_sega_header(&[]), None);
    }

    #[test]
    fn out_of_bounds_offset_is_not_matched_not_panic() {
        let short_rom = vec![0u8; 100];
        assert!(!looks_like_tmr_sega_at(&short_rom, 0x7FF0));
    }

    // ------------------------------------------------------------------
    // Header parsing
    // ------------------------------------------------------------------

    #[test]
    fn parses_checksum_and_product_code() {
        let rom = synthetic_rom(0x7FF0, 6);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        assert_eq!(fact.checksum, 0x1234);
        assert_eq!(fact.product_code_bcd, [0x56, 0x78]);
        assert_eq!(fact.version, 1);
    }

    #[test]
    fn wrong_offset_fails_closed() {
        let rom = synthetic_rom(0x7FF0, 6);
        assert_eq!(parse_tmr_sega_header(&rom, 0x1FF0), None);
    }

    #[test]
    fn truncated_region_fails_closed() {
        let mut rom = synthetic_rom(0x7FF0, 6);
        rom.truncate(0x7FF0 + 8);
        assert_eq!(parse_tmr_sega_header(&rom, 0x7FF0), None);
    }

    // ------------------------------------------------------------------
    // System/region nibble
    // ------------------------------------------------------------------

    #[test]
    fn sms_export_nibble_is_recognized() {
        let rom = synthetic_rom(0x7FF0, 3);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        assert_eq!(fact.system, SmsGgSystem::SmsExport);
    }

    #[test]
    fn sms_japan_nibble_is_recognized() {
        let rom = synthetic_rom(0x7FF0, 4);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        assert_eq!(fact.system, SmsGgSystem::SmsJapan);
    }

    #[test]
    fn gg_japan_nibble_is_recognized() {
        let rom = synthetic_rom(0x7FF0, 5);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        assert_eq!(fact.system, SmsGgSystem::GgJapan);
    }

    #[test]
    fn gg_export_nibble_is_recognized() {
        let rom = synthetic_rom(0x7FF0, 6);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        assert_eq!(fact.system, SmsGgSystem::GgExport);
    }

    #[test]
    fn gg_international_nibble_is_recognized() {
        let rom = synthetic_rom(0x7FF0, 7);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        assert_eq!(fact.system, SmsGgSystem::GgInternational);
    }

    #[test]
    fn unrecognized_nibble_is_reported_honestly() {
        let rom = synthetic_rom(0x7FF0, 0xA);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        assert_eq!(fact.system, SmsGgSystem::Unknown(0xA));
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn evidence_includes_strong_magic_and_corroborated_system() {
        let rom = synthetic_rom(0x7FF0, 6);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        let evidence = observe_tmr_sega_evidence(&fact);
        assert_eq!(evidence.len(), 2);
        let magic = evidence
            .iter()
            .find(|item| item.value == "TMR SEGA")
            .unwrap();
        assert_eq!(magic.confidence, ContentEvidenceConfidence::Strong);
        let system = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ContentSignature)
            .unwrap();
        assert_eq!(system.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn evidence_never_claims_sms_or_gg_as_a_platform_decision() {
        let rom = synthetic_rom(0x7FF0, 6);
        let fact = parse_tmr_sega_header(&rom, 0x7FF0).unwrap();
        for item in observe_tmr_sega_evidence(&fact) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ContentSignature
            ));
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let rom = synthetic_rom(0x7FF0, 6);
        assert_eq!(
            parse_tmr_sega_header(&rom, 0x7FF0),
            parse_tmr_sega_header(&rom, 0x7FF0)
        );
    }

    #[test]
    fn parsing_never_mutates_input() {
        let rom = synthetic_rom(0x7FF0, 6);
        let before = rom.clone();
        let _ = parse_tmr_sega_header(&rom, 0x7FF0);
        assert_eq!(rom, before);
    }
}

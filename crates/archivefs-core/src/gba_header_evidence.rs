//! Pure, read-only Game Boy Advance cartridge header evidence.
//!
//! # Format verified, not assumed
//!
//! Verified against GBATEK (`https://problemkaputt.de/gbatek.htm`), the
//! long-standing primary-source-grade GBA/NDS hardware reference every
//! mainstream GBA emulator's loader is written against:
//!
//! ```text
//! [0x00..0x04]  entry_point       ARM branch instruction
//! [0x04..0xA0]  nintendo_logo     156-byte compressed bitmap (not checked
//!                                 by this module - see below)
//! [0xA0..0xAC]  game_title        12 bytes, uppercase ASCII
//! [0xAC..0xB0]  game_code         4 bytes, uppercase ASCII
//! [0xB0..0xB2]  maker_code        2 bytes, uppercase ASCII
//! [0xB2]        fixed_value       must be 0x96
//! [0xB3]        main_unit_code
//! [0xB4]        device_type
//! [0xBC]        software_version
//! [0xBD]        complement_check  see [`compute_complement_check`]
//! ```
//!
//! Complement-check algorithm, quoted from GBATEK: `chk=0: for i=0A0h to
//! 0BCh: chk=chk-[i]: next: chk=(chk-19h) and 0FFh` - see
//! [`compute_complement_check`], which implements exactly this loop.
//!
//! # Why the Nintendo logo bitmap is not checked here
//!
//! Unlike [`crate::gb_header_evidence`] (whose 48-byte Game Boy logo is
//! small enough to transcribe and cross-check with confidence), the GBA
//! logo is a 156-byte compressed bitmap; embedding it by hand here would add
//! real transcription-error risk for a check this module does not actually
//! need - `fixed_value == 0x96` (itself a real hardware-enforced constant no
//! non-GBA data coincidentally satisfies alongside a valid complement
//! check) and the complement-check formula together are already two
//! independent structural facts, matching this crate's "checksum validates
//! structure" discipline (see [`crate::snes_header_evidence`]'s identical
//! reasoning for why title bytes alone are never enough).

use crate::cartridge_header::ascii_field;
use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const GBA_HEADER_BYTES: usize = 0xC0;

const GAME_TITLE_OFFSET: usize = 0xA0;
const GAME_TITLE_LEN: usize = 12;
const GAME_CODE_OFFSET: usize = 0xAC;
const GAME_CODE_LEN: usize = 4;
const MAKER_CODE_OFFSET: usize = 0xB0;
const MAKER_CODE_LEN: usize = 2;
const FIXED_VALUE_OFFSET: usize = 0xB2;
const MAIN_UNIT_CODE_OFFSET: usize = 0xB3;
const DEVICE_TYPE_OFFSET: usize = 0xB4;
const SOFTWARE_VERSION_OFFSET: usize = 0xBC;
const COMPLEMENT_CHECK_OFFSET: usize = 0xBD;

const EXPECTED_FIXED_VALUE: u8 = 0x96;
const CHECKSUM_RANGE_START: usize = 0xA0;
const CHECKSUM_RANGE_END_INCLUSIVE: usize = 0xBC;

/// What a parsed GBA header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GbaHeaderFact {
    pub game_title: String,
    pub game_code: String,
    pub maker_code: String,
    pub fixed_value_valid: bool,
    pub main_unit_code: u8,
    pub device_type: u8,
    pub software_version: u8,
    pub complement_check: u8,
    pub complement_check_valid: bool,
}

/// Computes the GBA complement checksum over `bytes[0xA0..=0xBC]` -
/// `bytes` must be at least [`GBA_HEADER_BYTES`] long. `None` on a short
/// buffer.
pub fn compute_complement_check(bytes: &[u8]) -> Option<u8> {
    let region = bytes.get(CHECKSUM_RANGE_START..=CHECKSUM_RANGE_END_INCLUSIVE)?;
    let mut checksum: u8 = 0;
    for &byte in region {
        checksum = checksum.wrapping_sub(byte);
    }
    Some(checksum.wrapping_sub(0x19))
}

/// Parses `bytes` (must be at least [`GBA_HEADER_BYTES`] long). Fails closed
/// (`None`) only on a short buffer - a structurally invalid header (wrong
/// fixed value, wrong checksum) still parses, so a caller can see exactly
/// what failed - matching [`crate::gb_header_evidence::parse_gb_header`]'s
/// precedent.
pub fn parse_gba_header(bytes: &[u8]) -> Option<GbaHeaderFact> {
    if bytes.len() < GBA_HEADER_BYTES {
        return None;
    }
    let game_title = ascii_field(bytes, GAME_TITLE_OFFSET, GAME_TITLE_LEN)?;
    let game_code = ascii_field(bytes, GAME_CODE_OFFSET, GAME_CODE_LEN)?;
    let maker_code = ascii_field(bytes, MAKER_CODE_OFFSET, MAKER_CODE_LEN)?;
    let complement_check = bytes[COMPLEMENT_CHECK_OFFSET];
    let complement_check_valid = compute_complement_check(bytes) == Some(complement_check);

    Some(GbaHeaderFact {
        game_title,
        game_code,
        maker_code,
        fixed_value_valid: bytes[FIXED_VALUE_OFFSET] == EXPECTED_FIXED_VALUE,
        main_unit_code: bytes[MAIN_UNIT_CODE_OFFSET],
        device_type: bytes[DEVICE_TYPE_OFFSET],
        software_version: bytes[SOFTWARE_VERSION_OFFSET],
        complement_check,
        complement_check_valid,
    })
}

/// Neutral evidence for a parsed [`GbaHeaderFact`]:
///
/// - `fixed_value_valid` **and** `complement_check_valid`: `Strong`
///   `BootStructure` - two independent structural facts agree.
/// - Either alone: `Weak` - real but individually inconclusive (a random
///   byte has a 1/256 chance of matching `0x96`; a checksum without the
///   fixed-value constant validating too is a weaker signal on its own).
/// - Neither: no evidence.
///
/// The `game_code` field, when non-empty, is additionally reported as a
/// `Corroborated` `ProductCode` - a candidate identifier, not verified
/// against a canonical release list, matching every other `ProductCode`
/// fact this crate emits.
pub fn observe_gba_evidence(fact: &GbaHeaderFact) -> Vec<ContentEvidence> {
    let mut evidence = Vec::new();
    let structure_confidence = match (fact.fixed_value_valid, fact.complement_check_valid) {
        (true, true) => Some(ContentEvidenceConfidence::Strong),
        (true, false) | (false, true) => Some(ContentEvidenceConfidence::Weak),
        (false, false) => None,
    };
    if let Some(confidence) = structure_confidence {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "GBA cartridge header",
            confidence,
            format!(
                "fixed value 0x96 {}; complement checksum {}",
                if fact.fixed_value_valid {
                    "present"
                } else {
                    "absent"
                },
                if fact.complement_check_valid {
                    "valid"
                } else {
                    "did not validate"
                }
            ),
        ));
    }
    if !fact.game_code.is_empty() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            fact.game_code.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate game code read from the GBA header - not verified against a canonical release list",
        ));
    }
    evidence
}

/// A [`ContentDetector`] wrapping [`parse_gba_header`]/[`observe_gba_evidence`].
/// Recognises only when at least one structural fact (fixed value or
/// complement checksum) validates - see [`observe_gba_evidence`].
pub struct GbaHeaderDetector;

impl ContentDetector for GbaHeaderDetector {
    fn id(&self) -> &'static str {
        "gba_cartridge_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match parse_gba_header(data) {
            Some(fact) if fact.fixed_value_valid || fact.complement_check_valid => {
                ContentDetectionOutcome::Recognized {
                    evidence: observe_gba_evidence(&fact),
                }
            }
            _ => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_header(title: &str, code: &str, corrupt_checksum: bool) -> Vec<u8> {
        let mut bytes = vec![0u8; GBA_HEADER_BYTES];
        let title_bytes = title.as_bytes();
        bytes[GAME_TITLE_OFFSET..GAME_TITLE_OFFSET + title_bytes.len().min(GAME_TITLE_LEN)]
            .copy_from_slice(&title_bytes[..title_bytes.len().min(GAME_TITLE_LEN)]);
        let code_bytes = code.as_bytes();
        bytes[GAME_CODE_OFFSET..GAME_CODE_OFFSET + code_bytes.len().min(GAME_CODE_LEN)]
            .copy_from_slice(&code_bytes[..code_bytes.len().min(GAME_CODE_LEN)]);
        bytes[FIXED_VALUE_OFFSET] = EXPECTED_FIXED_VALUE;
        let checksum = compute_complement_check(&bytes).unwrap();
        bytes[COMPLEMENT_CHECK_OFFSET] = if corrupt_checksum {
            checksum.wrapping_add(1)
        } else {
            checksum
        };
        bytes
    }

    #[test]
    fn truncated_header_fails_closed() {
        let header = synthetic_header("GAME", "ABCD", false);
        assert_eq!(parse_gba_header(&header[..0x50]), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_gba_header(&[]), None);
    }

    #[test]
    fn valid_header_parses_every_field() {
        let header = synthetic_header("METROID FUS", "AMTE", false);
        let fact = parse_gba_header(&header).unwrap();
        assert_eq!(fact.game_title, "METROID FUS");
        assert_eq!(fact.game_code, "AMTE");
        assert!(fact.fixed_value_valid);
        assert!(fact.complement_check_valid);
    }

    #[test]
    fn wrong_fixed_value_is_detected() {
        let mut header = synthetic_header("GAME", "ABCD", false);
        header[FIXED_VALUE_OFFSET] = 0x00;
        let fact = parse_gba_header(&header).unwrap();
        assert!(!fact.fixed_value_valid);
    }

    #[test]
    fn corrupted_checksum_is_detected() {
        let header = synthetic_header("GAME", "ABCD", true);
        let fact = parse_gba_header(&header).unwrap();
        assert!(!fact.complement_check_valid);
    }

    #[test]
    fn checksum_algorithm_matches_gbatek_formula() {
        // All-zero region 0xA0..=0xBC (29 bytes, all zero) except fixed_value
        // still zero here too: chk = 0 - 0x19 (each byte subtracts 0).
        let bytes = vec![0u8; GBA_HEADER_BYTES];
        let expected = 0u8.wrapping_sub(0x19);
        assert_eq!(compute_complement_check(&bytes), Some(expected));
    }

    #[test]
    fn checksum_range_excludes_bytes_outside_0xa0_0xbc() {
        let header = synthetic_header("GAME", "ABCD", false);
        let mut before_range = header.clone();
        before_range[GAME_TITLE_OFFSET - 1] ^= 0xFF;
        assert_eq!(
            compute_complement_check(&before_range),
            compute_complement_check(&header)
        );
    }

    #[test]
    fn maker_code_and_device_fields_are_read() {
        let mut header = synthetic_header("GAME", "ABCD", false);
        header[MAKER_CODE_OFFSET..MAKER_CODE_OFFSET + 2].copy_from_slice(b"01");
        header[MAIN_UNIT_CODE_OFFSET] = 0;
        header[DEVICE_TYPE_OFFSET] = 0;
        header[SOFTWARE_VERSION_OFFSET] = 1;
        let fact = parse_gba_header(&header).unwrap();
        assert_eq!(fact.maker_code, "01");
        assert_eq!(fact.software_version, 1);
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn both_structural_facts_valid_yields_strong_evidence() {
        let header = synthetic_header("GAME", "ABCD", false);
        let fact = parse_gba_header(&header).unwrap();
        let evidence = observe_gba_evidence(&fact);
        let boot = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::BootStructure)
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn only_fixed_value_valid_yields_weak_evidence() {
        let header = synthetic_header("GAME", "ABCD", true);
        let fact = parse_gba_header(&header).unwrap();
        let evidence = observe_gba_evidence(&fact);
        let boot = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::BootStructure)
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Weak);
    }

    #[test]
    fn neither_valid_yields_no_boot_structure_evidence() {
        let mut header = synthetic_header("GAME", "ABCD", true);
        header[FIXED_VALUE_OFFSET] = 0x00;
        let fact = parse_gba_header(&header).unwrap();
        let evidence = observe_gba_evidence(&fact);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::BootStructure)
        );
    }

    #[test]
    fn nonempty_game_code_yields_product_code_evidence() {
        let header = synthetic_header("GAME", "AMTE", false);
        let fact = parse_gba_header(&header).unwrap();
        let evidence = observe_gba_evidence(&fact);
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "AMTE");
        assert_eq!(product.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn empty_game_code_yields_no_product_code_evidence() {
        let header = synthetic_header("GAME", "", false);
        let fact = parse_gba_header(&header).unwrap();
        assert!(
            !observe_gba_evidence(&fact)
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode)
        );
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = synthetic_header("GAME", "ABCD", false);
        let fact = parse_gba_header(&header).unwrap();
        for item in observe_gba_evidence(&fact) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = synthetic_header("GAME", "ABCD", false);
        assert_eq!(parse_gba_header(&header), parse_gba_header(&header));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = synthetic_header("GAME", "ABCD", false);
        let before = header.clone();
        let _ = parse_gba_header(&header);
        assert_eq!(header, before);
    }
}

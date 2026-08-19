//! Pure, read-only Atari Lynx `.lnx` header field decoding - deeper than
//! [`crate::header_normalization`]'s magic-only recognition.
//!
//! # Format verified, not assumed
//!
//! The `.lnx` header (created by K. Wilkins for the Handy emulator, and
//! used across the Lynx emulation ecosystem since) has this documented
//! layout, corroborated across multiple independent Lynx-development
//! references converged on during this research pass (cc65's Lynx platform
//! docs, the AtariAge Lynx development community, and the LNX Header
//! Generator tool for cc65):
//!
//! ```text
//! [0x00..0x04]  magic              "LYNX"
//! [0x04..0x06]  bank0_page_size    2 bytes, little-endian
//! [0x06..0x08]  bank1_page_size    2 bytes, little-endian
//! [0x08..0x0A]  version            2 bytes, little-endian - must be 1
//! [0x0A..0x2A]  cart_name          32 bytes, ASCII
//! [0x2A..0x3A]  manufacturer       16 bytes, ASCII
//! [0x3A]        rotation           0=none, 1=left (buttons down),
//!                                  2=right (buttons up)
//! ```
//!
//! Matches [`crate::header_normalization::HeaderNormalizationKind::Lynx64`]'s
//! own already-reviewed 64-byte header length and offset-0 `LYNX` magic
//! exactly.
//!
//! # Physical header facts vs. normalized payload - kept separate
//!
//! Like every other reversible header this crate handles, an `.lnx` header
//! is an emulator/tool convenience wrapped around the raw cartridge dump,
//! not part of the cartridge's own physical contents - this module reports
//! only what the header bytes themselves declare.

use crate::cartridge_header::ascii_field;
use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::header_normalization::recognize_lynx;

pub const LYNX_HEADER_BYTES: usize = 64;

/// Matches [`crate::header_normalization`]'s own (private) `LYNX_MAGIC`
/// exactly; duplicated here only because that constant is not exported.
const LYNX_MAGIC: &[u8; 4] = b"LYNX";

const BANK0_PAGE_SIZE_OFFSET: usize = 0x04;
const BANK1_PAGE_SIZE_OFFSET: usize = 0x06;
const VERSION_OFFSET: usize = 0x08;
const CART_NAME_OFFSET: usize = 0x0A;
const CART_NAME_LEN: usize = 32;
const MANUFACTURER_OFFSET: usize = 0x2A;
const MANUFACTURER_LEN: usize = 16;
const ROTATION_OFFSET: usize = 0x3A;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LynxRotation {
    None,
    Left,
    Right,
    /// A value this module does not recognise - reported honestly.
    Unknown(u8),
}

/// What a parsed `.lnx` header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LynxHeaderFact {
    pub bank0_page_size: u16,
    pub bank1_page_size: u16,
    pub version: u16,
    /// Whether `version == 1` - the only version this format documents.
    pub version_recognized: bool,
    pub cart_name: String,
    pub manufacturer: String,
    pub rotation: LynxRotation,
}

/// Parses `bytes` (must be at least [`LYNX_HEADER_BYTES`] long and begin
/// with the `LYNX` magic - see
/// [`crate::header_normalization::recognize_lynx`]). Fails closed (`None`)
/// on a short buffer or wrong magic.
pub fn parse_lynx_header(bytes: &[u8]) -> Option<LynxHeaderFact> {
    if bytes.len() < LYNX_HEADER_BYTES || !recognize_lynx(bytes) {
        return None;
    }
    let bank0_page_size = u16::from_le_bytes(
        bytes[BANK0_PAGE_SIZE_OFFSET..BANK0_PAGE_SIZE_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    let bank1_page_size = u16::from_le_bytes(
        bytes[BANK1_PAGE_SIZE_OFFSET..BANK1_PAGE_SIZE_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    let version = u16::from_le_bytes(
        bytes[VERSION_OFFSET..VERSION_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    let rotation = match bytes[ROTATION_OFFSET] {
        0 => LynxRotation::None,
        1 => LynxRotation::Left,
        2 => LynxRotation::Right,
        other => LynxRotation::Unknown(other),
    };

    let _ = LYNX_MAGIC; // documented cross-check, consumed via recognize_lynx above

    Some(LynxHeaderFact {
        bank0_page_size,
        bank1_page_size,
        version,
        version_recognized: version == 1,
        cart_name: ascii_field(bytes, CART_NAME_OFFSET, CART_NAME_LEN)?,
        manufacturer: ascii_field(bytes, MANUFACTURER_OFFSET, MANUFACTURER_LEN)?,
        rotation,
    })
}

/// Neutral evidence: `Strong` `BootStructure` for the `LYNX` magic match -
/// matching [`crate::header_normalization::HeaderNormalizationKind::Lynx64`]'s
/// own `Strong` rating for the identical signature, plus (when non-empty) a
/// `Corroborated` `ProductCode` for the cart name.
pub fn observe_lynx_evidence(fact: &LynxHeaderFact) -> Vec<ContentEvidence> {
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        "LYNX",
        ContentEvidenceConfidence::Strong,
        format!(
            "LYNX header magic matched; version {}{}",
            fact.version,
            if fact.version_recognized {
                ""
            } else {
                " (unrecognised - only version 1 is documented)"
            }
        ),
    )];
    if !fact.cart_name.is_empty() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            fact.cart_name.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate cart name read from the .lnx header - not verified against a canonical \
             release list",
        ));
    }
    evidence
}

/// A [`ContentDetector`] wrapping [`parse_lynx_header`]/[`observe_lynx_evidence`].
pub struct LynxHeaderDetector;

impl ContentDetector for LynxHeaderDetector {
    fn id(&self) -> &'static str {
        "lynx_cartridge_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match parse_lynx_header(data) {
            Some(fact) => ContentDetectionOutcome::Recognized {
                evidence: observe_lynx_evidence(&fact),
            },
            None => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_header(name: &str, manufacturer: &str, version: u16, rotation: u8) -> Vec<u8> {
        let mut bytes = vec![0u8; LYNX_HEADER_BYTES];
        bytes[0..4].copy_from_slice(LYNX_MAGIC);
        bytes[BANK0_PAGE_SIZE_OFFSET..BANK0_PAGE_SIZE_OFFSET + 2]
            .copy_from_slice(&256u16.to_le_bytes());
        bytes[BANK1_PAGE_SIZE_OFFSET..BANK1_PAGE_SIZE_OFFSET + 2]
            .copy_from_slice(&0u16.to_le_bytes());
        bytes[VERSION_OFFSET..VERSION_OFFSET + 2].copy_from_slice(&version.to_le_bytes());
        let name_bytes = name.as_bytes();
        bytes[CART_NAME_OFFSET..CART_NAME_OFFSET + name_bytes.len().min(CART_NAME_LEN)]
            .copy_from_slice(&name_bytes[..name_bytes.len().min(CART_NAME_LEN)]);
        let manufacturer_bytes = manufacturer.as_bytes();
        bytes[MANUFACTURER_OFFSET
            ..MANUFACTURER_OFFSET + manufacturer_bytes.len().min(MANUFACTURER_LEN)]
            .copy_from_slice(&manufacturer_bytes[..manufacturer_bytes.len().min(MANUFACTURER_LEN)]);
        bytes[ROTATION_OFFSET] = rotation;
        bytes
    }

    #[test]
    fn truncated_header_fails_closed() {
        let header = synthetic_header("GAME", "ATARI", 1, 0);
        assert_eq!(parse_lynx_header(&header[..32]), None);
    }

    #[test]
    fn wrong_magic_fails_closed() {
        let mut header = synthetic_header("GAME", "ATARI", 1, 0);
        header[0] = b'X';
        assert_eq!(parse_lynx_header(&header), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_lynx_header(&[]), None);
    }

    #[test]
    fn valid_header_parses_every_field() {
        let header = synthetic_header("CALIFORNIA GAMES", "EPYX", 1, 0);
        let fact = parse_lynx_header(&header).unwrap();
        assert_eq!(fact.cart_name, "CALIFORNIA GAMES");
        assert_eq!(fact.manufacturer, "EPYX");
        assert_eq!(fact.bank0_page_size, 256);
        assert!(fact.version_recognized);
        assert_eq!(fact.rotation, LynxRotation::None);
    }

    #[test]
    fn unrecognised_version_is_reported() {
        let header = synthetic_header("GAME", "ATARI", 2, 0);
        let fact = parse_lynx_header(&header).unwrap();
        assert!(!fact.version_recognized);
    }

    #[test]
    fn rotation_left_is_decoded() {
        let header = synthetic_header("GAME", "ATARI", 1, 1);
        let fact = parse_lynx_header(&header).unwrap();
        assert_eq!(fact.rotation, LynxRotation::Left);
    }

    #[test]
    fn rotation_right_is_decoded() {
        let header = synthetic_header("GAME", "ATARI", 1, 2);
        let fact = parse_lynx_header(&header).unwrap();
        assert_eq!(fact.rotation, LynxRotation::Right);
    }

    #[test]
    fn unrecognised_rotation_is_reported_honestly() {
        let header = synthetic_header("GAME", "ATARI", 1, 9);
        let fact = parse_lynx_header(&header).unwrap();
        assert_eq!(fact.rotation, LynxRotation::Unknown(9));
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn evidence_includes_strong_magic_and_product_code() {
        let header = synthetic_header("CALIFORNIA GAMES", "EPYX", 1, 0);
        let fact = parse_lynx_header(&header).unwrap();
        let evidence = observe_lynx_evidence(&fact);
        let boot = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::BootStructure)
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Strong);
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "CALIFORNIA GAMES");
    }

    #[test]
    fn empty_cart_name_yields_no_product_code() {
        let header = synthetic_header("", "ATARI", 1, 0);
        let fact = parse_lynx_header(&header).unwrap();
        assert!(
            !observe_lynx_evidence(&fact)
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode)
        );
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = synthetic_header("GAME", "ATARI", 1, 0);
        let fact = parse_lynx_header(&header).unwrap();
        for item in observe_lynx_evidence(&fact) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn matches_existing_header_normalization_recognition() {
        let header = synthetic_header("GAME", "ATARI", 1, 0);
        assert!(recognize_lynx(&header));
        assert!(parse_lynx_header(&header).is_some());
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = synthetic_header("GAME", "ATARI", 1, 0);
        assert_eq!(parse_lynx_header(&header), parse_lynx_header(&header));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = synthetic_header("GAME", "ATARI", 1, 0);
        let before = header.clone();
        let _ = parse_lynx_header(&header);
        assert_eq!(header, before);
    }
}

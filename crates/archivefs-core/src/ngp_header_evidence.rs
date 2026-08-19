//! Pure, read-only Neo Geo Pocket / Neo Geo Pocket Color cartridge header
//! evidence.
//!
//! # Format verified, not assumed
//!
//! Verified against the NGP cartridge specification document referenced in
//! `hiddenpalaceorg/rom-info` issue #24 (a documented `ngpcspec.txt`
//! reference used by real ROM-identification tooling), whose field layout,
//! expressed relative to the cartridge's file offset 0 (the spec's own
//! absolute addresses begin at the NGP memory map's `0x200000` cartridge
//! base - subtracted out below):
//!
//! ```text
//! [0x00..0x1C]  copyright        28 bytes, ASCII - either
//!                                "COPYRIGHT BY SNK CORPORATION" (SNK
//!                                titles) or "LICENSED BY SNK CORPORATION"
//!                                (third-party titles)
//! [0x1C..0x20]  entry_point      4 bytes, little-endian
//! [0x20..0x22]  software_id      2 bytes, little-endian BCD
//! [0x22]        version
//! [0x23]        system_flag      0x00 = monochrome NGP, 0x10 = color NGPC
//! [0x24..0x30]  title            12 bytes, ASCII
//! [0x30..0x40]  reserved         16 bytes, documented as zero-filled
//! ```

use crate::cartridge_header::ascii_field;
use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const NGP_HEADER_BYTES: usize = 0x40;

const COPYRIGHT_OFFSET: usize = 0x00;
const COPYRIGHT_LEN: usize = 28;
const ENTRY_POINT_OFFSET: usize = 0x1C;
const SOFTWARE_ID_OFFSET: usize = 0x20;
const VERSION_OFFSET: usize = 0x22;
const SYSTEM_FLAG_OFFSET: usize = 0x23;
const TITLE_OFFSET: usize = 0x24;
const TITLE_LEN: usize = 12;

const COPYRIGHT_SNK: &str = "COPYRIGHT BY SNK CORPORATION";
const COPYRIGHT_LICENSED: &str = "LICENSED BY SNK CORPORATION";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NgpSystemFlag {
    Monochrome,
    Color,
    /// A value this module does not recognise - reported honestly.
    Unknown(u8),
}

/// What a parsed NGP/NGPC header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NgpHeaderFact {
    pub copyright: String,
    /// Whether `copyright` matched one of the two documented values
    /// exactly.
    pub copyright_recognized: bool,
    pub entry_point: u32,
    pub software_id: u16,
    pub version: u8,
    pub system_flag: NgpSystemFlag,
    pub title: String,
}

/// Parses `bytes` (must be at least [`NGP_HEADER_BYTES`] long). Fails closed
/// (`None`) only on a short buffer - a structurally invalid header
/// (unrecognised copyright string) still parses, matching
/// [`crate::gb_header_evidence::parse_gb_header`]'s precedent.
pub fn parse_ngp_header(bytes: &[u8]) -> Option<NgpHeaderFact> {
    if bytes.len() < NGP_HEADER_BYTES {
        return None;
    }
    let copyright = ascii_field(bytes, COPYRIGHT_OFFSET, COPYRIGHT_LEN)?;
    let copyright_recognized = copyright == COPYRIGHT_SNK || copyright == COPYRIGHT_LICENSED;
    let system_flag = match bytes[SYSTEM_FLAG_OFFSET] {
        0x00 => NgpSystemFlag::Monochrome,
        0x10 => NgpSystemFlag::Color,
        other => NgpSystemFlag::Unknown(other),
    };
    Some(NgpHeaderFact {
        copyright_recognized,
        copyright,
        entry_point: u32::from_le_bytes(
            bytes[ENTRY_POINT_OFFSET..ENTRY_POINT_OFFSET + 4]
                .try_into()
                .unwrap(),
        ),
        software_id: u16::from_le_bytes(
            bytes[SOFTWARE_ID_OFFSET..SOFTWARE_ID_OFFSET + 2]
                .try_into()
                .unwrap(),
        ),
        version: bytes[VERSION_OFFSET],
        system_flag,
        title: ascii_field(bytes, TITLE_OFFSET, TITLE_LEN)?,
    })
}

/// Neutral evidence: `Strong` `BootStructure` only when `copyright_recognized`
/// (an exact match against one of the two documented copyright strings - a
/// specific, multi-word signature, not a single magic byte). No evidence at
/// all when unrecognised, matching this crate's "unrecognised signature
/// emits nothing" precedent. `system_flag`, when not `Unknown`, is reported
/// as its own `Corroborated` fact - real, but this module never resolves it
/// into "this is an NGP-only" or "NGPC-only" platform decision.
pub fn observe_ngp_evidence(fact: &NgpHeaderFact) -> Vec<ContentEvidence> {
    if !fact.copyright_recognized {
        return Vec::new();
    }
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        fact.copyright.clone(),
        ContentEvidenceConfidence::Strong,
        "Neo Geo Pocket cartridge copyright string matched exactly",
    )];
    let system_label = match fact.system_flag {
        NgpSystemFlag::Monochrome => Some("Monochrome (NGP)"),
        NgpSystemFlag::Color => Some("Color (NGPC)"),
        NgpSystemFlag::Unknown(_) => None,
    };
    if let Some(label) = system_label {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            label,
            ContentEvidenceConfidence::Corroborated,
            "system-flag byte read from the NGP header - a real declared compatibility fact, \
             never resolved into a platform decision by this module",
        ));
    }
    if !fact.title.is_empty() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            fact.title.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate title read from the NGP header - not verified against a canonical release list",
        ));
    }
    evidence
}

/// A [`ContentDetector`] wrapping [`parse_ngp_header`]/[`observe_ngp_evidence`].
pub struct NgpHeaderDetector;

impl ContentDetector for NgpHeaderDetector {
    fn id(&self) -> &'static str {
        "ngp_cartridge_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match parse_ngp_header(data) {
            Some(fact) if fact.copyright_recognized => ContentDetectionOutcome::Recognized {
                evidence: observe_ngp_evidence(&fact),
            },
            _ => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_header(copyright: &str, system_flag: u8, title: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; NGP_HEADER_BYTES];
        let copyright_bytes = copyright.as_bytes();
        bytes[COPYRIGHT_OFFSET..COPYRIGHT_OFFSET + copyright_bytes.len().min(COPYRIGHT_LEN)]
            .copy_from_slice(&copyright_bytes[..copyright_bytes.len().min(COPYRIGHT_LEN)]);
        bytes[ENTRY_POINT_OFFSET..ENTRY_POINT_OFFSET + 4].copy_from_slice(&0x1000u32.to_le_bytes());
        bytes[SOFTWARE_ID_OFFSET..SOFTWARE_ID_OFFSET + 2].copy_from_slice(&0x0042u16.to_le_bytes());
        bytes[VERSION_OFFSET] = 1;
        bytes[SYSTEM_FLAG_OFFSET] = system_flag;
        let title_bytes = title.as_bytes();
        bytes[TITLE_OFFSET..TITLE_OFFSET + title_bytes.len().min(TITLE_LEN)]
            .copy_from_slice(&title_bytes[..title_bytes.len().min(TITLE_LEN)]);
        bytes
    }

    #[test]
    fn truncated_header_fails_closed() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x00, "GAME");
        assert_eq!(parse_ngp_header(&header[..32]), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_ngp_header(&[]), None);
    }

    #[test]
    fn snk_copyright_is_recognized() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x00, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        assert!(fact.copyright_recognized);
    }

    #[test]
    fn licensed_copyright_is_recognized() {
        let header = synthetic_header(COPYRIGHT_LICENSED, 0x00, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        assert!(fact.copyright_recognized);
    }

    #[test]
    fn unrecognized_copyright_is_reported_false_but_still_parses() {
        let header = synthetic_header("NOT A REAL COPYRIGHT STRING", 0x00, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        assert!(!fact.copyright_recognized);
    }

    #[test]
    fn monochrome_system_flag_is_decoded() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x00, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        assert_eq!(fact.system_flag, NgpSystemFlag::Monochrome);
    }

    #[test]
    fn color_system_flag_is_decoded() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x10, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        assert_eq!(fact.system_flag, NgpSystemFlag::Color);
    }

    #[test]
    fn unrecognized_system_flag_is_reported_honestly() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x55, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        assert_eq!(fact.system_flag, NgpSystemFlag::Unknown(0x55));
    }

    #[test]
    fn title_and_ids_are_parsed() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x00, "SNK VS CAPCO");
        let fact = parse_ngp_header(&header).unwrap();
        assert_eq!(fact.title, "SNK VS CAPCO");
        assert_eq!(fact.software_id, 0x0042);
        assert_eq!(fact.entry_point, 0x1000);
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn recognized_copyright_yields_strong_evidence() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x00, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        let evidence = observe_ngp_evidence(&fact);
        let boot = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::BootStructure)
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn unrecognized_copyright_yields_no_evidence() {
        let header = synthetic_header("SOMETHING ELSE", 0x00, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        assert!(observe_ngp_evidence(&fact).is_empty());
    }

    #[test]
    fn color_system_flag_yields_content_signature_fact() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x10, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        let evidence = observe_ngp_evidence(&fact);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature
                    && item.value == "Color (NGPC)")
        );
    }

    #[test]
    fn unknown_system_flag_yields_no_content_signature_fact() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x99, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        assert!(
            !observe_ngp_evidence(&fact)
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature)
        );
    }

    #[test]
    fn nonempty_title_yields_product_code() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x00, "FATAL FURY");
        let fact = parse_ngp_header(&header).unwrap();
        let product = observe_ngp_evidence(&fact)
            .into_iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "FATAL FURY");
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x10, "GAME");
        let fact = parse_ngp_header(&header).unwrap();
        for item in observe_ngp_evidence(&fact) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure
                    | ContentEvidenceKind::ContentSignature
                    | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x00, "GAME");
        assert_eq!(parse_ngp_header(&header), parse_ngp_header(&header));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = synthetic_header(COPYRIGHT_SNK, 0x00, "GAME");
        let before = header.clone();
        let _ = parse_ngp_header(&header);
        assert_eq!(header, before);
    }
}

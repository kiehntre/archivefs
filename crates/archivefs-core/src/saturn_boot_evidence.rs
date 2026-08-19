//! Pure, read-only Sega Saturn boot-header ("System ID") evidence
//! extraction.
//!
//! # Format verified, not assumed
//!
//! Verified directly against Sega's own official SDK document, **"Disc
//! Format Standards Specification Sheet" (Doc. # ST-040-R4-051795, Ver.
//! 1.0)**, section 4.2 "System ID" / Figure 4.1 and section 4.3
//! ("Description of the System ID") - the authoritative primary source,
//! not a secondhand wiki table. Cross-checked against an independent
//! source (Mednafen's `ss.cpp` boot-signature/game-ID/area-code reads,
//! `https://github.com/OpenEmu/Mednafen-Core/blob/master/mednafen/ss/ss.cpp`),
//! which reads the same hardware-ID string and offset region.
//!
//! ```text
//! [0x00..0x10] hardware_id       16 bytes  "SEGA SEGASATURN" (padded)
//! [0x10..0x20] maker_id          16 bytes  "SEGA ENTERPRISES", or
//!                                          "SEGATP<company code>" for 3rd party
//! [0x20..0x2A] product_number    10 bytes
//! [0x2A..0x30] version            6 bytes  "V" + 1 digit + "." + 3 digits
//! [0x30..0x38] release_date       8 bytes  YYYYMMDD
//! [0x38..0x40] device_info        8 bytes  e.g. "CD-1/1  "
//! [0x40..0x4A] area_symbols      10 bytes  closely-packed region letters
//! [0x4A..0x50] (reserved/space)   6 bytes
//! [0x50..0x60] peripherals       16 bytes  closely-packed capability letters
//! [0x60..0xD0] game_title       112 bytes
//! ```
//!
//! Area symbols (per the spec's own "List of Area Symbols"): Japan `J`,
//! Asia NTSC `T`, North America `U`, Europe/other PAL `E`. Multiple
//! letters may appear packed together (e.g. `JTU`) - this module reports
//! the raw trimmed string, not a single resolved region, since more than
//! one region can legitimately apply.
//!
//! # Collision safety
//!
//! `SEGA SEGASATURN` is Saturn's own hardware-identifier field, required
//! by the spec for the disc to boot at all on real hardware - a
//! [`ContentEvidenceKind::BootStructure`] fact this module treats as
//! `Strong`. It is still only a boot-signature *candidate*, not a final
//! platform decision on its own - see the crate-level architecture
//! principle every module in this arc follows.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const SATURN_SYSTEM_ID_BYTES: usize = 0x100;

const HARDWARE_ID: (usize, usize) = (0x00, 0x10);
const MAKER_ID: (usize, usize) = (0x10, 0x10);
const PRODUCT_NUMBER: (usize, usize) = (0x20, 0x0A);
const VERSION: (usize, usize) = (0x2A, 0x06);
const RELEASE_DATE: (usize, usize) = (0x30, 0x08);
const DEVICE_INFO: (usize, usize) = (0x38, 0x08);
const AREA_SYMBOLS: (usize, usize) = (0x40, 0x0A);
const PERIPHERALS: (usize, usize) = (0x50, 0x10);
const GAME_TITLE: (usize, usize) = (0x60, 0x70);

const RECOGNIZED_HARDWARE_ID: &str = "SEGA SEGASATURN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaturnSystemIdFact {
    pub hardware_id: String,
    pub hardware_id_recognized: bool,
    pub maker_id: String,
    pub product_number: String,
    pub version: String,
    pub release_date: String,
    pub device_info: String,
    pub area_symbols: String,
    pub peripherals: String,
    pub game_title: String,
}

fn field(bytes: &[u8], (offset, length): (usize, usize)) -> String {
    String::from_utf8_lossy(&bytes[offset..offset + length])
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

/// Parses the Saturn "System ID" from `bytes`, which must be at least
/// [`SATURN_SYSTEM_ID_BYTES`] long. Fails closed (`None`) on a shorter
/// buffer rather than a partial struct.
pub fn parse_saturn_system_id(bytes: &[u8]) -> Option<SaturnSystemIdFact> {
    if bytes.len() < SATURN_SYSTEM_ID_BYTES {
        return None;
    }
    let hardware_id = field(bytes, HARDWARE_ID);
    let hardware_id_recognized = hardware_id == RECOGNIZED_HARDWARE_ID;
    Some(SaturnSystemIdFact {
        hardware_id_recognized,
        hardware_id,
        maker_id: field(bytes, MAKER_ID),
        product_number: field(bytes, PRODUCT_NUMBER),
        version: field(bytes, VERSION),
        release_date: field(bytes, RELEASE_DATE),
        device_info: field(bytes, DEVICE_INFO),
        area_symbols: field(bytes, AREA_SYMBOLS),
        peripherals: field(bytes, PERIPHERALS),
        game_title: field(bytes, GAME_TITLE),
    })
}

/// Emits nothing when `hardware_id` was not recognised - see
/// [`crate::dreamcast_boot_evidence::observe_ip_bin_evidence`] for the same
/// pattern and its rationale. When recognised: `BootStructure` (`Strong`),
/// plus `ProductCode` (`Corroborated`) only if `product_number` is
/// non-empty.
pub fn observe_saturn_evidence(fact: &SaturnSystemIdFact) -> Vec<ContentEvidence> {
    if !fact.hardware_id_recognized {
        return Vec::new();
    }
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        fact.hardware_id.clone(),
        ContentEvidenceConfidence::Strong,
        "System ID hardware identifier matches the Sega Saturn boot signature (verified against Sega's own SDK spec)",
    )];
    if !fact.product_number.is_empty() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            fact.product_number.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate product number read from the Saturn System ID - not verified against a canonical release list",
        ));
    }
    evidence
}

pub struct SaturnSystemIdDetector;

impl ContentDetector for SaturnSystemIdDetector {
    fn id(&self) -> &'static str {
        "saturn_system_id"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match parse_saturn_system_id(data) {
            Some(fact) if fact.hardware_id_recognized => ContentDetectionOutcome::Recognized {
                evidence: observe_saturn_evidence(&fact),
            },
            _ => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(bytes: &mut [u8], (offset, length): (usize, usize), value: &[u8]) {
        let end = (offset + value.len()).min(offset + length);
        bytes[offset..end].copy_from_slice(&value[..end - offset]);
    }

    fn synthetic_system_id() -> Vec<u8> {
        let mut bytes = vec![b' '; SATURN_SYSTEM_ID_BYTES];
        put(&mut bytes, HARDWARE_ID, b"SEGA SEGASATURN ");
        put(&mut bytes, MAKER_ID, b"SEGA ENTERPRISES");
        put(&mut bytes, PRODUCT_NUMBER, b"T-7101G   ");
        put(&mut bytes, VERSION, b"V1.004");
        put(&mut bytes, RELEASE_DATE, b"19961117");
        put(&mut bytes, DEVICE_INFO, b"CD-1/1  ");
        put(&mut bytes, AREA_SYMBOLS, b"JTU       ");
        put(&mut bytes, PERIPHERALS, b"JAMKST          ");
        put(&mut bytes, GAME_TITLE, b"VIRTUAL KYOUTEI");
        bytes
    }

    #[test]
    fn recognised_hardware_id_is_detected() {
        let fact = parse_saturn_system_id(&synthetic_system_id()).unwrap();
        assert_eq!(fact.hardware_id, "SEGA SEGASATURN");
        assert!(fact.hardware_id_recognized);
    }

    #[test]
    fn unrecognised_hardware_id_yields_no_evidence() {
        let mut bytes = synthetic_system_id();
        put(&mut bytes, HARDWARE_ID, b"NOT A SATURN    ");
        let fact = parse_saturn_system_id(&bytes).unwrap();
        assert!(!fact.hardware_id_recognized);
        assert!(observe_saturn_evidence(&fact).is_empty());
    }

    #[test]
    fn malformed_truncated_header_fails_closed() {
        let bytes = synthetic_system_id();
        assert_eq!(parse_saturn_system_id(&bytes[..0x50]), None);
        assert_eq!(
            SaturnSystemIdDetector.detect(&bytes[..0x50]),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn product_number_is_extracted() {
        let fact = parse_saturn_system_id(&synthetic_system_id()).unwrap();
        assert_eq!(fact.product_number, "T-7101G");
    }

    #[test]
    fn version_is_extracted() {
        let fact = parse_saturn_system_id(&synthetic_system_id()).unwrap();
        assert_eq!(fact.version, "V1.004");
    }

    #[test]
    fn area_symbols_are_extracted() {
        let fact = parse_saturn_system_id(&synthetic_system_id()).unwrap();
        assert_eq!(fact.area_symbols, "JTU");
    }

    #[test]
    fn boot_signature_evidence_is_strong() {
        let fact = parse_saturn_system_id(&synthetic_system_id()).unwrap();
        let evidence = observe_saturn_evidence(&fact);
        let boot = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::BootStructure)
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn saturn_evidence_never_assigns_a_platform() {
        let fact = parse_saturn_system_id(&synthetic_system_id()).unwrap();
        for item in observe_saturn_evidence(&fact) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let bytes = synthetic_system_id();
        assert_eq!(
            parse_saturn_system_id(&bytes),
            parse_saturn_system_id(&bytes)
        );
    }
}

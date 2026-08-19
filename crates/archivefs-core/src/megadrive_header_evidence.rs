//! Pure, read-only Sega Mega Drive / Genesis cartridge header field
//! decoding - deeper than [`crate::platform::PLATFORMS`]'s own `MegaDrive`
//! [`crate::platform::MagicRule`] (which checks only the 4-byte `SEGA`
//! prefix at offset `0x100`).
//!
//! # Format verified, not assumed
//!
//! Verified against Plutiedev's ROM header reference
//! (`https://plutiedev.com/rom-header`), cross-checked against this crate's
//! own already-reviewed `MegaDrive` platform entry (same `SEGA`-at-`0x100`
//! convention):
//!
//! ```text
//! [0x100..0x110]  console_name       16 bytes, ASCII (begins "SEGA")
//! [0x110..0x120]  copyright_release  16 bytes, ASCII
//! [0x120..0x150]  domestic_title     48 bytes, ASCII
//! [0x150..0x180]  overseas_title     48 bytes, ASCII
//! [0x180..0x18E]  serial_number      14 bytes, ASCII
//! [0x18E..0x190]  checksum           2 bytes, big-endian
//! [0x190..0x1A0]  device_support     16 bytes, ASCII
//! [0x1A0..0x1A8]  rom_address_range  8 bytes (start/end, 4 bytes each)
//! [0x1A8..0x1B0]  ram_address_range  8 bytes (start/end, 4 bytes each)
//! [0x1F0..0x1F3]  region_support     3 bytes, ASCII
//! ```
//!
//! # Checksum validation is a separate, opt-in, whole-ROM operation
//!
//! The Mega Drive checksum is the 16-bit wrapping sum of every big-endian
//! word from offset `0x200` to end-of-file - by construction it needs the
//! *entire* ROM, unlike every fixed-offset field this module's
//! [`parse_megadrive_header`] reads from a small bounded prefix. Per this
//! crate's performance discipline (bounded reads by default, whole-file
//! operations opt-in and never mandatory for initial identification), that
//! computation lives in its own function, [`verify_megadrive_checksum`],
//! which a caller invokes only when it already has (or is willing to read)
//! the whole ROM - [`parse_megadrive_header`] itself never requires more
//! than the first `0x1F3` bytes.
//!
//! # `SEGA` alone is not exclusive platform proof
//!
//! Matching this crate's own `MegaDrive` platform entry (`Corroborated`, not
//! `Strong` - the header alone is "the only proof" the entry's own
//! explanation already documents as insufficient for higher confidence).
//! This module does not change or duplicate that platform-level judgement;
//! it reports the same underlying fact at the content-evidence layer, plus
//! the additional fields a resolver could use to corroborate it further
//! (title, serial, region).

use crate::cartridge_header::ascii_field;
use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

/// The bounded prefix [`parse_megadrive_header`] needs - covers every field
/// through `region_support` (`0x1F0..0x1F3`).
pub const MEGADRIVE_HEADER_PEEK_BYTES: usize = 0x1F3;

const CONSOLE_NAME_OFFSET: usize = 0x100;
const CONSOLE_NAME_LEN: usize = 16;
const DOMESTIC_TITLE_OFFSET: usize = 0x120;
const DOMESTIC_TITLE_LEN: usize = 48;
const OVERSEAS_TITLE_OFFSET: usize = 0x150;
const OVERSEAS_TITLE_LEN: usize = 48;
const SERIAL_NUMBER_OFFSET: usize = 0x180;
const SERIAL_NUMBER_LEN: usize = 14;
const CHECKSUM_OFFSET: usize = 0x18E;
const REGION_SUPPORT_OFFSET: usize = 0x1F0;
const REGION_SUPPORT_LEN: usize = 3;

const CONSOLE_NAME_PREFIX: &str = "SEGA";
const CHECKSUM_RANGE_START: usize = 0x200;

/// What a parsed Mega Drive header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MegaDriveHeaderFact {
    pub console_name: String,
    /// Whether `console_name` begins with `"SEGA"` - the same convention
    /// this crate's `MegaDrive` [`crate::platform::MagicRule`] already
    /// checks.
    pub console_name_recognized: bool,
    pub domestic_title: String,
    pub overseas_title: String,
    pub serial_number: String,
    /// The declared checksum, as read - not independently validated by this
    /// function. See [`verify_megadrive_checksum`].
    pub checksum: u16,
    pub region_support: String,
}

/// Parses `bytes` (must be at least [`MEGADRIVE_HEADER_PEEK_BYTES`] long).
/// `None` on a short buffer - never a partial struct.
pub fn parse_megadrive_header(bytes: &[u8]) -> Option<MegaDriveHeaderFact> {
    if bytes.len() < MEGADRIVE_HEADER_PEEK_BYTES {
        return None;
    }
    let console_name = ascii_field(bytes, CONSOLE_NAME_OFFSET, CONSOLE_NAME_LEN)?;
    let console_name_recognized = console_name.starts_with(CONSOLE_NAME_PREFIX);
    Some(MegaDriveHeaderFact {
        console_name_recognized,
        console_name,
        domestic_title: ascii_field(bytes, DOMESTIC_TITLE_OFFSET, DOMESTIC_TITLE_LEN)?,
        overseas_title: ascii_field(bytes, OVERSEAS_TITLE_OFFSET, OVERSEAS_TITLE_LEN)?,
        serial_number: ascii_field(bytes, SERIAL_NUMBER_OFFSET, SERIAL_NUMBER_LEN)?,
        checksum: u16::from_be_bytes(
            bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 2]
                .try_into()
                .unwrap(),
        ),
        region_support: ascii_field(bytes, REGION_SUPPORT_OFFSET, REGION_SUPPORT_LEN)?,
    })
}

/// Computes the Mega Drive ROM checksum: the 16-bit wrapping sum of every
/// big-endian word from offset [`CHECKSUM_RANGE_START`] (`0x200`) to the end
/// of `rom` - the whole ROM, not a bounded prefix. `None` if `rom` is
/// shorter than `0x200` (nothing to sum) or its length past that point is
/// odd (an incomplete trailing word - refused, not silently ignored).
pub fn compute_megadrive_checksum(rom: &[u8]) -> Option<u16> {
    let region = rom.get(CHECKSUM_RANGE_START..)?;
    if !region.len().is_multiple_of(2) {
        return None;
    }
    let mut checksum: u16 = 0;
    for word in region.chunks_exact(2) {
        checksum = checksum.wrapping_add(u16::from_be_bytes([word[0], word[1]]));
    }
    Some(checksum)
}

/// Whether `rom`'s computed checksum ([`compute_megadrive_checksum`])
/// matches `declared` (the value [`MegaDriveHeaderFact::checksum`] read from
/// the header). `false` when the computation itself could not run (see
/// [`compute_megadrive_checksum`]) - a caller that wants to distinguish
/// "did not match" from "could not be computed" should call
/// [`compute_megadrive_checksum`] directly instead.
pub fn verify_megadrive_checksum(rom: &[u8], declared: u16) -> bool {
    compute_megadrive_checksum(rom) == Some(declared)
}

/// Neutral evidence for a parsed [`MegaDriveHeaderFact`]: `Corroborated`
/// `BootStructure` when `console_name_recognized` - matching this crate's
/// existing `MegaDrive` platform entry's own confidence for the identical
/// underlying fact (see the module documentation). No evidence at all when
/// the console name is not recognised.
pub fn observe_megadrive_evidence(fact: &MegaDriveHeaderFact) -> Vec<ContentEvidence> {
    if !fact.console_name_recognized {
        return Vec::new();
    }
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        fact.console_name.clone(),
        ContentEvidenceConfidence::Corroborated,
        "Mega Drive/Genesis console-name field begins with SEGA at offset 0x100 - matches this \
         crate's existing MegaDrive platform magic rule, at the same confidence",
    )];
    if !fact.serial_number.is_empty() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            fact.serial_number.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate serial number read from the Mega Drive header - not verified against a \
             canonical release list",
        ));
    }
    evidence
}

/// A [`ContentDetector`] wrapping [`parse_megadrive_header`]/
/// [`observe_megadrive_evidence`].
pub struct MegaDriveHeaderDetector;

impl ContentDetector for MegaDriveHeaderDetector {
    fn id(&self) -> &'static str {
        "megadrive_cartridge_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match parse_megadrive_header(data) {
            Some(fact) if fact.console_name_recognized => ContentDetectionOutcome::Recognized {
                evidence: observe_megadrive_evidence(&fact),
            },
            _ => ContentDetectionOutcome::NotRecognized,
        }
    }
}

/// Test-only builder shared with [`crate::sega32x_header_evidence`]'s own
/// tests, so that module's fixtures build on the identical synthetic header
/// this module's tests already use, rather than a second, possibly
/// drifting copy.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    pub(crate) fn synthetic_header_for_tests(console_name: &[u8], serial: &str) -> Vec<u8> {
        tests::synthetic_header(console_name, serial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn synthetic_header(console_name: &[u8], serial: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; MEGADRIVE_HEADER_PEEK_BYTES];
        bytes[CONSOLE_NAME_OFFSET..CONSOLE_NAME_OFFSET + console_name.len().min(CONSOLE_NAME_LEN)]
            .copy_from_slice(&console_name[..console_name.len().min(CONSOLE_NAME_LEN)]);
        let domestic = b"SONIC THE HEDGEHOG 2";
        bytes[DOMESTIC_TITLE_OFFSET..DOMESTIC_TITLE_OFFSET + domestic.len()]
            .copy_from_slice(domestic);
        let serial_bytes = serial.as_bytes();
        bytes[SERIAL_NUMBER_OFFSET
            ..SERIAL_NUMBER_OFFSET + serial_bytes.len().min(SERIAL_NUMBER_LEN)]
            .copy_from_slice(&serial_bytes[..serial_bytes.len().min(SERIAL_NUMBER_LEN)]);
        bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 2].copy_from_slice(&0x1234u16.to_be_bytes());
        bytes[REGION_SUPPORT_OFFSET..REGION_SUPPORT_OFFSET + 3].copy_from_slice(b"JUE");
        bytes
    }

    #[test]
    fn truncated_header_fails_closed() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00000000");
        assert_eq!(parse_megadrive_header(&header[..0x100]), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_megadrive_header(&[]), None);
    }

    #[test]
    fn recognized_console_name_variants_are_detected() {
        for name in [
            b"SEGA GENESIS".as_slice(),
            b"SEGA MEGA DRIVE".as_slice(),
            b"SEGA 32X".as_slice(),
        ] {
            let header = synthetic_header(name, "GM 00000000");
            let fact = parse_megadrive_header(&header).unwrap();
            assert!(
                fact.console_name_recognized,
                "{name:?} should be recognized"
            );
        }
    }

    #[test]
    fn unrecognized_console_name_is_reported_false() {
        let header = synthetic_header(b"NOT SEGA AT ALL", "GM 00000000");
        let fact = parse_megadrive_header(&header).unwrap();
        assert!(!fact.console_name_recognized);
    }

    #[test]
    fn titles_and_serial_are_parsed() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00001051");
        let fact = parse_megadrive_header(&header).unwrap();
        assert_eq!(fact.domestic_title, "SONIC THE HEDGEHOG 2");
        assert_eq!(fact.serial_number, "GM 00001051");
    }

    #[test]
    fn checksum_field_is_read_but_not_validated_here() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00000000");
        let fact = parse_megadrive_header(&header).unwrap();
        assert_eq!(fact.checksum, 0x1234);
    }

    #[test]
    fn region_support_is_read() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00000000");
        let fact = parse_megadrive_header(&header).unwrap();
        assert_eq!(fact.region_support, "JUE");
    }

    // ------------------------------------------------------------------
    // Checksum verification (whole-ROM, opt-in)
    // ------------------------------------------------------------------

    #[test]
    fn checksum_computation_matches_a_hand_built_rom() {
        let mut rom = vec![0u8; CHECKSUM_RANGE_START + 4];
        rom[CHECKSUM_RANGE_START..CHECKSUM_RANGE_START + 2]
            .copy_from_slice(&0x0001u16.to_be_bytes());
        rom[CHECKSUM_RANGE_START + 2..CHECKSUM_RANGE_START + 4]
            .copy_from_slice(&0x0002u16.to_be_bytes());
        assert_eq!(compute_megadrive_checksum(&rom), Some(3));
    }

    #[test]
    fn checksum_computation_wraps_around() {
        let mut rom = vec![0u8; CHECKSUM_RANGE_START + 4];
        rom[CHECKSUM_RANGE_START..CHECKSUM_RANGE_START + 2]
            .copy_from_slice(&0xFFFFu16.to_be_bytes());
        rom[CHECKSUM_RANGE_START + 2..CHECKSUM_RANGE_START + 4]
            .copy_from_slice(&0x0002u16.to_be_bytes());
        assert_eq!(compute_megadrive_checksum(&rom), Some(1));
    }

    #[test]
    fn checksum_computation_rejects_odd_trailing_length() {
        let rom = vec![0u8; CHECKSUM_RANGE_START + 3];
        assert_eq!(compute_megadrive_checksum(&rom), None);
    }

    #[test]
    fn checksum_computation_too_short_for_range_fails_closed() {
        let rom = vec![0u8; 100];
        assert_eq!(compute_megadrive_checksum(&rom), None);
    }

    #[test]
    fn empty_region_past_0x200_sums_to_zero() {
        let rom = vec![0u8; CHECKSUM_RANGE_START];
        assert_eq!(compute_megadrive_checksum(&rom), Some(0));
    }

    #[test]
    fn verify_matches_computed_value() {
        let mut rom = vec![0u8; CHECKSUM_RANGE_START + 2];
        rom[CHECKSUM_RANGE_START..CHECKSUM_RANGE_START + 2]
            .copy_from_slice(&0x00AAu16.to_be_bytes());
        assert!(verify_megadrive_checksum(&rom, 0x00AA));
        assert!(!verify_megadrive_checksum(&rom, 0x00AB));
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn recognized_console_name_yields_corroborated_boot_structure() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00001051");
        let fact = parse_megadrive_header(&header).unwrap();
        let evidence = observe_megadrive_evidence(&fact);
        let boot = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::BootStructure)
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn nonempty_serial_yields_product_code_evidence() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00001051");
        let fact = parse_megadrive_header(&header).unwrap();
        let product = observe_megadrive_evidence(&fact)
            .into_iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "GM 00001051");
    }

    #[test]
    fn unrecognized_console_name_yields_no_evidence() {
        let header = synthetic_header(b"NOT SEGA", "GM 00000000");
        let fact = parse_megadrive_header(&header).unwrap();
        assert!(observe_megadrive_evidence(&fact).is_empty());
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00001051");
        let fact = parse_megadrive_header(&header).unwrap();
        for item in observe_megadrive_evidence(&fact) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00001051");
        assert_eq!(
            parse_megadrive_header(&header),
            parse_megadrive_header(&header)
        );
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = synthetic_header(b"SEGA GENESIS", "GM 00001051");
        let before = header.clone();
        let _ = parse_megadrive_header(&header);
        assert_eq!(header, before);
    }
}

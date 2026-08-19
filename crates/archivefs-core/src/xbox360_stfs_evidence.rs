//! Pure, read-only Xbox 360 STFS ("Secure Transacted File System") package
//! metadata evidence - the digital-package envelope format used for
//! Xbox Live Arcade titles, downloadable content, saved games, avatar
//! items, and other Xbox 360 content, distinct from the on-disc
//! [`crate::xbox360_boot_evidence`] (XDVDFS + XEX2) this crate already
//! covers.
//!
//! # Format verified, not assumed
//!
//! Every field offset below is cross-checked against two independent
//! sources that agree exactly: the Free60 wiki's STFS page
//! (`https://free60.org/System-Software/Formats/STFS/`) and
//! `arkem/py360`'s `stfs.py` parser
//! (`https://github.com/arkem/py360`), a real, independently-authored
//! Python STFS reader. Both agree on every numeric field offset from
//! `content_type` (`0x344`) through `save_game_id` (`0x368`), and on the
//! `display_name`/`title_name` byte offsets and 128-byte-per-field width.
//!
//! This module's own offsets and the UTF-16BE decoding of the string
//! fields are additionally verified end-to-end against a real specimen in
//! this project's corpus: a genuine Xbox Live Arcade STFS package (magic
//! `LIVE`) whose `title_id` field (`0x58411252`) matches the hex `TitleID`
//! folder Xbox 360 content storage itself names the package under
//! (`<TitleID>/<ContentType>/<hash>`), and whose decoded `display_name`
//! and `title_name` fields both read `"Double Dragon"` - see the
//! crate-level milestone report for the exact path. `content_type` for
//! that same specimen (`0x000D0000`) also matches the on-disk
//! `ContentType` folder name (`000D0000`) byte-for-byte, giving a second,
//! independent real-world cross-check beyond the two written sources.
//!
//! ```text
//! STFS package header (fixed-position metadata fields; every multi-byte
//! numeric field is big-endian):
//! [0x000..0x004]   magic              4 bytes   "CON ", "LIVE", or "PIRS"
//! -- signature/certificate block: 0x004..0x22C, layout differs between
//!    CON (console-signed, embeds a full certificate chain) and
//!    LIVE/PIRS (Microsoft-signed, a simpler signature+padding block) -
//!    this module never reads any of it; see "Scope" below.
//! [0x340..0x344]   header_size        4 bytes
//! [0x344..0x348]   content_type       4 bytes
//! [0x348..0x34C]   metadata_version   4 bytes
//! [0x34C..0x354]   content_size       8 bytes
//! [0x354..0x358]   media_id           4 bytes
//! [0x358..0x35C]   version            4 bytes
//! [0x35C..0x360]   base_version       4 bytes
//! [0x360..0x364]   title_id           4 bytes
//! [0x364]          platform           1 byte    (2 = Xbox 360, 4 = PC)
//! [0x365]          executable_type    1 byte
//! [0x366]          disc_number        1 byte
//! [0x367]          disc_in_set        1 byte
//! [0x368..0x36C]   save_game_id       4 bytes
//! [0x411..0x491]   display_name       128 bytes UTF-16BE (first locale
//!                                     of an 18-locale, 2304-byte block -
//!                                     only the first, conventionally
//!                                     English, locale is read)
//! [0x1691..0x1711] title_name         128 bytes UTF-16BE
//! ```
//!
//! # Scope: fixed metadata fields only, never the signature/certificate
//! block or file listing
//!
//! This module reads only the fixed-position metadata fields documented
//! above - never the RSA signature, certificate chain, license entries, or
//! the block-hash-table/file-listing structure that follows the header
//! (whose own layout depends on `header_size` and package version, and
//! which is not needed for identity observation). No signature is
//! verified, no license is checked, no directory entry is walked, no file
//! is extracted, and nothing is ever decrypted - this is a read-only
//! identity/metadata peek, not an STFS filesystem implementation.
//!
//! # Xbox digital collision policy (see the crate-level milestone report)
//!
//! STFS is a package *envelope* used across many content classes - Xbox
//! Live Arcade games, DLC, saved games, avatar items, themes, gamer
//! pictures, and more - distinguished from one another only by
//! `content_type`, a plain numeric field this module exposes raw and
//! **never** interprets into a claim like "this is a game." `magic`
//! (`CON `/`LIVE`/`PIRS`) is a *signing* distinction (console-signed vs.
//! Microsoft-signed), orthogonal to content class - this module never
//! conflates the two, and never promotes either to a platform or content-
//! class decision. See [`observe_stfs_evidence`]'s own documentation for
//! exactly what evidence is (and is not) emitted.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const STFS_MAGIC_CON: &[u8; 4] = b"CON ";
pub const STFS_MAGIC_LIVE: &[u8; 4] = b"LIVE";
pub const STFS_MAGIC_PIRS: &[u8; 4] = b"PIRS";

const HEADER_SIZE_OFFSET: usize = 0x340;
const CONTENT_TYPE_OFFSET: usize = 0x344;
const METADATA_VERSION_OFFSET: usize = 0x348;
const CONTENT_SIZE_OFFSET: usize = 0x34C;
const MEDIA_ID_OFFSET: usize = 0x354;
const VERSION_OFFSET: usize = 0x358;
const BASE_VERSION_OFFSET: usize = 0x35C;
const TITLE_ID_OFFSET: usize = 0x360;
const PLATFORM_OFFSET: usize = 0x364;
const EXECUTABLE_TYPE_OFFSET: usize = 0x365;
const DISC_NUMBER_OFFSET: usize = 0x366;
const DISC_IN_SET_OFFSET: usize = 0x367;
const SAVE_GAME_ID_OFFSET: usize = 0x368;
const DISPLAY_NAME_OFFSET: usize = 0x411;
const DISPLAY_NAME_BYTES: usize = 0x80;
const TITLE_NAME_OFFSET: usize = 0x1691;
const TITLE_NAME_BYTES: usize = 0x80;

/// Bounded prefix this module ever reads or requires - covers every field
/// documented above (the last, `title_name`, ends at `0x1691 + 0x80 =
/// 0x1711`) with no attempt to reach the much larger file-listing/hash-
/// table structure that follows. A real STFS package can be several
/// gigabytes; this module never needs more than this fixed prefix.
pub const STFS_HEADER_PEEK_BYTES: usize = TITLE_NAME_OFFSET + TITLE_NAME_BYTES;

/// Which STFS signing variant a package's magic declares - a *signing*
/// distinction, not a content-class one. See the module documentation's
/// collision policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StfsPackageVariant {
    /// `"CON "` - console-signed (embeds a full certificate chain).
    Con,
    /// `"LIVE"` - Microsoft-signed, distributed via Xbox Live.
    Live,
    /// `"PIRS"` - Microsoft-signed, not distributed via Xbox Live (e.g.
    /// some disc-based title updates).
    Pirs,
}

impl StfsPackageVariant {
    pub fn magic_str(self) -> &'static str {
        match self {
            Self::Con => "CON",
            Self::Live => "LIVE",
            Self::Pirs => "PIRS",
        }
    }
}

/// Detects the STFS signing variant from the first 4 bytes of `header`, if
/// any. Never reads past 4 bytes.
pub fn detect_stfs_variant(header: &[u8]) -> Option<StfsPackageVariant> {
    if header.len() < 4 {
        return None;
    }
    match &header[..4] {
        magic if magic == STFS_MAGIC_CON.as_slice() => Some(StfsPackageVariant::Con),
        magic if magic == STFS_MAGIC_LIVE.as_slice() => Some(StfsPackageVariant::Live),
        magic if magic == STFS_MAGIC_PIRS.as_slice() => Some(StfsPackageVariant::Pirs),
        _ => None,
    }
}

/// A cheap, magic-only check for whether `header` is worth attempting
/// [`parse_stfs_header`] on - mirrors this crate's existing
/// `looks_like_pkg`/`looks_like_chd` convention.
pub fn looks_like_stfs(header: &[u8]) -> bool {
    detect_stfs_variant(header).is_some()
}

/// What a parsed STFS metadata header directly states - see the module
/// documentation for the exact, two-source-corroborated field layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StfsHeaderFact {
    pub variant: StfsPackageVariant,
    pub header_size: u32,
    /// Raw content-type value - a package-envelope fact, never resolved
    /// into a content-class claim by this module. See the module
    /// documentation's collision policy.
    pub content_type: u32,
    pub metadata_version: u32,
    pub content_size: u64,
    pub media_id: u32,
    pub version: u32,
    pub base_version: u32,
    /// The package's Title ID, formatted as an 8-digit uppercase hex
    /// string (e.g. `"58411252"`) - the conventional form this identifier
    /// is displayed/stored as across the Xbox 360 ecosystem (matching the
    /// hex folder name real Xbox 360 content storage itself uses).
    pub title_id: u32,
    pub platform: u8,
    pub executable_type: u8,
    pub disc_number: u8,
    pub disc_in_set: u8,
    pub save_game_id: u32,
    /// The first (conventionally English) locale of the display-name
    /// block, UTF-16BE decoded and NUL-trimmed. Empty when the field is
    /// all zero bytes.
    pub display_name: String,
    /// UTF-16BE decoded and NUL-trimmed. Empty when the field is all zero
    /// bytes.
    pub title_name: String,
}

fn read_be_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_be_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// Decodes `bytes` as UTF-16BE up to (not including) the first all-zero
/// code unit, or the end of `bytes` if none is found - never panics on an
/// odd trailing byte (it is simply not included in a final incomplete
/// pair).
fn decode_utf16be_trimmed(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Parses the fixed-position STFS metadata fields from `header`, which
/// must be at least [`STFS_HEADER_PEEK_BYTES`] long and begin with a
/// recognised magic. Fails closed (`None`) on a short buffer or
/// unrecognised magic - never a partial struct.
pub fn parse_stfs_header(header: &[u8]) -> Option<StfsHeaderFact> {
    let variant = detect_stfs_variant(header)?;
    if header.len() < STFS_HEADER_PEEK_BYTES {
        return None;
    }
    Some(StfsHeaderFact {
        variant,
        header_size: read_be_u32(header, HEADER_SIZE_OFFSET),
        content_type: read_be_u32(header, CONTENT_TYPE_OFFSET),
        metadata_version: read_be_u32(header, METADATA_VERSION_OFFSET),
        content_size: read_be_u64(header, CONTENT_SIZE_OFFSET),
        media_id: read_be_u32(header, MEDIA_ID_OFFSET),
        version: read_be_u32(header, VERSION_OFFSET),
        base_version: read_be_u32(header, BASE_VERSION_OFFSET),
        title_id: read_be_u32(header, TITLE_ID_OFFSET),
        platform: header[PLATFORM_OFFSET],
        executable_type: header[EXECUTABLE_TYPE_OFFSET],
        disc_number: header[DISC_NUMBER_OFFSET],
        disc_in_set: header[DISC_IN_SET_OFFSET],
        save_game_id: read_be_u32(header, SAVE_GAME_ID_OFFSET),
        display_name: decode_utf16be_trimmed(
            &header[DISPLAY_NAME_OFFSET..DISPLAY_NAME_OFFSET + DISPLAY_NAME_BYTES],
        ),
        title_name: decode_utf16be_trimmed(
            &header[TITLE_NAME_OFFSET..TITLE_NAME_OFFSET + TITLE_NAME_BYTES],
        ),
    })
}

/// Neutral evidence for a parsed STFS header:
///
/// - `Container` = `"STFS"` (`Strong`) - the magic matched and the fixed
///   metadata header parsed.
/// - `ContentSignature` = the signing variant's magic string (`Strong`) -
///   a signing-chain fact, never a content-class claim.
/// - `ProductCode` = the hex Title ID (`Corroborated`), only if nonzero -
///   a candidate identifier, not verified against a canonical release
///   list, and never proof this package is a playable game on its own
///   (see the module documentation's collision policy - STFS covers save
///   data, DLC, avatar items, and more, not just games).
pub fn observe_stfs_evidence(fact: &StfsHeaderFact) -> Vec<ContentEvidence> {
    let mut evidence = vec![
        ContentEvidence::new(
            ContentEvidenceKind::Container,
            "STFS",
            ContentEvidenceConfidence::Strong,
            "STFS package magic matched and the fixed metadata header parsed",
        ),
        ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            fact.variant.magic_str(),
            ContentEvidenceConfidence::Strong,
            "STFS package signing variant (CON=console-signed, LIVE/PIRS=Microsoft-signed) - \
             a signing-chain fact only, never proof of content class",
        ),
    ];
    if fact.title_id != 0 {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            format!("{:08X}", fact.title_id),
            ContentEvidenceConfidence::Corroborated,
            "candidate Title ID read from the STFS metadata header - not verified against a \
             canonical release list, and the package envelope alone never proves this is a \
             playable game (STFS also covers saved games, DLC, avatar items, and more)",
        ));
    }
    evidence
}

pub struct StfsDetector;

impl ContentDetector for StfsDetector {
    fn id(&self) -> &'static str {
        "xbox360_stfs_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_stfs(data) {
            return ContentDetectionOutcome::NotRecognized;
        }
        match parse_stfs_header(data) {
            Some(fact) => ContentDetectionOutcome::Recognized {
                evidence: observe_stfs_evidence(&fact),
            },
            None => ContentDetectionOutcome::Malformed {
                evidence: Vec::new(),
                diagnostic: crate::content_detector::ContentDiagnostic {
                    detector_id: "xbox360_stfs_header",
                    category: "truncated",
                    message: format!(
                        "STFS magic present but fewer than {STFS_HEADER_PEEK_BYTES} header bytes were supplied"
                    ),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(bytes: &mut [u8], offset: usize, value: &[u8]) {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    }

    fn encode_utf16be(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .flat_map(|unit| unit.to_be_bytes())
            .collect()
    }

    fn synthetic_header(magic: &[u8; 4]) -> Vec<u8> {
        let mut header = vec![0u8; STFS_HEADER_PEEK_BYTES];
        put(&mut header, 0, magic);
        put(&mut header, HEADER_SIZE_OFFSET, &0xAD0Eu32.to_be_bytes());
        put(
            &mut header,
            CONTENT_TYPE_OFFSET,
            &0x000D_0000u32.to_be_bytes(),
        );
        put(&mut header, METADATA_VERSION_OFFSET, &2u32.to_be_bytes());
        put(
            &mut header,
            CONTENT_SIZE_OFFSET,
            &2_405_105_664u64.to_be_bytes(),
        );
        put(&mut header, MEDIA_ID_OFFSET, &0x1E74_589Fu32.to_be_bytes());
        put(&mut header, VERSION_OFFSET, &1u32.to_be_bytes());
        put(&mut header, BASE_VERSION_OFFSET, &1u32.to_be_bytes());
        put(&mut header, TITLE_ID_OFFSET, &0x5841_1252u32.to_be_bytes());
        header[PLATFORM_OFFSET] = 2;
        header[EXECUTABLE_TYPE_OFFSET] = 0;
        header[DISC_NUMBER_OFFSET] = 1;
        header[DISC_IN_SET_OFFSET] = 1;
        put(&mut header, SAVE_GAME_ID_OFFSET, &0u32.to_be_bytes());
        let name = encode_utf16be("Double Dragon");
        put(&mut header, DISPLAY_NAME_OFFSET, &name);
        put(&mut header, TITLE_NAME_OFFSET, &name);
        header
    }

    // ------------------------------------------------------------------
    // Variant detection
    // ------------------------------------------------------------------

    #[test]
    fn con_magic_is_detected() {
        assert_eq!(
            detect_stfs_variant(STFS_MAGIC_CON.as_slice()),
            Some(StfsPackageVariant::Con)
        );
    }

    #[test]
    fn live_magic_is_detected() {
        assert_eq!(
            detect_stfs_variant(STFS_MAGIC_LIVE.as_slice()),
            Some(StfsPackageVariant::Live)
        );
    }

    #[test]
    fn pirs_magic_is_detected() {
        assert_eq!(
            detect_stfs_variant(STFS_MAGIC_PIRS.as_slice()),
            Some(StfsPackageVariant::Pirs)
        );
    }

    #[test]
    fn unrelated_magic_is_not_detected() {
        assert_eq!(detect_stfs_variant(b"ZZZZ"), None);
    }

    #[test]
    fn short_buffer_fails_closed_not_panic() {
        assert_eq!(detect_stfs_variant(b"CO"), None);
        assert_eq!(detect_stfs_variant(&[]), None);
    }

    #[test]
    fn looks_like_stfs_matches_all_three_variants() {
        assert!(looks_like_stfs(STFS_MAGIC_CON.as_slice()));
        assert!(looks_like_stfs(STFS_MAGIC_LIVE.as_slice()));
        assert!(looks_like_stfs(STFS_MAGIC_PIRS.as_slice()));
        assert!(!looks_like_stfs(b"ZZZZ"));
    }

    // ------------------------------------------------------------------
    // Header parsing - real-specimen-derived fixture
    // ------------------------------------------------------------------

    #[test]
    fn live_header_parses_every_field() {
        let header = synthetic_header(STFS_MAGIC_LIVE);
        let fact = parse_stfs_header(&header).unwrap();
        assert_eq!(fact.variant, StfsPackageVariant::Live);
        assert_eq!(fact.header_size, 0xAD0E);
        assert_eq!(fact.content_type, 0x000D_0000);
        assert_eq!(fact.metadata_version, 2);
        assert_eq!(fact.content_size, 2_405_105_664);
        assert_eq!(fact.media_id, 0x1E74_589F);
        assert_eq!(fact.title_id, 0x5841_1252);
        assert_eq!(fact.platform, 2);
        assert_eq!(fact.disc_number, 1);
        assert_eq!(fact.display_name, "Double Dragon");
        assert_eq!(fact.title_name, "Double Dragon");
    }

    #[test]
    fn con_header_parses() {
        let header = synthetic_header(STFS_MAGIC_CON);
        let fact = parse_stfs_header(&header).unwrap();
        assert_eq!(fact.variant, StfsPackageVariant::Con);
    }

    #[test]
    fn pirs_header_parses() {
        let header = synthetic_header(STFS_MAGIC_PIRS);
        let fact = parse_stfs_header(&header).unwrap();
        assert_eq!(fact.variant, StfsPackageVariant::Pirs);
    }

    #[test]
    fn unrecognised_magic_fails_closed() {
        let mut header = synthetic_header(STFS_MAGIC_LIVE);
        put(&mut header, 0, b"ZZZZ");
        assert_eq!(parse_stfs_header(&header), None);
    }

    #[test]
    fn truncated_header_fails_closed() {
        let header = synthetic_header(STFS_MAGIC_LIVE);
        assert_eq!(
            parse_stfs_header(&header[..STFS_HEADER_PEEK_BYTES - 1]),
            None
        );
    }

    #[test]
    fn magic_only_prefix_fails_closed_not_panic() {
        assert_eq!(parse_stfs_header(STFS_MAGIC_LIVE.as_slice()), None);
    }

    #[test]
    fn empty_display_name_decodes_to_empty_string() {
        let mut header = synthetic_header(STFS_MAGIC_LIVE);
        header[DISPLAY_NAME_OFFSET..DISPLAY_NAME_OFFSET + DISPLAY_NAME_BYTES].fill(0);
        let fact = parse_stfs_header(&header).unwrap();
        assert_eq!(fact.display_name, "");
    }

    #[test]
    fn zero_title_id_parses_but_is_not_promoted_to_evidence() {
        let mut header = synthetic_header(STFS_MAGIC_LIVE);
        put(&mut header, TITLE_ID_OFFSET, &0u32.to_be_bytes());
        let fact = parse_stfs_header(&header).unwrap();
        assert_eq!(fact.title_id, 0);
        assert!(
            !observe_stfs_evidence(&fact)
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode)
        );
    }

    #[test]
    fn utf16be_decode_stops_at_nul_unit() {
        let mut raw = encode_utf16be("Hi");
        raw.extend_from_slice(&[0, 0]);
        raw.extend_from_slice(&encode_utf16be("Ignored"));
        assert_eq!(decode_utf16be_trimmed(&raw), "Hi");
    }

    #[test]
    fn utf16be_decode_handles_odd_trailing_byte_without_panic() {
        let raw = [0x00u8, b'H', 0x00, b'i', 0xFF];
        assert_eq!(decode_utf16be_trimmed(&raw), "Hi");
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn evidence_includes_container_and_signature_facts() {
        let fact = parse_stfs_header(&synthetic_header(STFS_MAGIC_LIVE)).unwrap();
        let evidence = observe_stfs_evidence(&fact);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::Container && item.value == "STFS")
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature
                    && item.value == "LIVE")
        );
    }

    #[test]
    fn evidence_includes_hex_formatted_title_id() {
        let fact = parse_stfs_header(&synthetic_header(STFS_MAGIC_LIVE)).unwrap();
        let evidence = observe_stfs_evidence(&fact);
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "58411252");
        assert_eq!(product.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn evidence_never_claims_game_content_class() {
        let fact = parse_stfs_header(&synthetic_header(STFS_MAGIC_LIVE)).unwrap();
        for item in observe_stfs_evidence(&fact) {
            let lower = item.detail.to_lowercase();
            assert!(!lower.contains("this is a game"));
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::Container
                    | ContentEvidenceKind::ContentSignature
                    | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let fact = parse_stfs_header(&synthetic_header(STFS_MAGIC_LIVE)).unwrap();
        for item in observe_stfs_evidence(&fact) {
            let lower = item.value.to_lowercase();
            for platform in ["xbox 360", "playstation", "dreamcast"] {
                assert!(!lower.contains(platform));
            }
        }
    }

    #[test]
    fn evidence_never_mentions_svod_or_god_container_families() {
        // STFS and SVOD/GOD are distinct Xbox 360 digital-container
        // families (see the crate-level milestone report's Xbox digital
        // collision policy) - this module must never blur that line by
        // labelling its own evidence with the other family's name.
        let fact = parse_stfs_header(&synthetic_header(STFS_MAGIC_LIVE)).unwrap();
        for item in observe_stfs_evidence(&fact) {
            assert!(!item.value.contains("SVOD"));
            assert!(!item.value.contains("GOD"));
        }
    }

    // ------------------------------------------------------------------
    // Detector / collision safety
    // ------------------------------------------------------------------

    #[test]
    fn detector_recognizes_a_valid_header() {
        let header = synthetic_header(STFS_MAGIC_LIVE);
        assert!(StfsDetector.detect(&header).is_recognized());
    }

    #[test]
    fn detector_reports_not_recognized_for_unrelated_bytes() {
        assert_eq!(
            StfsDetector.detect(b"not an stfs package"),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn detector_reports_malformed_for_truncated_magic_match() {
        let outcome = StfsDetector.detect(STFS_MAGIC_CON.as_slice());
        assert!(outcome.is_malformed());
    }

    #[test]
    fn con_and_live_are_distinct_signing_variants_not_conflated() {
        let con = parse_stfs_header(&synthetic_header(STFS_MAGIC_CON)).unwrap();
        let live = parse_stfs_header(&synthetic_header(STFS_MAGIC_LIVE)).unwrap();
        assert_ne!(con.variant, live.variant);
        let con_evidence = observe_stfs_evidence(&con);
        let live_evidence = observe_stfs_evidence(&live);
        assert_ne!(
            con_evidence
                .iter()
                .find(|item| item.kind == ContentEvidenceKind::ContentSignature)
                .unwrap()
                .value,
            live_evidence
                .iter()
                .find(|item| item.kind == ContentEvidenceKind::ContentSignature)
                .unwrap()
                .value
        );
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = synthetic_header(STFS_MAGIC_LIVE);
        assert_eq!(parse_stfs_header(&header), parse_stfs_header(&header));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = synthetic_header(STFS_MAGIC_LIVE);
        let before = header.clone();
        let _ = parse_stfs_header(&header);
        assert_eq!(header, before);
    }
}

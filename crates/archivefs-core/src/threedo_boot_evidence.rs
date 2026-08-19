//! Pure, read-only 3DO ("Opera" filesystem) volume-header evidence.
//!
//! # Format verified, not assumed
//!
//! The Opera filesystem volume header layout is cross-checked against two
//! independent sources that agree on field order, sizes, and semantics:
//! `barbeque/3dodump`'s `OperaFS-Format.md`
//! (`https://github.com/barbeque/3dodump/blob/master/OperaFS-Format.md`,
//! byte-exact offset table) and `gameblabla/operafs`'s Linux kernel module
//! (`https://github.com/gameblabla/operafs/blob/master/main.c`, an
//! independent from-scratch implementation whose validation logic - magic
//! sync bytes, `record_type`, `record_version`, big-endian fields - matches
//! the same layout). The header sits at byte offset 0 of the volume (the
//! disc's first logical sector).
//!
//! ```text
//! Opera filesystem volume header (132 bytes, all multi-byte fields
//! big-endian):
//! [0]      record_type            u8    (always 1 on a valid disc)
//! [1..6]   sync_bytes             u8[5] (always 0x5A 0x5A 0x5A 0x5A 0x5A)
//! [6]      record_version         u8    (always 1 on a valid disc)
//! [7]      volume_flags           u8
//! [8..40]  volume_comment         ASCII, 32 bytes
//! [40..72] volume_label           ASCII, 32 bytes
//! [72..76] volume_id              u32
//! [76..80] block_size             u32   (power of two, minimum 256)
//! [80..84] block_count            u32
//! [84..88] root_directory_id      u32
//! [88..92] root_directory_blocks  u32   (root directory size, in blocks)
//! [92..96] root_block_size        u32   (block size used within the root directory)
//! ```
//!
//! No field in this header is a serial/catalog/product code - unlike
//! [`crate::saturn_boot_evidence`]'s `PRODUCT_NUMBER`, no independently
//! corroborated source found in this research pass identifies any Opera
//! volume-header field as a release identifier, so this module emits no
//! [`crate::content_evidence::ContentEvidenceKind::ProductCode`] fact -
//! `volume_label`/`volume_comment` are exposed as plain fields only, never
//! promoted to evidence.
//!
//! # Collision safety
//!
//! `record_type`/`sync_bytes`/`record_version` together are a specific,
//! multi-field structural match - not a single magic byte - so this is
//! treated as `Strong` `BootStructure` evidence, the same confidence level
//! [`crate::saturn_boot_evidence`] gives its own multi-field `SEGA
//! SEGASATURN` hardware ID match. Still never platform proof by itself:
//! this module has no field for, and makes no claim about, which canonical
//! platform a disc with a valid Opera header belongs to.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const OPERA_HEADER_BYTES: usize = 96;
const RECORD_TYPE_OFFSET: usize = 0;
const SYNC_BYTES_OFFSET: usize = 1;
const SYNC_BYTES_LEN: usize = 5;
const RECORD_VERSION_OFFSET: usize = 6;
const VOLUME_FLAGS_OFFSET: usize = 7;
const VOLUME_COMMENT_OFFSET: usize = 8;
const VOLUME_COMMENT_LEN: usize = 32;
const VOLUME_LABEL_OFFSET: usize = 40;
const VOLUME_LABEL_LEN: usize = 32;
const VOLUME_ID_OFFSET: usize = 72;
const BLOCK_SIZE_OFFSET: usize = 76;
const BLOCK_COUNT_OFFSET: usize = 80;
const ROOT_DIRECTORY_ID_OFFSET: usize = 84;
const ROOT_DIRECTORY_BLOCKS_OFFSET: usize = 88;
const ROOT_BLOCK_SIZE_OFFSET: usize = 92;

const EXPECTED_SYNC_BYTE: u8 = 0x5A;

/// What a parsed Opera volume header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperaVolumeHeaderFact {
    pub record_type: u8,
    /// Whether all five sync bytes matched [`EXPECTED_SYNC_BYTE`].
    pub sync_valid: bool,
    pub record_version: u8,
    pub volume_flags: u8,
    pub volume_comment: String,
    pub volume_label: String,
    pub volume_id: u32,
    pub block_size: u32,
    pub block_count: u32,
    pub root_directory_id: u32,
    pub root_directory_blocks: u32,
    pub root_block_size: u32,
}

impl OperaVolumeHeaderFact {
    /// Whether every one of `record_type`/sync bytes/`record_version`
    /// matches a valid Opera volume header - the multi-field structural
    /// match [`observe_threedo_evidence`] requires before emitting any
    /// evidence at all.
    pub fn header_is_valid(&self) -> bool {
        self.record_type == 1 && self.sync_valid && self.record_version == 1
    }
}

fn ascii_trimmed(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end])
        .trim_end()
        .to_string()
}

/// Parses an Opera volume header from `bytes` (must contain at least
/// [`OPERA_HEADER_BYTES`]). Returns `None` only when `bytes` is too short -
/// unlike most parsers in this crate, a *structurally* invalid header
/// (wrong sync bytes/record type) still parses successfully so
/// [`OperaVolumeHeaderFact::header_is_valid`] can report exactly what
/// failed, rather than collapsing "too short" and "wrong content" into the
/// same `None`.
pub fn parse_opera_volume_header(bytes: &[u8]) -> Option<OperaVolumeHeaderFact> {
    if bytes.len() < OPERA_HEADER_BYTES {
        return None;
    }
    let u32_at = |offset: usize| u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let sync_valid = bytes[SYNC_BYTES_OFFSET..SYNC_BYTES_OFFSET + SYNC_BYTES_LEN]
        .iter()
        .all(|&b| b == EXPECTED_SYNC_BYTE);

    Some(OperaVolumeHeaderFact {
        record_type: bytes[RECORD_TYPE_OFFSET],
        sync_valid,
        record_version: bytes[RECORD_VERSION_OFFSET],
        volume_flags: bytes[VOLUME_FLAGS_OFFSET],
        volume_comment: ascii_trimmed(
            &bytes[VOLUME_COMMENT_OFFSET..VOLUME_COMMENT_OFFSET + VOLUME_COMMENT_LEN],
        ),
        volume_label: ascii_trimmed(
            &bytes[VOLUME_LABEL_OFFSET..VOLUME_LABEL_OFFSET + VOLUME_LABEL_LEN],
        ),
        volume_id: u32_at(VOLUME_ID_OFFSET),
        block_size: u32_at(BLOCK_SIZE_OFFSET),
        block_count: u32_at(BLOCK_COUNT_OFFSET),
        root_directory_id: u32_at(ROOT_DIRECTORY_ID_OFFSET),
        root_directory_blocks: u32_at(ROOT_DIRECTORY_BLOCKS_OFFSET),
        root_block_size: u32_at(ROOT_BLOCK_SIZE_OFFSET),
    })
}

/// Neutral evidence: `Strong` `BootStructure` = `"OperaFS"`, only when
/// [`OperaVolumeHeaderFact::header_is_valid`] - a structurally invalid
/// header (this crate's collision-safety discipline: never guess) yields
/// no evidence at all, not a `Weak` one.
pub fn observe_threedo_evidence(fact: &OperaVolumeHeaderFact) -> Vec<ContentEvidence> {
    if !fact.header_is_valid() {
        return Vec::new();
    }
    vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        "OperaFS",
        ContentEvidenceConfidence::Strong,
        "Opera filesystem volume header validated (record_type=1, sync bytes 0x5A x5, record_version=1) - a real disc-structure fact, never platform proof on its own",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_header_bytes() -> Vec<u8> {
        let mut bytes = vec![0u8; OPERA_HEADER_BYTES];
        bytes[RECORD_TYPE_OFFSET] = 1;
        bytes[SYNC_BYTES_OFFSET..SYNC_BYTES_OFFSET + SYNC_BYTES_LEN].fill(EXPECTED_SYNC_BYTE);
        bytes[RECORD_VERSION_OFFSET] = 1;
        bytes[VOLUME_LABEL_OFFSET..VOLUME_LABEL_OFFSET + 11].copy_from_slice(b"SAMPLE DISC");
        bytes[BLOCK_SIZE_OFFSET..BLOCK_SIZE_OFFSET + 4].copy_from_slice(&2048u32.to_be_bytes());
        bytes[BLOCK_COUNT_OFFSET..BLOCK_COUNT_OFFSET + 4]
            .copy_from_slice(&327_680u32.to_be_bytes());
        bytes
    }

    #[test]
    fn valid_header_parses_and_validates() {
        let fact = parse_opera_volume_header(&valid_header_bytes()).unwrap();
        assert!(fact.header_is_valid());
        assert_eq!(fact.block_size, 2048);
        assert_eq!(fact.volume_label, "SAMPLE DISC");
    }

    #[test]
    fn wrong_sync_bytes_fails_validation_but_still_parses() {
        let mut bytes = valid_header_bytes();
        bytes[SYNC_BYTES_OFFSET] = 0x00;
        let fact = parse_opera_volume_header(&bytes).unwrap();
        assert!(!fact.sync_valid);
        assert!(!fact.header_is_valid());
    }

    #[test]
    fn wrong_record_type_fails_validation() {
        let mut bytes = valid_header_bytes();
        bytes[RECORD_TYPE_OFFSET] = 2;
        let fact = parse_opera_volume_header(&bytes).unwrap();
        assert!(!fact.header_is_valid());
    }

    #[test]
    fn wrong_record_version_fails_validation() {
        let mut bytes = valid_header_bytes();
        bytes[RECORD_VERSION_OFFSET] = 9;
        let fact = parse_opera_volume_header(&bytes).unwrap();
        assert!(!fact.header_is_valid());
    }

    #[test]
    fn truncated_bytes_fail_closed_not_panic() {
        assert_eq!(parse_opera_volume_header(&[0u8; 10]), None);
    }

    #[test]
    fn empty_bytes_fail_closed_not_panic() {
        assert_eq!(parse_opera_volume_header(&[]), None);
    }

    #[test]
    fn valid_header_yields_strong_boot_structure_evidence() {
        let fact = parse_opera_volume_header(&valid_header_bytes()).unwrap();
        let evidence = observe_threedo_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, ContentEvidenceKind::BootStructure);
        assert_eq!(evidence[0].value, "OperaFS");
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn invalid_header_yields_no_evidence() {
        let mut bytes = valid_header_bytes();
        bytes[RECORD_TYPE_OFFSET] = 0xFF;
        let fact = parse_opera_volume_header(&bytes).unwrap();
        assert!(observe_threedo_evidence(&fact).is_empty());
    }

    #[test]
    fn no_field_is_ever_promoted_to_product_code() {
        let fact = parse_opera_volume_header(&valid_header_bytes()).unwrap();
        for item in observe_threedo_evidence(&fact) {
            assert_ne!(item.kind, ContentEvidenceKind::ProductCode);
        }
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let fact = parse_opera_volume_header(&valid_header_bytes()).unwrap();
        for item in observe_threedo_evidence(&fact) {
            assert!(matches!(item.kind, ContentEvidenceKind::BootStructure));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let fact = parse_opera_volume_header(&valid_header_bytes()).unwrap();
        assert_eq!(
            observe_threedo_evidence(&fact),
            observe_threedo_evidence(&fact)
        );
    }

    #[test]
    fn volume_comment_is_extracted_and_nul_trimmed() {
        let mut bytes = valid_header_bytes();
        bytes[VOLUME_COMMENT_OFFSET..VOLUME_COMMENT_OFFSET + 4].copy_from_slice(b"TEST");
        let fact = parse_opera_volume_header(&bytes).unwrap();
        assert_eq!(fact.volume_comment, "TEST");
    }
}

//! Pure, read-only Xbox 360 boot evidence: XDVDFS volume signature (via
//! [`crate::xdvdfs_signature`], shared with [`crate::xbox_boot_evidence`])
//! and `default.xex`/XEX2 header (via [`crate::executable_signatures`]).
//!
//! # Collision safety
//!
//! - XDVDFS alone never distinguishes original Xbox from Xbox 360 - see
//!   [`crate::xbox_boot_evidence`]'s own notes.
//! - `default.xex` is a filename convention; only an actual XEX2 magic
//!   check (via [`crate::executable_signatures::looks_like_xex`]) is
//!   treated as a real format signature here.
//!
//! # `observe_xbox360_disc`: the wired-up, whole-image entry point
//!
//! Same shape as [`crate::xbox_boot_evidence::observe_xbox_disc`]: takes a
//! whole XDVDFS logical-image byte slice, checks the volume signature, and
//! - only if present - looks up `/default.xex` via
//! [`crate::xdvdfs_traversal::find_path`]/[`crate::xdvdfs_traversal::read_file_prefix`]
//! and parses its XEX2 header. [`XEX_PREFIX_READ_BYTES`] bounds the read;
//! never a whole-executable read.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::executable_signatures::{Xex2HeaderFact, looks_like_xex, parse_xex2_header};
use crate::xdvdfs_signature::{
    XDVDFS_VOLUME_DESCRIPTOR_OFFSET, XDVDFS_VOLUME_HEADER_MAGIC, looks_like_xdvdfs,
};
use crate::xdvdfs_traversal::{find_path, read_file_prefix};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Xbox360BootObservation {
    pub xdvdfs_signature_present: bool,
    pub default_xex_present: bool,
    pub xex2_header: Option<Xex2HeaderFact>,
}

/// Neutral evidence: XDVDFS (`Strong` `Filesystem`), `default.xex`
/// (`Corroborated` `BootStructure`), and a `ProductCode` fact for the
/// XEX2 `title_id` (`Corroborated`) when a header was actually parsed -
/// distinct from the `Strong` `ContentSignature` fact for the XEX2 magic
/// itself, which [`crate::executable_signatures::XexDetector`] already
/// covers and this module does not duplicate.
pub fn observe_xbox360_evidence(observation: &Xbox360BootObservation) -> Vec<ContentEvidence> {
    let mut evidence = Vec::new();
    if observation.xdvdfs_signature_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::Filesystem,
            "XDVDFS",
            ContentEvidenceConfidence::Strong,
            "XDVDFS volume descriptor magic present at logical sector 32 - shared with the original Xbox, never platform proof on its own",
        ));
    }
    if observation.default_xex_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "default.xex",
            ContentEvidenceConfidence::Corroborated,
            "default.xex exists on the filesystem - a naming convention, not a format signature by itself",
        ));
    }
    if let Some(header) = &observation.xex2_header {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            "XEX2",
            ContentEvidenceConfidence::Strong,
            "default.xex's own header begins with the XEX2 magic",
        ));
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            header.title_id.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate title ID read from the XEX2 execution-info header - not verified against a canonical release list",
        ));
    }
    evidence
}

pub fn check_xdvdfs_signature(sector: &[u8]) -> bool {
    looks_like_xdvdfs(sector)
}

pub fn check_xex2_magic(header: &[u8]) -> bool {
    looks_like_xex(header)
}

/// Bounded prefix read for `default.xex` - large enough for the fixed
/// header plus its optional-header table and execution-info block at any
/// reasonable offset, never a whole-executable read.
pub const XEX_PREFIX_READ_BYTES: usize = 8192;

/// Observes a whole XDVDFS logical image: the volume signature, and - only
/// if present - `default.xex`'s existence and XEX2 header via bounded
/// traversal. See the module documentation for the exact shape `bytes` is
/// expected to be in.
pub fn observe_xbox360_disc(bytes: &[u8]) -> Xbox360BootObservation {
    let mut observation = Xbox360BootObservation::default();

    let magic_len = XDVDFS_VOLUME_HEADER_MAGIC.len();
    observation.xdvdfs_signature_present = (bytes.len() as u64)
        >= XDVDFS_VOLUME_DESCRIPTOR_OFFSET + magic_len as u64
        && looks_like_xdvdfs(&bytes[XDVDFS_VOLUME_DESCRIPTOR_OFFSET as usize..]);
    if !observation.xdvdfs_signature_present {
        return observation;
    }

    let Ok(Some(entry)) = find_path(bytes, "default.xex") else {
        return observation;
    };
    if entry.is_directory {
        return observation;
    }
    observation.default_xex_present = true;

    let Ok(Some(prefix)) = read_file_prefix(bytes, "default.xex", XEX_PREFIX_READ_BYTES) else {
        return observation;
    };
    observation.xex2_header = parse_xex2_header(&prefix);
    observation
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> Xex2HeaderFact {
        Xex2HeaderFact {
            media_id: "41414141".to_string(),
            title_id: "42424242".to_string(),
        }
    }

    #[test]
    fn xdvdfs_signature_is_observed() {
        let observation = Xbox360BootObservation {
            xdvdfs_signature_present: true,
            ..Default::default()
        };
        let evidence = observe_xbox360_evidence(&observation);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::Filesystem && item.value == "XDVDFS")
        );
    }

    #[test]
    fn default_xex_presence_is_corroborated_not_strong() {
        let observation = Xbox360BootObservation {
            default_xex_present: true,
            ..Default::default()
        };
        let evidence = observe_xbox360_evidence(&observation);
        let item = evidence
            .iter()
            .find(|item| item.value == "default.xex")
            .unwrap();
        assert_eq!(item.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn xex2_header_yields_signature_and_product_code() {
        let observation = Xbox360BootObservation {
            xex2_header: Some(sample_header()),
            ..Default::default()
        };
        let evidence = observe_xbox360_evidence(&observation);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature
                    && item.value == "XEX2")
        );
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "42424242");
        assert_eq!(product.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn xex2_check_uses_shared_signature_module() {
        assert!(check_xex2_magic(b"XEX2"));
        assert!(!check_xex2_magic(b"not xex"));
    }

    #[test]
    fn default_xex_filename_alone_never_becomes_content_signature() {
        let observation = Xbox360BootObservation {
            default_xex_present: true,
            ..Default::default()
        };
        let evidence = observe_xbox360_evidence(&observation);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature)
        );
    }

    #[test]
    fn no_facts_yields_no_evidence() {
        assert!(observe_xbox360_evidence(&Xbox360BootObservation::default()).is_empty());
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let observation = Xbox360BootObservation {
            xdvdfs_signature_present: true,
            default_xex_present: true,
            xex2_header: Some(sample_header()),
        };
        for item in observe_xbox360_evidence(&observation) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::Filesystem
                    | ContentEvidenceKind::BootStructure
                    | ContentEvidenceKind::ContentSignature
                    | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let observation = Xbox360BootObservation {
            xdvdfs_signature_present: true,
            ..Default::default()
        };
        assert_eq!(
            observe_xbox360_evidence(&observation),
            observe_xbox360_evidence(&observation)
        );
    }

    // ------------------------------------------------------------------
    // observe_xbox360_disc: wired-up XDVDFS traversal
    // ------------------------------------------------------------------

    const XEX_OPT_HEADER_TABLE_OFFSET: usize = 0x18;
    const XEX_HEADER_COUNT_OFFSET: usize = 0x14;
    const XEX_OPT_HEADER_ENTRY_BYTES: usize = 8;
    const XEX_EXECUTION_INFO_KEY: u32 = 0x0004_0006;
    const XEX_EXECUTION_INFO_BYTES: usize = 0x18;

    fn synthetic_xex2_file(media_id: u32, title_id: u32) -> Vec<u8> {
        let mut data =
            vec![
                0u8;
                XEX_OPT_HEADER_TABLE_OFFSET + XEX_OPT_HEADER_ENTRY_BYTES + XEX_EXECUTION_INFO_BYTES
            ];
        data[0..4].copy_from_slice(b"XEX2");
        data[XEX_HEADER_COUNT_OFFSET..XEX_HEADER_COUNT_OFFSET + 4]
            .copy_from_slice(&1u32.to_be_bytes());
        let execution_info_offset =
            (XEX_OPT_HEADER_TABLE_OFFSET + XEX_OPT_HEADER_ENTRY_BYTES) as u32;
        data[XEX_OPT_HEADER_TABLE_OFFSET..XEX_OPT_HEADER_TABLE_OFFSET + 4]
            .copy_from_slice(&XEX_EXECUTION_INFO_KEY.to_be_bytes());
        data[XEX_OPT_HEADER_TABLE_OFFSET + 4..XEX_OPT_HEADER_TABLE_OFFSET + 8]
            .copy_from_slice(&execution_info_offset.to_be_bytes());
        let info_start = execution_info_offset as usize;
        data[info_start..info_start + 4].copy_from_slice(&media_id.to_be_bytes());
        data[info_start + 0xC..info_start + 0x10].copy_from_slice(&title_id.to_be_bytes());
        data
    }

    #[test]
    fn observe_xbox360_disc_finds_and_parses_default_xex() {
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "DEFAULT.XEX",
            &synthetic_xex2_file(0x4141_4141, 0x4242_4242),
        );
        let observation = observe_xbox360_disc(&image);
        assert!(observation.xdvdfs_signature_present);
        assert!(observation.default_xex_present);
        let header = observation.xex2_header.expect("xex2 header should parse");
        assert_eq!(header.title_id, "42424242");
        assert_eq!(header.media_id, "41414141");
    }

    #[test]
    fn observe_xbox360_disc_evidence_includes_product_code() {
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "DEFAULT.XEX",
            &synthetic_xex2_file(1, 0x99999999),
        );
        let observation = observe_xbox360_disc(&image);
        let evidence = observe_xbox360_evidence(&observation);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::Filesystem)
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature
                    && item.value == "XEX2")
        );
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "99999999");
    }

    #[test]
    fn observe_xbox360_disc_with_no_default_xex_still_reports_filesystem() {
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "SOMETHING.ELSE",
            b"not a xex",
        );
        let observation = observe_xbox360_disc(&image);
        assert!(observation.xdvdfs_signature_present);
        assert!(!observation.default_xex_present);
        assert_eq!(observation.xex2_header, None);
    }

    #[test]
    fn observe_xbox360_disc_fake_text_default_xex_is_not_treated_as_a_real_header() {
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "DEFAULT.XEX",
            b"this is just a text file, not a xex",
        );
        let observation = observe_xbox360_disc(&image);
        assert!(observation.default_xex_present);
        assert_eq!(observation.xex2_header, None);
        let evidence = observe_xbox360_evidence(&observation);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature)
        );
    }

    #[test]
    fn observe_xbox360_disc_on_a_non_xdvdfs_image_reports_nothing() {
        let observation = observe_xbox360_disc(b"not an xdvdfs image at all");
        assert_eq!(observation, Xbox360BootObservation::default());
    }

    #[test]
    fn xbox_and_xbox360_disc_observers_agree_on_xdvdfs_signature_for_the_same_image() {
        // Neither observer's XDVDFS check alone distinguishes the two
        // consoles - see the module documentation - so both must report the
        // same filesystem fact for the same image.
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "SOMETHING.ELSE",
            b"data",
        );
        let xbox = crate::xbox_boot_evidence::observe_xbox_disc(&image);
        let xbox360 = observe_xbox360_disc(&image);
        assert_eq!(
            xbox.xdvdfs_signature_present,
            xbox360.xdvdfs_signature_present
        );
    }
}

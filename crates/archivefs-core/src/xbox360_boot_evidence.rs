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

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::executable_signatures::{Xex2HeaderFact, looks_like_xex};
use crate::xdvdfs_signature::looks_like_xdvdfs;

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
}

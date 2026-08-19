//! Pure, read-only original Xbox boot evidence: XDVDFS volume signature
//! (via [`crate::xdvdfs_signature`]) and `default.xbe` header/certificate
//! (via [`crate::executable_signatures`]).
//!
//! # Collision safety
//!
//! - XDVDFS is shared between the original Xbox and Xbox 360 - its
//!   signature alone never distinguishes them; see
//!   [`crate::xbox360_boot_evidence`].
//! - `default.xbe` is a filename *convention*, not a format signature;
//!   this module only calls it evidence once the file's own header magic
//!   (`"XBEH"`) has actually been checked, never from the filename alone.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::executable_signatures::{XbeHeaderFact, looks_like_xbe};
use crate::xdvdfs_signature::looks_like_xdvdfs;

/// What was observed about an original-Xbox-style disc - never a platform
/// decision.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XboxBootObservation {
    pub xdvdfs_signature_present: bool,
    pub default_xbe_present: bool,
    pub xbe_header: Option<XbeHeaderFact>,
}

/// Neutral evidence: XDVDFS (`Strong` `Filesystem`), `default.xbe` (`Corroborated`
/// `BootStructure`, the filename-existence fact only), and XBE magic
/// (`Strong` `ContentSignature`, only when `xbe_header` was actually
/// parsed).
pub fn observe_xbox_evidence(observation: &XboxBootObservation) -> Vec<ContentEvidence> {
    let mut evidence = Vec::new();
    if observation.xdvdfs_signature_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::Filesystem,
            "XDVDFS",
            ContentEvidenceConfidence::Strong,
            "XDVDFS volume descriptor magic present at logical sector 32 - a real filesystem fact, shared with Xbox 360, never platform proof on its own",
        ));
    }
    if observation.default_xbe_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "default.xbe",
            ContentEvidenceConfidence::Corroborated,
            "default.xbe exists on the filesystem - a naming convention, not a format signature by itself",
        ));
    }
    if observation.xbe_header.is_some() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            "XBEH",
            ContentEvidenceConfidence::Strong,
            "default.xbe's own header begins with the XBEH magic",
        ));
    }
    evidence
}

/// Whether `sector` (bytes read at [`crate::xdvdfs_signature::XDVDFS_VOLUME_DESCRIPTOR_OFFSET`])
/// begins with the XDVDFS magic - re-exported here under the Xbox-specific
/// name for callers that only care about this one platform's evidence.
pub fn check_xdvdfs_signature(sector: &[u8]) -> bool {
    looks_like_xdvdfs(sector)
}

pub fn check_xbe_magic(header: &[u8]) -> bool {
    looks_like_xbe(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdvdfs_signature_is_observed() {
        let observation = XboxBootObservation {
            xdvdfs_signature_present: true,
            ..Default::default()
        };
        let evidence = observe_xbox_evidence(&observation);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::Filesystem && item.value == "XDVDFS")
        );
    }

    #[test]
    fn default_xbe_presence_is_corroborated_not_strong() {
        let observation = XboxBootObservation {
            default_xbe_present: true,
            ..Default::default()
        };
        let evidence = observe_xbox_evidence(&observation);
        let item = evidence
            .iter()
            .find(|item| item.value == "default.xbe")
            .unwrap();
        assert_eq!(item.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn xbe_header_check_uses_shared_signature_module() {
        assert!(check_xbe_magic(b"XBEH"));
        assert!(!check_xbe_magic(b"not xbe"));
    }

    #[test]
    fn xdvdfs_check_uses_shared_signature_module() {
        assert!(check_xdvdfs_signature(b"MICROSOFT*XBOX*MEDIA"));
        assert!(!check_xdvdfs_signature(b"not xdvdfs"));
    }

    #[test]
    fn no_facts_yields_no_evidence() {
        assert!(observe_xbox_evidence(&XboxBootObservation::default()).is_empty());
    }

    #[test]
    fn default_xbe_filename_alone_never_becomes_content_signature() {
        // Only default_xbe_present (filename-existence) is set - not
        // xbe_header - so no ContentSignature fact should appear, only the
        // weaker BootStructure filename fact.
        let observation = XboxBootObservation {
            default_xbe_present: true,
            ..Default::default()
        };
        let evidence = observe_xbox_evidence(&observation);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature)
        );
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let observation = XboxBootObservation {
            xdvdfs_signature_present: true,
            default_xbe_present: true,
            xbe_header: Some(XbeHeaderFact {
                title_id: None,
                title_name: None,
            }),
        };
        for item in observe_xbox_evidence(&observation) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::Filesystem
                    | ContentEvidenceKind::BootStructure
                    | ContentEvidenceKind::ContentSignature
            ));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let observation = XboxBootObservation {
            xdvdfs_signature_present: true,
            ..Default::default()
        };
        assert_eq!(
            observe_xbox_evidence(&observation),
            observe_xbox_evidence(&observation)
        );
    }
}

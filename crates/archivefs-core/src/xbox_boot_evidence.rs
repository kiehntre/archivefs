//! Pure, read-only original Xbox boot evidence: XDVDFS volume signature
//! (via [`crate::xdvdfs_signature`]) and `default.xbe` header/certificate
//! (via [`crate::executable_signatures`]), located through bounded XDVDFS
//! traversal (via [`crate::xdvdfs_traversal`]).
//!
//! # Collision safety
//!
//! - XDVDFS is shared between the original Xbox and Xbox 360 - its
//!   signature alone never distinguishes them; see
//!   [`crate::xbox360_boot_evidence`].
//! - `default.xbe` is a filename *convention*, not a format signature;
//!   this module only calls it evidence once the file's own header magic
//!   (`"XBEH"`) has actually been checked, never from the filename alone.
//!
//! # `observe_xbox_disc`: the wired-up, whole-image entry point
//!
//! [`observe_xbox_disc`] takes a whole logical-image byte slice (the same
//! shape [`crate::iso9660::observe_iso9660`] and
//! [`crate::dreamcast_boot_evidence`] already accept - a plain `.iso`/XISO
//! byte stream, sector 0 = start of the volume), checks the XDVDFS
//! signature, and - only if present - looks up `/default.xbe` via
//! [`crate::xdvdfs_traversal::find_path`] and reads a bounded prefix via
//! [`crate::xdvdfs_traversal::read_file_prefix`] to parse its header and
//! certificate. [`XBE_PREFIX_READ_BYTES`] is a generous but fixed bound,
//! never a whole-executable read.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::executable_signatures::{
    XBE_CERTIFICATE_READ_BYTES, XbeHeaderFact, looks_like_xbe, parse_xbe_header,
    xbe_certificate_file_offset,
};
use crate::xdvdfs_signature::{
    XDVDFS_VOLUME_DESCRIPTOR_OFFSET, XDVDFS_VOLUME_HEADER_MAGIC, looks_like_xdvdfs,
};
use crate::xdvdfs_traversal::{find_path, read_file_prefix};

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
    if let Some(header) = &observation.xbe_header {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            "XBEH",
            ContentEvidenceConfidence::Strong,
            "default.xbe's own header begins with the XBEH magic",
        ));
        if let Some(title_id) = &header.title_id {
            evidence.push(ContentEvidence::new(
                ContentEvidenceKind::ProductCode,
                title_id.clone(),
                ContentEvidenceConfidence::Corroborated,
                "candidate title ID read from the XBE certificate - not verified against a canonical release list",
            ));
        }
    }
    evidence
}

/// Bounded prefix read for `default.xbe` - large enough for the fixed
/// header plus a certificate at any reasonable offset within it, never a
/// whole-executable read.
pub const XBE_PREFIX_READ_BYTES: usize = 8192;

/// Observes a whole XDVDFS logical image: the volume signature, and - only
/// if present - `default.xbe`'s existence, header, and certificate via
/// bounded traversal. See the module documentation for the exact shape
/// `bytes` is expected to be in.
pub fn observe_xbox_disc(bytes: &[u8]) -> XboxBootObservation {
    let mut observation = XboxBootObservation::default();

    let magic_len = XDVDFS_VOLUME_HEADER_MAGIC.len();
    observation.xdvdfs_signature_present = (bytes.len() as u64)
        >= XDVDFS_VOLUME_DESCRIPTOR_OFFSET + magic_len as u64
        && looks_like_xdvdfs(&bytes[XDVDFS_VOLUME_DESCRIPTOR_OFFSET as usize..]);
    if !observation.xdvdfs_signature_present {
        return observation;
    }

    let Ok(Some(entry)) = find_path(bytes, "default.xbe") else {
        return observation;
    };
    if entry.is_directory {
        return observation;
    }
    observation.default_xbe_present = true;

    let Ok(Some(prefix)) = read_file_prefix(bytes, "default.xbe", XBE_PREFIX_READ_BYTES) else {
        return observation;
    };
    if !looks_like_xbe(&prefix) {
        return observation;
    }
    let certificate = xbe_certificate_file_offset(&prefix).and_then(|offset| {
        let start = usize::try_from(offset).ok()?;
        let end = start
            .checked_add(XBE_CERTIFICATE_READ_BYTES)?
            .min(prefix.len());
        prefix.get(start..end)
    });
    observation.xbe_header = parse_xbe_header(&prefix, certificate);
    observation
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
                    | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn title_id_yields_corroborated_product_code() {
        let observation = XboxBootObservation {
            xdvdfs_signature_present: true,
            default_xbe_present: true,
            xbe_header: Some(XbeHeaderFact {
                title_id: Some("4D5A0058".to_string()),
                title_name: None,
            }),
        };
        let evidence = observe_xbox_evidence(&observation);
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "4D5A0058");
        assert_eq!(product.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn no_title_id_yields_no_product_code() {
        let observation = XboxBootObservation {
            xdvdfs_signature_present: true,
            default_xbe_present: true,
            xbe_header: Some(XbeHeaderFact {
                title_id: None,
                title_name: None,
            }),
        };
        let evidence = observe_xbox_evidence(&observation);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode)
        );
    }

    // ------------------------------------------------------------------
    // observe_xbox_disc: wired-up XDVDFS traversal
    // ------------------------------------------------------------------

    const XBE_HEADER_PREFIX_BYTES: usize = 0x11C;
    const XBE_BASE_OFFSET: usize = 0x104;
    const XBE_CERT_ADDR_OFFSET: usize = 0x118;
    const XBE_CERT_TITLE_ID_OFFSET: usize = 0x8;

    /// A synthetic `default.xbe` file: header (with base/cert-addr fields
    /// pointing at a certificate placed right after the header) followed by
    /// a certificate encoding `title_id`.
    fn synthetic_xbe_file(title_id: u32) -> Vec<u8> {
        let base = 0x10000u32;
        let cert_file_offset = 0x200usize;
        let cert_addr = base + cert_file_offset as u32;

        let mut file = vec![0u8; cert_file_offset + XBE_CERTIFICATE_READ_BYTES];
        file[0..4].copy_from_slice(b"XBEH");
        file[XBE_BASE_OFFSET..XBE_BASE_OFFSET + 4].copy_from_slice(&base.to_le_bytes());
        file[XBE_CERT_ADDR_OFFSET..XBE_CERT_ADDR_OFFSET + 4]
            .copy_from_slice(&cert_addr.to_le_bytes());
        assert!(XBE_HEADER_PREFIX_BYTES <= cert_file_offset);

        let cert_start = cert_file_offset;
        file[cert_start + XBE_CERT_TITLE_ID_OFFSET..cert_start + XBE_CERT_TITLE_ID_OFFSET + 4]
            .copy_from_slice(&title_id.to_le_bytes());
        file
    }

    #[test]
    fn observe_xbox_disc_finds_and_parses_default_xbe() {
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "DEFAULT.XBE",
            &synthetic_xbe_file(0x4D5A0058),
        );
        let observation = observe_xbox_disc(&image);
        assert!(observation.xdvdfs_signature_present);
        assert!(observation.default_xbe_present);
        let header = observation.xbe_header.expect("xbe header should parse");
        assert_eq!(header.title_id.as_deref(), Some("4D5A0058"));
    }

    #[test]
    fn observe_xbox_disc_evidence_includes_product_code() {
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "DEFAULT.XBE",
            &synthetic_xbe_file(0x11223344),
        );
        let observation = observe_xbox_disc(&image);
        let evidence = observe_xbox_evidence(&observation);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::Filesystem)
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature
                    && item.value == "XBEH")
        );
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "11223344");
    }

    #[test]
    fn observe_xbox_disc_with_no_default_xbe_still_reports_filesystem() {
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "SOMETHING.ELSE",
            b"not an xbe",
        );
        let observation = observe_xbox_disc(&image);
        assert!(observation.xdvdfs_signature_present);
        assert!(!observation.default_xbe_present);
        assert_eq!(observation.xbe_header, None);
    }

    #[test]
    fn observe_xbox_disc_fake_text_default_xbe_is_not_treated_as_a_real_header() {
        // default.xbe exists as a filename, but its content is plain text,
        // not a real XBE header - the filename convention alone must never
        // produce a ContentSignature/XBEH fact.
        let image = crate::xdvdfs_traversal::test_support::synthetic_single_root_file_image(
            "DEFAULT.XBE",
            b"this is just a text file, not an xbe",
        );
        let observation = observe_xbox_disc(&image);
        assert!(observation.default_xbe_present);
        assert_eq!(observation.xbe_header, None);
        let evidence = observe_xbox_evidence(&observation);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature)
        );
    }

    #[test]
    fn observe_xbox_disc_on_a_non_xdvdfs_image_reports_nothing() {
        let observation = observe_xbox_disc(b"not an xdvdfs image at all");
        assert_eq!(observation, XboxBootObservation::default());
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

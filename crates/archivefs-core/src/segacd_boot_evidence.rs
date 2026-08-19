//! Pure, read-only Sega CD / Mega-CD boot-signature evidence extraction.
//!
//! # Deliberately narrow scope
//!
//! Unlike [`crate::saturn_boot_evidence`]/[`crate::dreamcast_boot_evidence`],
//! this module recognises **only** the `SEGADISCSYSTEM` boot identifier at
//! the start of the volume header - it does not extract product number,
//! version, or region. Sega's own official Mega-CD Disc Format
//! Specification PDF (referenced from `segaretro.org`) blocks automated
//! fetches, and no independently-corroborated primary source for the
//! CD-specific product/version/region field offsets was found during this
//! chunk's research (as distinct from the *cartridge* Mega Drive ROM
//! header at `$100`-`$1FF`, a different, well-documented structure that
//! some Sega CD boot sectors also embed, but at an offset this module has
//! not independently verified for the CD context specifically). Rather
//! than guess at those offsets, this module stops at the one fact that is
//! solidly, independently corroborated across multiple sources:
//!
//! - `https://www.retrodev.com/segacd.html`
//! - the SpritesMind.Net Mega-CD development forum
//!   (`http://gendev.spritesmind.net/forum/viewtopic.php?t=2996`)
//! - Clownacy's `clownmdemu` development blog on booting Sonic CD
//!
//! all of which independently describe the same fact: the Mega-CD volume
//! header begins at offset 0 with the ASCII string `"SEGADISCSYSTEM"`,
//! immediately followed by `"CDBOOTLOADR"`.
//!
//! # Collision safety
//!
//! `SEGADISCSYSTEM` is a Sega-specific boot marker, not a generic optical
//! disc convention - but "Mega-CD" and "Sega CD" are the same hardware
//! under two regional names, and this signature alone says nothing about
//! which regional branding applies; that distinction (where it matters at
//! all) belongs to a resolver, never this module.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const SEGA_CD_BOOT_SIGNATURE: &[u8; 14] = b"SEGADISCSYSTEM";

pub fn looks_like_sega_cd_boot_sector(bytes: &[u8]) -> bool {
    bytes.len() >= SEGA_CD_BOOT_SIGNATURE.len()
        && &bytes[..SEGA_CD_BOOT_SIGNATURE.len()] == SEGA_CD_BOOT_SIGNATURE.as_slice()
}

/// `Strong` `BootStructure` evidence when `bytes` begins with
/// [`SEGA_CD_BOOT_SIGNATURE`], otherwise no evidence at all.
pub fn observe_segacd_evidence(bytes: &[u8]) -> Vec<ContentEvidence> {
    if !looks_like_sega_cd_boot_sector(bytes) {
        return Vec::new();
    }
    vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        "SEGADISCSYSTEM",
        ContentEvidenceConfidence::Strong,
        "Sega CD/Mega-CD volume header boot identifier present at offset 0",
    )]
}

pub struct SegaCdBootDetector;

impl ContentDetector for SegaCdBootDetector {
    fn id(&self) -> &'static str {
        "segacd_boot_signature"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_sega_cd_boot_sector(data) {
            return ContentDetectionOutcome::NotRecognized;
        }
        ContentDetectionOutcome::Recognized {
            evidence: observe_segacd_evidence(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_signature_is_detected() {
        let mut data = SEGA_CD_BOOT_SIGNATURE.to_vec();
        data.extend_from_slice(b"CDBOOTLOADR");
        assert!(looks_like_sega_cd_boot_sector(&data));
        assert!(SegaCdBootDetector.detect(&data).is_recognized());
    }

    #[test]
    fn non_matching_bytes_are_not_recognized() {
        assert!(!looks_like_sega_cd_boot_sector(b"not a sega cd disc"));
        assert_eq!(
            SegaCdBootDetector.detect(b"not a sega cd disc"),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn truncated_signature_fails_closed() {
        assert!(!looks_like_sega_cd_boot_sector(b"SEGADISC"));
    }

    #[test]
    fn evidence_is_strong_boot_structure() {
        let evidence = observe_segacd_evidence(SEGA_CD_BOOT_SIGNATURE.as_slice());
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, ContentEvidenceKind::BootStructure);
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn no_signature_yields_no_evidence() {
        assert!(observe_segacd_evidence(b"random bytes here").is_empty());
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        for item in observe_segacd_evidence(SEGA_CD_BOOT_SIGNATURE.as_slice()) {
            assert_eq!(item.kind, ContentEvidenceKind::BootStructure);
        }
    }
}

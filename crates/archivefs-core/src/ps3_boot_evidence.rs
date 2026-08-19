//! Pure, read-only PS3 boot/layout evidence: `PS3_GAME/` layout,
//! `PARAM.SFO` (via [`crate::param_sfo`]), and a bounded SELF magic check
//! (via [`crate::executable_signatures`]) - never a decrypted/interpreted
//! executable body.
//!
//! # Collision safety
//!
//! - `PS3_GAME/`, `EBOOT.BIN`, and `PARAM.SFO` are shared Sony-ecosystem
//!   conventions - see [`crate::psp_boot_evidence`]'s own notes for the
//!   PSP side of this collision. `PS3_GAME/USRDIR/EBOOT.BIN` differs from
//!   PSP's `PSP_GAME/SYSDIR/EBOOT.BIN` only in the intermediate directory
//!   name, not in what it proves.
//! - `TITLE_ID` is the conventional PS3 product-code key, not proof of
//!   platform on its own.
//! - SELF is a container *format* signature (a signed wrapper around an
//!   ELF), not decrypted or parsed further here - see
//!   [`crate::executable_signatures::SelfDetector`].

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::executable_signatures::looks_like_self;
use crate::param_sfo::{SfoObservation, product_code_evidence};

pub const PS3_LAYOUT_PATHS: &[&str] = &[
    "PS3_GAME",
    "PS3_GAME/USRDIR",
    "PS3_GAME/USRDIR/EBOOT.BIN",
    "PS3_GAME/PARAM.SFO",
];

/// What was observed about a PS3-style disc layout - never a platform
/// decision.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ps3LayoutObservation {
    pub ps3_game_dir_present: bool,
    pub usrdir_present: bool,
    pub eboot_bin_present: bool,
    pub param_sfo: Option<SfoObservation>,
    /// Whether `PS3_GAME/USRDIR/EBOOT.BIN`'s header (when a caller
    /// supplied it) began with the SELF magic. `None` when no header was
    /// checked at all.
    pub eboot_self_magic_present: Option<bool>,
}

impl Ps3LayoutObservation {
    pub fn title_id(&self) -> Option<&str> {
        self.param_sfo.as_ref()?.get_text("TITLE_ID")
    }

    pub fn title(&self) -> Option<&str> {
        self.param_sfo.as_ref()?.get_text("TITLE")
    }

    pub fn category(&self) -> Option<&str> {
        self.param_sfo.as_ref()?.get_text("CATEGORY")
    }

    pub fn app_version(&self) -> Option<&str> {
        self.param_sfo.as_ref()?.get_text("APP_VER")
    }
}

/// Fills in [`Ps3LayoutObservation::eboot_self_magic_present`] from an
/// already-read, bounded executable header. Pure - performs no I/O itself.
pub fn check_eboot_self_magic(observation: &mut Ps3LayoutObservation, header: &[u8]) {
    observation.eboot_self_magic_present = Some(looks_like_self(header));
}

/// Neutral evidence for a [`Ps3LayoutObservation`] - `PS3_GAME/`
/// (`Corroborated` structural candidate), `TITLE_ID` (`Corroborated`
/// `ProductCode`), and SELF magic (`Strong` `ContentSignature`, reusing
/// [`crate::executable_signatures::SelfDetector`]'s own confidence - the
/// magic itself is an unambiguous format signature, even though the
/// platform it belongs to still is not).
pub fn observe_ps3_evidence(observation: &Ps3LayoutObservation) -> Vec<ContentEvidence> {
    let mut evidence = Vec::new();
    if observation.ps3_game_dir_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "PS3_GAME",
            ContentEvidenceConfidence::Corroborated,
            "PS3_GAME root directory present - a conventional layout marker, not unique proof of platform",
        ));
    }
    if let Some(sfo) = &observation.param_sfo
        && let Some(evidence_item) = product_code_evidence(sfo, "TITLE_ID")
    {
        evidence.push(evidence_item);
    }
    if observation.eboot_self_magic_present == Some(true) {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            "SELF",
            ContentEvidenceConfidence::Strong,
            "PS3 SELF magic present in PS3_GAME/USRDIR/EBOOT.BIN",
        ));
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::param_sfo::{SfoEntry, SfoValue};

    fn sfo_with(key: &str, value: &str) -> SfoObservation {
        SfoObservation {
            entries: vec![SfoEntry {
                key: key.to_string(),
                value: SfoValue::Text(value.to_string()),
            }],
        }
    }

    #[test]
    fn ps3_game_layout_is_observed() {
        let observation = Ps3LayoutObservation {
            ps3_game_dir_present: true,
            ..Default::default()
        };
        let evidence = observe_ps3_evidence(&observation);
        assert!(evidence.iter().any(
            |item| item.kind == ContentEvidenceKind::BootStructure && item.value == "PS3_GAME"
        ));
    }

    #[test]
    fn title_id_is_extracted() {
        let observation = Ps3LayoutObservation {
            param_sfo: Some(sfo_with("TITLE_ID", "BLUS30000")),
            ..Default::default()
        };
        assert_eq!(observation.title_id(), Some("BLUS30000"));
    }

    #[test]
    fn title_id_evidence_is_corroborated() {
        let observation = Ps3LayoutObservation {
            param_sfo: Some(sfo_with("TITLE_ID", "BLUS30000")),
            ..Default::default()
        };
        let evidence = observe_ps3_evidence(&observation);
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn self_magic_is_checked_from_header() {
        let mut observation = Ps3LayoutObservation::default();
        check_eboot_self_magic(&mut observation, &[0x53, 0x43, 0x45, 0x00]);
        assert_eq!(observation.eboot_self_magic_present, Some(true));
    }

    #[test]
    fn non_self_header_is_observed_as_false() {
        let mut observation = Ps3LayoutObservation::default();
        check_eboot_self_magic(&mut observation, b"not a self file");
        assert_eq!(observation.eboot_self_magic_present, Some(false));
    }

    #[test]
    fn self_magic_evidence_is_strong() {
        let mut observation = Ps3LayoutObservation::default();
        check_eboot_self_magic(&mut observation, &[0x53, 0x43, 0x45, 0x00]);
        let evidence = observe_ps3_evidence(&observation);
        let self_evidence = evidence.iter().find(|item| item.value == "SELF").unwrap();
        assert_eq!(self_evidence.confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn param_sfo_or_eboot_alone_does_not_assign_ps3() {
        // Structural: no platform field anywhere in Ps3LayoutObservation or
        // the evidence derived from it.
        let observation = Ps3LayoutObservation {
            eboot_bin_present: true,
            ..Default::default()
        };
        assert!(observe_ps3_evidence(&observation).is_empty());
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let mut observation = Ps3LayoutObservation {
            ps3_game_dir_present: true,
            param_sfo: Some(sfo_with("TITLE_ID", "BLUS30000")),
            ..Default::default()
        };
        check_eboot_self_magic(&mut observation, &[0x53, 0x43, 0x45, 0x00]);
        for item in observe_ps3_evidence(&observation) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure
                    | ContentEvidenceKind::ProductCode
                    | ContentEvidenceKind::ContentSignature
            ));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let observation = Ps3LayoutObservation {
            ps3_game_dir_present: true,
            ..Default::default()
        };
        assert_eq!(
            observe_ps3_evidence(&observation),
            observe_ps3_evidence(&observation)
        );
    }
}

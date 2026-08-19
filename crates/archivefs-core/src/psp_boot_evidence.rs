//! Pure, read-only PSP boot/layout evidence: `PSP_GAME/` layout and
//! `PARAM.SFO` (via [`crate::param_sfo`]).
//!
//! # Collision safety
//!
//! - `PSP_GAME/`, `EBOOT.BIN`, and `PARAM.SFO` are Sony-ecosystem
//!   conventions shared with PS3 (`PS3_GAME/`) and other PlayStation
//!   platforms - none of these paths or the SFO format itself are
//!   PSP-exclusive. See [`crate::ps3_boot_evidence`]'s own notes for the
//!   PS3 side of this collision.
//! - `DISC_ID` is the conventional PSP product-code key, but nothing
//!   prevents a non-PSP PARAM.SFO from also carrying it; this module never
//!   asserts platform from the key's presence alone.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::param_sfo::{SfoObservation, product_code_evidence};

/// Well-known PSP layout paths, existence-only facts - see
/// [`crate::iso9660::INTERESTING_ROOT_PATHS`] for the same convention
/// already established for PS1/PSP/PS3/Xbox paths.
pub const PSP_LAYOUT_PATHS: &[&str] = &[
    "PSP_GAME",
    "PSP_GAME/SYSDIR",
    "PSP_GAME/SYSDIR/EBOOT.BIN",
    "PSP_GAME/PARAM.SFO",
    "UMD_DATA.BIN",
];

/// What was observed about a PSP-style disc layout - never a platform
/// decision. Each field is independently `bool`/`Option`; a caller
/// populates only what it actually found on the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PspLayoutObservation {
    pub psp_game_dir_present: bool,
    pub sysdir_present: bool,
    pub eboot_bin_present: bool,
    /// `UMD_DATA.BIN`'s root-level presence - the Universal Media Disc
    /// identification file. UMD is the PSP's own physical disc format
    /// name (never used by PS3/Blu-ray or any other Sony optical
    /// convention in this crate) - see [`observe_psp_evidence`]'s own
    /// documentation for why this, not `PSP_GAME` alone, is this module's
    /// platform-specific strong leg (Batch 6).
    pub umd_data_bin_present: bool,
    pub param_sfo: Option<SfoObservation>,
}

impl PspLayoutObservation {
    /// `PARAM.SFO`'s `DISC_ID` value, if the file was present, parsed, and
    /// carried that key.
    pub fn disc_id(&self) -> Option<&str> {
        self.param_sfo.as_ref()?.get_text("DISC_ID")
    }

    pub fn title(&self) -> Option<&str> {
        self.param_sfo.as_ref()?.get_text("TITLE")
    }

    pub fn category(&self) -> Option<&str> {
        self.param_sfo.as_ref()?.get_text("CATEGORY")
    }

    pub fn disc_version(&self) -> Option<&str> {
        self.param_sfo.as_ref()?.get_text("DISC_VERSION")
    }
}

/// Neutral evidence for a [`PspLayoutObservation`].
///
/// `PSP_GAME/` existing alone is `Corroborated` structural-candidate
/// evidence (a real, conventional directory was found - but the
/// convention itself is not unique to any one platform, shared with PS3's
/// `PS3_GAME/`). `DISC_ID` (via [`crate::param_sfo::product_code_evidence`])
/// is `Corroborated` `ProductCode` evidence.
///
/// # The `UMD_DATA.BIN` strong leg (Batch 6)
///
/// `UMD_DATA.BIN`'s presence at the disc root is `Strong` `BootStructure`
/// evidence: UMD ("Universal Media Disc") is the PSP's own physical disc
/// format's name, and `UMD_DATA.BIN` is the medium-identification file
/// every real PSP UMD carries there - verified against the real God of
/// War: Ghost of Sparta UMD specimen this session. No other Sony optical
/// format this crate recognizes (PS1's `SYSTEM.CNF`, PS2's `SYSTEM.CNF`,
/// PS3's `PS3_GAME/`) uses this file or this name; PS3 discs are
/// Blu-ray, not UMD, so this is genuinely platform-specific rather than
/// merely "another conventional directory marker" the way `PSP_GAME/`
/// itself is. [`crate::content_evidence_scope`] scopes it
/// `PlatformSpecific("PSP")` accordingly.
///
/// `PSP_GAME/` alone (without `UMD_DATA.BIN`) still never resolves
/// anything on its own - it remains a candidate-only leg for callers that
/// could not check for `UMD_DATA.BIN` (a digital PSN-style dump missing
/// the physical-medium file entirely, for example).
pub fn observe_psp_evidence(observation: &PspLayoutObservation) -> Vec<ContentEvidence> {
    let mut evidence = Vec::new();
    if observation.psp_game_dir_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "PSP_GAME",
            ContentEvidenceConfidence::Corroborated,
            "PSP_GAME root directory present - a conventional layout marker, not unique proof of platform",
        ));
    }
    if observation.umd_data_bin_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "UMD_DATA.BIN",
            ContentEvidenceConfidence::Strong,
            "UMD_DATA.BIN medium-identification file present at the disc root - PSP-UMD-exclusive, no other Sony optical format in this crate uses this file",
        ));
    }
    if let Some(sfo) = &observation.param_sfo
        && let Some(evidence_item) = product_code_evidence(sfo, "DISC_ID")
    {
        evidence.push(evidence_item);
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
    fn psp_game_layout_is_observed() {
        let observation = PspLayoutObservation {
            psp_game_dir_present: true,
            sysdir_present: true,
            eboot_bin_present: true,
            param_sfo: None,
            ..Default::default()
        };
        let evidence = observe_psp_evidence(&observation);
        assert!(evidence.iter().any(
            |item| item.kind == ContentEvidenceKind::BootStructure && item.value == "PSP_GAME"
        ));
    }

    #[test]
    fn disc_id_is_extracted() {
        let observation = PspLayoutObservation {
            param_sfo: Some(sfo_with("DISC_ID", "ULUS10000")),
            ..Default::default()
        };
        assert_eq!(observation.disc_id(), Some("ULUS10000"));
    }

    #[test]
    fn disc_id_evidence_is_corroborated() {
        let observation = PspLayoutObservation {
            param_sfo: Some(sfo_with("DISC_ID", "ULUS10000")),
            ..Default::default()
        };
        let evidence = observe_psp_evidence(&observation);
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.confidence, ContentEvidenceConfidence::Corroborated);
        assert_eq!(product.value, "ULUS10000");
    }

    #[test]
    fn no_sfo_yields_no_product_code_evidence() {
        let observation = PspLayoutObservation::default();
        let evidence = observe_psp_evidence(&observation);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode)
        );
    }

    #[test]
    fn eboot_bin_alone_does_not_assign_psp() {
        // Structural: PspLayoutObservation has no platform field, and
        // eboot_bin_present alone never appears in observe_psp_evidence's
        // output at all - it is exposed for a probe/caller, not promoted.
        let observation = PspLayoutObservation {
            eboot_bin_present: true,
            ..Default::default()
        };
        let evidence = observe_psp_evidence(&observation);
        assert!(evidence.is_empty());
    }

    #[test]
    fn category_and_version_are_exposed() {
        let sfo = SfoObservation {
            entries: vec![
                SfoEntry {
                    key: "CATEGORY".to_string(),
                    value: SfoValue::Text("UG".to_string()),
                },
                SfoEntry {
                    key: "DISC_VERSION".to_string(),
                    value: SfoValue::Text("1.00".to_string()),
                },
            ],
        };
        let observation = PspLayoutObservation {
            param_sfo: Some(sfo),
            ..Default::default()
        };
        assert_eq!(observation.category(), Some("UG"));
        assert_eq!(observation.disc_version(), Some("1.00"));
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let observation = PspLayoutObservation {
            psp_game_dir_present: true,
            param_sfo: Some(sfo_with("DISC_ID", "ULUS10000")),
            ..Default::default()
        };
        for item in observe_psp_evidence(&observation) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let observation = PspLayoutObservation {
            psp_game_dir_present: true,
            ..Default::default()
        };
        assert_eq!(
            observe_psp_evidence(&observation),
            observe_psp_evidence(&observation)
        );
    }

    // ------------------------------------------------------------------
    // UMD_DATA.BIN strong leg (Batch 6) - see observe_psp_evidence's own
    // doc comment for the full justification.
    // ------------------------------------------------------------------

    #[test]
    fn umd_data_bin_present_yields_strong_evidence() {
        let observation = PspLayoutObservation {
            umd_data_bin_present: true,
            ..Default::default()
        };
        let evidence = observe_psp_evidence(&observation);
        let umd = evidence
            .iter()
            .find(|item| item.value == "UMD_DATA.BIN")
            .unwrap();
        assert_eq!(umd.confidence, ContentEvidenceConfidence::Strong);
        assert_eq!(umd.kind, ContentEvidenceKind::BootStructure);
    }

    #[test]
    fn umd_data_bin_absent_yields_no_umd_evidence() {
        let observation = PspLayoutObservation {
            psp_game_dir_present: true,
            umd_data_bin_present: false,
            ..Default::default()
        };
        let evidence = observe_psp_evidence(&observation);
        assert!(!evidence.iter().any(|item| item.value == "UMD_DATA.BIN"));
    }

    #[test]
    fn umd_data_bin_and_psp_game_both_appear_together() {
        let observation = PspLayoutObservation {
            psp_game_dir_present: true,
            umd_data_bin_present: true,
            ..Default::default()
        };
        let evidence = observe_psp_evidence(&observation);
        assert!(evidence.iter().any(|item| item.value == "PSP_GAME"));
        assert!(evidence.iter().any(|item| item.value == "UMD_DATA.BIN"));
    }

    #[test]
    fn umd_data_bin_alone_without_psp_game_still_yields_its_own_evidence() {
        // The observer itself makes no combination decision - that lives
        // entirely in platform_evidence_fusion::RULES, which does require
        // both facts together. This module just reports what it saw.
        let observation = PspLayoutObservation {
            umd_data_bin_present: true,
            psp_game_dir_present: false,
            ..Default::default()
        };
        let evidence = observe_psp_evidence(&observation);
        assert!(evidence.iter().any(|item| item.value == "UMD_DATA.BIN"));
        assert!(!evidence.iter().any(|item| item.value == "PSP_GAME"));
    }

    #[test]
    fn umd_data_bin_detail_names_it_as_psp_exclusive() {
        let observation = PspLayoutObservation {
            umd_data_bin_present: true,
            ..Default::default()
        };
        let evidence = observe_psp_evidence(&observation);
        let umd = evidence
            .iter()
            .find(|item| item.value == "UMD_DATA.BIN")
            .unwrap();
        assert!(umd.detail.contains("PSP-UMD-exclusive"));
    }

    #[test]
    fn umd_data_bin_evidence_is_deterministic() {
        let observation = PspLayoutObservation {
            psp_game_dir_present: true,
            umd_data_bin_present: true,
            ..Default::default()
        };
        assert_eq!(
            observe_psp_evidence(&observation),
            observe_psp_evidence(&observation)
        );
    }
}

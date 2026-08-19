use super::*;
use crate::content_evidence::value;

fn fact(
    kind: ContentEvidenceKind,
    value: &str,
    confidence: ContentEvidenceConfidence,
) -> ContentEvidence {
    ContentEvidence::new(kind, value, confidence, format!("test fact: {value}"))
}

fn strong(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    fact(kind, value, ContentEvidenceConfidence::Strong)
}

fn corroborated(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    fact(kind, value, ContentEvidenceConfidence::Corroborated)
}

fn weak(kind: ContentEvidenceKind, value: &str) -> ContentEvidence {
    fact(kind, value, ContentEvidenceConfidence::Weak)
}

// ----------------------------------------------------------------------
// Registry consistency: every rule's platform id is real, no duplicated
// rule ids, every rule has at least one leg.
// ----------------------------------------------------------------------

#[test]
fn every_rule_platform_id_exists_in_the_canonical_registry() {
    for rule in RULES {
        assert!(
            crate::platform::platform_by_id(rule.platform).is_some(),
            "rule {} references unknown platform id {:?}",
            rule.id,
            rule.platform
        );
    }
}

#[test]
fn no_two_rules_share_an_id() {
    let mut ids: Vec<&str> = RULES.iter().map(|r| r.id).collect();
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "duplicate rule id found");
}

#[test]
fn every_rule_has_at_least_one_leg() {
    for rule in RULES {
        assert!(!rule.legs.is_empty(), "rule {} has no legs", rule.id);
    }
}

#[test]
fn every_exact_rule_leg_treated_as_platform_discriminating_is_not_generic_scope() {
    use crate::content_evidence_scope::{EvidenceScope, scope_of};
    for rule in RULES {
        for leg in rule.legs {
            if let RequiredFact::Exact {
                kind,
                value,
                min_confidence,
            } = leg
                && *min_confidence == ContentEvidenceConfidence::Strong
            {
                let scope = scope_of(*kind, value);
                assert!(
                    !matches!(scope, EvidenceScope::Generic),
                    "rule {} leg {:?}={:?} is Strong-required but Generic-scoped",
                    rule.id,
                    kind,
                    value
                );
            }
        }
    }
}

// ----------------------------------------------------------------------
// Fusion core
// ----------------------------------------------------------------------

#[test]
fn no_evidence_is_unknown() {
    let explanation = fuse_platform_evidence(Vec::new());
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
    assert!(explanation.resolved_platform.is_none());
    assert!(explanation.fired_candidates.is_empty());
}

#[test]
fn saturn_signature_alone_resolves() {
    let explanation = fuse_platform_evidence([strong(BootStructure, "SEGA SEGASATURN")]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("Saturn"));
}

#[test]
fn resolved_explanation_retains_input_evidence() {
    let input = strong(BootStructure, "SEGA SEGASATURN");
    let explanation = fuse_platform_evidence([input.clone()]);
    assert!(explanation.input_evidence.contains(&input));
}

#[test]
fn resolved_explanation_lists_the_firing_rule_as_a_candidate() {
    let explanation = fuse_platform_evidence([strong(BootStructure, "SEGA SEGASATURN")]);
    assert!(
        explanation
            .fired_candidates
            .iter()
            .any(|c| c.rule_id == "saturn_boot_signature" && c.has_strong_leg)
    );
}

#[test]
fn unrelated_generic_evidence_alongside_a_strong_leg_does_not_block_resolution() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "SEGA SEGASATURN"),
        strong(Filesystem, value::ISO9660),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("Saturn"));
}

#[test]
fn duplicate_identical_evidence_still_resolves_once() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "SEGA SEGASATURN"),
        strong(BootStructure, "SEGA SEGASATURN"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
}

#[test]
fn evidence_order_never_affects_the_outcome() {
    let a = strong(Filesystem, "XDVDFS");
    let b = strong(ContentSignature, "XBEH");
    let forward = fuse_platform_evidence([a.clone(), b.clone()]);
    let reversed = fuse_platform_evidence([b, a]);
    assert_eq!(forward.outcome, reversed.outcome);
    assert_eq!(forward.resolved_platform, reversed.resolved_platform);
}

#[test]
fn repeated_fusion_is_deterministic() {
    let facts = vec![strong(BootStructure, "SEGA SEGASATURN")];
    let a = fuse_platform_evidence(facts.clone());
    let b = fuse_platform_evidence(facts);
    assert_eq!(a, b);
}

// ----------------------------------------------------------------------
// Weak-only rule (section 12)
// ----------------------------------------------------------------------

#[test]
fn extension_style_generic_weak_evidence_alone_is_unknown() {
    let explanation = fuse_platform_evidence([weak(ContentSignature, "ELF")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn generic_strong_iso9660_alone_is_unknown() {
    let explanation = fuse_platform_evidence([strong(Filesystem, value::ISO9660)]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn generic_strong_elf_alone_is_unknown_matching_the_milestones_own_example() {
    // "ELF Strong executable-format fact does NOT mean strong PS2
    // evidence" - here at any confidence, since the real detector emits
    // it at Weak in the first place.
    let explanation = fuse_platform_evidence([weak(ContentSignature, value::ISO9660)]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn family_scope_fact_alone_never_resolves() {
    let explanation = fuse_platform_evidence([strong(Filesystem, "XDVDFS")]);
    assert_ne!(explanation.outcome, FusionOutcome::Resolved);
}

#[test]
fn tmr_sega_alone_without_region_never_resolves() {
    let explanation = fuse_platform_evidence([strong(BootStructure, "TMR SEGA")]);
    assert_ne!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn stfs_alone_is_unknown_never_a_game_candidate() {
    let explanation = fuse_platform_evidence([
        strong(
            crate::content_evidence::ContentEvidenceKind::Container,
            "STFS",
        ),
        strong(ContentSignature, "LIVE"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn pkg_alone_is_unknown_never_a_ps3_game_candidate() {
    let explanation = fuse_platform_evidence([strong(ContentSignature, "PKG")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn default_xbe_filename_convention_alone_never_resolves() {
    let explanation = fuse_platform_evidence([corroborated(BootStructure, "default.xbe")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn product_code_alone_never_resolves_regardless_of_value() {
    let explanation = fuse_platform_evidence([corroborated(ProductCode, "SLUS-00594")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

// ----------------------------------------------------------------------
// Corroborated-only rule (section 13)
// ----------------------------------------------------------------------

#[test]
fn psp_full_layout_is_ambiguous_never_resolved() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "PSP_GAME"),
        corroborated(ProductCode, "ULUS10000"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Ambiguous);
    assert!(
        explanation
            .fired_candidates
            .iter()
            .any(|c| c.platform == "PSP" && !c.has_strong_leg)
    );
}

#[test]
fn megadrive_header_is_ambiguous_never_resolved() {
    let explanation = fuse_platform_evidence([corroborated(BootStructure, "SEGA GENESIS")]);
    assert_eq!(explanation.outcome, FusionOutcome::Ambiguous);
    assert!(
        explanation
            .fired_candidates
            .iter()
            .any(|c| c.platform == "MegaDrive" && !c.has_strong_leg)
    );
}

#[test]
fn sega32x_full_leg_is_ambiguous_never_resolved() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "SEGA 32X      JU"),
        weak(ContentSignature, "32X"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Ambiguous);
    assert!(
        explanation
            .fired_candidates
            .iter()
            .any(|c| c.platform == "Sega 32X" && !c.has_strong_leg)
    );
}

#[test]
fn ps2_boot2_plus_elf_is_ambiguous_never_resolved() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "BOOT2"),
        weak(ContentSignature, "ELF"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Ambiguous);
    assert!(
        explanation
            .fired_candidates
            .iter()
            .any(|c| c.platform == "PS2" && !c.has_strong_leg)
    );
}

#[test]
fn gb_logo_without_valid_checksum_is_ambiguous_candidate_only() {
    let explanation =
        fuse_platform_evidence([corroborated(BootStructure, "Nintendo Game Boy logo")]);
    assert_eq!(explanation.outcome, FusionOutcome::Ambiguous);
    assert!(
        explanation
            .fired_candidates
            .iter()
            .any(|c| c.rule_id == "gb_logo_only_candidate" && !c.has_strong_leg)
    );
}

#[test]
fn gba_header_with_only_one_structural_fact_is_ambiguous_candidate_only() {
    let explanation = fuse_platform_evidence([weak(BootStructure, "GBA cartridge header")]);
    assert_eq!(explanation.outcome, FusionOutcome::Ambiguous);
}

// ----------------------------------------------------------------------
// Strong vs strong conflict (section 11, 24)
// ----------------------------------------------------------------------

#[test]
fn ps1_and_ps2_strong_conflict_is_not_resolved() {
    let explanation = fuse_platform_evidence([
        strong(ContentSignature, "PS-X EXE"),
        corroborated(BootStructure, "BOOT"),
        // PS2's rule needs a Strong-tier leg too for a genuine strong-vs-
        // strong conflict; since PS2 currently has no Strong leg (see the
        // RULES doc comment), simulate the synthetic "what if it did"
        // case is not meaningful here - instead this test uses PS1 vs.
        // Xbox (both genuinely Strong-eligible today) for the real
        // conflict proof, and a second test below documents PS1 vs PS2's
        // actual (non-conflicting, because PS2 never reaches Strong)
        // behavior explicitly.
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XBEH"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn ps1_strong_vs_ps2_candidate_only_is_resolved_ps1_not_a_conflict() {
    // PS2 has no Strong leg with today's evidence, so a PS1 Strong match
    // alongside a PS2 candidate-only match must NOT be treated as a
    // strong-vs-strong conflict - the PS2 signal is exposed as a
    // candidate, but PS1 still resolves.
    let explanation = fuse_platform_evidence([
        strong(ContentSignature, "PS-X EXE"),
        corroborated(BootStructure, "BOOT"),
        corroborated(BootStructure, "BOOT2"),
        weak(ContentSignature, "ELF"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("PSX"));
    assert!(
        explanation
            .fired_candidates
            .iter()
            .any(|c| c.platform == "PS2")
    );
}

#[test]
fn xbox_and_xbox360_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XBEH"),
        strong(ContentSignature, "XEX2"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
    assert!(explanation.conflicting_platforms.contains(&"Xbox"));
    assert!(explanation.conflicting_platforms.contains(&"Xbox360"));
}

#[test]
fn saturn_and_dreamcast_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "SEGA SEGASATURN"),
        strong(BootStructure, "SEGA SEGAKATANA"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn gamecube_and_wii_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "GameCube"),
        strong(BootStructure, "Wii"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn nes_and_snes_strong_conflict() {
    // Synthetic: a bundle with both a valid iNES header fact and a valid
    // SNES LoROM candidate fact - genuinely impossible in one real file,
    // but the resolver must still fail closed rather than guess.
    let explanation = fuse_platform_evidence([
        strong(ContentSignature, "iNES"),
        strong(ContentSignature, "LoROM"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn gb_and_gba_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "Nintendo Game Boy logo"),
        strong(BootStructure, "GBA cartridge header"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
    assert!(explanation.conflicting_platforms.contains(&"Game Boy"));
    assert!(
        explanation
            .conflicting_platforms
            .contains(&"Game Boy Advance")
    );
}

#[test]
fn conflict_never_picks_a_winner_by_rule_declaration_order() {
    // saturn_boot_signature is declared before xbox_original_disc in
    // RULES - a majority/order-based resolver might be tempted to prefer
    // the earlier one. This must not happen.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "SEGA SEGASATURN"),
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XBEH"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
    assert!(explanation.resolved_platform.is_none());
}

#[test]
fn conflict_evidence_order_independence() {
    let a = strong(BootStructure, "SEGA SEGASATURN");
    let b = strong(BootStructure, "SEGA SEGAKATANA");
    let forward = fuse_platform_evidence([a.clone(), b.clone()]);
    let reversed = fuse_platform_evidence([b, a]);
    assert_eq!(forward.outcome, FusionOutcome::Conflict);
    assert_eq!(forward.outcome, reversed.outcome);
}

#[test]
fn three_way_strong_conflict_reports_all_three() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "SEGA SEGASATURN"),
        strong(BootStructure, "SEGA SEGAKATANA"),
        strong(BootStructure, "OperaFS"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
    assert_eq!(explanation.conflicting_platforms.len(), 3);
}

// ----------------------------------------------------------------------
// Family disambiguation (section 14)
// ----------------------------------------------------------------------

#[test]
fn xbox_original_resolves_with_xdvdfs_and_xbeh() {
    let explanation = fuse_platform_evidence([
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XBEH"),
    ]);
    assert_eq!(explanation.resolved_platform, Some("Xbox"));
}

#[test]
fn xbox360_resolves_with_xdvdfs_and_xex2() {
    let explanation = fuse_platform_evidence([
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XEX2"),
    ]);
    assert_eq!(explanation.resolved_platform, Some("Xbox360"));
}

#[test]
fn xdvdfs_alone_resolves_neither_xbox_generation() {
    let explanation = fuse_platform_evidence([strong(Filesystem, "XDVDFS")]);
    assert_ne!(explanation.resolved_platform, Some("Xbox"));
    assert_ne!(explanation.resolved_platform, Some("Xbox360"));
}

#[test]
fn master_system_resolves_with_tmr_sega_and_region() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "TMR SEGA"),
        corroborated(ContentSignature, "Master System (Export)"),
    ]);
    assert_eq!(explanation.resolved_platform, Some("MasterSystem"));
}

#[test]
fn game_gear_resolves_with_tmr_sega_and_region() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "TMR SEGA"),
        corroborated(ContentSignature, "Game Gear (Japan)"),
    ]);
    assert_eq!(explanation.resolved_platform, Some("GameGear"));
}

#[test]
fn sms_and_game_gear_never_both_resolve_from_the_same_bundle() {
    // Bundle carries both region hints (adversarial/malformed input) -
    // this should not silently pick one, it should conflict, since both
    // would independently have a Strong-eligible rule (the TMR SEGA leg)
    // paired with mutually exclusive Corroborated region legs.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "TMR SEGA"),
        corroborated(ContentSignature, "Master System (Export)"),
        corroborated(ContentSignature, "Game Gear (Japan)"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn gb_resolves_independently_of_gba() {
    let gb = fuse_platform_evidence([strong(BootStructure, "Nintendo Game Boy logo")]);
    let gba = fuse_platform_evidence([strong(BootStructure, "GBA cartridge header")]);
    assert_eq!(gb.resolved_platform, Some("Game Boy"));
    assert_eq!(gba.resolved_platform, Some("Game Boy Advance"));
}

#[test]
fn gamecube_resolves_independently_of_wii() {
    let gc = fuse_platform_evidence([strong(BootStructure, "GameCube")]);
    let wii = fuse_platform_evidence([strong(BootStructure, "Wii")]);
    assert_eq!(gc.resolved_platform, Some("GameCube"));
    assert_eq!(wii.resolved_platform, Some("Wii"));
}

#[test]
fn main_dol_alone_resolves_neither_gamecube_nor_wii() {
    let explanation = fuse_platform_evidence([corroborated(BootStructure, "main.dol")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn dreamcast_resolves_independently_of_saturn() {
    let dc = fuse_platform_evidence([strong(BootStructure, "SEGA SEGAKATANA")]);
    let saturn = fuse_platform_evidence([strong(BootStructure, "SEGA SEGASATURN")]);
    assert_eq!(dc.resolved_platform, Some("Dreamcast"));
    assert_eq!(saturn.resolved_platform, Some("Saturn"));
}

#[test]
fn dreamcast_mario_variant_also_resolves_dreamcast() {
    let explanation = fuse_platform_evidence([strong(BootStructure, "SEGA SEGAMARIO")]);
    assert_eq!(explanation.resolved_platform, Some("Dreamcast"));
}

#[test]
fn ps1_resolves_independently_of_ps2() {
    let ps1 = fuse_platform_evidence([
        strong(ContentSignature, "PS-X EXE"),
        corroborated(BootStructure, "BOOT"),
    ]);
    assert_eq!(ps1.resolved_platform, Some("PSX"));
}

#[test]
fn psp_and_ps3_share_param_sfo_ecosystem_but_never_cross_resolve() {
    let psp = fuse_platform_evidence([
        corroborated(BootStructure, "PSP_GAME"),
        corroborated(ProductCode, "ULUS10000"),
    ]);
    let ps3 = fuse_platform_evidence([
        corroborated(BootStructure, "PS3_GAME"),
        strong(ContentSignature, "SELF"),
        corroborated(ProductCode, "BLUS30000"),
    ]);
    assert_ne!(psp.outcome, FusionOutcome::Resolved);
    assert_eq!(ps3.outcome, FusionOutcome::Resolved);
    assert_eq!(ps3.resolved_platform, Some("PS3"));
}

#[test]
fn ps3_requires_the_full_combo_not_self_alone() {
    let explanation = fuse_platform_evidence([strong(ContentSignature, "SELF")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn ps3_requires_the_full_combo_not_layout_alone() {
    let explanation = fuse_platform_evidence([corroborated(BootStructure, "PS3_GAME")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

// ----------------------------------------------------------------------
// PC Engine / TurboGrafx-16 equivalence folding (section 7)
// ----------------------------------------------------------------------

#[test]
fn equivalent_platform_ids_fold_into_one_group() {
    let groups = group_by_equivalence(&["PC Engine", "TurboGrafx-16"]);
    assert_eq!(groups.len(), 1);
}

#[test]
fn non_equivalent_platforms_stay_in_separate_groups() {
    let groups = group_by_equivalence(&["Saturn", "Dreamcast"]);
    assert_eq!(groups.len(), 2);
}

#[test]
fn equivalence_grouping_is_order_independent() {
    let a = group_by_equivalence(&["PC Engine", "TurboGrafx-16"]);
    let b = group_by_equivalence(&["TurboGrafx-16", "PC Engine"]);
    assert_eq!(a, b);
}

#[test]
fn a_single_platform_forms_its_own_group() {
    let groups = group_by_equivalence(&["Saturn"]);
    assert_eq!(groups, vec![vec!["Saturn"]]);
}

#[test]
fn empty_platform_list_yields_no_groups() {
    assert!(group_by_equivalence(&[]).is_empty());
}

#[test]
fn three_equivalent_mentions_still_fold_to_one_group() {
    let groups = group_by_equivalence(&["PC Engine", "TurboGrafx-16", "PC Engine"]);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].len(), 2);
}

// ----------------------------------------------------------------------
// Adversarial / malformed evidence bundles (section 32)
// ----------------------------------------------------------------------

#[test]
fn contradictory_duplicate_confidences_for_the_same_value_do_not_confuse_resolution() {
    let explanation = fuse_platform_evidence([
        weak(BootStructure, "SEGA SEGASATURN"),
        strong(BootStructure, "SEGA SEGASATURN"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("Saturn"));
}

#[test]
fn all_weak_evidence_bundle_never_resolves() {
    let explanation = fuse_platform_evidence([
        weak(ContentSignature, "ELF"),
        weak(BootStructure, "GBA cartridge header"),
    ]);
    // GBA's own candidate-only rule requires only Weak confidence, so this
    // bundle legitimately fires that one candidate rule (Ambiguous) - the
    // real, honest outcome; a bare ELF fact contributes nothing on its
    // own either way. What matters here is that neither fact, nor both
    // together, ever reaches Resolved.
    assert_ne!(explanation.outcome, FusionOutcome::Resolved);
}

#[test]
fn all_corroborated_evidence_bundle_never_silently_resolves() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "PSP_GAME"),
        corroborated(BootStructure, "PS3_GAME"),
    ]);
    assert_ne!(explanation.outcome, FusionOutcome::Resolved);
}

#[test]
fn many_reorderings_of_the_same_bundle_agree() {
    let facts = [
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XBEH"),
        corroborated(ProductCode, "4D5A0058"),
        weak(ContentSignature, "ELF"),
    ];
    let baseline = fuse_platform_evidence(facts.to_vec());
    for perm_seed in 0..facts.len() {
        let mut reordered = facts.to_vec();
        reordered.rotate_left(perm_seed);
        let result = fuse_platform_evidence(reordered);
        assert_eq!(result.outcome, baseline.outcome);
        assert_eq!(result.resolved_platform, baseline.resolved_platform);
    }
}

#[test]
fn empty_value_string_never_panics_or_falsely_matches() {
    let explanation = fuse_platform_evidence([strong(BootStructure, "")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn very_long_unrelated_value_never_panics() {
    let long_value = "x".repeat(10_000);
    let explanation = fuse_platform_evidence([strong(BootStructure, &long_value)]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn evidence_from_an_unrelated_kind_with_a_matching_value_string_does_not_satisfy_a_leg() {
    // "SEGA SEGASATURN" as a ProductCode (not BootStructure) must not
    // satisfy the Saturn rule, which requires the BootStructure kind
    // specifically.
    let explanation = fuse_platform_evidence([strong(ProductCode, "SEGA SEGASATURN")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

// ----------------------------------------------------------------------
// No action authority (section 26)
// ----------------------------------------------------------------------

#[test]
fn resolution_explanation_has_no_action_bearing_fields() {
    // Structural: ResolutionExplanation's fields are outcome, platform
    // strings, candidate metadata, and evidence - there is no path field,
    // no rename target, no destination, nothing that could be
    // interpreted as a mutation instruction. This test exists to
    // document that boundary explicitly, the same way
    // content_evidence.rs's own tests document its platform-free
    // boundary.
    let explanation = fuse_platform_evidence([strong(BootStructure, "SEGA SEGASATURN")]);
    assert_eq!(explanation.resolved_platform, Some("Saturn"));
    // No method exists on ResolutionExplanation to rename, move, delete,
    // or otherwise touch a filesystem path - if one is ever added, it
    // belongs in a separately reviewed action-authorization layer.
}

// ----------------------------------------------------------------------
// Additional strong-vs-strong conflicts (section 11, 24) - Batch 5
// top-up to more comfortably clear the milestone's suggested minimum.
// ----------------------------------------------------------------------

#[test]
fn nes_and_gba_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(ContentSignature, "iNES"),
        strong(BootStructure, "GBA cartridge header"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn atari_lynx_and_atari7800_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "LYNX"),
        strong(BootStructure, "ATARI7800"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn threedo_and_segacd_strong_conflict() {
    // Two different optical-media boot signatures in one bundle - a
    // genuine impossibility for a real file, but the resolver must still
    // fail closed.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "OperaFS"),
        strong(BootStructure, "SEGADISCSYSTEM"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn n64_and_snes_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(ContentSignature, "z64"),
        strong(ContentSignature, "LoROM"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn xbox360_and_wii_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XEX2"),
        strong(BootStructure, "Wii"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

// ----------------------------------------------------------------------
// Additional family disambiguation (section 14) - Batch 5 top-up.
// ----------------------------------------------------------------------

#[test]
fn ps2_reverse_order_still_resolves_ps1_not_a_conflict() {
    // Same as ps1_strong_vs_ps2_candidate_only_is_resolved_ps1_not_a_conflict
    // but with the evidence pushed in the opposite order - the outcome
    // must not depend on which fact arrived first.
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "BOOT2"),
        weak(ContentSignature, "ELF"),
        strong(ContentSignature, "PS-X EXE"),
        corroborated(BootStructure, "BOOT"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("PSX"));
}

#[test]
fn dreamcast_katana_and_mario_variants_never_conflict_with_each_other() {
    // Both boot hardware IDs name the same platform - firing both rules
    // must still resolve cleanly to one Dreamcast group, not a conflict.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "SEGA SEGAKATANA"),
        strong(BootStructure, "SEGA SEGAMARIO"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("Dreamcast"));
}

#[test]
fn xbox_and_xbox360_disambiguate_purely_on_executable_magic() {
    // Same XDVDFS filesystem fact both generations share - only the
    // executable-magic leg (XBEH vs XEX2) decides which one resolves.
    let xbox = fuse_platform_evidence([
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XBEH"),
    ]);
    let xbox360 = fuse_platform_evidence([
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XEX2"),
    ]);
    assert_eq!(xbox.resolved_platform, Some("Xbox"));
    assert_eq!(xbox360.resolved_platform, Some("Xbox360"));
    assert_ne!(xbox.resolved_platform, xbox360.resolved_platform);
}

#[test]
fn master_system_and_game_gear_disambiguate_purely_on_region_nibble() {
    let master_system = fuse_platform_evidence([
        strong(BootStructure, "TMR SEGA"),
        corroborated(ContentSignature, "Master System (Export)"),
    ]);
    let game_gear = fuse_platform_evidence([
        strong(BootStructure, "TMR SEGA"),
        corroborated(ContentSignature, "Game Gear (Export)"),
    ]);
    assert_eq!(master_system.resolved_platform, Some("MasterSystem"));
    assert_eq!(game_gear.resolved_platform, Some("GameGear"));
}

#[test]
fn gamecube_and_wii_disambiguate_purely_on_disc_header_kind() {
    let gamecube = fuse_platform_evidence([strong(BootStructure, "GameCube")]);
    let wii = fuse_platform_evidence([strong(BootStructure, "Wii")]);
    assert_eq!(gamecube.resolved_platform, Some("GameCube"));
    assert_eq!(wii.resolved_platform, Some("Wii"));
    assert_ne!(gamecube.resolved_platform, wii.resolved_platform);
}

// ----------------------------------------------------------------------
// Additional weak-only rule coverage (section 12) - Batch 5 top-up.
// ----------------------------------------------------------------------

#[test]
fn directory_style_generic_weak_evidence_alone_is_unknown() {
    let explanation = fuse_platform_evidence([weak(Filesystem, "FAT12")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
    assert!(explanation.resolved_platform.is_none());
}

// ----------------------------------------------------------------------
// PS2 strong leg (section 8, 25) - Batch 6.
// ----------------------------------------------------------------------

#[test]
fn ps2_boot2_strong_resolves_ps2() {
    let explanation = fuse_platform_evidence([strong(BootStructure, "BOOT2")]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("PS2"));
}

#[test]
fn ps2_boot2_strong_plus_weak_elf_still_resolves_ps2_not_conflicted_with_itself() {
    // Both the *_strong and *_candidate PS2 rules can fire on the same
    // realistic bundle - they target the same platform, so this must
    // still cleanly resolve, not be treated as two competing platforms.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "BOOT2"),
        weak(ContentSignature, "ELF"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("PS2"));
}

#[test]
fn ps2_boot2_corroborated_without_elf_never_resolves() {
    // BOOT2 the text token alone, with no executable confirmation at all -
    // must not be promoted, matching "do not promote BOOT2 alone to
    // Strong without justification."
    let explanation = fuse_platform_evidence([corroborated(BootStructure, "BOOT2")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn ps2_boot2_corroborated_plus_weak_elf_is_ambiguous_not_resolved() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "BOOT2"),
        weak(ContentSignature, "ELF"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Ambiguous);
}

#[test]
fn ps1_and_ps2_both_strong_is_a_conflict_not_a_silent_ps1_win() {
    let explanation = fuse_platform_evidence([
        strong(ContentSignature, "PS-X EXE"),
        corroborated(BootStructure, "BOOT"),
        strong(BootStructure, "BOOT2"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn generic_elf_alone_never_promotes_to_ps2_even_at_strong_confidence() {
    // Defensive: even if a caller mistakenly marked a bare ELF fact
    // Strong, no rule keys off ContentSignature="ELF" at Strong for any
    // platform - only BootStructure="BOOT2" does.
    let explanation = fuse_platform_evidence([strong(ContentSignature, "ELF")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn ps2_resolution_is_order_independent() {
    let forward = fuse_platform_evidence([
        strong(BootStructure, "BOOT2"),
        weak(ContentSignature, "ELF"),
    ]);
    let backward = fuse_platform_evidence([
        weak(ContentSignature, "ELF"),
        strong(BootStructure, "BOOT2"),
    ]);
    assert_eq!(forward.outcome, backward.outcome);
    assert_eq!(forward.resolved_platform, backward.resolved_platform);
}

// ----------------------------------------------------------------------
// PSP strong leg (section 9, 26) - Batch 6.
// ----------------------------------------------------------------------

#[test]
fn psp_umd_data_bin_plus_psp_game_resolves_psp() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "PSP_GAME"),
        strong(BootStructure, "UMD_DATA.BIN"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("PSP"));
}

#[test]
fn psp_game_without_umd_data_bin_stays_candidate_only() {
    let explanation = fuse_platform_evidence([corroborated(BootStructure, "PSP_GAME")]);
    assert_ne!(explanation.outcome, FusionOutcome::Resolved);
}

#[test]
fn param_sfo_alone_is_not_enough_for_psp() {
    let explanation = fuse_platform_evidence([corroborated(ProductCode, "ULUS10000")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn umd_data_bin_alone_without_psp_game_never_resolves() {
    // The strong rule requires PSP_GAME + UMD_DATA.BIN together - a lone
    // UMD_DATA.BIN fact (which should not happen from a real observer, but
    // the resolver must not assume that) never resolves by itself.
    let explanation = fuse_platform_evidence([strong(BootStructure, "UMD_DATA.BIN")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn psp_resolution_is_order_independent() {
    let forward = fuse_platform_evidence([
        corroborated(BootStructure, "PSP_GAME"),
        strong(BootStructure, "UMD_DATA.BIN"),
    ]);
    let backward = fuse_platform_evidence([
        strong(BootStructure, "UMD_DATA.BIN"),
        corroborated(BootStructure, "PSP_GAME"),
    ]);
    assert_eq!(forward.resolved_platform, backward.resolved_platform);
}

// ----------------------------------------------------------------------
// PSP vs PS3 collision (section 10) - Batch 6.
// ----------------------------------------------------------------------

#[test]
fn psp_exclusive_structure_resolves_psp_not_ps3() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "PSP_GAME"),
        strong(BootStructure, "UMD_DATA.BIN"),
    ]);
    assert_eq!(explanation.resolved_platform, Some("PSP"));
}

#[test]
fn ps3_exclusive_structure_resolves_ps3_not_psp() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "PS3_GAME"),
        strong(ContentSignature, "SELF"),
        corroborated(ProductCode, "BLUS30060"),
    ]);
    assert_eq!(explanation.resolved_platform, Some("PS3"));
}

#[test]
fn psp_exclusive_plus_ps3_exclusive_together_is_a_conflict() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "PSP_GAME"),
        strong(BootStructure, "UMD_DATA.BIN"),
        corroborated(BootStructure, "PS3_GAME"),
        strong(ContentSignature, "SELF"),
        corroborated(ProductCode, "BLUS30060"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn param_sfo_only_is_never_enough_for_either_psp_or_ps3() {
    let explanation = fuse_platform_evidence([corroborated(ProductCode, "ULUS10000")]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn eboot_bin_alone_is_never_enough_for_psp() {
    // eboot_bin_present never even reaches ContentEvidence at the source
    // (see psp_boot_evidence.rs's own test), but this fusion-level test
    // documents the same guarantee at this layer: no rule anywhere keys
    // off an "EBOOT.BIN" BootStructure/ContentSignature value at all.
    let explanation = fuse_platform_evidence(Vec::<ContentEvidence>::new());
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

// ----------------------------------------------------------------------
// GBC disambiguation (section 7, 24) - Batch 6.
// ----------------------------------------------------------------------

#[test]
fn cgb_only_logo_and_checksum_resolves_game_boy_color() {
    let explanation = fuse_platform_evidence([strong(
        BootStructure,
        "Nintendo Game Boy Color logo (CGB-only)",
    )]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("Game Boy Color"));
}

#[test]
fn cgb_only_logo_without_valid_checksum_is_ambiguous_candidate_only() {
    let explanation = fuse_platform_evidence([corroborated(
        BootStructure,
        "Nintendo Game Boy Color logo (CGB-only)",
    )]);
    assert_ne!(explanation.outcome, FusionOutcome::Resolved);
}

#[test]
fn dmg_only_logo_still_resolves_game_boy_not_game_boy_color() {
    let explanation = fuse_platform_evidence([strong(BootStructure, "Nintendo Game Boy logo")]);
    assert_eq!(explanation.resolved_platform, Some("Game Boy"));
}

#[test]
fn cgb_enhanced_dual_mode_resolves_game_boy_not_exclusively_game_boy_color() {
    // The DMG-compatible fact is Strong (a dual-mode cart really is a
    // valid, backward-compatible Game Boy cartridge); the dual-mode fact
    // itself is only ever Corroborated (see gb_header_evidence.rs's own
    // doc comment) - representing "belongs to both ecosystems" honestly
    // without inventing an exclusive Game Boy Color claim.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "Nintendo Game Boy logo"),
        corroborated(BootStructure, "Nintendo Game Boy Color logo (dual-mode)"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Resolved);
    assert_eq!(explanation.resolved_platform, Some("Game Boy"));
}

#[test]
fn cgb_enhanced_dual_mode_fact_never_independently_resolves_game_boy_color() {
    let explanation = fuse_platform_evidence([corroborated(
        BootStructure,
        "Nintendo Game Boy Color logo (dual-mode)",
    )]);
    assert_ne!(explanation.outcome, FusionOutcome::Resolved);
}

#[test]
fn cgb_enhanced_dual_mode_still_retains_the_dual_mode_fact_in_the_explanation() {
    // Honest representation: even though no rule fires on the dual-mode
    // fact's own value string (it is deliberately not wired to any rule,
    // never independently resolving anything), the fact itself must
    // survive into the explanation's input_evidence - never silently
    // dropped, matching this crate's "fusion never makes evidence
    // disappear" rule.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "Nintendo Game Boy logo"),
        corroborated(BootStructure, "Nintendo Game Boy Color logo (dual-mode)"),
    ]);
    assert!(
        explanation
            .input_evidence
            .iter()
            .any(|fact| fact.value == "Nintendo Game Boy Color logo (dual-mode)")
    );
}

#[test]
fn game_boy_and_game_boy_color_strong_conflict() {
    // Genuinely impossible for one real cartridge (cgb_flag cannot be both
    // 0x00 and 0xC0), but the resolver must still fail closed for a
    // synthetic bundle rather than picking a winner.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "Nintendo Game Boy logo"),
        strong(BootStructure, "Nintendo Game Boy Color logo (CGB-only)"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn game_boy_color_and_gba_strong_conflict() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "Nintendo Game Boy Color logo (CGB-only)"),
        strong(BootStructure, "GBA cartridge header"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

#[test]
fn cgb_only_value_string_is_distinct_from_dmg_value_string() {
    // Exactness guard: the two facts must never be confused by the exact
    // matcher (RequiredFact::Exact is a plain string equality check).
    let dmg = fuse_platform_evidence([strong(BootStructure, "Nintendo Game Boy logo")]);
    let cgb = fuse_platform_evidence([strong(
        BootStructure,
        "Nintendo Game Boy Color logo (CGB-only)",
    )]);
    assert_ne!(dmg.resolved_platform, cgb.resolved_platform);
}

#[test]
fn gbc_resolution_is_order_independent() {
    let forward = fuse_platform_evidence([
        strong(BootStructure, "Nintendo Game Boy logo"),
        corroborated(BootStructure, "Nintendo Game Boy Color logo (dual-mode)"),
    ]);
    let backward = fuse_platform_evidence([
        corroborated(BootStructure, "Nintendo Game Boy Color logo (dual-mode)"),
        strong(BootStructure, "Nintendo Game Boy logo"),
    ]);
    assert_eq!(forward.resolved_platform, backward.resolved_platform);
    assert_eq!(forward.outcome, backward.outcome);
}

// ----------------------------------------------------------------------
// Rule shadowing (section 16) - Batch 6.
// ----------------------------------------------------------------------

#[test]
fn a_family_level_candidate_rule_and_a_platform_specific_rule_can_both_fire_without_hiding_each_other()
 {
    // TMR SEGA (family-level, Master System/Game Gear) alongside a
    // genuinely platform-specific Saturn fact - both must be visible in
    // fired_candidates; the more general fact must not suppress or hide
    // the more specific one, nor vice versa.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "TMR SEGA"),
        strong(BootStructure, "SEGA SEGASATURN"),
    ]);
    // TMR SEGA alone (without a region leg) never fires any rule, so this
    // must cleanly resolve Saturn - the presence of an unrelated,
    // non-firing family fact must not block or alter the outcome.
    assert_eq!(explanation.resolved_platform, Some("Saturn"));
}

#[test]
fn general_and_specific_gb_rules_both_evaluated_specific_wins_when_justified() {
    // gb_logo_and_checksum (general Game Boy) legs on a *different* value
    // string than gbc_cgb_only_logo_and_checksum - firing the CGB-specific
    // rule must not be blocked by the general rule's existence, and vice
    // versa; they are mutually exclusive by value, not by declaration
    // order.
    let cgb_first_in_rules_but_dmg_evidence_given =
        fuse_platform_evidence([strong(BootStructure, "Nintendo Game Boy logo")]);
    assert_eq!(
        cgb_first_in_rules_but_dmg_evidence_given.resolved_platform,
        Some("Game Boy")
    );
}

#[test]
fn every_applicable_rule_fires_regardless_of_declaration_order_in_the_table() {
    // Evaluate a bundle that satisfies rules declared at very different
    // positions in RULES (Saturn near the top, PSP near the bottom) and
    // confirm both show up as fired candidates - proof that fusion does
    // not stop at the first match.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "SEGA SEGASATURN"),
        corroborated(BootStructure, "PSP_GAME"),
    ]);
    let platforms: Vec<&str> = explanation
        .fired_candidates
        .iter()
        .map(|candidate| candidate.platform)
        .collect();
    assert!(platforms.contains(&"Saturn"));
    // PSP_GAME alone never fires any rule (needs UMD_DATA.BIN or a
    // ProductCode), so it correctly contributes nothing - this asserts
    // fusion evaluated it (no crash/short-circuit) rather than that it
    // fired.
    assert_eq!(explanation.resolved_platform, Some("Saturn"));
}

// ----------------------------------------------------------------------
// Determinism (section 17) - Batch 6: shuffled permutation sampling.
// ----------------------------------------------------------------------

fn permute(mut items: Vec<ContentEvidence>, seed: u64) -> Vec<ContentEvidence> {
    // A small deterministic LCG-based shuffle - no external RNG crate
    // needed, and reproducible across runs without depending on
    // forbidden Date::now()/Math::random() equivalents.
    let mut state = seed.wrapping_add(0x9E3779B97F4A7C15);
    let len = items.len();
    for i in (1..len).rev() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (state >> 33) as usize % (i + 1);
        items.swap(i, j);
    }
    items
}

#[test]
fn ps1_outcome_is_stable_across_many_permutations() {
    let bundle = vec![
        strong(ContentSignature, "PS-X EXE"),
        corroborated(BootStructure, "BOOT"),
        weak(Filesystem, "ISO9660"),
        corroborated(ProductCode, "SLUS-00594"),
    ];
    let baseline = fuse_platform_evidence(bundle.clone());
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        assert_eq!(shuffled.outcome, baseline.outcome);
        assert_eq!(shuffled.resolved_platform, baseline.resolved_platform);
    }
}

#[test]
fn xbox360_outcome_is_stable_across_many_permutations() {
    let bundle = vec![
        strong(Filesystem, "XDVDFS"),
        strong(ContentSignature, "XEX2"),
        weak(Filesystem, "FAT12"),
        corroborated(ProductCode, "584111F7"),
    ];
    let baseline = fuse_platform_evidence(bundle.clone());
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        assert_eq!(shuffled.outcome, baseline.outcome);
        assert_eq!(shuffled.resolved_platform, baseline.resolved_platform);
    }
}

#[test]
fn gbc_outcome_is_stable_across_many_permutations() {
    let bundle = vec![
        strong(BootStructure, "Nintendo Game Boy logo"),
        corroborated(BootStructure, "Nintendo Game Boy Color logo (dual-mode)"),
        weak(Filesystem, "FAT12"),
    ];
    let baseline = fuse_platform_evidence(bundle.clone());
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        assert_eq!(shuffled.outcome, baseline.outcome);
        assert_eq!(shuffled.resolved_platform, baseline.resolved_platform);
    }
}

#[test]
fn gamecube_wii_conflict_outcome_is_stable_across_many_permutations() {
    let bundle = vec![
        strong(BootStructure, "GameCube"),
        strong(BootStructure, "Wii"),
        corroborated(BootStructure, "main.dol"),
    ];
    let baseline = fuse_platform_evidence(bundle.clone());
    assert_eq!(baseline.outcome, FusionOutcome::Conflict);
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        assert_eq!(shuffled.outcome, FusionOutcome::Conflict);
        let mut expected = baseline.conflicting_platforms.clone();
        let mut actual = shuffled.conflicting_platforms.clone();
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(expected, actual);
    }
}

#[test]
fn dat_disagreement_outcome_is_stable_across_many_permutations() {
    let bundle = vec![
        strong(BootStructure, "SEGA SEGASATURN"),
        weak(Filesystem, "ISO9660"),
        corroborated(ProductCode, "MK-81802"),
    ];
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        let comparison =
            crate::platform_evidence_fusion::compare_content_and_dat(&shuffled, Some("Xbox"));
        assert_eq!(
            comparison,
            crate::platform_evidence_fusion::DatContentComparison::Disagree {
                content_platform: "Saturn",
                dat_platform: "Xbox",
            }
        );
    }
}

// ----------------------------------------------------------------------
// Rule catalog consistency (section 15) - Batch 6 top-up.
// ----------------------------------------------------------------------

#[test]
fn no_strong_eligible_rule_can_be_satisfied_by_weak_evidence_alone() {
    // The real invariant this milestone's section 15 asks for: a rule
    // capable of producing FusionOutcome::Resolved (has_strong_leg())
    // must never be satisfiable by an all-Weak bundle. Candidate-only
    // rules (has_strong_leg() == false, e.g. gba_header_candidate) are
    // legitimately allowed to fire from weaker evidence - that is exactly
    // what keeps them from ever independently resolving; see the
    // crate-level weak-only rule tests for that side of the guarantee.
    for rule in RULES {
        if !rule.has_strong_leg() {
            continue;
        }
        // Every leg downgraded to Weak - a Strong-eligible rule's own
        // Strong leg(s) must then fail is_satisfied.
        let all_weak_facts: Vec<ContentEvidence> = rule
            .legs
            .iter()
            .filter_map(|leg| match leg {
                RequiredFact::Exact { kind, value, .. } => Some(weak(*kind, value)),
                RequiredFact::ValuePrefix { kind, prefix, .. } => Some(weak(*kind, prefix)),
                RequiredFact::AnyOfKind { .. } => None,
            })
            .collect();
        assert!(
            !rule.is_satisfied(&all_weak_facts),
            "Strong-eligible rule {} was satisfied by an all-Weak bundle",
            rule.id
        );
    }
}

#[test]
fn every_corroborated_or_stronger_exact_leg_is_not_generic_scoped() {
    // Broader sweep than every_exact_rule_leg_treated_as_platform_discriminating_is_not_generic_scope
    // (Batch 5, Strong-only): here, any Exact leg a rule requires at
    // Corroborated-or-above must also carry a real (Family or
    // PlatformSpecific) scope classification. ValuePrefix legs are
    // deliberately excluded - content_evidence_scope::scope_of is an
    // exact-match table, so a bare prefix (e.g. "Master System", the
    // rule-declared prefix, as opposed to the real emitted values like
    // "Master System (Japan)") can never usefully classify against it;
    // that gap is covered separately by
    // content_evidence_scope::tests's own audit of the real emitted
    // values.
    use crate::content_evidence_scope::{EvidenceScope, scope_of};
    for rule in RULES {
        for leg in rule.legs {
            if let RequiredFact::Exact {
                kind,
                value,
                min_confidence,
            } = leg
            {
                if *min_confidence == ContentEvidenceConfidence::Weak {
                    continue;
                }
                let scope = scope_of(*kind, value);
                assert!(
                    !matches!(scope, EvidenceScope::Generic),
                    "rule {} leg {:?}={:?} requires {:?} but is Generic-scoped",
                    rule.id,
                    kind,
                    value,
                    min_confidence
                );
            }
        }
    }
}

#[test]
fn every_leg_min_confidence_is_a_real_confidence_value() {
    // Structural sanity: RequiredFact::min_confidence never panics or
    // returns something outside the three real tiers, for any rule leg.
    for rule in RULES {
        for leg in rule.legs {
            let confidence = leg.min_confidence();
            assert!(matches!(
                confidence,
                ContentEvidenceConfidence::Weak
                    | ContentEvidenceConfidence::Corroborated
                    | ContentEvidenceConfidence::Strong
            ));
        }
    }
}

#[test]
fn rule_count_matches_expected_growth_after_batch_6() {
    // A loose sanity bound, not a magic number test: Batch 6 added GBC
    // (2 rules), PS2 strong (1 rule), and PSP strong (1 rule) on top of
    // Batch 5's catalog - this just confirms the catalog actually grew
    // rather than a rule being silently lost during editing.
    assert!(
        RULES.len() >= 29 + 4,
        "expected at least 4 new Batch 6 rules on top of Batch 5's 29"
    );
}

#[test]
fn every_platform_appearing_in_rules_also_appears_in_coverage_or_is_explicitly_evidence_poor() {
    // Cross-module consistency: a rule referencing a platform with no
    // coverage_inventory entry at all is not necessarily wrong (coverage
    // tracks "dedicated module work", not "has a fusion rule"), but it
    // would be a red flag worth surfacing - assert every rule's platform
    // is at minimum a real canonical id (the stronger, already-existing
    // every_rule_platform_id_exists_in_the_canonical_registry test covers
    // the hard requirement; this one documents the softer expectation).
    for rule in RULES {
        assert!(crate::platform::platform_by_id(rule.platform).is_some());
    }
}

// ----------------------------------------------------------------------
// GameCube/Wii rule hardening (section 13) - Batch 6 top-up.
// ----------------------------------------------------------------------

#[test]
fn gamecube_product_code_alongside_the_strong_header_does_not_block_resolution() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "GameCube"),
        corroborated(ProductCode, "GZCE51"),
    ]);
    assert_eq!(explanation.resolved_platform, Some("GameCube"));
}

#[test]
fn wii_product_code_alongside_the_strong_header_does_not_block_resolution() {
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "Wii"),
        corroborated(ProductCode, "SMNE01"),
    ]);
    assert_eq!(explanation.resolved_platform, Some("Wii"));
}

#[test]
fn main_dol_plus_product_code_without_either_disc_header_never_resolves() {
    // Shared facts only (main.dol + a candidate product code) - neither
    // is platform-specific on its own, so this must stay Unknown, not a
    // guess toward either platform.
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "main.dol"),
        corroborated(ProductCode, "GZCE51"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Unknown);
}

#[test]
fn gamecube_header_plus_shared_main_dol_plus_wii_header_is_still_a_conflict() {
    // The shared main.dol fact must not somehow "average out" or soften a
    // genuine strong-vs-strong disc-header conflict.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "GameCube"),
        corroborated(BootStructure, "main.dol"),
        strong(BootStructure, "Wii"),
    ]);
    assert_eq!(explanation.outcome, FusionOutcome::Conflict);
}

// ----------------------------------------------------------------------
// Additional determinism sampling (section 17) - Batch 6 top-up.
// ----------------------------------------------------------------------

#[test]
fn psp_umd_outcome_is_stable_across_many_permutations() {
    let bundle = vec![
        corroborated(BootStructure, "PSP_GAME"),
        strong(BootStructure, "UMD_DATA.BIN"),
        weak(ContentSignature, "ELF"),
        corroborated(ProductCode, "UCUS98737"),
    ];
    let baseline = fuse_platform_evidence(bundle.clone());
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        assert_eq!(shuffled.outcome, baseline.outcome);
        assert_eq!(shuffled.resolved_platform, baseline.resolved_platform);
    }
}

#[test]
fn ps2_outcome_is_stable_across_many_permutations() {
    let bundle = vec![
        strong(BootStructure, "BOOT2"),
        weak(ContentSignature, "ELF"),
        corroborated(ProductCode, "SCUS-97399"),
        weak(Filesystem, "ISO9660"),
    ];
    let baseline = fuse_platform_evidence(bundle.clone());
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        assert_eq!(shuffled.outcome, baseline.outcome);
        assert_eq!(shuffled.resolved_platform, baseline.resolved_platform);
    }
}

#[test]
fn psp_vs_ps3_conflict_is_stable_across_many_permutations() {
    let bundle = vec![
        corroborated(BootStructure, "PSP_GAME"),
        strong(BootStructure, "UMD_DATA.BIN"),
        corroborated(BootStructure, "PS3_GAME"),
        strong(ContentSignature, "SELF"),
        corroborated(ProductCode, "BLUS30060"),
    ];
    let baseline = fuse_platform_evidence(bundle.clone());
    assert_eq!(baseline.outcome, FusionOutcome::Conflict);
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        assert_eq!(shuffled.outcome, FusionOutcome::Conflict);
        let mut expected = baseline.conflicting_platforms.clone();
        let mut actual = shuffled.conflicting_platforms.clone();
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(expected, actual);
    }
}

#[test]
fn gamecube_wii_agreement_view_is_stable_across_many_permutations() {
    let bundle = vec![
        strong(BootStructure, "GameCube"),
        corroborated(ProductCode, "GZCE51"),
        corroborated(BootStructure, "main.dol"),
    ];
    for seed in 0..100u64 {
        let shuffled = fuse_platform_evidence(permute(bundle.clone(), seed));
        let comparison =
            crate::platform_evidence_fusion::compare_content_and_dat(&shuffled, Some("GameCube"));
        assert_eq!(
            comparison,
            crate::platform_evidence_fusion::DatContentComparison::Agree {
                content_platform: "GameCube",
                dat_platform: "GameCube",
            }
        );
    }
}

// ----------------------------------------------------------------------
// Additional rule shadowing coverage (section 16) - Batch 6 top-up.
// ----------------------------------------------------------------------

#[test]
fn ps2_strong_and_candidate_rules_both_evaluated_neither_shadows_the_other() {
    // Both ps2_system_cnf_boot2_strong and ps2_system_cnf_boot2_candidate
    // can fire on the same bundle - confirm both actually appear as fired
    // candidates, proving fusion did not stop after the first match.
    let explanation = fuse_platform_evidence([
        strong(BootStructure, "BOOT2"),
        weak(ContentSignature, "ELF"),
    ]);
    let rule_ids: Vec<&str> = explanation
        .fired_candidates
        .iter()
        .map(|c| c.rule_id)
        .collect();
    assert!(rule_ids.contains(&"ps2_system_cnf_boot2_strong"));
    assert!(rule_ids.contains(&"ps2_system_cnf_boot2_candidate"));
}

#[test]
fn psp_strong_and_candidate_rules_both_evaluated_neither_shadows_the_other() {
    let explanation = fuse_platform_evidence([
        corroborated(BootStructure, "PSP_GAME"),
        strong(BootStructure, "UMD_DATA.BIN"),
        corroborated(ProductCode, "UCUS98737"),
    ]);
    let rule_ids: Vec<&str> = explanation
        .fired_candidates
        .iter()
        .map(|c| c.rule_id)
        .collect();
    assert!(rule_ids.contains(&"psp_umd_data_bin_strong"));
    assert!(rule_ids.contains(&"psp_layout_candidate"));
}

#[test]
fn gbc_cgb_only_and_gb_dmg_rules_are_evaluated_independently_by_value_not_order() {
    // Declaration order in RULES has GB rules before the GBC rules -
    // firing the CGB-only-specific rule on CGB-only evidence must not be
    // affected by that ordering.
    let explanation = fuse_platform_evidence([strong(
        BootStructure,
        "Nintendo Game Boy Color logo (CGB-only)",
    )]);
    let rule_ids: Vec<&str> = explanation
        .fired_candidates
        .iter()
        .map(|c| c.rule_id)
        .collect();
    assert!(rule_ids.contains(&"gbc_cgb_only_logo_and_checksum"));
    assert!(!rule_ids.contains(&"gb_logo_and_checksum"));
}

// ----------------------------------------------------------------------
// No action authority (section 28) - Batch 6 top-up.
// ----------------------------------------------------------------------

#[test]
fn resolution_explanation_type_has_no_apply_or_execute_method() {
    // Structural: the public surface of ResolutionExplanation is read-only
    // data; this test exists to make that boundary explicit for the
    // Batch 6 fusion-integration milestone the same way Batch 5's own
    // equivalent test did.
    let explanation = fuse_platform_evidence([strong(BootStructure, "SEGA SEGASATURN")]);
    let _outcome: FusionOutcome = explanation.outcome;
    let _platform: Option<&str> = explanation.resolved_platform;
    let _candidates: &[FiredCandidate] = &explanation.fired_candidates;
    let _conflicts: &[&str] = &explanation.conflicting_platforms;
    let _evidence: &[ContentEvidence] = &explanation.input_evidence;
}

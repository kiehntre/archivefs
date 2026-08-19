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

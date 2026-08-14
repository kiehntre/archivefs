//! Stage 2d dependency-resolution tests.
//!
//! Every test here seeds each set's *storage* verdict as
//! [`SetState::Complete`] unless it is specifically testing the no-upgrade
//! rule. That isolates Stage 2d: any state that comes back weaker than
//! `Complete` was weakened by dependency resolution and nothing else, and any
//! test asserting `Complete` is asserting that this stage found real proof.
//!
//! The last module is an adversarial pass: each test there is an attempt to
//! manufacture a `Complete` out of evidence that does not support one.

#![cfg(test)]

use std::collections::BTreeMap;

use super::graph::MAX_DEPENDENCY_DEPTH;
use super::resolve::{CollectionEvidence, resolve_collection};
use super::{
    DependencyKind, DependencyOutcome, DependencyRequirement, DependencyState, DependencyTarget,
    SetDependencyReport, apply_dependency_state,
};
use crate::dat::archive::{
    ArchiveMemberEvidence, ArchiveMemberHashes, ArchiveMemberStatus, ArchivePassCompletion,
};
use crate::dat::audit::AuditVerdict;
use crate::dat::disk_audit::{DatDiskAudit, DiskAuditVerdict};
use crate::dat::index::{
    DatDiskKey, DatDiskRef, DatMemberKey, DatRomRef, DiskLocation, MemberLocation,
};
use crate::dat::model::{
    ChecksumAlgorithm, DatBiosSetEntry, DatChecksum, DatDeviceRefEntry, DatDiskEntry, DatGameEntry,
    DatRomEntry, DatSampleEntry,
};
use crate::dat::set::{BadMetadataReason, NeedsReviewReason, SetIdentity, SetResolution, SetState};
use crate::dat::sources::audit_run::{DatArchiveAudit, DatArchiveMemberAudit};

// ---------------------------------------------------------------- helpers --

/// A distinct, valid, non-zero 40-hex identity per `tag` character.
///
/// Hex-encoding the tag byte keeps every identity syntactically valid for
/// `parse_disk_sha1` and `DatChecksum::parse` while letting tests name
/// contents with a readable single character.
fn hash(tag: char) -> String {
    format!("{:02x}", tag as u32 & 0xff).repeat(20)
}

/// A required ROM whose content identity is `digit` repeated.
fn rom(name: &str, digit: char) -> DatRomEntry {
    DatRomEntry {
        name: name.to_string(),
        size_bytes: Some(4),
        sha1: Some(hash(digit)),
        ..Default::default()
    }
}

/// A ROM declared as borrowed from `merge` in the set's provider.
fn merged_rom(name: &str, digit: char, merge: &str) -> DatRomEntry {
    DatRomEntry {
        merge: Some(merge.to_string()),
        ..rom(name, digit)
    }
}

fn disk(name: &str, digit: char) -> DatDiskEntry {
    DatDiskEntry {
        name: Some(name.to_string()),
        sha1: Some(hash(digit)),
        ..Default::default()
    }
}

fn merged_disk(name: &str, digit: char, merge: &str) -> DatDiskEntry {
    DatDiskEntry {
        merge: Some(merge.to_string()),
        ..disk(name, digit)
    }
}

fn game(name: &str, roms: Vec<DatRomEntry>) -> DatGameEntry {
    DatGameEntry {
        name: name.to_string(),
        roms,
        ..Default::default()
    }
}

fn device_ref(name: &str) -> DatDeviceRefEntry {
    DatDeviceRefEntry {
        name: Some(name.to_string()),
    }
}

fn bios_set(name: &str) -> DatBiosSetEntry {
    DatBiosSetEntry {
        name: Some(name.to_string()),
        ..Default::default()
    }
}

/// Marks a set as a device node the way MAME's `-listxml` does.
fn as_device(mut entry: DatGameEntry) -> DatGameEntry {
    entry.is_device = Some("yes".into());
    entry.runnable = Some("no".into());
    entry
}

fn top_ref(game_index: usize, games: &[DatGameEntry], rom_index: usize) -> DatRomRef {
    let entry = &games[game_index];
    let member = &entry.roms[rom_index];
    DatRomRef {
        game_index,
        game_name: entry.name.clone(),
        rom_index,
        member_key: DatMemberKey {
            game_index,
            location: MemberLocation::TopLevel { rom_index },
        },
        rom_name: member.name.clone(),
        size_bytes: member.size_bytes,
        checksums: vec![
            DatChecksum::parse(ChecksumAlgorithm::Sha1, member.sha1.as_deref().unwrap()).unwrap(),
        ],
        status: member.status.clone(),
        merge: member.merge.clone(),
        content_classification: entry.content_classification.clone(),
        original_metadata: entry.original_metadata.clone(),
    }
}

fn disk_ref(game_index: usize, games: &[DatGameEntry], disk_index: usize) -> DatDiskRef {
    let entry = &games[game_index];
    let declared = &entry.disks[disk_index];
    DatDiskRef {
        game_index,
        game_name: entry.name.clone(),
        disk_key: DatDiskKey {
            game_index,
            location: DiskLocation::TopLevel { disk_index },
        },
        disk_name: declared.name.clone().unwrap_or_default(),
        sha1: declared.sha1.clone().unwrap(),
        status: declared.status.clone(),
        merge: declared.merge.clone(),
        optional: declared.optional.clone(),
    }
}

fn member(index: usize, refs: Vec<DatRomRef>) -> DatArchiveMemberAudit {
    let verdict = if refs.len() == 1 {
        AuditVerdict::Exact {
            game_name: refs[0].game_name.clone(),
            rom_name: refs[0].rom_name.clone(),
            algorithm: "SHA-1",
        }
    } else {
        AuditVerdict::ExactMultipleCandidates {
            algorithm: "SHA-1",
            count: refs.len(),
            game_names: refs.iter().map(|entry| entry.game_name.clone()).collect(),
        }
    };
    DatArchiveMemberAudit {
        evidence: ArchiveMemberEvidence {
            archive_path: "collection.zip".into(),
            member_name_raw: refs[0].rom_name.as_bytes().to_vec(),
            member_name_display: refs[0].rom_name.clone(),
            index,
            logical_size: 4,
            is_nested_archive: false,
            status: ArchiveMemberStatus::HashComplete,
            hashes: Some(ArchiveMemberHashes {
                crc32: "deadbeef".into(),
                md5: String::new(),
                sha1: refs[0]
                    .checksums
                    .first()
                    .map(|sum| sum.value.clone())
                    .unwrap_or_default(),
                sha256: String::new(),
            }),
        },
        verdict: Some(verdict),
        matched_refs: refs,
    }
}

fn archive(members: Vec<DatArchiveMemberAudit>) -> DatArchiveAudit {
    DatArchiveAudit {
        archive_path: "collection.zip".into(),
        outer_identity: None,
        format: "zip".to_string(),
        total_members: members.len(),
        completion: ArchivePassCompletion::Complete,
        members,
    }
}

fn chd(path: &str, identity: char, parent: Option<char>, refs: Vec<DatDiskRef>) -> DatDiskAudit {
    let verdict = match refs.len() {
        0 => DiskAuditVerdict::NotInDat,
        1 => DiskAuditVerdict::Exact {
            game_name: refs[0].game_name.clone(),
            disk_name: refs[0].disk_name.clone(),
        },
        count => DiskAuditVerdict::ExactMultipleCandidates {
            count,
            game_names: refs.iter().map(|entry| entry.game_name.clone()).collect(),
        },
    };
    DatDiskAudit {
        chd_path: path.into(),
        overall_sha1: Some(hash(identity)),
        parent_required: parent.is_some(),
        parent_sha1: parent.map(hash),
        verdict: Some(verdict),
        matched_refs: refs,
    }
}

/// Evidence in which exactly the listed `(game_index, rom_index)` top-level
/// ROM slots were positionally verified, and nothing else.
fn verifying(games: &[DatGameEntry], slots: &[(usize, usize)]) -> CollectionEvidence {
    let members = slots
        .iter()
        .enumerate()
        .map(|(position, (game_index, rom_index))| {
            member(position, vec![top_ref(*game_index, games, *rom_index)])
        })
        .collect();
    CollectionEvidence::build(&[archive(members)], &[], games, true)
}

/// Evidence with no archive or disk findings at all.
fn nothing_found(games: &[DatGameEntry]) -> CollectionEvidence {
    CollectionEvidence::build(&[], &[], games, true)
}

/// Resolves every game, seeding each set's storage verdict as `seed`.
fn resolve_with(
    games: &[DatGameEntry],
    evidence: &CollectionEvidence,
    seed: SetState,
) -> BTreeMap<String, (SetState, SetDependencyReport)> {
    let mut resolutions: Vec<SetResolution> = games
        .iter()
        .map(|entry| SetResolution {
            identity: SetIdentity {
                source_id: "collection".into(),
                game_name: entry.name.clone(),
            },
            archive_path: "collection.zip".into(),
            state: seed.clone(),
            members_required: Vec::new(),
            members_verified: Vec::new(),
            members_bad: Vec::new(),
            members_optional: Vec::new(),
            members_borrowed: Vec::new(),
            disks_required: Vec::new(),
            disks_verified: Vec::new(),
            disks_parent_required: Vec::new(),
            dependencies: SetDependencyReport::not_evaluated(),
        })
        .collect();
    resolve_collection(&mut resolutions, games, evidence);
    resolutions
        .into_iter()
        .map(|entry| {
            (
                entry.identity.game_name.clone(),
                (entry.state, entry.dependencies),
            )
        })
        .collect()
}

/// The common case: storage was `Complete`, so the reported state is purely
/// this stage's verdict.
fn resolve(
    games: &[DatGameEntry],
    evidence: &CollectionEvidence,
) -> BTreeMap<String, (SetState, SetDependencyReport)> {
    resolve_with(games, evidence, SetState::Complete)
}

fn state_of(
    resolved: &BTreeMap<String, (SetState, SetDependencyReport)>,
    name: &str,
) -> DependencyState {
    resolved[name].1.state
}

fn requirements<'a>(
    resolved: &'a BTreeMap<String, (SetState, SetDependencyReport)>,
    name: &str,
    kind: DependencyKind,
) -> Vec<&'a DependencyRequirement> {
    resolved[name]
        .1
        .requirements
        .iter()
        .filter(|entry| entry.kind == kind)
        .collect()
}

fn only_outcome(
    resolved: &BTreeMap<String, (SetState, SetDependencyReport)>,
    name: &str,
    kind: DependencyKind,
) -> DependencyOutcome {
    let found = requirements(resolved, name, kind);
    assert_eq!(
        found.len(),
        1,
        "expected exactly one {kind:?} requirement for {name}, got {found:#?}"
    );
    found[0].outcome
}

const ALL_DEPENDENCY_STATES: [DependencyState; 9] = [
    DependencyState::NotApplicable,
    DependencyState::NotEvaluated,
    DependencyState::Satisfied,
    DependencyState::Missing,
    DependencyState::Ambiguous,
    DependencyState::Cycle,
    DependencyState::Contradictory,
    DependencyState::Unsupported,
    DependencyState::EvidenceUnavailable,
];

// ------------------------------------------------------- combine / no-upgrade

mod combine {
    use super::*;

    #[test]
    fn dependency_resolution_never_upgrades_a_non_complete_storage_state() {
        let storage_states = [
            SetState::Incomplete,
            SetState::BadMetadata(BadMetadataReason::NoDump),
            SetState::BadMetadata(BadMetadataReason::BadDump),
            SetState::NeedsReview(NeedsReviewReason::AmbiguousMemberAttribution),
            SetState::NeedsReview(NeedsReviewReason::PartialArchivePass),
            SetState::NeedsReview(NeedsReviewReason::UnsupportedSetStructure),
            SetState::NeedsReview(NeedsReviewReason::DuplicateGameName),
            SetState::NeedsReview(NeedsReviewReason::NoDeclaredMembers),
        ];
        for storage in storage_states {
            for dependency in ALL_DEPENDENCY_STATES {
                assert_eq!(
                    apply_dependency_state(storage.clone(), dependency),
                    storage,
                    "{storage:?} must survive {dependency:?} unchanged"
                );
            }
        }
    }

    #[test]
    fn complete_survives_only_a_satisfied_or_inapplicable_dependency_state() {
        for dependency in ALL_DEPENDENCY_STATES {
            let folded = apply_dependency_state(SetState::Complete, dependency);
            assert_eq!(
                folded == SetState::Complete,
                dependency.permits_complete(),
                "{dependency:?} disagreed with permits_complete()"
            );
        }
    }

    #[test]
    fn nothing_ever_folds_up_to_complete_from_a_weaker_storage_state() {
        let storage_states = [
            SetState::Incomplete,
            SetState::BadMetadata(BadMetadataReason::NoDump),
            SetState::NeedsReview(NeedsReviewReason::DependencyCycle),
        ];
        for storage in storage_states {
            for dependency in ALL_DEPENDENCY_STATES {
                assert_ne!(
                    apply_dependency_state(storage.clone(), dependency),
                    SetState::Complete
                );
            }
        }
    }

    #[test]
    fn a_missing_dependency_downgrades_complete_to_incomplete() {
        assert_eq!(
            apply_dependency_state(SetState::Complete, DependencyState::Missing),
            SetState::Incomplete
        );
    }

    #[test]
    fn structural_dependency_faults_downgrade_complete_to_their_own_review_reason() {
        let expected = [
            (
                DependencyState::Ambiguous,
                NeedsReviewReason::AmbiguousDependency,
            ),
            (DependencyState::Cycle, NeedsReviewReason::DependencyCycle),
            (
                DependencyState::Contradictory,
                NeedsReviewReason::ContradictoryDependencyMetadata,
            ),
            (
                DependencyState::Unsupported,
                NeedsReviewReason::UnsupportedDependencyStructure,
            ),
            (
                DependencyState::EvidenceUnavailable,
                NeedsReviewReason::DependencyEvidenceIncomplete,
            ),
            (
                DependencyState::NotEvaluated,
                NeedsReviewReason::DependencyEvidenceIncomplete,
            ),
        ];
        for (dependency, reason) in expected {
            assert_eq!(
                apply_dependency_state(SetState::Complete, dependency),
                SetState::NeedsReview(reason)
            );
        }
    }

    #[test]
    fn a_set_with_no_declared_dependencies_is_not_applicable_and_stays_complete() {
        let games = vec![game("solo", vec![rom("a.bin", 'a')])];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(state_of(&resolved, "solo"), DependencyState::NotApplicable);
        assert_eq!(resolved["solo"].0, SetState::Complete);
        assert!(resolved["solo"].1.requirements.is_empty());
    }

    #[test]
    fn roll_up_reports_the_structural_fault_over_a_plain_absence() {
        let outcomes = [DependencyOutcome::Missing, DependencyOutcome::Cycle];
        assert_eq!(
            DependencyState::roll_up(outcomes.iter()),
            DependencyState::Cycle
        );
        let outcomes = [
            DependencyOutcome::Satisfied,
            DependencyOutcome::Missing,
            DependencyOutcome::Contradictory,
        ];
        assert_eq!(
            DependencyState::roll_up(outcomes.iter()),
            DependencyState::Contradictory
        );
    }

    #[test]
    fn roll_up_of_all_satisfied_is_satisfied_and_of_none_is_not_applicable() {
        assert_eq!(
            DependencyState::roll_up([DependencyOutcome::Satisfied].iter()),
            DependencyState::Satisfied
        );
        assert_eq!(
            DependencyState::roll_up([].iter()),
            DependencyState::NotApplicable
        );
    }

    #[test]
    fn a_partial_scan_reports_unavailable_evidence_rather_than_a_missing_dependency() {
        let games = vec![game("parent", vec![rom("shared.bin", 'a')]), {
            let mut clone = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
            clone.rom_of = Some("parent".into());
            clone
        }];
        let complete = CollectionEvidence::build(&[], &[], &games, true);
        let partial = CollectionEvidence::build(&[], &[], &games, false);

        assert_eq!(
            state_of(&resolve(&games, &complete), "child"),
            DependencyState::Missing
        );
        assert_eq!(
            state_of(&resolve(&games, &partial), "child"),
            DependencyState::EvidenceUnavailable
        );
    }

    #[test]
    fn a_partial_scan_never_weakens_a_positive_verification() {
        let games = vec![game("parent", vec![rom("shared.bin", 'a')]), {
            let mut clone = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
            clone.rom_of = Some("parent".into());
            clone
        }];
        let members = vec![member(0, vec![top_ref(0, &games, 0)])];
        let partial = CollectionEvidence::build(&[archive(members)], &[], &games, false);
        assert_eq!(
            state_of(&resolve(&games, &partial), "child"),
            DependencyState::Satisfied
        );
    }
}

// -------------------------------------------------------- cloneof vs romof --

mod clone_and_rom_source {
    use super::*;

    fn parent_and(child: DatGameEntry) -> Vec<DatGameEntry> {
        vec![
            game("parent", vec![rom("p.bin", 'a')]),
            game("other", vec![rom("o.bin", 'b')]),
            child,
        ]
    }

    #[test]
    fn cloneof_alone_produces_a_parent_set_requirement_and_no_rom_source_one() {
        let mut child = game("child", vec![rom("c.bin", 'c')]);
        child.clone_of = Some("parent".into());
        let games = parent_and(child);
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0), (2, 0)]));

        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::ParentSet),
            DependencyOutcome::Satisfied
        );
        assert!(requirements(&resolved, "child", DependencyKind::RomSource).is_empty());
        assert_eq!(resolved["child"].0, SetState::Complete);
    }

    #[test]
    fn romof_alone_produces_a_rom_source_requirement_and_no_parent_set_one() {
        let mut child = game("child", vec![rom("c.bin", 'c')]);
        child.rom_of = Some("parent".into());
        let games = parent_and(child);
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0), (2, 0)]));

        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::RomSource),
            DependencyOutcome::Satisfied
        );
        assert!(requirements(&resolved, "child", DependencyKind::ParentSet).is_empty());
    }

    #[test]
    fn cloneof_and_romof_at_the_same_target_stay_two_separate_requirements() {
        let mut child = game("child", vec![rom("c.bin", 'c')]);
        child.clone_of = Some("parent".into());
        child.rom_of = Some("parent".into());
        let games = parent_and(child);
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0), (2, 0)]));

        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::ParentSet),
            DependencyOutcome::Satisfied
        );
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::RomSource),
            DependencyOutcome::Satisfied
        );
    }

    #[test]
    fn cloneof_and_romof_at_different_targets_are_both_resolved_independently() {
        let mut child = game("child", vec![rom("c.bin", 'c')]);
        child.clone_of = Some("parent".into());
        child.rom_of = Some("other".into());
        let games = parent_and(child);
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0), (2, 0)]));

        let parent = requirements(&resolved, "child", DependencyKind::ParentSet);
        let source = requirements(&resolved, "child", DependencyKind::RomSource);
        assert_eq!(
            parent[0].target,
            DependencyTarget::Set {
                name: "parent".into()
            }
        );
        assert_eq!(
            source[0].target,
            DependencyTarget::Set {
                name: "other".into()
            }
        );
    }

    #[test]
    fn a_missing_cloneof_target_never_becomes_a_rom_source_and_vice_versa() {
        let mut clone_only = game("clone_only", vec![rom("a.bin", 'c')]);
        clone_only.clone_of = Some("ghost".into());
        let mut rom_only = game("rom_only", vec![rom("b.bin", 'd')]);
        rom_only.rom_of = Some("ghost".into());
        let games = vec![clone_only, rom_only];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0)]));

        assert_eq!(
            only_outcome(&resolved, "clone_only", DependencyKind::ParentSet),
            DependencyOutcome::Contradictory
        );
        assert!(requirements(&resolved, "clone_only", DependencyKind::RomSource).is_empty());
        assert_eq!(
            only_outcome(&resolved, "rom_only", DependencyKind::RomSource),
            DependencyOutcome::Contradictory
        );
        assert!(requirements(&resolved, "rom_only", DependencyKind::ParentSet).is_empty());
    }

    #[test]
    fn a_duplicated_target_name_is_ambiguous_and_never_resolved_by_position() {
        let mut child = game("child", vec![rom("c.bin", 'c')]);
        child.rom_of = Some("twin".into());
        let games = vec![
            game("twin", vec![rom("x.bin", 'a')]),
            game("twin", vec![rom("y.bin", 'b')]),
            child,
        ];
        let resolved = resolve(&games, &verifying(&games, &[(2, 0)]));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::RomSource),
            DependencyOutcome::Ambiguous
        );
        assert_eq!(
            resolved["child"].0,
            SetState::NeedsReview(NeedsReviewReason::AmbiguousDependency)
        );
    }

    #[test]
    fn a_self_reference_is_contradictory_for_both_kinds() {
        let mut clone_self = game("a", vec![rom("a.bin", 'a')]);
        clone_self.clone_of = Some("a".into());
        let mut rom_self = game("b", vec![rom("b.bin", 'b')]);
        rom_self.rom_of = Some("b".into());
        let games = vec![clone_self, rom_self];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0)]));

        assert_eq!(
            only_outcome(&resolved, "a", DependencyKind::ParentSet),
            DependencyOutcome::Contradictory
        );
        assert_eq!(
            only_outcome(&resolved, "b", DependencyKind::RomSource),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn a_cloneof_cycle_is_reported_as_a_cycle_and_never_reaches_complete() {
        let mut first = game("a", vec![rom("a.bin", 'a')]);
        first.clone_of = Some("b".into());
        let mut second = game("b", vec![rom("b.bin", 'b')]);
        second.clone_of = Some("a".into());
        let games = vec![first, second];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0)]));

        assert_eq!(state_of(&resolved, "a"), DependencyState::Cycle);
        assert_eq!(
            resolved["a"].0,
            SetState::NeedsReview(NeedsReviewReason::DependencyCycle)
        );
    }

    #[test]
    fn a_cycle_in_one_attribute_is_not_reported_against_the_other() {
        // `a` and `b` loop through `cloneof` only; their `romof` links both
        // point at a clean root. A resolver that walked one chain for both
        // kinds would report the loop twice.
        let mut first = game("a", vec![rom("a.bin", 'a')]);
        first.clone_of = Some("b".into());
        first.rom_of = Some("root".into());
        let mut second = game("b", vec![rom("b.bin", 'b')]);
        second.clone_of = Some("a".into());
        second.rom_of = Some("root".into());
        let games = vec![game("root", vec![rom("r.bin", 'r')]), first, second];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0), (2, 0)]));

        assert_eq!(
            only_outcome(&resolved, "a", DependencyKind::ParentSet),
            DependencyOutcome::Cycle
        );
        assert_eq!(
            only_outcome(&resolved, "a", DependencyKind::RomSource),
            DependencyOutcome::Satisfied
        );
    }

    #[test]
    fn an_empty_parent_name_is_contradictory_not_absent() {
        let mut child = game("child", vec![rom("c.bin", 'c')]);
        child.clone_of = Some("   ".into());
        let games = vec![child];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        let found = requirements(&resolved, "child", DependencyKind::ParentSet);
        assert_eq!(found[0].outcome, DependencyOutcome::Contradictory);
        assert_eq!(found[0].target, DependencyTarget::Undeclared);
    }

    #[test]
    fn a_chain_deeper_than_the_bound_is_refused_rather_than_walked_forever() {
        let mut games: Vec<DatGameEntry> = (0..MAX_DEPENDENCY_DEPTH + 4)
            .map(|step| game(&format!("s{step}"), vec![rom("r.bin", 'a')]))
            .collect();
        for step in 0..games.len() - 1 {
            games[step].clone_of = Some(format!("s{}", step + 1));
        }
        let resolved = resolve(&games, &nothing_found(&games));
        assert_eq!(state_of(&resolved, "s0"), DependencyState::Unsupported);
    }
}

// -------------------------------------------------------------- merge rules --

mod merge {
    use super::*;

    /// parent declares `shared.bin`; child borrows it by name.
    fn borrow_pair() -> Vec<DatGameEntry> {
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.rom_of = Some("parent".into());
        vec![game("parent", vec![rom("shared.bin", 'a')]), child]
    }

    #[test]
    fn a_borrow_is_satisfied_only_by_the_providers_own_verified_declaration() {
        let games = borrow_pair();
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Satisfied
        );
        assert_eq!(resolved["child"].0, SetState::Complete);
    }

    #[test]
    fn a_borrow_whose_provider_declaration_was_never_verified_is_missing() {
        let games = borrow_pair();
        let resolved = resolve(&games, &nothing_found(&games));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Missing
        );
        assert_eq!(resolved["child"].0, SetState::Incomplete);
    }

    #[test]
    fn a_same_named_member_in_an_unrelated_set_never_satisfies_a_borrow() {
        // `stranger` declares a member with the same name *and* the same
        // content, and it is the only thing verified. The borrow points at
        // `parent`, so it must stay unsatisfied.
        let mut games = borrow_pair();
        games.push(game("stranger", vec![rom("shared.bin", 'a')]));
        let resolved = resolve(&games, &verifying(&games, &[(2, 0)]));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Missing
        );
        assert_ne!(resolved["child"].0, SetState::Complete);
    }

    #[test]
    fn a_merge_naming_a_member_the_provider_does_not_declare_is_contradictory() {
        let mut child = game("child", vec![merged_rom("x.bin", 'a', "not-there.bin")]);
        child.rom_of = Some("parent".into());
        let games = vec![game("parent", vec![rom("shared.bin", 'a')]), child];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn a_merge_matching_two_declarations_in_the_provider_is_ambiguous() {
        let mut child = game("child", vec![merged_rom("dup.bin", 'a', "dup.bin")]);
        child.rom_of = Some("parent".into());
        let games = vec![
            game("parent", vec![rom("dup.bin", 'a'), rom("dup.bin", 'b')]),
            child,
        ];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (0, 1)]));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Ambiguous
        );
    }

    #[test]
    fn a_merge_chain_resolves_through_to_the_declaration_that_owns_the_content() {
        let mut middle = game("middle", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        middle.rom_of = Some("root".into());
        let mut leaf = game("leaf", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        leaf.rom_of = Some("middle".into());
        let games = vec![game("root", vec![rom("shared.bin", 'a')]), middle, leaf];

        let satisfied = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&satisfied, "leaf", DependencyKind::MergedRom),
            DependencyOutcome::Satisfied
        );

        let absent = resolve(&games, &nothing_found(&games));
        assert_eq!(
            only_outcome(&absent, "leaf", DependencyKind::MergedRom),
            DependencyOutcome::Missing
        );
    }

    #[test]
    fn a_merge_cycle_is_reported_as_a_cycle_and_terminates() {
        let mut first = game("a", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        first.rom_of = Some("b".into());
        let mut second = game("b", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        second.rom_of = Some("a".into());
        let games = vec![first, second];
        let resolved = resolve(&games, &nothing_found(&games));
        assert_eq!(
            only_outcome(&resolved, "a", DependencyKind::MergedRom),
            DependencyOutcome::Cycle
        );
        assert_eq!(
            resolved["a"].0,
            SetState::NeedsReview(NeedsReviewReason::DependencyCycle)
        );
    }

    #[test]
    fn a_merge_whose_target_declares_different_content_is_contradictory() {
        // Same member name in the provider, different SHA-1. Matching by name
        // alone would route this borrow at bytes it does not describe.
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.rom_of = Some("parent".into());
        let games = vec![game("parent", vec![rom("shared.bin", 'f')]), child];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn a_merge_with_no_declared_provider_is_contradictory() {
        let games = vec![game(
            "orphan",
            vec![merged_rom("shared.bin", 'a', "shared.bin")],
        )];
        let resolved = resolve(&games, &nothing_found(&games));
        assert_eq!(
            only_outcome(&resolved, "orphan", DependencyKind::MergedRom),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn romof_is_preferred_over_cloneof_as_the_borrow_source() {
        // `cloneof` points at a set that declares the name; `romof` points at
        // one that does not. Following `cloneof` for the borrow would wrongly
        // resolve it.
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.clone_of = Some("hierarchy".into());
        child.rom_of = Some("source".into());
        let games = vec![
            game("hierarchy", vec![rom("shared.bin", 'a')]),
            game("source", vec![rom("different.bin", 'b')]),
            child,
        ];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0)]));
        let found = requirements(&resolved, "child", DependencyKind::MergedRom);
        assert_eq!(found[0].outcome, DependencyOutcome::Contradictory);
        assert_eq!(
            found[0].target,
            DependencyTarget::SetMember {
                set_name: "source".into(),
                member_name: "shared.bin".into(),
            }
        );
    }

    #[test]
    fn cloneof_is_used_as_the_borrow_source_when_romof_is_absent() {
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.clone_of = Some("parent".into());
        let games = vec![game("parent", vec![rom("shared.bin", 'a')]), child];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Satisfied
        );
    }

    #[test]
    fn a_borrow_from_a_set_whose_evidence_was_ambiguous_is_ambiguous_not_satisfied() {
        let games = borrow_pair();
        // One archive member whose hash matched two catalogue entries: the
        // provider's evidence is not attributable, so nothing it "verifies"
        // may satisfy a borrow.
        let members = vec![member(
            0,
            vec![top_ref(0, &games, 0), top_ref(1, &games, 0)],
        )];
        let evidence = CollectionEvidence::build(&[archive(members)], &[], &games, true);
        let resolved = resolve(&games, &evidence);
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Ambiguous
        );
    }
}

// --------------------------------------------------------------------- BIOS --

mod bios {
    use super::*;

    fn bios_root(name: &str, roms: Vec<DatRomEntry>) -> DatGameEntry {
        let mut entry = game(name, roms);
        entry.is_bios = Some("yes".into());
        entry
    }

    #[test]
    fn a_present_bios_provider_satisfies_the_bios_requirement() {
        let mut child = game("game", vec![rom("g.bin", 'g')]);
        child.rom_of = Some("bios".into());
        let games = vec![bios_root("bios", vec![rom("b.bin", 'b')]), child];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0)]));
        assert_eq!(
            only_outcome(&resolved, "game", DependencyKind::Bios),
            DependencyOutcome::Satisfied
        );
        assert_eq!(resolved["game"].0, SetState::Complete);
    }

    #[test]
    fn a_bios_provider_whose_own_roms_are_absent_leaves_the_game_incomplete() {
        let mut child = game("game", vec![rom("g.bin", 'g')]);
        child.rom_of = Some("bios".into());
        let games = vec![bios_root("bios", vec![rom("b.bin", 'b')]), child];
        let resolved = resolve(&games, &verifying(&games, &[(1, 0)]));
        assert_eq!(
            only_outcome(&resolved, "game", DependencyKind::Bios),
            DependencyOutcome::Missing
        );
        assert_eq!(resolved["game"].0, SetState::Incomplete);
    }

    #[test]
    fn a_non_bios_parent_produces_no_bios_requirement() {
        let mut child = game("game", vec![rom("g.bin", 'g')]);
        child.rom_of = Some("plain".into());
        let games = vec![game("plain", vec![rom("p.bin", 'p')]), child];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0)]));
        assert!(requirements(&resolved, "game", DependencyKind::Bios).is_empty());
    }

    #[test]
    fn a_bios_tagged_rom_naming_a_declared_variant_is_satisfied() {
        let mut entry = bios_root("bios", vec![rom("b.bin", 'b')]);
        entry.roms[0].bios = Some("euro".into());
        entry.bios_sets = vec![bios_set("euro"), bios_set("japan")];
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        let found = requirements(&resolved, "bios", DependencyKind::Bios);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].outcome, DependencyOutcome::Satisfied);
        assert_eq!(
            found[0].target,
            DependencyTarget::BiosSet {
                set_name: "bios".into(),
                bios_set: "euro".into(),
            }
        );
    }

    #[test]
    fn a_bios_tagged_rom_naming_no_declared_variant_is_contradictory() {
        let mut entry = bios_root("bios", vec![rom("b.bin", 'b')]);
        entry.roms[0].bios = Some("ghost".into());
        entry.bios_sets = vec![bios_set("euro")];
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "bios", DependencyKind::Bios),
            DependencyOutcome::Contradictory
        );
        assert_eq!(
            resolved["bios"].0,
            SetState::NeedsReview(NeedsReviewReason::ContradictoryDependencyMetadata)
        );
    }

    #[test]
    fn a_bios_tagged_rom_on_a_set_declaring_no_variants_at_all_is_contradictory() {
        let mut entry = bios_root("bios", vec![rom("b.bin", 'b')]);
        entry.roms[0].bios = Some("euro".into());
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "bios", DependencyKind::Bios),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn a_duplicated_bios_variant_name_is_ambiguous_never_resolved_by_order() {
        let mut entry = bios_root("bios", vec![rom("b.bin", 'b')]);
        entry.roms[0].bios = Some("euro".into());
        entry.bios_sets = vec![bios_set("euro"), bios_set("euro")];
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "bios", DependencyKind::Bios),
            DependencyOutcome::Ambiguous
        );
    }

    #[test]
    fn several_bios_variants_produce_one_requirement_each_in_a_stable_order() {
        let mut entry = bios_root(
            "bios",
            vec![rom("a.bin", 'a'), rom("b.bin", 'b'), rom("c.bin", 'c')],
        );
        entry.roms[0].bios = Some("euro".into());
        entry.roms[1].bios = Some("japan".into());
        entry.roms[2].bios = Some("us".into());
        entry.bios_sets = vec![bios_set("euro"), bios_set("japan"), bios_set("us")];
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (0, 1), (0, 2)]));
        let found = requirements(&resolved, "bios", DependencyKind::Bios);
        assert_eq!(found.len(), 3);
        assert!(
            found
                .iter()
                .all(|entry| entry.outcome == DependencyOutcome::Satisfied)
        );
    }

    #[test]
    fn a_malformed_biosset_name_is_contradictory() {
        let mut entry = bios_root("bios", vec![rom("b.bin", 'b')]);
        entry.bios_sets = vec![bios_set("  ")];
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "bios", DependencyKind::Bios),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn bios_storage_presence_is_never_reported_as_a_runnability_claim() {
        // A set can be storage-and-dependency complete under this stage while
        // nothing has been decided about which BIOS variant a run would
        // select. The constant records that boundary explicitly.
        const { assert!(super::super::BIOS_RUNTIME_SELECTION_NOT_MODELLED) };
        let mut child = game("game", vec![rom("g.bin", 'g')]);
        child.rom_of = Some("bios".into());
        let mut root = bios_root("bios", vec![rom("b.bin", 'b'), rom("j.bin", 'j')]);
        root.roms[0].bios = Some("euro".into());
        root.roms[1].bios = Some("japan".into());
        root.bios_sets = vec![bios_set("euro"), bios_set("japan")];
        let games = vec![root, child];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (0, 1), (1, 0)]));
        assert_eq!(resolved["game"].0, SetState::Complete);
    }
}

// ------------------------------------------------------------------ devices --

mod devices {
    use super::*;

    #[test]
    fn a_present_device_with_its_roms_verified_is_satisfied() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("dev")];
        let games = vec![as_device(game("dev", vec![rom("d.bin", 'd')])), host];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Satisfied
        );
        assert_eq!(resolved["host"].0, SetState::Complete);
    }

    #[test]
    fn a_device_whose_roms_are_absent_makes_the_host_incomplete() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("dev")];
        let games = vec![as_device(game("dev", vec![rom("d.bin", 'd')])), host];
        let resolved = resolve(&games, &verifying(&games, &[(1, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Missing
        );
        assert_eq!(resolved["host"].0, SetState::Incomplete);
    }

    #[test]
    fn a_device_with_no_declared_storage_is_vacuously_satisfied() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("dev")];
        let games = vec![as_device(game("dev", Vec::new())), host];
        let resolved = resolve(&games, &verifying(&games, &[(1, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Satisfied
        );
    }

    #[test]
    fn device_requirements_are_transitive() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("outer")];
        let mut outer = as_device(game("outer", Vec::new()));
        outer.device_refs = vec![device_ref("inner")];
        let games = vec![
            as_device(game("inner", vec![rom("i.bin", 'i')])),
            outer,
            host,
        ];

        let absent = resolve(&games, &verifying(&games, &[(2, 0)]));
        assert_eq!(
            only_outcome(&absent, "host", DependencyKind::Device),
            DependencyOutcome::Missing
        );

        let present = resolve(&games, &verifying(&games, &[(0, 0), (2, 0)]));
        assert_eq!(
            only_outcome(&present, "host", DependencyKind::Device),
            DependencyOutcome::Satisfied
        );
    }

    #[test]
    fn a_device_target_absent_from_the_catalogue_is_contradictory_never_ignored() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("stripped")];
        let games = vec![host];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Contradictory
        );
        assert_ne!(resolved["host"].0, SetState::Complete);
    }

    #[test]
    fn a_duplicated_device_target_name_is_ambiguous() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("dev")];
        let games = vec![
            as_device(game("dev", vec![rom("a.bin", 'a')])),
            as_device(game("dev", vec![rom("b.bin", 'b')])),
            host,
        ];
        let resolved = resolve(&games, &verifying(&games, &[(2, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Ambiguous
        );
    }

    #[test]
    fn a_device_cycle_terminates_and_is_reported_as_a_cycle() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("a")];
        let mut first = as_device(game("a", Vec::new()));
        first.device_refs = vec![device_ref("b")];
        let mut second = as_device(game("b", Vec::new()));
        second.device_refs = vec![device_ref("a")];
        let games = vec![first, second, host];
        let resolved = resolve(&games, &verifying(&games, &[(2, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Cycle
        );
    }

    #[test]
    fn a_diamond_in_the_device_graph_is_not_a_cycle() {
        // host -> outer -> {left, right} -> shared. `shared` is reached twice
        // by two independent paths *within one traversal*. A walk sharing one
        // visited set across siblings would call the second arrival a cycle
        // and refuse a perfectly ordinary catalogue.
        let mut host = game("host", vec![rom("h.bin", '1')]);
        host.device_refs = vec![device_ref("outer")];
        let mut outer = as_device(game("outer", Vec::new()));
        outer.device_refs = vec![device_ref("left"), device_ref("right")];
        let mut left = as_device(game("left", Vec::new()));
        left.device_refs = vec![device_ref("shared")];
        let mut right = as_device(game("right", Vec::new()));
        right.device_refs = vec![device_ref("shared")];
        let games = vec![
            as_device(game("shared", vec![rom("s.bin", '2')])),
            left,
            right,
            outer,
            host,
        ];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (4, 0)]));
        for found in requirements(&resolved, "host", DependencyKind::Device) {
            assert_eq!(found.outcome, DependencyOutcome::Satisfied);
        }
        assert_eq!(resolved["host"].0, SetState::Complete);
    }

    #[test]
    fn two_members_borrowed_from_one_provider_are_not_a_cycle() {
        // The same fan-out hazard on the merge path: one provider reached
        // twice from one set is a diamond, not a loop.
        let mut device = as_device(game("dev", Vec::new()));
        device.roms = vec![
            merged_rom("a.bin", '3', "a.bin"),
            merged_rom("b.bin", '4', "b.bin"),
        ];
        device.rom_of = Some("devparent".into());
        let mut host = game("host", vec![rom("h.bin", '1')]);
        host.device_refs = vec![device_ref("dev")];
        let games = vec![
            as_device(game(
                "devparent",
                vec![rom("a.bin", '3'), rom("b.bin", '4')],
            )),
            device,
            host,
        ];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (0, 1), (2, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Satisfied
        );
    }

    #[test]
    fn a_device_ref_pointing_at_a_declared_game_is_contradictory() {
        // The target exists and is fully verified, but the catalogue says it
        // is a runnable game, not a device. Accepting it would let an
        // unrelated game's storage satisfy a device requirement.
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("actually_a_game")];
        let mut target = game("actually_a_game", vec![rom("t.bin", 't')]);
        target.is_device = Some("no".into());
        target.runnable = Some("yes".into());
        let games = vec![target, host];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (1, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn a_self_referencing_device_is_contradictory() {
        let mut host = as_device(game("host", vec![rom("h.bin", 'h')]));
        host.device_refs = vec![device_ref("host")];
        let games = vec![host];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn an_unnamed_device_ref_is_contradictory() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![DatDeviceRefEntry { name: None }];
        let games = vec![host];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        let found = requirements(&resolved, "host", DependencyKind::Device);
        assert_eq!(found[0].outcome, DependencyOutcome::Contradictory);
        assert_eq!(found[0].target, DependencyTarget::Undeclared);
    }
}

// ------------------------------------------------------------------ samples --

mod samples {
    use super::*;

    #[test]
    fn a_sample_dependency_is_its_own_kind_and_is_reported_unsupported() {
        let mut entry = game("game", vec![rom("g.bin", 'g')]);
        entry.sample_of = Some("sharedsamples".into());
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        let found = requirements(&resolved, "game", DependencyKind::Sample);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].outcome, DependencyOutcome::Unsupported);
        assert_eq!(
            found[0].target,
            DependencyTarget::SampleSet {
                name: "sharedsamples".into()
            }
        );
    }

    #[test]
    fn a_sample_dependency_blocks_complete_rather_than_being_assumed_present() {
        let mut entry = game("game", vec![rom("g.bin", 'g')]);
        entry.samples = vec![DatSampleEntry {
            name: Some("payout".into()),
        }];
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            resolved["game"].0,
            SetState::NeedsReview(NeedsReviewReason::UnsupportedDependencyStructure)
        );
    }

    #[test]
    fn a_rom_sharing_a_samples_name_never_satisfies_the_sample_dependency() {
        // The verified ROM is called `payout`, exactly like the sample. If
        // sample resolution consulted ROM evidence at all, this would flip to
        // satisfied.
        let mut entry = game("game", vec![rom("payout", 'p')]);
        entry.samples = vec![DatSampleEntry {
            name: Some("payout".into()),
        }];
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "game", DependencyKind::Sample),
            DependencyOutcome::Unsupported
        );
        assert_ne!(resolved["game"].0, SetState::Complete);
    }

    #[test]
    fn a_set_declaring_no_samples_gets_no_sample_requirement() {
        let games = vec![game("game", vec![rom("g.bin", 'g')])];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert!(requirements(&resolved, "game", DependencyKind::Sample).is_empty());
        assert_eq!(resolved["game"].0, SetState::Complete);
    }

    #[test]
    fn an_empty_sampleof_name_is_contradictory() {
        let mut entry = game("game", vec![rom("g.bin", 'g')]);
        entry.sample_of = Some(String::new());
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "game", DependencyKind::Sample),
            DependencyOutcome::Contradictory
        );
    }
}

// --------------------------------------------------------- disks and CHDs --

mod disks_and_chd {
    use super::*;

    fn with_disk(name: &str, disks: Vec<DatDiskEntry>) -> DatGameEntry {
        let mut entry = game(name, Vec::new());
        entry.disks = disks;
        entry
    }

    #[test]
    fn a_verified_delta_chd_with_its_parent_present_is_satisfied() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let evidence = CollectionEvidence::build(
            &[],
            &[
                chd("g.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)]),
                chd("parent.chd", 'b', None, Vec::new()),
            ],
            &games,
            true,
        );
        let resolved = resolve(&games, &evidence);
        assert_eq!(
            only_outcome(&resolved, "game", DependencyKind::ChdParent),
            DependencyOutcome::Satisfied
        );
        assert_eq!(resolved["game"].0, SetState::Complete);
    }

    #[test]
    fn a_verified_delta_chd_with_no_parent_anywhere_is_missing() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let evidence = CollectionEvidence::build(
            &[],
            &[chd("g.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)])],
            &games,
            true,
        );
        let resolved = resolve(&games, &evidence);
        assert_eq!(
            only_outcome(&resolved, "game", DependencyKind::ChdParent),
            DependencyOutcome::Missing
        );
        assert_eq!(resolved["game"].0, SetState::Incomplete);
    }

    #[test]
    fn a_chd_with_no_parent_link_produces_no_parent_requirement() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let evidence = CollectionEvidence::build(
            &[],
            &[chd("g.chd", 'a', None, vec![disk_ref(0, &games, 0)])],
            &games,
            true,
        );
        let resolved = resolve(&games, &evidence);
        assert!(requirements(&resolved, "game", DependencyKind::ChdParent).is_empty());
        assert_eq!(resolved["game"].0, SetState::Complete);
    }

    #[test]
    fn a_parent_chain_is_followed_transitively() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let evidence = CollectionEvidence::build(
            &[],
            &[
                chd("g.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)]),
                chd("mid.chd", 'b', Some('c'), Vec::new()),
                chd("root.chd", 'c', None, Vec::new()),
            ],
            &games,
            true,
        );
        assert_eq!(
            only_outcome(
                &resolve(&games, &evidence),
                "game",
                DependencyKind::ChdParent
            ),
            DependencyOutcome::Satisfied
        );
    }

    #[test]
    fn a_broken_link_partway_up_the_parent_chain_is_missing() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let evidence = CollectionEvidence::build(
            &[],
            &[
                chd("g.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)]),
                chd("mid.chd", 'b', Some('c'), Vec::new()),
            ],
            &games,
            true,
        );
        assert_eq!(
            only_outcome(
                &resolve(&games, &evidence),
                "game",
                DependencyKind::ChdParent
            ),
            DependencyOutcome::Missing
        );
    }

    #[test]
    fn a_chd_parent_cycle_terminates_and_is_reported_as_a_cycle() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let evidence = CollectionEvidence::build(
            &[],
            &[
                chd("g.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)]),
                chd("other.chd", 'b', Some('a'), Vec::new()),
            ],
            &games,
            true,
        );
        assert_eq!(
            only_outcome(
                &resolve(&games, &evidence),
                "game",
                DependencyKind::ChdParent
            ),
            DependencyOutcome::Cycle
        );
    }

    #[test]
    fn a_chd_naming_itself_as_its_own_parent_is_contradictory() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let evidence = CollectionEvidence::build(
            &[],
            &[chd("g.chd", 'a', Some('a'), vec![disk_ref(0, &games, 0)])],
            &games,
            true,
        );
        assert_eq!(
            only_outcome(
                &resolve(&games, &evidence),
                "game",
                DependencyKind::ChdParent
            ),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn two_images_claiming_one_identity_but_different_parents_is_contradictory() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let evidence = CollectionEvidence::build(
            &[],
            &[
                chd("g.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)]),
                chd("one.chd", 'b', None, Vec::new()),
                chd("two.chd", 'b', Some('c'), Vec::new()),
            ],
            &games,
            true,
        );
        assert_eq!(
            only_outcome(
                &resolve(&games, &evidence),
                "game",
                DependencyKind::ChdParent
            ),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn a_declared_parent_with_an_unusable_identity_is_contradictory() {
        let games = vec![with_disk("game", vec![disk("g.chd", 'a')])];
        let mut audit = chd("g.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)]);
        // The header said "I have a parent" and then gave nothing usable.
        audit.parent_sha1 = None;
        let evidence = CollectionEvidence::build(&[], &[audit], &games, true);
        assert_eq!(
            only_outcome(
                &resolve(&games, &evidence),
                "game",
                DependencyKind::ChdParent
            ),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn disk_merge_and_chd_parent_are_separate_requirements_on_one_disk() {
        // The disk is borrowed *and* its image is a delta. The two facts come
        // from different places - the catalogue's `merge=` and the file's own
        // header - and neither can stand in for the other.
        let mut child = with_disk("child", vec![merged_disk("shared.chd", 'a', "shared.chd")]);
        child.rom_of = Some("parent".into());
        let games = vec![with_disk("parent", vec![disk("shared.chd", 'a')]), child];
        let evidence = CollectionEvidence::build(
            &[],
            &[
                chd("shared.chd", 'a', Some('z'), vec![disk_ref(0, &games, 0)]),
                chd("root.chd", 'z', None, Vec::new()),
            ],
            &games,
            true,
        );
        let resolved = resolve(&games, &evidence);
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedDisk),
            DependencyOutcome::Satisfied
        );
        assert_eq!(resolved["parent"].0, SetState::Complete);
        assert_eq!(
            only_outcome(&resolved, "parent", DependencyKind::ChdParent),
            DependencyOutcome::Satisfied
        );
    }

    #[test]
    fn a_satisfied_disk_merge_never_covers_a_missing_chd_parent() {
        let mut child = with_disk("child", vec![merged_disk("shared.chd", 'a', "shared.chd")]);
        child.rom_of = Some("parent".into());
        let games = vec![with_disk("parent", vec![disk("shared.chd", 'a')]), child];
        let evidence = CollectionEvidence::build(
            &[],
            // The image exists and the merge resolves; its parent does not.
            &[chd(
                "shared.chd",
                'a',
                Some('z'),
                vec![disk_ref(0, &games, 0)],
            )],
            &games,
            true,
        );
        let resolved = resolve(&games, &evidence);
        // The provider owns the disk, so the parent link is reported there...
        assert_eq!(
            only_outcome(&resolved, "parent", DependencyKind::ChdParent),
            DependencyOutcome::Missing
        );
        assert_eq!(resolved["parent"].0, SetState::Incomplete);
        // ...and the borrower is *not* told its borrow is fine. The image it
        // borrows is present but unusable without the parent, so a borrow
        // resolved purely on "the provider's slot is verified" would hand the
        // child a false Complete.
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedDisk),
            DependencyOutcome::Missing
        );
        assert_eq!(resolved["child"].0, SetState::Incomplete);
    }

    #[test]
    fn a_disk_merge_naming_a_disk_the_provider_does_not_declare_is_contradictory() {
        let mut child = with_disk("child", vec![merged_disk("x.chd", 'a', "missing.chd")]);
        child.rom_of = Some("parent".into());
        let games = vec![with_disk("parent", vec![disk("shared.chd", 'a')]), child];
        let evidence = CollectionEvidence::build(&[], &[], &games, true);
        assert_eq!(
            only_outcome(
                &resolve(&games, &evidence),
                "child",
                DependencyKind::MergedDisk
            ),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn a_disk_merge_whose_target_declares_a_different_sha1_is_contradictory() {
        let mut child = with_disk("child", vec![merged_disk("shared.chd", 'a', "shared.chd")]);
        child.rom_of = Some("parent".into());
        let games = vec![with_disk("parent", vec![disk("shared.chd", 'f')]), child];
        let evidence = CollectionEvidence::build(&[], &[], &games, true);
        assert_eq!(
            only_outcome(
                &resolve(&games, &evidence),
                "child",
                DependencyKind::MergedDisk
            ),
            DependencyOutcome::Contradictory
        );
    }

    #[test]
    fn a_device_whose_own_chd_needs_an_absent_parent_leaves_the_host_incomplete() {
        // The device's image is present and verified, so a check that stopped
        // at "the device's declared disks are all verified" would call the
        // host complete. The image is a delta and its parent is nowhere.
        let mut host = game("host", vec![rom("h.bin", '1')]);
        host.device_refs = vec![device_ref("dev")];
        let mut device = as_device(game("dev", Vec::new()));
        device.disks = vec![disk("d.chd", 'a')];
        let games = vec![device, host];
        let evidence = CollectionEvidence::build(
            &[archive(vec![member(0, vec![top_ref(1, &games, 0)])])],
            &[chd("d.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)])],
            &games,
            true,
        );
        let resolved = resolve(&games, &evidence);
        assert_eq!(
            only_outcome(&resolved, "host", DependencyKind::Device),
            DependencyOutcome::Missing
        );
        assert_ne!(resolved["host"].0, SetState::Complete);
    }

    #[test]
    fn a_rom_hash_colliding_with_a_disk_sha1_never_satisfies_the_disk_dependency() {
        // The ROM `decoy.bin` carries the exact SHA-1 the borrowed disk
        // declares, and it is verified. Disk resolution must not see it.
        let mut child = with_disk("child", vec![merged_disk("shared.chd", 'a', "shared.chd")]);
        child.rom_of = Some("parent".into());
        let mut parent = with_disk("parent", vec![disk("shared.chd", 'a')]);
        parent.roms = vec![rom("decoy.bin", 'a')];
        let games = vec![parent, child];
        let evidence = CollectionEvidence::build(
            &[archive(vec![member(0, vec![top_ref(0, &games, 0)])])],
            &[],
            &games,
            true,
        );
        let resolved = resolve(&games, &evidence);
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedDisk),
            DependencyOutcome::Missing
        );
        assert_ne!(resolved["child"].0, SetState::Complete);
    }
}

// ------------------------------------------------------------------ general --

mod general {
    use super::*;

    #[test]
    fn resolution_is_independent_of_catalogue_declaration_order() {
        let build = |reversed: bool| {
            let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
            child.rom_of = Some("parent".into());
            let parent = game("parent", vec![rom("shared.bin", 'a')]);
            if reversed {
                vec![child, parent]
            } else {
                vec![parent, child]
            }
        };
        let forward = build(false);
        let forward_evidence = verifying(&forward, &[(0, 0)]);
        let reverse = build(true);
        let reverse_evidence = verifying(&reverse, &[(1, 0)]);

        assert_eq!(
            state_of(&resolve(&forward, &forward_evidence), "child"),
            state_of(&resolve(&reverse, &reverse_evidence), "child")
        );
    }

    #[test]
    fn every_non_satisfied_requirement_is_surfaced_with_a_reason() {
        let mut entry = game("game", vec![rom("g.bin", 'g')]);
        entry.device_refs = vec![device_ref("gone")];
        entry.sample_of = Some("samples".into());
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        let blocking: Vec<_> = resolved["game"].1.blocking().collect();
        assert_eq!(blocking.len(), 2);
        assert!(
            blocking
                .iter()
                .any(|entry| entry.kind == DependencyKind::Device)
        );
        assert!(
            blocking
                .iter()
                .any(|entry| entry.kind == DependencyKind::Sample)
        );
    }

    #[test]
    fn a_set_absent_from_the_catalogue_is_left_unevaluated_and_unchanged() {
        let games = vec![game("known", vec![rom("k.bin", 'k')])];
        let evidence = verifying(&games, &[(0, 0)]);
        let mut resolutions = vec![SetResolution {
            identity: SetIdentity {
                source_id: "collection".into(),
                game_name: "ghost".into(),
            },
            archive_path: "collection.zip".into(),
            state: SetState::NeedsReview(NeedsReviewReason::DuplicateGameName),
            members_required: Vec::new(),
            members_verified: Vec::new(),
            members_bad: Vec::new(),
            members_optional: Vec::new(),
            members_borrowed: Vec::new(),
            disks_required: Vec::new(),
            disks_verified: Vec::new(),
            disks_parent_required: Vec::new(),
            dependencies: SetDependencyReport::not_evaluated(),
        }];
        resolve_collection(&mut resolutions, &games, &evidence);
        assert_eq!(
            resolutions[0].state,
            SetState::NeedsReview(NeedsReviewReason::DuplicateGameName)
        );
        assert_eq!(
            resolutions[0].dependencies.state,
            DependencyState::NotEvaluated
        );
    }

    #[test]
    fn two_archives_naming_the_same_set_receive_the_same_dependency_verdict() {
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.rom_of = Some("parent".into());
        let games = vec![game("parent", vec![rom("shared.bin", 'a')]), child];
        let evidence = verifying(&games, &[(0, 0)]);
        let make = |path: &str| SetResolution {
            identity: SetIdentity {
                source_id: "collection".into(),
                game_name: "child".into(),
            },
            archive_path: path.into(),
            state: SetState::Complete,
            members_required: Vec::new(),
            members_verified: Vec::new(),
            members_bad: Vec::new(),
            members_optional: Vec::new(),
            members_borrowed: Vec::new(),
            disks_required: Vec::new(),
            disks_verified: Vec::new(),
            disks_parent_required: Vec::new(),
            dependencies: SetDependencyReport::not_evaluated(),
        };
        let mut resolutions = vec![make("one.zip"), make("two.zip")];
        resolve_collection(&mut resolutions, &games, &evidence);
        assert_eq!(resolutions[0].dependencies, resolutions[1].dependencies);
        assert_eq!(resolutions[0].state, resolutions[1].state);
    }

    #[test]
    fn a_borrow_from_a_clrmamepro_style_unsupported_provider_is_unsupported() {
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.rom_of = Some("parent".into());
        let mut parent = game("parent", vec![rom("shared.bin", 'a')]);
        parent.unsupported_structure = true;
        let games = vec![parent, child];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Unsupported
        );
    }

    #[test]
    fn a_borrow_landing_on_a_nodump_declaration_is_unsupported_not_missing() {
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.rom_of = Some("parent".into());
        let mut parent = game("parent", vec![rom("shared.bin", 'a')]);
        parent.roms[0].status = Some("nodump".into());
        let games = vec![parent, child];
        let resolved = resolve(&games, &nothing_found(&games));
        assert_eq!(
            only_outcome(&resolved, "child", DependencyKind::MergedRom),
            DependencyOutcome::Unsupported
        );
    }
}

// -------------------------------------------------- false-Complete attacks --

mod false_complete_attacks {
    use super::*;

    /// Every attack asserts the same thing: the set did not come back
    /// `Complete`. The specific reason is asserted elsewhere; here the only
    /// property under test is that the attack failed.
    fn assert_not_complete(
        resolved: &BTreeMap<String, (SetState, SetDependencyReport)>,
        name: &str,
    ) {
        assert_ne!(
            resolved[name].0,
            SetState::Complete,
            "{name} reached Complete on evidence that does not support it: {:#?}",
            resolved[name].1
        );
    }

    #[test]
    fn attack_cloneof_cannot_stand_in_for_a_missing_romof_borrow() {
        // The set clones a present parent, but borrows from a *different*
        // set that is absent. Treating cloneof as romof would satisfy it.
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.clone_of = Some("present".into());
        child.rom_of = Some("absent".into());
        let games = vec![
            game("present", vec![rom("shared.bin", 'a')]),
            game("absent", vec![rom("shared.bin", 'a')]),
            child,
        ];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_not_complete(&resolved, "child");
    }

    #[test]
    fn attack_a_filename_collision_across_unrelated_sets_satisfies_nothing() {
        let mut child = game("child", vec![merged_rom("common.bin", 'a', "common.bin")]);
        child.rom_of = Some("parent".into());
        let games = vec![
            game("parent", vec![rom("common.bin", 'a')]),
            game("decoy_one", vec![rom("common.bin", 'a')]),
            game("decoy_two", vec![rom("common.bin", 'a')]),
            child,
        ];
        // Only the decoys were verified.
        let resolved = resolve(&games, &verifying(&games, &[(1, 0), (2, 0)]));
        assert_not_complete(&resolved, "child");
    }

    #[test]
    fn attack_a_duplicate_set_name_cannot_be_resolved_by_taking_the_first() {
        let mut child = game("child", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        child.rom_of = Some("twin".into());
        let games = vec![
            game("twin", vec![rom("shared.bin", 'a')]),
            game("twin", vec![rom("other.bin", 'b')]),
            child,
        ];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_not_complete(&resolved, "child");
    }

    #[test]
    fn attack_a_merge_cycle_cannot_spin_into_a_satisfied_verdict() {
        let mut first = game("a", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        first.rom_of = Some("b".into());
        let mut second = game("b", vec![merged_rom("shared.bin", 'a', "shared.bin")]);
        second.rom_of = Some("a".into());
        let games = vec![first, second];
        let resolved = resolve(&games, &nothing_found(&games));
        assert_not_complete(&resolved, "a");
        assert_not_complete(&resolved, "b");
    }

    #[test]
    fn attack_a_device_cycle_cannot_spin_into_a_satisfied_verdict() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("a")];
        let mut first = as_device(game("a", Vec::new()));
        first.device_refs = vec![device_ref("b")];
        let mut second = as_device(game("b", Vec::new()));
        second.device_refs = vec![device_ref("a")];
        let games = vec![first, second, host];
        let resolved = resolve(&games, &verifying(&games, &[(2, 0)]));
        assert_not_complete(&resolved, "host");
    }

    #[test]
    fn attack_a_missing_intermediate_node_does_not_hide_a_transitive_requirement() {
        // host -> outer (present, empty) -> inner (absent from the catalogue).
        // A resolver that stopped at one level would call this satisfied.
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("outer")];
        let mut outer = as_device(game("outer", Vec::new()));
        outer.device_refs = vec![device_ref("inner")];
        let games = vec![outer, host];
        let resolved = resolve(&games, &verifying(&games, &[(1, 0)]));
        assert_not_complete(&resolved, "host");
    }

    #[test]
    fn attack_a_self_dependency_is_never_trivially_satisfied() {
        let mut entry = game("loop", vec![rom("l.bin", 'l')]);
        entry.rom_of = Some("loop".into());
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_not_complete(&resolved, "loop");
    }

    #[test]
    fn attack_a_malformed_dependency_name_is_not_read_as_no_dependency() {
        let mut entry = game("game", vec![rom("g.bin", 'g')]);
        entry.rom_of = Some(String::new());
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_not_complete(&resolved, "game");
    }

    #[test]
    fn attack_a_stripped_device_node_is_not_read_as_a_device_without_roms() {
        let mut host = game("host", vec![rom("h.bin", 'h')]);
        host.device_refs = vec![device_ref("stripped")];
        let games = vec![host];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_not_complete(&resolved, "host");
    }

    #[test]
    fn attack_a_chd_identity_collision_does_not_manufacture_a_parent() {
        // Two images disagree about what the parent identity's own parent is.
        let games = vec![{
            let mut entry = game("game", Vec::new());
            entry.disks = vec![disk("g.chd", 'a')];
            entry
        }];
        let evidence = CollectionEvidence::build(
            &[],
            &[
                chd("g.chd", 'a', Some('b'), vec![disk_ref(0, &games, 0)]),
                chd("claim_one.chd", 'b', Some('c'), Vec::new()),
                chd("claim_two.chd", 'b', Some('d'), Vec::new()),
                chd("c.chd", 'c', None, Vec::new()),
                chd("d.chd", 'd', None, Vec::new()),
            ],
            &games,
            true,
        );
        assert_not_complete(&resolve(&games, &evidence), "game");
    }

    #[test]
    fn attack_a_sample_namespace_gap_is_not_read_as_satisfied() {
        let mut entry = game("game", vec![rom("g.bin", 'g')]);
        entry.sample_of = Some("shared".into());
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0)]));
        assert_not_complete(&resolved, "game");
    }

    #[test]
    fn attack_an_incomplete_storage_verdict_is_never_repaired_by_dependencies() {
        // Everything a dependency resolver could want is present and
        // satisfied; storage still said Incomplete, and that must stand.
        let games = vec![game("solo", vec![rom("a.bin", 'a')])];
        let resolved = resolve_with(&games, &verifying(&games, &[(0, 0)]), SetState::Incomplete);
        assert_eq!(resolved["solo"].0, SetState::Incomplete);
        assert_eq!(state_of(&resolved, "solo"), DependencyState::NotApplicable);
    }

    #[test]
    fn attack_ambiguous_member_evidence_still_outranks_a_dependency_success() {
        // Storage refused the set for ambiguous attribution. A satisfied
        // dependency set must not paper over that.
        let games = vec![game("solo", vec![rom("a.bin", 'a')])];
        let resolved = resolve_with(
            &games,
            &verifying(&games, &[(0, 0)]),
            SetState::NeedsReview(NeedsReviewReason::AmbiguousMemberAttribution),
        );
        assert_eq!(
            resolved["solo"].0,
            SetState::NeedsReview(NeedsReviewReason::AmbiguousMemberAttribution)
        );
    }

    #[test]
    fn attack_a_bios_variant_tag_cannot_be_satisfied_by_another_variants_content() {
        // Two variants declare identical content. The tag naming an
        // undeclared variant must still fail, hash equality notwithstanding.
        let mut entry = game("bios", vec![rom("a.bin", 'a'), rom("b.bin", 'a')]);
        entry.is_bios = Some("yes".into());
        entry.roms[0].bios = Some("euro".into());
        entry.roms[1].bios = Some("japan".into());
        entry.bios_sets = vec![bios_set("euro")];
        let games = vec![entry];
        let resolved = resolve(&games, &verifying(&games, &[(0, 0), (0, 1)]));
        assert_not_complete(&resolved, "bios");
    }
}

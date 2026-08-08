//! Integration tests for canonical ROM organisation: planning, apply, rollback,
//! symlink semantics, collisions, crash recovery and cancellation.
//!
//! Every mutation test uses temporary directories only - never a real user ROM
//! directory. Planning tests snapshot the source tree and assert zero changes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::dat::rom_organisation::*;
use crate::platform::identity::{
    PlatformIdentityConfidence, PlatformIdentityEvidence, PlatformIdentityResolution,
    PlatformIdentitySource, resolve_platform_identity,
};
use crate::safe_read::TrustedRoots;

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn cancelled() -> AtomicBool {
    AtomicBool::new(true)
}

/// The canonical RomM slug mapping a caller would derive from the imported
/// RomM identity cache. Folder names come from here, never from display text.
fn slug_for_platform(platform: &str) -> Option<String> {
    Some(
        match platform {
            "PSP" => "psp",
            "Xbox360" => "xbox360",
            "Nintendo DS" => "nds",
            "Switch" => "switch",
            "GameCube" => "gamecube",
            "Wii" => "wii",
            _ => return None,
        }
        .to_string(),
    )
}

fn resolved(platform: &str, source: PlatformIdentitySource) -> PlatformIdentityResolution {
    PlatformIdentityResolution::Resolved {
        generation: 1,
        platform: platform.to_string(),
        display_name: crate::platform::display_name_for(platform).to_string(),
        confidence: PlatformIdentityConfidence::High,
        evidence: vec![PlatformIdentityEvidence {
            platform: platform.to_string(),
            source,
            confidence: PlatformIdentityConfidence::High,
            generation: 1,
            detail: "test evidence".to_string(),
        }],
    }
}

fn candidate(
    dir: &Path,
    name: &str,
    resolution: PlatformIdentityResolution,
) -> OrganisationCandidate {
    let source_path = dir.join(name);
    std::fs::write(&source_path, b"fixture contents").unwrap();
    OrganisationCandidate {
        source_path,
        resolution,
        canonical_name: Some(name.to_string()),
    }
}

fn plan_for(
    master_root: &Path,
    mode: OrganisationMode,
    candidates: &[OrganisationCandidate],
    generation: u64,
) -> OrganisationPlan {
    build_organisation_plan(&OrganisationPlanRequest {
        master_root,
        mode,
        candidates,
        slug_for_platform: &slug_for_platform,
        generation,
    })
}

fn apply_plan(
    plan: &OrganisationPlan,
    approved: &BTreeSet<String>,
    journal_dir: &Path,
    cancel: &AtomicBool,
) -> crate::dat::rename_apply::executor::ApplyOutcome {
    // A configured master root exists (the user created it); the platform
    // directory does not yet exist and is what apply creates.
    std::fs::create_dir_all(&plan.master_root).unwrap();
    let mut tx =
        build_organisation_transaction(plan, approved, plan.generation).expect("build transaction");
    let mut trusted_roots =
        vec![std::fs::canonicalize(&plan.master_root).unwrap_or_else(|_| plan.master_root.clone())];
    for entry in plan.suggested() {
        if let Some(parent) = entry.source_path.parent()
            && let Ok(canonical) = std::fs::canonicalize(parent)
        {
            trusted_roots.push(canonical);
        }
    }
    apply_organisation_transaction(
        &mut tx,
        approved,
        plan.generation,
        TrustedRoots::from_paths(trusted_roots),
        journal_dir,
        cancel,
        plan.mode,
        &plan.master_root,
    )
    .expect("apply")
}

fn approved_of(sources: &[&Path]) -> BTreeSet<String> {
    sources
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Destination derivation and platform safety
// ---------------------------------------------------------------------------

#[test]
fn psp_identity_proposes_master_root_psp_name() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Lumines.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let entry = &plan.entries[0];
    assert_eq!(entry.status, OrganisationStatus::Suggested);
    assert_eq!(
        entry.destination_path,
        master.join("psp").join("Lumines.iso")
    );
    assert_eq!(entry.slug.as_deref(), Some("psp"));
}

#[test]
fn canonical_slug_is_used_not_the_display_label() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    // The display name is "Sony PlayStation Portable"; the slug must be "psp".
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(
        plan.entries[0]
            .destination_path
            .parent()
            .unwrap()
            .file_name()
            .unwrap(),
        "psp"
    );
}

#[test]
fn verified_dat_and_manual_and_romm_all_map_to_the_same_slug() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    for source_kind in [
        PlatformIdentitySource::VerifiedDat,
        PlatformIdentitySource::Manual,
        PlatformIdentitySource::Romm,
    ] {
        let name = format!("g-{:?}.iso", source_kind);
        let cand = candidate(&source, &name, resolved("PSP", source_kind));
        let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
        assert_eq!(
            plan.entries[0]
                .destination_path
                .parent()
                .unwrap()
                .file_name()
                .unwrap(),
            "psp",
            "provenance {source_kind:?} must use the same canonical slug"
        );
        assert_eq!(
            plan.entries[0].platform_source,
            source_kind.label().to_string()
        );
    }
}

#[test]
fn unknown_platform_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        PlatformIdentityResolution::Unknown { generation: 1 },
    );
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Blocked);
    assert!(plan.entries[0].reason.is_some());
}

#[test]
fn platform_conflict_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        PlatformIdentityResolution::Conflict {
            generation: 1,
            evidence: Vec::new(),
        },
    );
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Blocked);
}

#[test]
fn missing_slug_mapping_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("Atari2600", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Unsupported);
    assert!(
        plan.entries[0]
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("slug"),
        "{:?}",
        plan.entries[0].reason
    );
}

#[test]
fn rename_in_place_stays_in_the_current_directory() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::RenameInPlace,
        std::slice::from_ref(&cand),
        1,
    );
    let entry = &plan.entries[0];
    assert_eq!(
        entry.destination_path,
        source.join("Game.iso"),
        "rename in place must stay in the source directory"
    );
    assert!(
        entry.slug.is_none(),
        "rename in place needs no platform folder"
    );
}

#[test]
fn move_mode_proposes_the_canonical_directory() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.nds",
        resolved("Nintendo DS", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(
        plan.entries[0].destination_path,
        master.join("nds").join("Game.nds")
    );
}

#[test]
fn already_organised_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let organised = master.join("psp");
    std::fs::create_dir_all(&organised).unwrap();
    let cand = candidate(
        &organised,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::AlreadyOrganised);
}

// ---------------------------------------------------------------------------
// Collisions
// ---------------------------------------------------------------------------

#[test]
fn existing_destination_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    // The destination already exists.
    let psp = master.join("psp");
    std::fs::create_dir_all(&psp).unwrap();
    std::fs::write(psp.join("Game.iso"), b"taken").unwrap();
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Conflict);
}

#[test]
fn case_only_destination_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let psp = master.join("psp");
    std::fs::create_dir_all(&psp).unwrap();
    std::fs::write(psp.join("game.iso"), b"case twin").unwrap();
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Conflict);
}

#[test]
fn two_plans_targeting_one_destination_are_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    // Two candidates that derive to the same canonical name.
    let a = candidate(
        &source,
        "A.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let b = candidate(
        &source,
        "B.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let mut candidates = vec![a, b];
    candidates[0].canonical_name = Some("Same.iso".to_string());
    candidates[1].canonical_name = Some("Same.iso".to_string());
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &candidates, 1);
    assert!(
        plan.entries
            .iter()
            .all(|e| e.status == OrganisationStatus::Conflict)
    );
}

// ---------------------------------------------------------------------------
// Planning is read-only
// ---------------------------------------------------------------------------

#[test]
fn planning_creates_no_directories_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let before: Vec<PathBuf> = collect_tree(dir.path());
    let _ = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert!(!master.exists(), "planning must not create the master root");
    assert!(
        !master.join("psp").exists(),
        "planning must not create the platform directory"
    );
    assert_eq!(collect_tree(dir.path()), before, "planning changes nothing");
}

fn collect_tree(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    queue.push(path.clone());
                }
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Apply and rollback
// ---------------------------------------------------------------------------

#[test]
fn same_filesystem_real_file_move_succeeds_and_preserves_content() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(&plan, &approved_of(&[&cand.source_path]), &journal, &cancel);
    assert_eq!(
        outcome.transaction.state,
        crate::dat::rename_apply::TransactionState::Applied
    );
    assert!(!cand.source_path.exists(), "source moved away");
    let dest = master.join("psp").join("Game.iso");
    assert!(dest.exists());
    assert_eq!(std::fs::read(&dest).unwrap(), b"fixture contents");
}

#[test]
fn rollback_restores_original_path_and_content() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(&plan, &approved_of(&[&cand.source_path]), &journal, &cancel);
    let mut tx = outcome.transaction;
    let rollback = rollback_organisation_transaction(&mut tx, &journal, &cancel, &master).unwrap();
    assert!(cand.source_path.exists(), "source path restored");
    assert_eq!(
        std::fs::read(&cand.source_path).unwrap(),
        b"fixture contents"
    );
    assert!(!master.join("psp").join("Game.iso").exists());
    assert!(
        rollback.directories_removed.contains(&master.join("psp")),
        "the created platform directory is removed when empty: {:?}",
        rollback.directories_removed
    );
}

#[test]
fn cross_filesystem_move_is_refused_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let source_file = source.join("Game.iso");
    std::fs::write(&source_file, b"fixture contents").unwrap();

    // `/proc` is a different filesystem than any temp directory on Linux.
    let Some(master) = different_filesystem_root(dir.path()) else {
        return; // environment has no second filesystem; the helper is covered below
    };
    let master = master.join("roms");
    let cand = OrganisationCandidate {
        source_path: source_file.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: Some("Game.iso".to_string()),
    };
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    // apply_plan_result creates master_root; for the /proc master root that
    // would fail, so drive apply directly and assert refusal.
    let mut tx =
        build_organisation_transaction(&plan, &approved_of(&[&source_file]), 1).expect("build");
    let cancel = no_cancel();
    let result = apply_organisation_transaction(
        &mut tx,
        &approved_of(&[&source_file]),
        1,
        TrustedRoots::from_paths([dir.path()]),
        &journal,
        &cancel,
        plan.mode,
        &plan.master_root,
    );
    assert!(result.is_err(), "a cross-filesystem move must be refused");
    assert!(source_file.exists(), "the source is never touched");
}

fn apply_plan_result(
    plan: &OrganisationPlan,
    approved: &BTreeSet<String>,
    journal_dir: &Path,
    cancel: &AtomicBool,
) -> Result<
    crate::dat::rename_apply::executor::ApplyOutcome,
    crate::dat::rename_apply::executor::ApplyError,
> {
    std::fs::create_dir_all(&plan.master_root).unwrap();
    let mut tx =
        build_organisation_transaction(plan, approved, plan.generation).expect("build transaction");
    let mut trusted_roots =
        vec![std::fs::canonicalize(&plan.master_root).unwrap_or_else(|_| plan.master_root.clone())];
    for entry in plan.suggested() {
        if let Some(parent) = entry.source_path.parent()
            && let Ok(canonical) = std::fs::canonicalize(parent)
        {
            trusted_roots.push(canonical);
        }
    }
    apply_organisation_transaction(
        &mut tx,
        approved,
        plan.generation,
        TrustedRoots::from_paths(trusted_roots),
        journal_dir,
        cancel,
        plan.mode,
        &plan.master_root,
    )
}

/// A root on a different filesystem than `dir` (Linux: procfs), used to prove
/// a cross-filesystem move is refused. Returns `None` when no second
/// filesystem is observable.
fn different_filesystem_root(dir: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dir_dev = std::fs::metadata(dir).ok()?.dev();
        let proc = Path::new("/proc");
        let proc_dev = std::fs::metadata(proc).ok()?.dev();
        if proc_dev != dir_dev {
            return Some(proc.to_path_buf());
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

#[test]
fn apply_creates_only_the_canonical_platform_directory() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    std::fs::create_dir_all(&master).unwrap();
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let _ = apply_plan(&plan, &approved_of(&[&cand.source_path]), &journal, &cancel);
    assert!(master.join("psp").is_dir());
    let children: Vec<String> = std::fs::read_dir(&master)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        children,
        vec!["psp"],
        "only the canonical platform dir is created"
    );
}

#[test]
fn rollback_never_removes_a_pre_existing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    // The platform directory already exists and contains a user file.
    let psp = master.join("psp");
    std::fs::create_dir_all(&psp).unwrap();
    std::fs::write(psp.join("user-note.txt"), b"mine").unwrap();

    let _cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    // Destination is occupied -> plan Conflict, not suggested, so nothing is
    // built; but to exercise the rollback path, apply a different plan into a
    // fresh platform dir and check a pre-existing sibling is untouched.
    let cand2 = candidate(
        &source,
        "Other.iso",
        resolved("Switch", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand2),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(
        &plan,
        &approved_of(&[&cand2.source_path]),
        &journal,
        &cancel,
    );
    let mut tx = outcome.transaction;
    let _ = rollback_organisation_transaction(&mut tx, &journal, &cancel, &master).unwrap();
    assert!(
        psp.exists(),
        "the pre-existing directory must never be removed"
    );
    assert_eq!(std::fs::read(psp.join("user-note.txt")).unwrap(), b"mine");
    assert!(
        !master.join("switch").exists(),
        "the created dir is removed"
    );
}

#[test]
fn source_identity_changed_is_rejected_at_apply() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    // Build the transaction (identity snapshot) then change the source.
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::create_dir_all(&master).unwrap();
    let mut tx = build_organisation_transaction(&plan, &approved_of(&[&cand.source_path]), 1)
        .expect("build");
    std::fs::write(&cand.source_path, b"different bytes").unwrap();
    let cancel = no_cancel();
    let mut trusted_roots = vec![std::fs::canonicalize(&master).unwrap_or(master.clone())];
    if let Some(parent) = cand.source_path.parent()
        && let Ok(c) = std::fs::canonicalize(parent)
    {
        trusted_roots.push(c);
    }
    let result = apply_organisation_transaction(
        &mut tx,
        &approved_of(&[&cand.source_path]),
        1,
        TrustedRoots::from_paths(trusted_roots),
        &journal,
        &cancel,
        plan.mode,
        &plan.master_root,
    );
    assert!(result.is_err());
    assert!(cand.source_path.exists());
    assert_eq!(
        std::fs::read(&cand.source_path).unwrap(),
        b"different bytes"
    );
    assert!(!master.join("psp").exists(), "no mutation happened");
}

#[test]
fn destination_created_after_preview_is_rejected_at_apply() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    std::fs::create_dir_all(&master).unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let mut tx = build_organisation_transaction(&plan, &approved_of(&[&cand.source_path]), 1)
        .expect("build");
    // The destination file appears after the preview.
    let psp = master.join("psp");
    std::fs::create_dir_all(&psp).unwrap();
    std::fs::write(psp.join("Game.iso"), b"sneaky").unwrap();
    let mut trusted_roots = vec![std::fs::canonicalize(&master).unwrap_or(master.clone())];
    if let Some(parent) = cand.source_path.parent()
        && let Ok(c) = std::fs::canonicalize(parent)
    {
        trusted_roots.push(c);
    }
    let result = apply_organisation_transaction(
        &mut tx,
        &approved_of(&[&cand.source_path]),
        1,
        TrustedRoots::from_paths(trusted_roots),
        &journal,
        &cancel,
        plan.mode,
        &plan.master_root,
    );
    assert!(
        result.is_err(),
        "an appearing destination must abort the batch"
    );
    assert_eq!(
        std::fs::read(psp.join("Game.iso")).unwrap(),
        b"sneaky",
        "the appearing file is never overwritten"
    );
}

#[test]
fn stale_generation_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    // Apply against a newer generation: stale.
    std::fs::create_dir_all(&master).unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let mut tx = build_organisation_transaction(&plan, &approved_of(&[&cand.source_path]), 1)
        .expect("build");
    let cancel = no_cancel();
    let mut trusted_roots = vec![std::fs::canonicalize(&master).unwrap_or(master.clone())];
    if let Some(parent) = cand.source_path.parent()
        && let Ok(c) = std::fs::canonicalize(parent)
    {
        trusted_roots.push(c);
    }
    let result = apply_organisation_transaction(
        &mut tx,
        &approved_of(&[&cand.source_path]),
        2, // current generation is 2, plan was generation 1
        TrustedRoots::from_paths(trusted_roots),
        &journal,
        &cancel,
        plan.mode,
        &plan.master_root,
    );
    assert!(result.is_err(), "a stale plan must never apply");
}

// ---------------------------------------------------------------------------
// Symlink semantics
// ---------------------------------------------------------------------------

#[test]
fn symlink_object_move_preserves_the_target_text_and_never_dereferences() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let real_target = dir.path().join("elsewhere").join("real.bin");
    std::fs::create_dir_all(real_target.parent().unwrap()).unwrap();
    std::fs::write(&real_target, b"real content").unwrap();
    let link = source.join("Game.iso");
    std::os::unix::fs::symlink(&real_target, &link).unwrap();

    let cand = OrganisationCandidate {
        source_path: link.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: Some("Game.iso".to_string()),
    };
    let plan = plan_for(
        &master,
        OrganisationMode::OrganiseSymlinkOnly,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(&plan, &approved_of(&[&link]), &journal, &cancel);
    assert_eq!(
        outcome.transaction.state,
        crate::dat::rename_apply::TransactionState::Applied
    );
    let moved = master.join("psp").join("Game.iso");
    assert!(moved.symlink_metadata().is_ok(), "the link object moved");
    assert_eq!(
        std::fs::read_link(&moved).unwrap(),
        real_target,
        "the link target text is preserved exactly"
    );
    assert!(
        real_target.exists(),
        "the target is never dereferenced or moved"
    );
    assert_eq!(std::fs::read(&real_target).unwrap(), b"real content");
}

#[test]
fn symlink_only_mode_rejects_a_regular_file_source() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(&master, OrganisationMode::OrganiseSymlinkOnly, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Blocked);
    assert!(plan.entries[0].source_path.exists());
}

#[test]
fn move_real_file_mode_rejects_a_symlink_source() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let target = dir.path().join("real.bin");
    std::fs::write(&target, b"real").unwrap();
    let link = source.join("Game.iso");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let cand = OrganisationCandidate {
        source_path: link.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: Some("Game.iso".to_string()),
    };
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Blocked);
}

#[test]
fn broken_symlink_object_may_be_moved_with_target_text_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let link = source.join("Game.iso");
    std::os::unix::fs::symlink(dir.path().join("nowhere.bin"), &link).unwrap();
    let cand = OrganisationCandidate {
        source_path: link.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: Some("Game.iso".to_string()),
    };
    let plan = plan_for(
        &master,
        OrganisationMode::OrganiseSymlinkOnly,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(&plan, &approved_of(&[&link]), &journal, &cancel);
    assert_eq!(
        outcome.transaction.state,
        crate::dat::rename_apply::TransactionState::Applied
    );
    let moved = master.join("psp").join("Game.iso");
    assert_eq!(
        std::fs::read_link(&moved).unwrap(),
        dir.path().join("nowhere.bin")
    );
}

#[test]
fn symlink_to_directory_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let target_dir = dir.path().join("target-dir");
    std::fs::create_dir_all(&target_dir).unwrap();
    let link = source.join("Game.iso");
    std::os::unix::fs::symlink(&target_dir, &link).unwrap();
    let cand = OrganisationCandidate {
        source_path: link.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: Some("Game.iso".to_string()),
    };
    let plan = plan_for(&master, OrganisationMode::OrganiseSymlinkOnly, &[cand], 1);
    assert_eq!(
        plan.entries[0].status,
        OrganisationStatus::Suggested,
        "a symlink-to-directory link object may be moved (the object itself, never the target)"
    );
    assert!(target_dir.exists(), "the target directory is never touched");
}

#[test]
fn a_directory_source_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let rom_dir = source.join("GameFolder");
    std::fs::create_dir_all(&rom_dir).unwrap();
    let cand = OrganisationCandidate {
        source_path: rom_dir.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: Some("GameFolder".to_string()),
    };
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Blocked);
}

// ---------------------------------------------------------------------------
// Cancellation, crash recovery
// ---------------------------------------------------------------------------

#[test]
fn cancellation_before_first_mutation_moves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let _cancel = cancelled();
    let result = apply_plan_result(
        &plan,
        &approved_of(&[&cand.source_path]),
        &journal,
        &_cancel,
    );
    assert!(result.is_err());
    assert!(cand.source_path.exists());
    assert!(
        !master.join("psp").exists(),
        "no platform directory was created"
    );
    assert!(
        !master.join("psp").join("Game.iso").exists(),
        "no file was moved"
    );
}

#[test]
fn no_filesystem_escape_from_the_master_root() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    // A hostile candidate whose canonical name attempts a traversal. The
    // derive step blocks it; the destination stays inside the master root.
    let mut cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    cand.canonical_name = Some("../escape.iso".to_string());
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_ne!(plan.entries[0].status, OrganisationStatus::Suggested);
    assert!(
        !dir.path().join("escape.iso").exists(),
        "a traversal name must never produce a file"
    );
    assert!(
        !master.join("..").join("escape.iso").exists(),
        "destination must never escape the master root"
    );
}

// ---------------------------------------------------------------------------
// Canonical-name supply (blocker: GUI never supplied canonical filenames)
// ---------------------------------------------------------------------------

#[test]
fn a_verified_canonical_name_produces_the_canonical_destination_filename() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let source_path = source.join("Game_ugly.iso");
    std::fs::write(&source_path, b"data").unwrap();
    let mut cand = OrganisationCandidate {
        source_path: source_path.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: None,
    };
    // Ugly source name + authoritative canonical name.
    cand.canonical_name = Some("Game (Europe).iso".to_string());
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let entry = &plan.entries[0];
    assert_eq!(entry.status, OrganisationStatus::Suggested);
    assert_eq!(
        entry.destination_path,
        master.join("psp").join("Game (Europe).iso"),
        "the canonical name must drive the destination filename"
    );
}

#[test]
fn rename_in_place_proposes_a_rename_when_the_canonical_name_differs() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let source_path = source.join("game_ugly.iso");
    std::fs::write(&source_path, b"data").unwrap();
    let mut cand = OrganisationCandidate {
        source_path: source_path.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: None,
    };
    cand.canonical_name = Some("Game (Europe).iso".to_string());
    let plan = plan_for(
        &master,
        OrganisationMode::RenameInPlace,
        std::slice::from_ref(&cand),
        1,
    );
    let entry = &plan.entries[0];
    assert_eq!(
        entry.status,
        OrganisationStatus::Suggested,
        "a differing canonical name must propose a rename, not read as already organised"
    );
    assert_eq!(entry.destination_path, source.join("Game (Europe).iso"));
}

#[test]
fn move_mode_uses_the_canonical_filename() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let source_path = source.join("whatever.nds");
    std::fs::write(&source_path, b"data").unwrap();
    let mut cand = OrganisationCandidate {
        source_path: source_path.clone(),
        resolution: resolved("Nintendo DS", PlatformIdentitySource::Romm),
        canonical_name: None,
    };
    cand.canonical_name = Some("Sonic Rush (Europe).nds".to_string());
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(
        plan.entries[0].destination_path,
        master.join("nds").join("Sonic Rush (Europe).nds")
    );
}

#[test]
fn no_canonical_evidence_falls_back_to_the_source_basename() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let source_path = source.join("Game_ugly.iso");
    std::fs::write(&source_path, b"data").unwrap();
    let cand = OrganisationCandidate {
        source_path: source_path.clone(),
        resolution: resolved("PSP", PlatformIdentitySource::Romm),
        canonical_name: None,
    };
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(
        plan.entries[0].destination_path,
        master.join("psp").join("Game_ugly.iso"),
        "without a canonical name the existing basename is preserved"
    );
}

#[test]
fn an_unverified_identity_never_supplies_an_authoritative_name() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let source_path = source.join("Game.iso");
    std::fs::write(&source_path, b"data").unwrap();
    // An Unknown identity with a (suspiciously supplied) canonical name must
    // still be Blocked - the name never rescues an unverified platform.
    let cand = OrganisationCandidate {
        source_path: source_path.clone(),
        resolution: PlatformIdentityResolution::Unknown { generation: 1 },
        canonical_name: Some("Trusted (USA).iso".to_string()),
    };
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(plan.entries[0].status, OrganisationStatus::Blocked);
}

// ---------------------------------------------------------------------------
// Directory ownership (blocker: pre-existing platform dir could be journalled
// as ArchiveFS-owned and removed by rollback)
// ---------------------------------------------------------------------------

#[test]
fn a_pre_existing_empty_platform_directory_is_never_recorded_as_owned() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    std::fs::create_dir_all(&master).unwrap();
    // The platform directory already exists (empty).
    let psp = master.join("psp");
    std::fs::create_dir(&psp).unwrap();
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(&plan, &approved_of(&[&cand.source_path]), &journal, &cancel);
    assert!(
        !outcome.transaction.created_directories.contains(&psp),
        "a pre-existing directory must never be recorded as owned: {:?}",
        outcome.transaction.created_directories
    );
    assert!(psp.exists(), "the pre-existing directory stays");

    // Rollback (and recovery, which relies on created_directories) never
    // removes it.
    let mut tx = outcome.transaction;
    let _ = rollback_organisation_transaction(&mut tx, &journal, &cancel, &master).unwrap();
    assert!(
        psp.exists(),
        "rollback never removes a pre-existing directory"
    );
}

#[test]
fn a_newly_created_platform_directory_is_recorded_as_owned_and_removed_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    std::fs::create_dir_all(&master).unwrap();
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(&plan, &approved_of(&[&cand.source_path]), &journal, &cancel);
    let psp = master.join("psp");
    assert!(
        outcome.transaction.created_directories.contains(&psp),
        "a directory this apply created must be recorded as owned"
    );
    let mut tx = outcome.transaction;
    let rollback = rollback_organisation_transaction(&mut tx, &journal, &cancel, &master).unwrap();
    assert!(
        rollback.directories_removed.contains(&psp),
        "the owned, now-empty directory is removed: {:?}",
        rollback.directories_removed
    );
    assert!(!psp.exists());
}

#[test]
fn two_files_sharing_one_created_platform_directory_own_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    std::fs::create_dir_all(&master).unwrap();
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let a = candidate(
        &source,
        "A.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let b = candidate(
        &source,
        "B.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        &[a.clone(), b.clone()],
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(
        &plan,
        &approved_of(&[&a.source_path, &b.source_path]),
        &journal,
        &cancel,
    );
    let psp = master.join("psp");
    let owned = outcome
        .transaction
        .created_directories
        .iter()
        .filter(|dir| **dir == psp)
        .count();
    assert_eq!(owned, 1, "one shared directory is owned once");
    let mut tx = outcome.transaction;
    let rollback = rollback_organisation_transaction(&mut tx, &journal, &cancel, &master).unwrap();
    assert!(rollback.directories_removed.contains(&psp));
    assert!(!psp.exists());
}

#[test]
fn partial_rollback_leaves_a_non_empty_directory_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    std::fs::create_dir_all(&master).unwrap();
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let cancel = no_cancel();
    let outcome = apply_plan(&plan, &approved_of(&[&cand.source_path]), &journal, &cancel);
    // A user file appears in the created directory after apply.
    let psp = master.join("psp");
    std::fs::write(psp.join("user-note.txt"), b"mine").unwrap();
    let mut tx = outcome.transaction;
    let rollback = rollback_organisation_transaction(&mut tx, &journal, &cancel, &master).unwrap();
    assert!(
        rollback.directories_remaining.contains(&psp),
        "a non-empty owned directory is reported as remaining: {:?}",
        rollback.directories_remaining
    );
    assert!(psp.exists());
    assert_eq!(std::fs::read(psp.join("user-note.txt")).unwrap(), b"mine");
}

#[test]
fn an_old_journal_without_the_created_directories_field_loads_safely() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    // An old-format journal with no created_directories key.
    std::fs::write(
        journal.join("old-1.json"),
        r#"{
  "transaction_id": "old-1",
  "plan_generation": 1,
  "created_at_unix": 1,
  "source_scan_root": "/tmp/roms",
  "state": "applying",
  "entries": []
}
"#,
    )
    .unwrap();
    let loaded =
        crate::dat::rename_apply::journal::read_journal(&journal.join("old-1.json")).unwrap();
    assert!(
        loaded.created_directories.is_empty(),
        "a legacy journal must default to no owned directories"
    );
}

#[test]
fn a_crash_between_create_dir_and_ownership_journal_is_conservative() {
    // Simulate: a platform directory exists but the journal never recorded it
    // as owned (the crash window between create_dir and the ownership write).
    // Recovery/rollback must not delete it.
    let dir = tempfile::tempdir().unwrap();
    let master = dir.path().join("roms");
    std::fs::create_dir_all(&master).unwrap();
    let psp = master.join("psp");
    std::fs::create_dir(&psp).unwrap();
    let source = dir.path().join("library");
    std::fs::create_dir_all(&source).unwrap();
    let cand = candidate(
        &source,
        "Game.iso",
        resolved("PSP", PlatformIdentitySource::Romm),
    );
    let plan = plan_for(
        &master,
        OrganisationMode::MoveRealFile,
        std::slice::from_ref(&cand),
        1,
    );
    let journal = dir.path().join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    let mut tx =
        build_organisation_transaction(&plan, &approved_of(&[&cand.source_path]), 1).unwrap();
    // The journal that was durable before the ownership write: created_directories empty.
    tx.state = crate::dat::rename_apply::TransactionState::Applying;
    crate::dat::rename_apply::journal::write_journal(&journal, &tx).unwrap();
    assert!(tx.created_directories.is_empty());

    let cancel = no_cancel();
    let _ = rollback_organisation_transaction(&mut tx, &journal, &cancel, &master).unwrap();
    assert!(
        psp.exists(),
        "an unproven (un-journalled) directory is never deleted by recovery"
    );
}

// ---------------------------------------------------------------------------
// Live identity revalidation (blocker: stale platform identity not revalidated)
// ---------------------------------------------------------------------------

use crate::database::Database;

/// Creates a database with `file` registered under its parent folder and a
/// platform assignment, returning the open handle (assignments must be made
/// while the mutable handle is alive).
fn db_with_assignment(dir: &Path, file: &Path, platform: &str, source: &str) -> Database {
    let db_path = dir.join("test.db");
    let mut db = Database::open_or_create(&db_path).unwrap();
    let folder = file.parent().unwrap();
    let registered = db.register_source_folders(&[folder.to_path_buf()]).unwrap();
    let archive = crate::Archive::from_path(file).unwrap();
    db.upsert_archive(registered[0].id, folder, &archive)
        .unwrap();
    let archive_id = db
        .find_archive_id_by_absolute_path(file)
        .unwrap()
        .expect("archive registered");
    db.assign_platform(archive_id, Some(platform), source)
        .unwrap();
    db
}

fn resolution_from_db(db: &Database, file: &Path, generation: u64) -> PlatformIdentityResolution {
    let archive_id = db.find_archive_id_by_absolute_path(file).unwrap().unwrap();
    let evidence = db
        .current_platform_identity_evidence(archive_id, generation)
        .unwrap();
    resolve_platform_identity(generation, evidence)
}

fn plan_from_live_db(
    dir: &Path,
    file: &Path,
    db: &Database,
    mode: OrganisationMode,
    generation: u64,
) -> OrganisationPlan {
    let master = dir.join("roms");
    std::fs::create_dir_all(&master).unwrap();
    let cand = OrganisationCandidate {
        source_path: file.to_path_buf(),
        resolution: resolution_from_db(db, file, generation),
        canonical_name: None,
    };
    plan_for(&master, mode, &[cand], generation)
}

#[test]
fn a_platform_changed_by_another_process_rejects_the_stale_apply() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("library");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source_file = source_dir.join("Game.iso");
    std::fs::write(&source_file, b"data").unwrap();

    let mut db = db_with_assignment(dir.path(), &source_file, "PSP", "romm");
    let plan = plan_from_live_db(
        dir.path(),
        &source_file,
        &db,
        OrganisationMode::MoveRealFile,
        1,
    );
    assert_eq!(plan.entries[0].platform.as_deref(), Some("PSP"));
    assert_eq!(plan.entries[0].slug.as_deref(), Some("psp"));

    // Another ArchiveFS process changes PSP -> PS1 after planning.
    let archive_id = db
        .find_archive_id_by_absolute_path(&source_file)
        .unwrap()
        .unwrap();
    db.assign_platform(archive_id, Some("PS1"), "romm").unwrap();
    drop(db);

    let master = dir.path().join("roms");
    let error = revalidate_organisation_plan(
        &plan,
        &dir.path().join("test.db"),
        &|_| None,
        &slug_for_platform,
    )
    .unwrap_err();
    assert!(error.contains("changed"), "{error}");
    assert!(source_file.exists(), "zero mutation");
    assert!(!master.join("psp").exists(), "zero mutation");
}

#[test]
fn a_resolved_identity_becoming_unresolvable_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("library");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source_file = source_dir.join("Game.iso");
    std::fs::write(&source_file, b"data").unwrap();

    let mut db = db_with_assignment(dir.path(), &source_file, "PSP", "romm");
    let plan = plan_from_live_db(
        dir.path(),
        &source_file,
        &db,
        OrganisationMode::MoveRealFile,
        1,
    );

    // Another process replaces the assignment with text that cannot resolve
    // to a canonical platform: the identity becomes Unknown.
    let archive_id = db
        .find_archive_id_by_absolute_path(&source_file)
        .unwrap()
        .unwrap();
    db.assign_platform(archive_id, Some("not-a-registered-platform"), "romm")
        .unwrap();
    drop(db);
    let error = revalidate_organisation_plan(
        &plan,
        &dir.path().join("test.db"),
        &|_| None,
        &slug_for_platform,
    )
    .unwrap_err();
    assert!(error.contains("changed"), "{error}");
}

#[test]
fn a_changed_slug_mapping_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("library");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source_file = source_dir.join("Game.iso");
    std::fs::write(&source_file, b"data").unwrap();

    let db = db_with_assignment(dir.path(), &source_file, "PSP", "romm");
    let plan = plan_from_live_db(
        dir.path(),
        &source_file,
        &db,
        OrganisationMode::MoveRealFile,
        1,
    );
    assert_eq!(plan.entries[0].slug.as_deref(), Some("psp"));

    // The slug mapping changes: PSP now maps to "psp-portable".
    let changed_slug = |platform: &str| -> Option<String> {
        Some(match platform {
            "PSP" => "psp-portable".to_string(),
            other => slug_for_platform(other).unwrap_or_default(),
        })
    };
    let error =
        revalidate_organisation_plan(&plan, &dir.path().join("test.db"), &|_| None, &changed_slug)
            .unwrap_err();
    assert!(error.contains("changed"), "{error}");
}

#[test]
fn a_changed_canonical_name_is_rejected_when_the_destination_changes() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("library");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source_file = source_dir.join("Game_ugly.iso");
    std::fs::write(&source_file, b"data").unwrap();

    let db = db_with_assignment(dir.path(), &source_file, "PSP", "romm");
    // Plan with a canonical name.
    let master = dir.path().join("roms");
    std::fs::create_dir_all(&master).unwrap();
    let cand = OrganisationCandidate {
        source_path: source_file.clone(),
        resolution: resolution_from_db(&db, &source_file, 1),
        canonical_name: Some("Game (Europe).iso".to_string()),
    };
    let plan = plan_for(&master, OrganisationMode::MoveRealFile, &[cand], 1);
    assert_eq!(
        plan.entries[0].destination_path,
        master.join("psp").join("Game (Europe).iso")
    );

    // The authoritative canonical name changes between plan and apply.
    let changed_name = |path: &Path| -> Option<String> {
        if path == source_file {
            Some("Game (Japan).iso".to_string())
        } else {
            None
        }
    };
    let error = revalidate_organisation_plan(
        &plan,
        &dir.path().join("test.db"),
        &changed_name,
        &slug_for_platform,
    )
    .unwrap_err();
    assert!(error.contains("changed"), "{error}");
}

#[test]
fn an_unchanged_live_identity_passes_revalidation() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("library");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source_file = source_dir.join("Game.iso");
    std::fs::write(&source_file, b"data").unwrap();

    let db = db_with_assignment(dir.path(), &source_file, "PSP", "romm");
    let plan = plan_from_live_db(
        dir.path(),
        &source_file,
        &db,
        OrganisationMode::MoveRealFile,
        1,
    );
    drop(db);
    revalidate_organisation_plan(
        &plan,
        &dir.path().join("test.db"),
        &|_| None,
        &slug_for_platform,
    )
    .expect("an unchanged live identity must pass");
}

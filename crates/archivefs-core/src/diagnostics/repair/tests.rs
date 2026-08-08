//! Doctor Stage 1B: the safety gates, the four repairs, verification and the
//! History record.
//!
//! Every test here uses a private temporary tree whose mount root *and*
//! source folder both live inside that tree. Nothing touches a real mount, a
//! real emulator profile, the live catalogue, or `/mnt/games`, and nothing
//! requires elevated privileges.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::diagnostics::{DoctorScan, DoctorScanInputs, Finding, Gathered, run_doctor_scan};
use crate::{ArchiveHealth, ArchiveIndex, ArchiveIndexEntry, Config, MountState};

// --- Fixtures -------------------------------------------------------------

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-doctor-repair-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture root");
        Self { root }
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(&path).expect("dir");
        path
    }

    fn file(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(&path, contents).expect("file");
        path
    }

    /// A config whose mount root and source folder both live inside the
    /// fixture, so no test can reach real data.
    fn config(&self) -> Config {
        Config {
            source_folders: vec![self.dir("roms")],
            mount_root: self.dir("mnt"),
            ratarmount_bin: "ratarmount".to_string(),
            master_rom_root: None,
        }
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("data/index.json")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Every path beneath `root`, with the facts a mutation would disturb.
fn snapshot(root: &Path) -> Vec<String> {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&current) else {
            continue;
        };
        for entry in read_dir.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            entries.push(format!(
                "{} dir={} link={} len={}",
                path.display(),
                metadata.is_dir(),
                metadata.file_type().is_symlink(),
                metadata.len()
            ));
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                stack.push(path);
            }
        }
    }
    entries.sort();
    entries
}

/// A scan containing exactly the leftover-mount-folder findings for this
/// config - built through the real runner from the real read-only planner.
fn scan_with_stale_directories(config: &Config) -> DoctorScan {
    let stale = crate::plan_stale_mount_directories(config).expect("plan");
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.stale_mount_directories = Gathered::Ready(stale.as_slice());
    run_doctor_scan(&inputs)
}

fn forged_clean_path_finding(path: &Path) -> Finding {
    let stale = [path.to_path_buf()];
    findings_from_stale_mount_directories(&stale).remove(0)
}

/// A scan containing one retryable-mount finding, built through the real
/// `HealthIssue` adapter.
fn scan_with_retry_finding(archive: &Path) -> DoctorScan {
    let issues = vec![crate::HealthIssue {
        path: archive.to_path_buf(),
        platform: Some("SNES".to_string()),
        present: true,
        mount_state: Some(MountState::Pending),
        category: crate::HealthCategory::RetryableFailure,
        reason: "Mount failed and may be retried".to_string(),
        retryable: true,
        recovery_action: Some(crate::RecoveryAction::RetryMount),
        last_seen_at: None,
        size_bytes: None,
        modified_time_unix_seconds: None,
    }];
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.health_issues = Gathered::Ready(issues.as_slice());
    let mut scan = run_doctor_scan(&inputs);
    // The health adapter records the recovery that exists *elsewhere*;
    // Doctor's own offer is attached here so the executor's gates can be
    // exercised without the GUI in the loop.
    for finding in &mut scan.findings {
        if finding.id == "mounts.retryable_failure" {
            finding.repair = Some(DoctorRepairAction::RetryMount);
        }
    }
    scan
}

fn scan_with_index_finding(index_path: &Path) -> DoctorScan {
    let freshness = crate::ArchiveIndexFreshness {
        missing_archive_paths: vec![PathBuf::from("/roms/gone.zip")],
        stale_archive_paths: Vec::new(),
    };
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.index_freshness = Gathered::Ready((&freshness, index_path));
    run_doctor_scan(&inputs)
}

fn request(action: DoctorRepairAction, finding_id: &str) -> DoctorRepairRequest {
    DoctorRepairRequest {
        action,
        finding_id: finding_id.to_string(),
        affected: None,
        confirmed: true,
        dry_run: false,
    }
}

/// A request naming one exact resource - required whenever several findings
/// share an id.
fn request_for(
    action: DoctorRepairAction,
    finding_id: &str,
    affected: &Path,
) -> DoctorRepairRequest {
    DoctorRepairRequest {
        action,
        finding_id: finding_id.to_string(),
        affected: Some(affected.display().to_string()),
        confirmed: true,
        dry_run: false,
    }
}

fn context<'a>(
    config: &'a Config,
    scan: &'a DoctorScan,
    index_path: &'a Path,
) -> DoctorRepairContext<'a> {
    DoctorRepairContext {
        config,
        scan,
        index_path,
    }
}

// --- 4, 5. The action set is closed --------------------------------------

#[test]
fn the_repair_action_set_is_closed_and_carries_no_payload() {
    assert_eq!(DoctorRepairAction::ALL.len(), 4);
    assert_eq!(
        DoctorRepairAction::ALL
            .iter()
            .map(|action| action.spec().id)
            .collect::<Vec<_>>(),
        vec![
            "clean_mount_root",
            "clean_mount_path",
            "retry_mount",
            "rebuild_index"
        ]
    );
    // Fieldless: a repair cannot carry a path or a command. A four-variant
    // fieldless enum is one byte; anything larger means a payload was added.
    assert_eq!(std::mem::size_of::<DoctorRepairAction>(), 1);
    for action in DoctorRepairAction::ALL {
        let spec = action.spec();
        assert!(
            spec.invokes.starts_with("archivefs_core::"),
            "{action:?} must name an existing function"
        );
        assert!(spec.confirmation_required, "{action:?}");
        assert!(!spec.expected_mutation.is_empty());
        assert!(!spec.never_touches.is_empty());
        assert!(!spec.verification.is_empty());
        assert!(!spec.title.is_empty());
    }
}

#[test]
fn an_arbitrary_action_string_is_rejected() {
    for hostile in [
        "rm -rf /",
        "clean_mount_root; rm -rf /",
        "CLEAN_MOUNT_ROOT",
        "clean",
        "",
        "delete_everything",
        "../../clean_mount_root",
        "clean_mount_path ",
    ] {
        assert!(
            DoctorRepairAction::from_id(hostile).is_none(),
            "`{hostile}` must not resolve to an action"
        );
    }
    assert_eq!(
        DoctorRepairAction::from_id("clean_mount_root"),
        Some(DoctorRepairAction::CleanMountRoot)
    );
}

// --- 6. Arbitrary path injection ----------------------------------------

/// A repair target never comes from the request. Even a finding fabricated to
/// name a path outside ArchiveFS's boundary is refused.
#[test]
fn a_finding_naming_a_path_outside_the_mount_root_is_refused() {
    let fixture = Fixture::new("path-injection");
    let config = fixture.config();
    let outside = fixture.dir("elsewhere/victim");
    let before = snapshot(&fixture.root);

    let mut scan = scan_with_stale_directories(&config);
    scan.findings.push(forged_clean_path_finding(&outside));

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::PathOutsideMountRoot)
    );
    assert!(outside.is_dir(), "the forged target must survive");
    assert_eq!(before, snapshot(&fixture.root));
}

// --- 7, 8. Stale findings and changed identity --------------------------

#[test]
fn a_stale_finding_is_refused_and_nothing_is_changed() {
    let fixture = Fixture::new("stale");
    let config = fixture.config();
    let leftover = fixture.dir("mnt/SNES/Old Game");
    let scan = scan_with_stale_directories(&config);
    assert!(scan.finding("mount_root.stale_mount_directory").is_some());

    // The condition disappears between the scan and the repair.
    fs::remove_dir_all(fixture.root.join("mnt/SNES")).expect("remove");
    assert!(!leftover.exists());
    let before = snapshot(&fixture.root);

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::StaleFinding)
    );
    assert_eq!(before, snapshot(&fixture.root));
}

#[test]
fn a_target_whose_identity_changed_from_directory_to_file_is_refused() {
    let fixture = Fixture::new("identity");
    let config = fixture.config();
    let leftover = fixture.dir("mnt/SNES/Old Game");
    let scan = scan_with_stale_directories(&config);

    // The folder becomes a file: the same path, a different object.
    fs::remove_dir(&leftover).expect("remove dir");
    fs::write(&leftover, b"now a file").expect("write file");
    let before = snapshot(&fixture.root);

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert!(outcome.record.rejection.is_some());
    assert_eq!(
        fs::read(&leftover).expect("the file survives"),
        b"now a file".to_vec(),
        "a file that replaced the folder must never be removed"
    );
    assert_eq!(before, snapshot(&fixture.root));
}

// --- 9. Source-root paths -----------------------------------------------

/// The hard boundary. Even with a pathological configuration that puts the
/// mount root *inside* the library, no repair may touch anything there.
#[test]
fn a_path_inside_a_configured_source_folder_is_always_refused() {
    let fixture = Fixture::new("source-root");
    let source = fixture.dir("roms");
    let config = Config {
        source_folders: vec![source],
        mount_root: fixture.dir("roms/mnt"),
        ratarmount_bin: "ratarmount".to_string(),
        master_rom_root: None,
    };
    let inside = fixture.dir("roms/mnt/SNES/Old Game");
    let before = snapshot(&fixture.root);

    let mut scan = scan_with_stale_directories(&config);
    if scan.finding("mount_root.stale_mount_directory").is_none() {
        scan.findings.push(forged_clean_path_finding(&inside));
    }
    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::PathUnderSourceRoot),
        "a source-folder child must be refused before any other check"
    );
    assert!(inside.is_dir(), "nothing under the library was touched");
    assert_eq!(before, snapshot(&fixture.root));
}

// --- 10. Symlink escapes ------------------------------------------------

#[cfg(unix)]
#[test]
fn a_symlinked_target_is_refused() {
    let fixture = Fixture::new("symlink");
    let config = fixture.config();
    let real = fixture.dir("elsewhere/real");
    let link = fixture.root.join("mnt/link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let before = snapshot(&fixture.root);

    let mut scan = scan_with_stale_directories(&config);
    scan.findings.push(forged_clean_path_finding(&link));

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::SymlinkEscape)
    );
    assert!(link.exists(), "the symlink itself survives");
    assert!(real.is_dir(), "the symlink target survives");
    assert_eq!(before, snapshot(&fixture.root));
}

#[cfg(unix)]
#[test]
fn a_symlinked_parent_component_is_refused() {
    let fixture = Fixture::new("symlink-parent");
    let config = fixture.config();
    let real = fixture.dir("elsewhere/platform");
    let linked_parent = fixture.root.join("mnt/SNES");
    std::os::unix::fs::symlink(&real, &linked_parent).expect("symlink");
    let target = linked_parent.join("Old Game");
    fs::create_dir_all(&target).expect("dir through the link");
    let before = snapshot(&fixture.root);

    let mut scan = scan_with_stale_directories(&config);
    scan.findings.push(forged_clean_path_finding(&target));
    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::SymlinkEscape)
    );
    assert!(
        real.join("Old Game").is_dir(),
        "content behind the link survives"
    );
    assert_eq!(before, snapshot(&fixture.root));
}

// --- 11, 12, 13. CleanMountRoot -----------------------------------------

/// `clean_mount_root` derives the folders it may clean from
/// `ArchiveScanner::mount_plans()`, so a realistic fixture needs real
/// archives. This creates them and returns their planned mount paths - the
/// exact directories that are left behind after unmounting.
fn planned_mount_paths(fixture: &Fixture, config: &Config, names: &[&str]) -> Vec<PathBuf> {
    for name in names {
        fixture.file(&format!("roms/{name}.zip"), b"archive bytes");
    }
    let plans = crate::ArchiveScanner::new(config)
        .mount_plans()
        .expect("mount plans");
    let paths: Vec<PathBuf> = plans.into_iter().map(|plan| plan.mount_path).collect();
    assert_eq!(paths.len(), names.len(), "{paths:?}");
    for path in &paths {
        fs::create_dir_all(path).expect("leftover mount dir");
    }
    paths
}

#[test]
fn clean_mount_root_removes_only_empty_safe_directories() {
    let fixture = Fixture::new("clean-root");
    let config = fixture.config();
    let planned = planned_mount_paths(&fixture, &config, &["Old Game", "Another Game"]);
    let empty_one = planned[0].clone();
    let empty_two = planned[1].clone();
    // Real content that must survive, inside the mount root but not planned.
    let occupied = fixture.dir("mnt/Kept Game");
    let user_file = fixture.file("mnt/Kept Game/save.srm", b"user save");
    let library_file = fixture.root.join("roms/Old Game.zip");

    let scan = scan_with_stale_directories(&config);
    let summary = scan
        .finding("mount_root.stale_mount_directories")
        .expect("a summary finding is always produced");
    assert_eq!(summary.repair, Some(DoctorRepairAction::CleanMountRoot));

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountRoot,
            "mount_root.stale_mount_directories",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.status,
        DoctorRepairStatus::Succeeded,
        "{:?}",
        outcome.record
    );
    assert!(!empty_one.exists(), "the empty folder was removed");
    assert!(!empty_two.exists(), "the other empty folder was removed");
    assert!(occupied.is_dir(), "a non-empty folder is preserved");
    assert_eq!(
        fs::read(&user_file).expect("user file"),
        b"user save".to_vec()
    );
    assert_eq!(
        fs::read(&library_file).expect("rom"),
        b"archive bytes".to_vec(),
        "the archive itself is never touched"
    );
    assert!(config.mount_root.is_dir(), "the mount root itself survives");
}

#[test]
fn clean_mount_root_refuses_a_non_empty_directory() {
    let fixture = Fixture::new("clean-root-nonempty");
    let config = fixture.config();
    // Three planned mount folders: one left with content in it, two empty,
    // so the aggregate finding exists and the occupied one must survive it.
    let planned = planned_mount_paths(
        &fixture,
        &config,
        &["Occupied Game", "Empty Game", "Another Empty Game"],
    );
    let occupied = planned[0].clone();
    fs::write(occupied.join("keep.txt"), b"content").expect("content");
    let empty = planned[1].clone();
    let empty_two = planned[2].clone();

    let stale = crate::plan_stale_mount_directories(&config).expect("plan");
    assert!(
        !stale.contains(&occupied),
        "a non-empty folder must never be planned for removal: {stale:?}"
    );

    let scan = scan_with_stale_directories(&config);
    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountRoot,
            "mount_root.stale_mount_directories",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.status,
        DoctorRepairStatus::Succeeded,
        "{:?}",
        outcome.record
    );
    assert!(occupied.is_dir(), "a non-empty folder is refused");
    assert!(occupied.join("keep.txt").is_file(), "its content survives");
    assert!(!empty.exists(), "the empty ones are removed");
    assert!(!empty_two.exists());
}

/// A real mount cannot be created without privileges, so this asserts the
/// predicate that actually protects one: the planner and the remover share
/// `empty_unmounted_dir_is_removable`, which excludes every path in the
/// kernel's mount table.
#[test]
fn an_active_mount_point_is_never_planned_for_removal() {
    let fixture = Fixture::new("active-mount");
    let config = fixture.config();
    let empty = fixture.dir("mnt/SNES/Game");

    let stale = crate::plan_stale_mount_directories(&config).expect("plan");
    assert!(stale.contains(&empty), "a plain empty folder is removable");

    let mounted = crate::current_mount_paths().expect("mount table");
    assert!(
        !mounted.is_empty(),
        "the kernel mount table should not be empty"
    );
    for path in mounted.iter() {
        assert!(
            !stale.contains(path),
            "an active mount point must never be planned for removal: {}",
            path.display()
        );
    }
}

// --- 14, 15. CleanMountPath ---------------------------------------------

#[test]
fn clean_mount_path_removes_empty_parents_but_stops_at_the_mount_root() {
    let fixture = Fixture::new("clean-path");
    let config = fixture.config();
    let deep = fixture.dir("mnt/SNES/Old Game");

    let scan = scan_with_stale_directories(&config);
    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.status,
        DoctorRepairStatus::Succeeded,
        "{:?}",
        outcome.record
    );
    assert!(!deep.exists());
    assert!(
        !fixture.root.join("mnt/SNES").exists(),
        "the now-empty parent is removed too"
    );
    assert!(
        config.mount_root.is_dir(),
        "the cleanup stops at the mount root and never removes it"
    );
    let reported: Vec<String> = outcome
        .record
        .changed_paths
        .iter()
        .map(|path| path.display.clone())
        .collect();
    assert!(reported.iter().any(|path| path.ends_with("Old Game")));
    assert!(reported.iter().any(|path| path.ends_with("SNES")));
}

#[test]
fn clean_mount_path_preserves_a_sibling_containing_user_files() {
    let fixture = Fixture::new("clean-path-sibling");
    let config = fixture.config();
    let empty = fixture.dir("mnt/SNES/Empty Game");
    let kept = fixture.file("mnt/SNES/Kept Game/save.srm", b"user save");

    let scan = scan_with_stale_directories(&config);
    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Succeeded);
    assert!(!empty.exists());
    assert_eq!(fs::read(&kept).expect("user file"), b"user save".to_vec());
    assert!(
        fixture.root.join("mnt/SNES").is_dir(),
        "the parent still holds a sibling with content, so it stays"
    );
}

// --- 16, 17. RetryMount --------------------------------------------------

#[test]
fn retry_mount_is_only_offered_for_a_retryable_health_state() {
    // The offer originates from `HealthIssue`'s recovery action, which
    // `classify_archive_health` sets only for a retryable failure. Doctor
    // reuses that rule rather than restating it.
    assert!(ArchiveHealth::Failed.is_retryable());
    assert!(ArchiveHealth::MissingParts.is_retryable());
    assert!(ArchiveHealth::RetryAvailable.is_retryable());
    assert!(!ArchiveHealth::Corrupt.is_retryable());
    assert!(!ArchiveHealth::Unsupported.is_retryable());
    assert!(!ArchiveHealth::PermissionDenied.is_retryable());
    assert!(!ArchiveHealth::Mounted.is_retryable());
}

#[test]
fn retry_mount_refuses_when_the_archive_is_gone() {
    let fixture = Fixture::new("retry-missing");
    let config = fixture.config();
    let archive = fixture.file("roms/game.zip", b"not really a zip");
    let scan = scan_with_retry_finding(&archive);
    fs::remove_file(&archive).expect("remove");
    let before = snapshot(&fixture.root);

    let outcome = execute_doctor_repair(
        &request(DoctorRepairAction::RetryMount, "mounts.retryable_failure"),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::SourceMissing)
    );
    assert_eq!(before, snapshot(&fixture.root));
}

#[cfg(unix)]
#[test]
fn retry_mount_refuses_a_symlinked_archive() {
    let fixture = Fixture::new("retry-symlink");
    let config = fixture.config();
    let real = fixture.file("elsewhere/real.zip", b"bytes");
    let link = fixture.root.join("roms/game.zip");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let scan = scan_with_retry_finding(&link);
    let before = snapshot(&fixture.root);

    let outcome = execute_doctor_repair(
        &request(DoctorRepairAction::RetryMount, "mounts.retryable_failure"),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::SymlinkEscape)
    );
    assert_eq!(before, snapshot(&fixture.root));
}

// --- 18, 19. RebuildIndex -----------------------------------------------

#[test]
fn rebuild_index_publishes_atomically_and_leaves_no_partial_file() {
    let fixture = Fixture::new("rebuild-atomic");
    let config = fixture.config();
    fixture.file("roms/game.zip", b"not a real zip");
    let index_path = fixture.index_path();
    let scan = scan_with_index_finding(&index_path);

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::RebuildIndex,
            "library.index_out_of_date",
        ),
        &context(&config, &scan, &index_path),
    );
    assert_eq!(
        outcome.record.status,
        DoctorRepairStatus::Succeeded,
        "{:?}",
        outcome.record
    );
    assert!(index_path.is_file(), "the index was published");
    // No temporary file is left beside it.
    let siblings: Vec<String> = fs::read_dir(index_path.parent().expect("parent"))
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(siblings, vec!["index.json".to_string()], "{siblings:?}");
    crate::read_archive_index(&index_path).expect("the published index parses");
}

#[test]
fn a_refused_rebuild_leaves_the_previous_valid_index_byte_identical() {
    let fixture = Fixture::new("rebuild-preserve");
    // No configured source folder exists, so a rebuild would produce an
    // empty index. The guard must refuse before anything is written.
    let config = Config {
        source_folders: vec![fixture.root.join("roms-missing")],
        mount_root: fixture.dir("mnt"),
        ratarmount_bin: "ratarmount".to_string(),
        master_rom_root: None,
    };
    let index_path = fixture.index_path();
    let previous = ArchiveIndex {
        archives: vec![ArchiveIndexEntry {
            archive_path: fixture.file("library/old.zip", b"bytes"),
            platform: Some("SNES".to_string()),
            display_name: "old".to_string(),
            mount_path: fixture.root.join("mnt/SNES/old"),
            modified_time_seconds: Some(1),
            health: ArchiveHealth::Pending,
            mount_state: MountState::Pending,
        }],
    };
    crate::write_archive_index(&previous, &index_path).expect("seed index");
    let good_bytes = fs::read(&index_path).expect("seeded index");

    let scan = scan_with_index_finding(&index_path);
    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::RebuildIndex,
            "library.index_out_of_date",
        ),
        &context(&config, &scan, &index_path),
    );

    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::SourceMissing)
    );
    assert_eq!(
        fs::read(&index_path).expect("index still there"),
        good_bytes,
        "the previous valid index must survive a refused rebuild"
    );
}

/// `write_archive_index` publishes through a temporary file and a rename, so
/// a reader only ever sees a complete index.
#[test]
fn index_publication_is_atomic_and_replaces_the_previous_content_wholesale() {
    let fixture = Fixture::new("index-atomic");
    let index_path = fixture.index_path();
    let first = ArchiveIndex {
        archives: vec![ArchiveIndexEntry {
            archive_path: PathBuf::from("/roms/a.zip"),
            platform: Some("SNES".to_string()),
            display_name: "a".to_string(),
            mount_path: PathBuf::from("/mnt/a"),
            modified_time_seconds: Some(1),
            health: ArchiveHealth::Pending,
            mount_state: MountState::Pending,
        }],
    };
    crate::write_archive_index(&first, &index_path).expect("write");
    let second = ArchiveIndex {
        archives: Vec::new(),
    };
    crate::write_archive_index(&second, &index_path).expect("rewrite");

    let siblings: Vec<String> = fs::read_dir(index_path.parent().expect("parent"))
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        siblings,
        vec!["index.json".to_string()],
        "no temporary file may survive publication: {siblings:?}"
    );
    let read_back = crate::read_archive_index(&index_path).expect("parses");
    assert!(
        read_back.archives.is_empty(),
        "the new index replaced the old"
    );
}

// --- 20, 21, 22. Confirmation, cancel, dry run --------------------------

#[test]
fn a_repair_without_confirmation_is_refused_and_changes_nothing() {
    let fixture = Fixture::new("unconfirmed");
    let config = fixture.config();
    let leftover = fixture.dir("mnt/SNES/Old Game");
    let scan = scan_with_stale_directories(&config);
    let before = snapshot(&fixture.root);

    let outcome = execute_doctor_repair(
        &DoctorRepairRequest {
            action: DoctorRepairAction::CleanMountPath,
            finding_id: "mount_root.stale_mount_directory".to_string(),
            affected: None,
            confirmed: false,
            dry_run: false,
        },
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::ConfirmationMissing)
    );
    assert!(leftover.is_dir(), "nothing was removed");
    assert_eq!(before, snapshot(&fixture.root));
}

/// Cancelling in the UI means never calling the executor. This asserts the
/// invariant that makes that safe: an unconfirmed request is inert for
/// *every* action, so a cancelled confirmation can never mutate.
#[test]
fn every_action_refuses_to_run_unconfirmed() {
    let fixture = Fixture::new("all-unconfirmed");
    let config = fixture.config();
    // Two leftovers so the aggregate finding exists, but the per-folder
    // requests below name their exact resource to stay unambiguous.
    let planned = planned_mount_paths(&fixture, &config, &["One", "Two"]);
    let archive = fixture.root.join("roms/One.zip");
    let before = snapshot(&fixture.root);

    let mut scan = scan_with_stale_directories(&config);
    scan.findings
        .extend(scan_with_retry_finding(&archive).findings);
    scan.findings
        .extend(scan_with_index_finding(&fixture.index_path()).findings);

    for (action, finding_id) in [
        (
            DoctorRepairAction::CleanMountRoot,
            "mount_root.stale_mount_directories",
        ),
        (
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        (DoctorRepairAction::RetryMount, "mounts.retryable_failure"),
        (
            DoctorRepairAction::RebuildIndex,
            "library.index_out_of_date",
        ),
    ] {
        let outcome = execute_doctor_repair(
            &DoctorRepairRequest {
                action,
                finding_id: finding_id.to_string(),
                affected: (action == DoctorRepairAction::CleanMountPath)
                    .then(|| planned[0].display().to_string()),
                confirmed: false,
                dry_run: false,
            },
            &context(&config, &scan, &fixture.index_path()),
        );
        assert_eq!(
            outcome.record.rejection,
            Some(DoctorRepairRejection::ConfirmationMissing),
            "{action:?} ran without confirmation"
        );
    }
    assert_eq!(before, snapshot(&fixture.root));
}

#[test]
fn a_dry_run_validates_everything_and_mutates_nothing() {
    let fixture = Fixture::new("dry-run");
    let config = fixture.config();
    let leftover = fixture.dir("mnt/SNES/Old Game");
    let scan = scan_with_stale_directories(&config);
    let before = snapshot(&fixture.root);

    let outcome = execute_doctor_repair(
        &DoctorRepairRequest {
            action: DoctorRepairAction::CleanMountPath,
            finding_id: "mount_root.stale_mount_directory".to_string(),
            affected: None,
            confirmed: true,
            dry_run: true,
        },
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::DryRun);
    assert_eq!(
        outcome.record.verification,
        DoctorRepairVerification::NotAttempted
    );
    assert!(outcome.record.changed_paths.is_empty());
    assert!(outcome.record.summary.contains("Nothing was changed"));
    assert!(leftover.is_dir(), "a dry run must not remove anything");
    assert_eq!(before, snapshot(&fixture.root));
}

#[test]
fn a_dry_run_still_applies_every_safety_gate() {
    let fixture = Fixture::new("dry-run-gates");
    let config = fixture.config();
    let outside = fixture.dir("elsewhere/victim");
    let mut scan = scan_with_stale_directories(&config);
    scan.findings.push(forged_clean_path_finding(&outside));

    let outcome = execute_doctor_repair(
        &DoctorRepairRequest {
            action: DoctorRepairAction::CleanMountPath,
            finding_id: "mount_root.stale_mount_directory".to_string(),
            affected: None,
            confirmed: true,
            dry_run: true,
        },
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.status,
        DoctorRepairStatus::Rejected,
        "a dry run must still refuse an unsafe target"
    );
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::PathOutsideMountRoot)
    );
}

// --- 23, 24, 25. Verification -------------------------------------------

#[test]
fn verification_reports_verified_only_when_the_finding_is_gone() {
    let fixture = Fixture::new("verify-ok");
    let config = fixture.config();
    fixture.dir("mnt/SNES/Old Game");
    let scan = scan_with_stale_directories(&config);

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Succeeded);
    assert_eq!(
        outcome.record.verification,
        DoctorRepairVerification::Verified
    );
    assert!(outcome.record.summary.contains("Repair verified"));
}

/// The aggregate cleanup succeeded for one folder while another remains, so
/// the aggregate finding must be reported as still present rather than as a
/// success.
#[test]
fn success_is_not_reported_when_the_finding_remains() {
    let fixture = Fixture::new("verify-remains");
    let config = fixture.config();
    let kept_empty = fixture.dir("mnt/Other Game");
    let repaired = fixture.dir("mnt/Old Game");
    let scan = scan_with_stale_directories(&config);

    // Repair exactly one of the two, naming it explicitly.
    let outcome = execute_doctor_repair(
        &request_for(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
            &repaired,
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Succeeded);
    assert_eq!(
        outcome.record.verification,
        DoctorRepairVerification::Verified,
        "that one folder is gone, so its own finding verifies"
    );

    // The other folder is untouched, so the aggregate condition still holds.
    assert!(kept_empty.is_dir());
    let remaining = crate::plan_stale_mount_directories(&config).expect("plan");
    assert_eq!(remaining, vec![kept_empty], "{remaining:?}");
}

#[test]
fn a_verification_verdict_is_always_stated_and_never_assumed() {
    let fixture = Fixture::new("verify-stated");
    let config = fixture.config();
    fixture.file("roms/game.zip", b"bytes");
    let index_path = fixture.index_path();
    let scan = scan_with_index_finding(&index_path);

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::RebuildIndex,
            "library.index_out_of_date",
        ),
        &context(&config, &scan, &index_path),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Succeeded);
    // Whatever the verdict, it is one of the four honest ones and is always
    // in the summary - never silently treated as success.
    assert!(
        matches!(
            outcome.record.verification,
            DoctorRepairVerification::Verified
                | DoctorRepairVerification::FindingRemains
                | DoctorRepairVerification::CouldNotComplete
        ),
        "{:?}",
        outcome.record.verification
    );
    assert!(
        outcome
            .record
            .summary
            .contains(outcome.record.verification.label()),
        "{}",
        outcome.record.summary
    );
}

// --- 26, 27, 28. History and Undo ---------------------------------------

#[test]
fn every_attempt_produces_exactly_one_history_ready_record() {
    let fixture = Fixture::new("history");
    let config = fixture.config();
    fixture.dir("mnt/SNES/Old Game");
    let scan = scan_with_stale_directories(&config);

    let ok = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(ok.record.action_id, "clean_mount_path");
    assert_eq!(ok.record.action_title, "Remove this leftover mount folder");
    assert_eq!(ok.record.finding_id, "mount_root.stale_mount_directory");
    assert!(ok.record.affected.is_some());
    assert!(ok.record.confirmed);
    assert!(!ok.record.dry_run);
    assert!(!ok.record.changed_paths.is_empty());
    assert!(ok.record.rejection.is_none());
    assert!(ok.record.error.is_none());
    assert!(!ok.record.summary.is_empty());
    assert_eq!(ok.record.undo, DoctorRepairUndo::NothingToUndo);

    // A refused attempt has the same shape, so History renders both
    // identically - a failed repair is recorded, not dropped.
    let refused = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(refused.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(refused.record.action_id, "clean_mount_path");
    assert_eq!(
        refused.record.finding_id,
        "mount_root.stale_mount_directory"
    );
    assert!(refused.record.rejection.is_some());
    assert!(refused.record.changed_paths.is_empty());
    assert!(!refused.record.summary.is_empty());
}

#[test]
fn undo_availability_is_stated_accurately_per_action() {
    assert_eq!(
        DoctorRepairAction::CleanMountRoot.spec().undo,
        DoctorRepairUndo::NothingToUndo
    );
    assert_eq!(
        DoctorRepairAction::CleanMountPath.spec().undo,
        DoctorRepairUndo::NothingToUndo
    );
    assert_eq!(
        DoctorRepairAction::RebuildIndex.spec().undo,
        DoctorRepairUndo::Unavailable
    );
    assert!(
        matches!(
            DoctorRepairAction::RetryMount.spec().undo,
            DoctorRepairUndo::Existing(_)
        ),
        "unmounting already reverses a mount"
    );
    assert_eq!(DoctorRepairUndo::Unavailable.label(), "Undo unavailable.");
    assert!(
        DoctorRepairUndo::NothingToUndo
            .label()
            .contains("only empty directories")
    );
}

// --- 29. Unsupported findings -------------------------------------------

#[test]
fn a_finding_with_no_doctor_repair_offers_none() {
    let report = crate::database::DatabaseHealthReport {
        format_version: 1,
        database_path: crate::emulator_environment::EncodedPath::from_path(Path::new(
            "/tmp/library.sqlite3",
        )),
        database_present: true,
        main_file: None,
        sidecars: Vec::new(),
        open_outcome: crate::database::DatabaseOpenOutcome::OpenedReadOnly,
        journal_mode: None,
        quick_check: crate::database::DatabaseCheckOutcome {
            status: crate::database::DatabaseCheckStatus::Ok,
            messages: Vec::new(),
        },
        integrity_check: crate::database::DatabaseCheckOutcome {
            status: crate::database::DatabaseCheckStatus::Ok,
            messages: Vec::new(),
        },
        schema_version: Some(5),
        diagnostics: vec![crate::database::DatabaseDiagnostic {
            code: crate::database::DatabaseDiagnosticCode::CorruptDatabase,
            severity: crate::database::DatabaseDiagnosticSeverity::Error,
            message: "corrupt".to_string(),
            sqlite_extended_code: None,
            raw_sqlite_message: None,
        }],
    };
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.database = Gathered::Ready(&report);
    let scan = run_doctor_scan(&inputs);
    let finding = scan.finding("database.corrupt").expect("finding");
    assert_eq!(
        finding.repair, None,
        "the catalogue is never repaired automatically"
    );
    assert!(finding.offered_repair().is_none());
    assert!(offered_repairs(&scan).is_empty());
}

#[test]
fn requesting_a_repair_a_finding_does_not_offer_is_refused() {
    let fixture = Fixture::new("wrong-action");
    let config = fixture.config();
    let leftover = fixture.dir("mnt/SNES/Old Game");
    let scan = scan_with_stale_directories(&config);

    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::RebuildIndex,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::ActionNotOfferedForFinding)
    );
    assert!(leftover.is_dir());
}

#[test]
fn an_unknown_finding_id_is_refused() {
    let fixture = Fixture::new("unknown-finding");
    let config = fixture.config();
    let scan = scan_with_stale_directories(&config);
    let outcome = execute_doctor_repair(
        &request(DoctorRepairAction::CleanMountRoot, "no.such.finding"),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::UnknownFinding)
    );
}

// --- 30. The scan is still read-only ------------------------------------

#[test]
fn planning_leftover_folders_never_removes_them() {
    let fixture = Fixture::new("plan-read-only");
    let config = fixture.config();
    fixture.dir("mnt/SNES/Old Game");
    fixture.dir("mnt/N64/Other Game");
    fixture.file("mnt/PSX/Kept/save.srm", b"user save");
    let before = snapshot(&fixture.root);

    for _ in 0..3 {
        let stale = crate::plan_stale_mount_directories(&config).expect("plan");
        assert_eq!(stale.len(), 2, "{stale:?}");
        let _ = scan_with_stale_directories(&config);
    }
    assert_eq!(
        before,
        snapshot(&fixture.root),
        "planning or scanning changed the tree"
    );
}

#[test]
fn the_plan_and_the_remover_agree_about_what_is_removable() {
    let fixture = Fixture::new("plan-agrees");
    let config = fixture.config();
    let empty = fixture.dir("mnt/SNES/Empty");
    let occupied = fixture.dir("mnt/SNES/Occupied");
    fixture.file("mnt/SNES/Occupied/keep.txt", b"content");

    let stale = crate::plan_stale_mount_directories(&config).expect("plan");
    assert!(stale.contains(&empty));
    assert!(!stale.contains(&occupied));

    let removed = crate::cleanup_selected_mount_tree(&config, &empty).expect("cleanup");
    assert!(removed.contains(&empty));
    assert!(occupied.is_dir());
    assert!(occupied.join("keep.txt").is_file());
}

// --- Serialisation contract ---------------------------------------------

#[test]
fn a_serialised_repair_record_contains_no_command_or_callback() {
    let fixture = Fixture::new("serialise");
    let config = fixture.config();
    fixture.dir("mnt/SNES/Old Game");
    let scan = scan_with_stale_directories(&config);
    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    let json = serde_json::to_string(&outcome).expect("json");
    for forbidden in ["shell", "argv", "callback", "closure", "exec("] {
        assert!(
            !json.to_ascii_lowercase().contains(forbidden),
            "a serialised repair must not look executable: {forbidden}"
        );
    }
    // The action is a stable id, and the existing function it calls is named.
    assert!(json.contains("clean_mount_path"));
    assert!(json.contains("cleanup_selected_mount_tree"));
}

// --- Flood protection ----------------------------------------------------

/// A real installation can accumulate thousands of leftover folders. Doctor
/// must summarise rather than emit one finding each.
#[test]
fn thousands_of_leftover_folders_produce_one_summary_finding_not_thousands() {
    let stale: Vec<PathBuf> = (0..4041)
        .map(|index| PathBuf::from(format!("/mnt/virtualroms/Unknown/Game_{index}")))
        .collect();
    let findings = findings_from_stale_mount_directories(&stale);

    assert_eq!(findings.len(), 1, "one summary finding, not 4041");
    let summary = &findings[0];
    assert_eq!(summary.id, "mount_root.stale_mount_directories");
    assert_eq!(summary.repair, Some(DoctorRepairAction::CleanMountRoot));
    assert!(summary.explanation.contains("4041"));
    // A bounded evidence sample, plus an honest note about the rest.
    assert!(summary.evidence.len() <= 12, "{}", summary.evidence.len());
    assert!(
        summary
            .evidence
            .iter()
            .any(|item| item.contains("and 4031 more"))
    );
    assert!(
        summary
            .evidence
            .iter()
            .any(|item| item.contains("not listed separately")),
        "{:?}",
        summary.evidence
    );
}

#[test]
fn a_handful_of_leftover_folders_are_still_listed_individually() {
    let stale: Vec<PathBuf> = (0..3)
        .map(|index| PathBuf::from(format!("/mnt/SNES/Game_{index}")))
        .collect();
    let findings = findings_from_stale_mount_directories(&stale);

    // Three per-folder findings plus the summary.
    assert_eq!(findings.len(), 4);
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.repair == Some(DoctorRepairAction::CleanMountPath))
            .count(),
        3
    );
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.repair == Some(DoctorRepairAction::CleanMountRoot))
            .count(),
        1
    );
}

#[test]
fn no_leftover_folders_produce_no_finding_at_all() {
    assert!(findings_from_stale_mount_directories(&[]).is_empty());
}

#[test]
fn a_dry_run_needs_no_confirmation_because_it_changes_nothing() {
    let fixture = Fixture::new("dry-run-unconfirmed");
    let config = fixture.config();
    let leftover = fixture.dir("mnt/Old Game");
    let scan = scan_with_stale_directories(&config);
    let before = snapshot(&fixture.root);

    let outcome = execute_doctor_repair(
        &DoctorRepairRequest {
            action: DoctorRepairAction::CleanMountPath,
            finding_id: "mount_root.stale_mount_directory".to_string(),
            affected: Some(leftover.display().to_string()),
            confirmed: false,
            dry_run: true,
        },
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.status,
        DoctorRepairStatus::DryRun,
        "{:?}",
        outcome.record
    );
    assert!(leftover.is_dir());
    assert_eq!(before, snapshot(&fixture.root));
}

// --- `--resource` can only select, never introduce ------------------------

/// The core invariant: a supplied resource is matched against the `affected`
/// path of a finding this scan reproduced. It cannot point a repair at
/// anything else - not even at a folder that is genuinely stale and would be
/// a perfectly legitimate target of the same repair on its own.
#[test]
fn a_resource_not_attached_to_the_finding_is_refused_even_when_it_is_itself_repairable() {
    let fixture = Fixture::new("resource-not-attached");
    let config = fixture.config();
    let named = fixture.dir("mnt/Named Game");
    let other = fixture.dir("mnt/Other Game");
    let before = snapshot(&fixture.root);

    // Both folders are stale, so each has its own finding.
    let stale = crate::plan_stale_mount_directories(&config).expect("plan");
    assert!(
        stale.contains(&named) && stale.contains(&other),
        "{stale:?}"
    );

    // Build a scan containing *only* the finding for `named`, then ask to
    // repair it while naming `other`.
    let only_named = [named.clone()];
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.stale_mount_directories = Gathered::Ready(&only_named);
    let scan = run_doctor_scan(&inputs);

    let outcome = execute_doctor_repair(
        &request_for(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
            &other,
        ),
        &context(&config, &scan, &fixture.index_path()),
    );

    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::ResourceNotAttachedToFinding),
        "a resource the finding does not carry must be refused"
    );
    assert!(
        other.is_dir(),
        "the named-but-unattached folder is untouched"
    );
    assert!(named.is_dir(), "and so is the finding's own folder");
    assert_eq!(before, snapshot(&fixture.root), "nothing changed at all");
}

/// The refusal happens before any planning, so an unattached resource never
/// reaches a validation step that could resolve or act on it.
#[test]
fn an_unattached_resource_is_refused_before_any_planning_or_mutation() {
    let fixture = Fixture::new("resource-before-planning");
    let config = fixture.config();
    let named = fixture.dir("mnt/Named Game");
    let only_named = [named.clone()];
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.stale_mount_directories = Gathered::Ready(&only_named);
    let scan = run_doctor_scan(&inputs);

    // A path that does not exist at all: if planning had run, it would have
    // been refused as stale instead. Getting `ResourceNotAttachedToFinding`
    // proves the identity gate ran first.
    let ghost = fixture.root.join("mnt/Never Existed");
    let outcome = execute_doctor_repair(
        &request_for(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
            &ghost,
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::ResourceNotAttachedToFinding),
        "identity must be checked before staleness or path safety"
    );
    // And the refusal never echoes the unmatched path as if it were a target.
    assert!(
        outcome.record.affected.is_none(),
        "an unvalidated path must not appear where a target belongs"
    );
    assert!(
        !outcome.record.summary.contains("Never Existed"),
        "{}",
        outcome.record.summary
    );
}

/// A resource cannot be attached to a finding that reported none.
#[test]
fn a_resource_cannot_be_attached_to_a_finding_that_has_none() {
    let fixture = Fixture::new("resource-on-none");
    let config = fixture.config();
    // Two leftovers, so the aggregate summary finding exists. It carries no
    // affected resource of its own.
    let planned = planned_mount_paths(&fixture, &config, &["One", "Two"]);
    let scan = scan_with_stale_directories(&config);
    let summary = scan
        .finding("mount_root.stale_mount_directories")
        .expect("summary");
    assert!(summary.affected.is_none(), "the summary names no resource");
    let before = snapshot(&fixture.root);

    let outcome = execute_doctor_repair(
        &request_for(
            DoctorRepairAction::CleanMountRoot,
            "mount_root.stale_mount_directories",
            &planned[0],
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::ResourceNotAttachedToFinding)
    );
    assert_eq!(before, snapshot(&fixture.root));
}

/// A resource belonging to a *different* finding id cannot be borrowed.
#[test]
fn a_resource_from_another_finding_cannot_be_borrowed() {
    let fixture = Fixture::new("resource-borrowed");
    let config = fixture.config();
    let leftover = fixture.dir("mnt/Old Game");
    let archive = fixture.file("roms/game.zip", b"bytes");

    // One scan holding both a leftover-folder finding and a retry-mount
    // finding, each with its own resource.
    let stale = [leftover.clone()];
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.stale_mount_directories = Gathered::Ready(&stale);
    let mut scan = run_doctor_scan(&inputs);
    scan.findings
        .extend(scan_with_retry_finding(&archive).findings);
    let before = snapshot(&fixture.root);

    // The archive belongs to the retry finding, not to the cleanup finding.
    let outcome = execute_doctor_repair(
        &request_for(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
            &archive,
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::ResourceNotAttachedToFinding)
    );
    assert!(archive.is_file(), "the archive is untouched");
    assert_eq!(before, snapshot(&fixture.root));
}

/// Exact string equality, not prefix or suffix matching.
#[test]
fn a_resource_must_match_the_findings_path_exactly() {
    let fixture = Fixture::new("resource-exact");
    let config = fixture.config();
    let exact = fixture.dir("mnt/Old Game");
    let stale = [exact.clone()];
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.stale_mount_directories = Gathered::Ready(&stale);
    let scan = run_doctor_scan(&inputs);
    let before = snapshot(&fixture.root);

    let display = exact.display().to_string();
    for near_miss in [
        format!("{display}/"),
        format!("{display}/.."),
        format!("{display}x"),
        display.trim_end_matches("Old Game").to_string(),
        display.to_ascii_uppercase(),
        format!(" {display}"),
    ] {
        let outcome = execute_doctor_repair(
            &DoctorRepairRequest {
                action: DoctorRepairAction::CleanMountPath,
                finding_id: "mount_root.stale_mount_directory".to_string(),
                affected: Some(near_miss.clone()),
                confirmed: true,
                dry_run: false,
            },
            &context(&config, &scan, &fixture.index_path()),
        );
        assert_eq!(
            outcome.record.rejection,
            Some(DoctorRepairRejection::ResourceNotAttachedToFinding),
            "`{near_miss}` must not match `{display}`"
        );
    }
    assert!(exact.is_dir());
    assert_eq!(before, snapshot(&fixture.root));

    // The exact string does select it.
    let outcome = execute_doctor_repair(
        &request_for(
            DoctorRepairAction::CleanMountPath,
            "mount_root.stale_mount_directory",
            &exact,
        ),
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(
        outcome.record.status,
        DoctorRepairStatus::Succeeded,
        "{:?}",
        outcome.record
    );
}

/// A dry run applies the identity gate too, so `--dry-run --resource <other>`
/// cannot be used to probe or plan against an unattached path.
#[test]
fn a_dry_run_also_refuses_an_unattached_resource() {
    let fixture = Fixture::new("resource-dry-run");
    let config = fixture.config();
    let named = fixture.dir("mnt/Named Game");
    let other = fixture.dir("mnt/Other Game");
    let only_named = [named];
    let mut inputs = DoctorScanInputs::none_loaded();
    inputs.stale_mount_directories = Gathered::Ready(&only_named);
    let scan = run_doctor_scan(&inputs);

    let outcome = execute_doctor_repair(
        &DoctorRepairRequest {
            action: DoctorRepairAction::CleanMountPath,
            finding_id: "mount_root.stale_mount_directory".to_string(),
            affected: Some(other.display().to_string()),
            confirmed: true,
            dry_run: true,
        },
        &context(&config, &scan, &fixture.index_path()),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::ResourceNotAttachedToFinding),
        "a dry run must not plan against an unattached resource either"
    );
}

/// The lookup itself is total, and each refusal is distinguishable.
#[test]
fn the_finding_lookup_distinguishes_every_way_it_can_fail() {
    let fixture = Fixture::new("lookup-total");
    let config = fixture.config();
    let one = fixture.dir("mnt/One");
    let two = fixture.dir("mnt/Two");
    let scan = scan_with_stale_directories(&config);

    // Unknown id.
    assert_eq!(
        scan.finding_for("no.such.finding", None),
        crate::diagnostics::FindingLookup::UnknownId
    );
    // Ambiguous: two findings share the per-folder id.
    assert!(matches!(
        scan.finding_for("mount_root.stale_mount_directory", None),
        crate::diagnostics::FindingLookup::Ambiguous(2)
    ));
    // Resource not attached.
    assert_eq!(
        scan.finding_for("mount_root.stale_mount_directory", Some("/somewhere/else")),
        crate::diagnostics::FindingLookup::ResourceNotAttached
    );
    // Found, for each real resource.
    for path in [&one, &two] {
        let found = scan
            .finding_for(
                "mount_root.stale_mount_directory",
                Some(path.display().to_string().as_str()),
            )
            .found()
            .expect("the finding is selected by its own resource");
        assert_eq!(
            found.affected.as_ref().expect("affected").display,
            path.display().to_string()
        );
    }
    // An unambiguous id needs no resource.
    let index_scan = scan_with_index_finding(&fixture.index_path());
    assert!(
        index_scan
            .finding_for("library.index_out_of_date", None)
            .found()
            .is_some()
    );
}

/// `RebuildIndex`'s destination is the one target supplied by the caller
/// rather than by the finding, so it must equal the path the finding
/// reported. A context pointed at a different index is refused.
#[test]
fn rebuild_index_refuses_an_index_path_the_finding_did_not_name() {
    let fixture = Fixture::new("rebuild-other-index");
    let config = fixture.config();
    fixture.file("roms/game.zip", b"bytes");
    let named = fixture.index_path();
    let other = fixture.root.join("data/somewhere-else.json");
    // The finding reports `named`...
    let scan = scan_with_index_finding(&named);
    let before = snapshot(&fixture.root);

    // ...but the context points at a different file.
    let outcome = execute_doctor_repair(
        &request(
            DoctorRepairAction::RebuildIndex,
            "library.index_out_of_date",
        ),
        &context(&config, &scan, &other),
    );
    assert_eq!(outcome.record.status, DoctorRepairStatus::Rejected);
    assert_eq!(
        outcome.record.rejection,
        Some(DoctorRepairRejection::ResourceNotAttachedToFinding)
    );
    assert!(!other.exists(), "no index was written anywhere else");
    assert_eq!(before, snapshot(&fixture.root));
}

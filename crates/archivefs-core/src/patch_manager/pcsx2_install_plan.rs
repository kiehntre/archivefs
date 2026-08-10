//! Materializes a selected, merged PNACH into private staging and feeds the
//! existing shared preview/transaction pipeline. This module never writes to
//! the live PCSX2 profile.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use super::pcsx2::{normalize_crc, normalize_serial};
use super::pcsx2_identity::{Pcsx2GameIdentity, pcsx2_cheats_directory};
use super::pcsx2_local::Pcsx2Profile;
use super::pcsx2_pnach::{
    MAX_MANAGED_PNACH_BYTES, ManagedPnachCheat, append_raw_managed_blocks, extract_managed_blocks,
    merge_managed_pnach_cheats, parse_pnach_document, remove_managed_blocks,
};
use super::shared_preview::{
    PreviewAdapter, PreviewIdentity, PreviewIdentityKind, PreviewIdentityState,
    PreviewMatchStrength, PreviewSourceItem, SharedPreviewReport, SharedPreviewRequest,
    build_shared_preview,
};

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pcsx2InstallPlanErrorKind {
    SelectionStale,
    IdentityUnavailable,
    ProfileUnavailable,
    InvalidCrc,
    DestinationUnsafe,
    DestinationUnreadable,
    DestinationTooLarge,
    DocumentUnsafe,
    NoSelectedCheats,
    StagingUnavailable,
    GeneratedFileTooLarge,
    PreviewFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcsx2InstallPlanError {
    pub kind: Pcsx2InstallPlanErrorKind,
    pub path: Option<PathBuf>,
    pub detail: String,
}

impl std::fmt::Display for Pcsx2InstallPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for Pcsx2InstallPlanError {}

fn error(
    kind: Pcsx2InstallPlanErrorKind,
    path: Option<&Path>,
    detail: impl Into<String>,
) -> Pcsx2InstallPlanError {
    Pcsx2InstallPlanError {
        kind,
        path: path.map(Path::to_path_buf),
        detail: detail.into(),
    }
}

pub fn pcsx2_crc_filename(crc: &str) -> Result<String, Pcsx2InstallPlanError> {
    normalize_crc(crc)
        .map(|crc| format!("{crc}.pnach"))
        .ok_or_else(|| {
            error(
                Pcsx2InstallPlanErrorKind::InvalidCrc,
                None,
                "PCSX2 PNACH filenames require exactly eight hexadecimal CRC characters",
            )
        })
}

/// The exact upstream PCSX2 `patches/pcsx2_patches` naming convention:
/// `<SERIAL>_<CRC>.pnach` when a verified disc serial is available (the
/// only shape this PCSX2 build actually reads cheats from), falling back
/// to the legacy bare `<CRC>.pnach` only when no verified serial exists.
pub fn pcsx2_pnach_filename(
    serial: Option<&str>,
    crc: &str,
) -> Result<String, Pcsx2InstallPlanError> {
    if let Some(serial) = serial.and_then(normalize_serial) {
        let crc = normalize_crc(crc).ok_or_else(|| {
            error(
                Pcsx2InstallPlanErrorKind::InvalidCrc,
                None,
                "PCSX2 PNACH filenames require exactly eight hexadecimal CRC characters",
            )
        })?;
        return Ok(format!("{serial}_{crc}.pnach"));
    }
    pcsx2_crc_filename(crc)
}

/// Reads a regular destination without following a final-component symlink.
/// Missing is represented by an empty byte vector.
pub fn load_existing_pcsx2_pnach(path: &Path) -> Result<Vec<u8>, Pcsx2InstallPlanError> {
    match fs::symlink_metadata(path) {
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(failure) => Err(error(
            Pcsx2InstallPlanErrorKind::DestinationUnreadable,
            Some(path),
            format!("existing PNACH could not be inspected: {failure}"),
        )),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(error(
            Pcsx2InstallPlanErrorKind::DestinationUnsafe,
            Some(path),
            "existing PNACH is not a regular non-symlink file",
        )),
        Ok(metadata) if metadata.len() > MAX_MANAGED_PNACH_BYTES as u64 => Err(error(
            Pcsx2InstallPlanErrorKind::DestinationTooLarge,
            Some(path),
            "existing PNACH exceeds the managed-file byte limit",
        )),
        Ok(_) => {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            let file = options.open(path).map_err(|failure| {
                error(
                    Pcsx2InstallPlanErrorKind::DestinationUnreadable,
                    Some(path),
                    format!("existing PNACH could not be opened: {failure}"),
                )
            })?;
            let mut bytes = Vec::new();
            file.take((MAX_MANAGED_PNACH_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|failure| {
                    error(
                        Pcsx2InstallPlanErrorKind::DestinationUnreadable,
                        Some(path),
                        format!("existing PNACH could not be read: {failure}"),
                    )
                })?;
            if bytes.len() > MAX_MANAGED_PNACH_BYTES {
                return Err(error(
                    Pcsx2InstallPlanErrorKind::DestinationTooLarge,
                    Some(path),
                    "existing PNACH grew beyond the managed-file byte limit",
                ));
            }
            Ok(bytes)
        }
    }
}

#[derive(Debug, Clone)]
pub struct StagedPcsx2Pnach {
    pub staging_root: PathBuf,
    pub path: PathBuf,
    pub digest: String,
    pub contents: Vec<u8>,
    pub selected_cheat_ids: Vec<String>,
    pub destination_path: PathBuf,
    pub destination_existed: bool,
    pub original_bytes: Vec<u8>,
    /// Present only when a legacy CRC-only file was found alongside a
    /// resolved `<SERIAL>_<CRC>.pnach` target and had EmuWiz-managed
    /// blocks that needed consolidating into the target this PCSX2 build
    /// actually reads.
    pub legacy_migration: Option<StagedPcsx2LegacyMigration>,
}

#[derive(Debug, Clone)]
pub struct StagedPcsx2LegacyMigration {
    pub staged_path: PathBuf,
    pub digest: String,
    pub contents: Vec<u8>,
    pub legacy_destination_path: PathBuf,
    pub legacy_original_bytes: Vec<u8>,
    /// Managed cheat IDs carried over from the legacy file into the new
    /// target's staged contents (excludes any ID already covered by the
    /// currently selected cheats, which are freshly rendered instead).
    pub migrated_block_ids: Vec<String>,
}

/// Intermediate result of inspecting a legacy CRC-only file for migration,
/// before its stripped content has been staged to disk.
struct PendingLegacyMigration {
    legacy_destination_path: PathBuf,
    legacy_original_bytes: Vec<u8>,
    stripped_legacy_contents: Vec<u8>,
    migrated_block_ids: Vec<String>,
}

pub fn stage_pcsx2_pnach(
    staging_root: &Path,
    profile: &Pcsx2Profile,
    serial: Option<&str>,
    crc: &str,
    selected: &[ManagedPnachCheat],
) -> Result<StagedPcsx2Pnach, Pcsx2InstallPlanError> {
    if selected.is_empty() {
        return Err(error(
            Pcsx2InstallPlanErrorKind::NoSelectedCheats,
            None,
            "select at least one compatible cheat before preview",
        ));
    }
    let cheats_directory = pcsx2_cheats_directory(profile).ok_or_else(|| {
        error(
            Pcsx2InstallPlanErrorKind::ProfileUnavailable,
            Some(&profile.configuration_path),
            "confirmed profile has no safe normal cheats directory",
        )
    })?;
    let file_name = pcsx2_pnach_filename(serial, crc)?;
    let legacy_file_name = pcsx2_crc_filename(crc)?;
    let destination_path = cheats_directory.join(&file_name);
    log::debug!(
        "pcsx2 install plan: profile {} target {} ({} cheat(s) selected)",
        profile.profile_id,
        destination_path.display(),
        selected.len(),
    );
    let original = load_existing_pcsx2_pnach(&destination_path)?;
    let document = parse_pnach_document(&original).map_err(|failure| {
        error(
            Pcsx2InstallPlanErrorKind::DocumentUnsafe,
            Some(&destination_path),
            failure.to_string(),
        )
    })?;
    let mut contents = merge_managed_pnach_cheats(&document, selected).map_err(|failure| {
        error(
            Pcsx2InstallPlanErrorKind::DocumentUnsafe,
            Some(&destination_path),
            failure.to_string(),
        )
    })?;

    // A legacy `<CRC>.pnach` file only needs attention when the resolved
    // target actually differs from it (a verified serial was available)
    // and it exists with EmuWiz-managed content that would otherwise
    // sit duplicated and unread by this PCSX2 build.
    let pending_migration = if file_name == legacy_file_name {
        None
    } else {
        let legacy_destination_path = cheats_directory.join(&legacy_file_name);
        let legacy_original = load_existing_pcsx2_pnach(&legacy_destination_path)?;
        let legacy_document = parse_pnach_document(&legacy_original).map_err(|failure| {
            error(
                Pcsx2InstallPlanErrorKind::DocumentUnsafe,
                Some(&legacy_destination_path),
                format!("legacy PNACH could not be inspected for migration: {failure}"),
            )
        })?;
        if legacy_document.managed_block_ids().is_empty() {
            None
        } else {
            let already_selected: BTreeSet<String> =
                selected.iter().map(|cheat| cheat.id.clone()).collect();
            let to_migrate: Vec<_> = extract_managed_blocks(&legacy_document)
                .into_iter()
                .filter(|block| !already_selected.contains(&block.id))
                .collect();
            let migrated_block_ids: Vec<String> =
                to_migrate.iter().map(|block| block.id.clone()).collect();
            let merged_document = parse_pnach_document(&contents).map_err(|failure| {
                error(
                    Pcsx2InstallPlanErrorKind::DocumentUnsafe,
                    Some(&destination_path),
                    failure.to_string(),
                )
            })?;
            contents =
                append_raw_managed_blocks(&merged_document, &to_migrate).map_err(|failure| {
                    error(
                        Pcsx2InstallPlanErrorKind::DocumentUnsafe,
                        Some(&destination_path),
                        failure.to_string(),
                    )
                })?;
            // Every EmuWiz-managed block that lived in the legacy file
            // is now accounted for in the new target (either migrated
            // verbatim, or freshly re-rendered because it was reselected),
            // so all of them are stripped from the legacy file - it never
            // silently keeps a duplicate active copy.
            let stripped_legacy =
                remove_managed_blocks(&legacy_document, legacy_document.managed_block_ids());
            log::info!(
                "pcsx2 install plan: migrating {} managed cheat(s) from legacy {} into {}",
                legacy_document.managed_block_ids().len(),
                legacy_destination_path.display(),
                destination_path.display(),
            );
            Some(PendingLegacyMigration {
                legacy_destination_path,
                legacy_original_bytes: legacy_original,
                stripped_legacy_contents: stripped_legacy,
                migrated_block_ids,
            })
        }
    };
    if contents.len() > MAX_MANAGED_PNACH_BYTES {
        return Err(error(
            Pcsx2InstallPlanErrorKind::GeneratedFileTooLarge,
            None,
            "generated PNACH exceeds the managed-file byte limit",
        ));
    }
    if !staging_root.is_absolute() || staging_root.parent().is_none() {
        return Err(error(
            Pcsx2InstallPlanErrorKind::StagingUnavailable,
            Some(staging_root),
            "staging root must be an absolute non-root path",
        ));
    }
    fs::create_dir_all(staging_root).map_err(|failure| {
        error(
            Pcsx2InstallPlanErrorKind::StagingUnavailable,
            Some(staging_root),
            format!("private staging directory could not be created: {failure}"),
        )
    })?;
    let path = stage_one(staging_root, &file_name, &contents)?;
    let legacy_migration = match pending_migration {
        Some(pending) => {
            let legacy_staged_name = format!("legacy-{legacy_file_name}");
            let staged_path = stage_one(
                staging_root,
                &legacy_staged_name,
                &pending.stripped_legacy_contents,
            )?;
            Some(StagedPcsx2LegacyMigration {
                digest: sha256(&pending.stripped_legacy_contents),
                staged_path,
                contents: pending.stripped_legacy_contents,
                legacy_destination_path: pending.legacy_destination_path,
                legacy_original_bytes: pending.legacy_original_bytes,
                migrated_block_ids: pending.migrated_block_ids,
            })
        }
        None => None,
    };
    Ok(StagedPcsx2Pnach {
        staging_root: staging_root.to_path_buf(),
        path,
        digest: sha256(&contents),
        contents,
        selected_cheat_ids: selected.iter().map(|cheat| cheat.id.clone()).collect(),
        destination_path,
        destination_existed: !original.is_empty()
            || fs::symlink_metadata(cheats_directory.join(&file_name)).is_ok(),
        original_bytes: original,
        legacy_migration,
    })
}

/// Atomically stages one file's final bytes under `staging_root` as
/// `file_name`, via a create-new temp file, `sync_all`, then rename.
fn stage_one(
    staging_root: &Path,
    file_name: &str,
    contents: &[u8],
) -> Result<PathBuf, Pcsx2InstallPlanError> {
    let path = staging_root.join(file_name);
    let temporary = staging_root.join(format!(
        ".{file_name}.{}.{}.partial",
        std::process::id(),
        NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if let Err(failure) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error(
            Pcsx2InstallPlanErrorKind::StagingUnavailable,
            Some(&path),
            format!("PNACH could not be staged atomically: {failure}"),
        ));
    }
    Ok(path)
}

#[derive(Debug, Clone)]
pub struct Pcsx2InstallPreviewRequest {
    pub selected_archive: PathBuf,
    pub profile: Pcsx2Profile,
    pub identity: Pcsx2GameIdentity,
    pub staged: StagedPcsx2Pnach,
}

#[derive(Debug, Clone)]
pub struct Pcsx2InstallPreview {
    pub report: SharedPreviewReport,
    pub staged: StagedPcsx2Pnach,
    pub plain_summary: String,
    pub technical_details: Vec<String>,
}

pub fn build_pcsx2_install_preview(
    request: &Pcsx2InstallPreviewRequest,
) -> Result<Pcsx2InstallPreview, Pcsx2InstallPlanError> {
    if request.selected_archive != request.identity.archive_path {
        return Err(error(
            Pcsx2InstallPlanErrorKind::SelectionStale,
            Some(&request.selected_archive),
            "selected game changed before PCSX2 preview completed",
        ));
    }
    let crc = request.identity.verified_crc().ok_or_else(|| {
        error(
            Pcsx2InstallPlanErrorKind::IdentityUnavailable,
            Some(&request.selected_archive),
            "verified PCSX2 executable CRC is required",
        )
    })?;
    let expected_name = pcsx2_pnach_filename(request.identity.serial.as_deref(), crc)?;
    if request
        .staged
        .path
        .file_name()
        .and_then(|name| name.to_str())
        != Some(&expected_name)
    {
        return Err(error(
            Pcsx2InstallPlanErrorKind::SelectionStale,
            Some(&request.staged.path),
            "staged PNACH no longer matches the selected game's identity",
        ));
    }
    let report = build_shared_preview(&SharedPreviewRequest {
        adapter: PreviewAdapter::Pcsx2,
        selected_archive: request.selected_archive.clone(),
        platform: Some("PS2".to_string()),
        identity: PreviewIdentity {
            kind: PreviewIdentityKind::Pcsx2ExecutableCrc,
            state: PreviewIdentityState::Verified,
            value: Some(crc.to_string()),
            archive_path: request.selected_archive.clone(),
            revision: None,
        },
        destination_root: request.profile.configuration_path.clone(),
        source_items: vec![PreviewSourceItem {
            adapter: PreviewAdapter::Pcsx2,
            source_path: request.staged.path.clone(),
            expected_source_digest: Some(request.staged.digest.clone()),
            destination_relative_paths: vec![PathBuf::from("cheats").join(&expected_name)],
            match_strength: PreviewMatchStrength::VerifiedExact,
        }],
    })
    .map_err(|failure| {
        error(
            Pcsx2InstallPlanErrorKind::PreviewFailed,
            None,
            failure.to_string(),
        )
    })?;
    let mut technical_details = vec![
        format!("Verified CRC: {crc}"),
        format!("Destination: {}", request.staged.destination_path.display()),
        format!("Staged SHA-256: {}", request.staged.digest),
    ];
    let mut plain_summary = format!(
        "Ready to review {} selected cheat{}",
        request.staged.selected_cheat_ids.len(),
        if request.staged.selected_cheat_ids.len() == 1 {
            ""
        } else {
            "s"
        }
    );
    if let Some(migration) = &request.staged.legacy_migration {
        let legacy_name = migration
            .legacy_destination_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("previous file");
        plain_summary.push_str(&if migration.migrated_block_ids.is_empty() {
            format!("; consolidating the legacy {legacy_name} (already covered by this selection)")
        } else {
            format!(
                "; migrating {} cheat{} from the legacy {legacy_name}",
                migration.migrated_block_ids.len(),
                if migration.migrated_block_ids.len() == 1 {
                    ""
                } else {
                    "s"
                },
            )
        });
        technical_details.push(format!(
            "Legacy file: {}",
            migration.legacy_destination_path.display()
        ));
        technical_details.push(format!(
            "Migrated managed cheat IDs: {}",
            migration.migrated_block_ids.join(", ")
        ));
    }
    Ok(Pcsx2InstallPreview {
        plain_summary,
        technical_details,
        report,
        staged: request.staged.clone(),
    })
}

/// Builds an independent, single-entry preview for stripping the migrated
/// managed blocks out of a legacy `<CRC>.pnach` file, when
/// `staged.legacy_migration` is present. Deliberately a *separate*
/// `build_shared_preview` call from `build_pcsx2_install_preview`: PCSX2's
/// shared preview pipeline treats two verified-exact entries for the same
/// identity in one report as an unresolvable ambiguity (see
/// `PreviewBlockerKind::MultipleExactMatches`), which is the right
/// invariant for every other PCSX2 install - so migration cleanup runs as
/// its own preview/plan/apply/rollback operation with its own journal,
/// chained after the primary install by the caller rather than folded into
/// the same transaction.
pub fn build_pcsx2_legacy_migration_preview(
    staged: &StagedPcsx2Pnach,
    profile: &Pcsx2Profile,
    selected_archive: &Path,
    crc: &str,
) -> Result<Option<Pcsx2InstallPreview>, Pcsx2InstallPlanError> {
    let Some(migration) = &staged.legacy_migration else {
        return Ok(None);
    };
    let legacy_name = migration
        .legacy_destination_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            error(
                Pcsx2InstallPlanErrorKind::DestinationUnsafe,
                Some(&migration.legacy_destination_path),
                "legacy PNACH filename is not valid UTF-8",
            )
        })?
        .to_string();
    let report = build_shared_preview(&SharedPreviewRequest {
        adapter: PreviewAdapter::Pcsx2,
        selected_archive: selected_archive.to_path_buf(),
        platform: Some("PS2".to_string()),
        identity: PreviewIdentity {
            kind: PreviewIdentityKind::Pcsx2ExecutableCrc,
            state: PreviewIdentityState::Verified,
            value: Some(crc.to_string()),
            archive_path: selected_archive.to_path_buf(),
            revision: None,
        },
        destination_root: profile.configuration_path.clone(),
        source_items: vec![PreviewSourceItem {
            adapter: PreviewAdapter::Pcsx2,
            source_path: migration.staged_path.clone(),
            expected_source_digest: Some(migration.digest.clone()),
            destination_relative_paths: vec![PathBuf::from("cheats").join(&legacy_name)],
            match_strength: PreviewMatchStrength::VerifiedExact,
        }],
    })
    .map_err(|failure| {
        error(
            Pcsx2InstallPlanErrorKind::PreviewFailed,
            None,
            failure.to_string(),
        )
    })?;
    Ok(Some(Pcsx2InstallPreview {
        plain_summary: if migration.migrated_block_ids.is_empty() {
            format!("Consolidating the legacy {legacy_name}: no longer active elsewhere")
        } else {
            format!(
                "Migrating {} cheat{} out of the legacy {legacy_name}",
                migration.migrated_block_ids.len(),
                if migration.migrated_block_ids.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        },
        technical_details: vec![
            format!(
                "Legacy destination: {}",
                migration.legacy_destination_path.display()
            ),
            format!(
                "Migrated managed cheat IDs: {}",
                migration.migrated_block_ids.join(", ")
            ),
            format!("Staged SHA-256: {}", migration.digest),
        ],
        report,
        staged: staged.clone(),
    }))
}

fn sha256(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::patch_manager::{
        Pcsx2IdentityState, Pcsx2InstallationType, Pcsx2PatchCategory, Pcsx2PatchDirectory,
        Pcsx2PatchDirectoryState, Pcsx2ProfileScope, PnachPatchLine,
    };

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "archivefs-pcsx2-plan-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn profile(root: &Path) -> Pcsx2Profile {
        Pcsx2Profile {
            profile_id: "fixture".to_string(),
            installation_type: Pcsx2InstallationType::Portable,
            scope: Pcsx2ProfileScope::Portable,
            configuration_path: root.to_path_buf(),
            provenance: "test",
            eligible: true,
            blockers: Vec::new(),
            patch_directories: vec![Pcsx2PatchDirectory {
                path: root.join("cheats"),
                category: Pcsx2PatchCategory::Cheats,
                state: Pcsx2PatchDirectoryState::Missing,
                warning: None,
                identity: None,
            }],
            configuration_identity: None,
        }
    }

    fn selected() -> Vec<ManagedPnachCheat> {
        vec![ManagedPnachCheat {
            id: "health".to_string(),
            name: "Health".to_string(),
            description: None,
            patch_lines: vec![PnachPatchLine::parse("patch=1,EE,20123456,word,1").unwrap()],
        }]
    }

    #[test]
    fn crc_filename_is_exact_uppercase_hex() {
        assert_eq!(pcsx2_crc_filename("a1b2c3d4").unwrap(), "A1B2C3D4.pnach");
        assert!(pcsx2_crc_filename("123").is_err());
        assert!(pcsx2_crc_filename("A1B2C3DX").is_err());
    }

    #[test]
    fn staging_preserves_existing_content_and_does_not_touch_destination() {
        let root = temp("preserve");
        fs::create_dir_all(root.join("cheats")).unwrap();
        let destination = root.join("cheats/A1B2C3D4.pnach");
        let original = b"// user\nunknown=keep\n";
        fs::write(&destination, original).unwrap();
        let staged = stage_pcsx2_pnach(
            &root.join("staging"),
            &profile(&root),
            None,
            "A1B2C3D4",
            &selected(),
        )
        .unwrap();
        assert!(staged.contents.starts_with(original));
        assert_eq!(fs::read(&destination).unwrap(), original);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_game_selection_is_rejected_before_preview() {
        let root = temp("stale");
        fs::create_dir_all(&root).unwrap();
        let staged = stage_pcsx2_pnach(
            &root.join("staging"),
            &profile(&root),
            None,
            "A1B2C3D4",
            &selected(),
        )
        .unwrap();
        let request = Pcsx2InstallPreviewRequest {
            selected_archive: PathBuf::from("/games/b.iso"),
            profile: profile(&root),
            identity: Pcsx2GameIdentity {
                archive_path: PathBuf::from("/games/a.iso"),
                title: "A".to_string(),
                region: None,
                serial: None,
                executable_crc: Some("A1B2C3D4".to_string()),
                state: Pcsx2IdentityState::Verified,
                evidence: Vec::new(),
                plain_failure_reason: None,
            },
            staged,
        };
        assert_eq!(
            build_pcsx2_install_preview(&request).unwrap_err().kind,
            Pcsx2InstallPlanErrorKind::SelectionStale
        );
        let _ = fs::remove_dir_all(root);
    }

    fn identity(serial: Option<&str>, crc: &str) -> Pcsx2GameIdentity {
        Pcsx2GameIdentity {
            archive_path: PathBuf::from("/games/a.iso"),
            title: "A".to_string(),
            region: None,
            serial: serial.map(str::to_string),
            executable_crc: Some(crc.to_string()),
            state: Pcsx2IdentityState::Verified,
            evidence: Vec::new(),
            plain_failure_reason: None,
        }
    }

    #[test]
    fn verified_serial_produces_the_serial_and_crc_filename_pcsx2_reads() {
        assert_eq!(
            pcsx2_pnach_filename(Some("SCUS-97399"), "d6385328").unwrap(),
            "SCUS-97399_D6385328.pnach"
        );
    }

    #[test]
    fn serial_normalization_is_uppercase_hyphenated_and_strips_unsafe_input() {
        // Already-canonical input round-trips unchanged.
        assert_eq!(
            pcsx2_pnach_filename(Some("scus-97399"), "d6385328").unwrap(),
            "SCUS-97399_D6385328.pnach"
        );
        // A serial that doesn't match PCSX2's exact 4-letter/hyphen/5-digit
        // shape is never used to build an unsafe or malformed filename -
        // it silently falls back to CRC-only naming instead.
        assert_eq!(
            pcsx2_pnach_filename(Some("../../etc/passwd"), "d6385328").unwrap(),
            "D6385328.pnach"
        );
        assert_eq!(
            pcsx2_pnach_filename(Some("SCUS9739"), "d6385328").unwrap(),
            "D6385328.pnach"
        );
        assert_eq!(
            pcsx2_pnach_filename(Some(""), "d6385328").unwrap(),
            "D6385328.pnach"
        );
    }

    #[test]
    fn missing_serial_falls_back_to_crc_only_naming() {
        assert_eq!(
            pcsx2_pnach_filename(None, "d6385328").unwrap(),
            "D6385328.pnach"
        );
    }

    #[test]
    fn zero_byte_existing_serial_and_crc_target_is_populated_safely() {
        let root = temp("zero-byte-serial-crc");
        fs::create_dir_all(root.join("cheats")).unwrap();
        let destination = root.join("cheats/SCUS-97399_D6385328.pnach");
        fs::write(&destination, b"").unwrap();
        let staged = stage_pcsx2_pnach(
            &root.join("staging"),
            &profile(&root),
            Some("SCUS-97399"),
            "D6385328",
            &selected(),
        )
        .unwrap();
        assert_eq!(
            staged.destination_path.file_name().unwrap().to_str(),
            Some("SCUS-97399_D6385328.pnach")
        );
        assert!(staged.destination_existed);
        assert!(staged.contents.contains(&b'\n'));
        assert!(
            String::from_utf8(staged.contents.clone())
                .unwrap()
                .contains("// ArchiveFS managed block: health")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_crc_only_file_with_managed_blocks_migrates_and_is_stripped() {
        let root = temp("migrate-legacy");
        fs::create_dir_all(root.join("cheats")).unwrap();
        let legacy_path = root.join("cheats/D6385328.pnach");
        fs::write(
            &legacy_path,
            b"// user note\n// ArchiveFS managed block: ammo\n// Ammo\npatch=1,EE,20999999,word,1\n// End ArchiveFS managed block\n",
        )
        .unwrap();
        let staged = stage_pcsx2_pnach(
            &root.join("staging"),
            &profile(&root),
            Some("SCUS-97399"),
            "D6385328",
            &selected(),
        )
        .unwrap();
        assert_eq!(
            staged.destination_path.file_name().unwrap().to_str(),
            Some("SCUS-97399_D6385328.pnach")
        );
        let migration = staged
            .legacy_migration
            .as_ref()
            .expect("a legacy file with managed blocks must be detected and migrated");
        assert_eq!(migration.migrated_block_ids, vec!["ammo".to_string()]);
        assert_eq!(migration.legacy_destination_path, legacy_path);

        let new_contents = String::from_utf8(staged.contents.clone()).unwrap();
        assert!(new_contents.contains("// ArchiveFS managed block: health"));
        assert!(new_contents.contains("// ArchiveFS managed block: ammo"));
        assert!(new_contents.contains("patch=1,EE,20999999,word,1"));

        let stripped_legacy = String::from_utf8(migration.contents.clone()).unwrap();
        assert!(stripped_legacy.contains("// user note"));
        assert!(!stripped_legacy.contains("ArchiveFS managed block"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn both_files_containing_content_preserve_unrelated_bytes_in_each() {
        let root = temp("both-files-content");
        fs::create_dir_all(root.join("cheats")).unwrap();
        let legacy_path = root.join("cheats/D6385328.pnach");
        fs::write(
            &legacy_path,
            b"// legacy user comment\r\nunknown=keep\r\n// ArchiveFS managed block: ammo\n// Ammo\npatch=1,EE,20999999,word,1\n// End ArchiveFS managed block\n",
        )
        .unwrap();
        let new_path = root.join("cheats/SCUS-97399_D6385328.pnach");
        fs::write(&new_path, b"// new file user comment\r\n").unwrap();
        let staged = stage_pcsx2_pnach(
            &root.join("staging"),
            &profile(&root),
            Some("SCUS-97399"),
            "D6385328",
            &selected(),
        )
        .unwrap();
        let new_contents = String::from_utf8(staged.contents.clone()).unwrap();
        assert!(new_contents.starts_with("// new file user comment\r\n"));
        assert!(new_contents.contains("// ArchiveFS managed block: health"));
        assert!(new_contents.contains("// ArchiveFS managed block: ammo"));

        let migration = staged.legacy_migration.as_ref().unwrap();
        let stripped_legacy = String::from_utf8(migration.contents.clone()).unwrap();
        assert!(stripped_legacy.starts_with("// legacy user comment\r\n"));
        assert!(stripped_legacy.contains("unknown=keep\r\n"));
        assert!(!stripped_legacy.contains("ArchiveFS managed block"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn already_selected_legacy_cheat_is_not_duplicated_in_new_file() {
        let root = temp("dedupe-legacy");
        fs::create_dir_all(root.join("cheats")).unwrap();
        let legacy_path = root.join("cheats/D6385328.pnach");
        fs::write(
            &legacy_path,
            b"// ArchiveFS managed block: health\n// Health\npatch=1,EE,20123456,word,1\n// End ArchiveFS managed block\n",
        )
        .unwrap();
        // "health" is both already in the legacy file *and* part of this
        // install's fresh selection - it must appear exactly once in the
        // merged output, not twice.
        let staged = stage_pcsx2_pnach(
            &root.join("staging"),
            &profile(&root),
            Some("SCUS-97399"),
            "D6385328",
            &selected(),
        )
        .unwrap();
        let migration = staged.legacy_migration.as_ref().unwrap();
        assert!(migration.migrated_block_ids.is_empty());
        let new_contents = String::from_utf8(staged.contents.clone()).unwrap();
        assert_eq!(new_contents.matches("managed block: health").count(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn migration_runs_as_a_chained_operation_with_its_own_journal_and_undo() {
        use crate::patch_manager::{
            SharedApplyConfirmation, SharedApplyOptions, SharedApplyStatus,
            SharedRollbackConfirmation, SharedRollbackOptions, build_shared_transaction_plan,
            execute_shared_apply, execute_shared_rollback, preview_shared_rollback,
        };

        let outer = temp("migration-journal-undo");
        let root = outer.join("profile");
        fs::create_dir_all(root.join("cheats")).unwrap();
        let legacy_path = root.join("cheats/D6385328.pnach");
        let legacy_original =
            b"// ArchiveFS managed block: ammo\n// Ammo\npatch=1,EE,20999999,word,1\n// End ArchiveFS managed block\n".to_vec();
        fs::write(&legacy_path, &legacy_original).unwrap();
        let new_path = root.join("cheats/SCUS-97399_D6385328.pnach");
        assert!(!new_path.exists());

        let staged = stage_pcsx2_pnach(
            &root.join("staging"),
            &profile(&root),
            Some("SCUS-97399"),
            "D6385328",
            &selected(),
        )
        .unwrap();
        // Two verified-exact entries for the same identity in *one* report
        // is treated as an unresolvable ambiguity everywhere else in PCSX2
        // (`PreviewBlockerKind::MultipleExactMatches`) - migration must
        // stay a genuinely separate preview/plan/apply/rollback operation.
        let primary_preview = build_pcsx2_install_preview(&Pcsx2InstallPreviewRequest {
            selected_archive: PathBuf::from("/games/a.iso"),
            profile: profile(&root),
            identity: identity(Some("SCUS-97399"), "D6385328"),
            staged: staged.clone(),
        })
        .unwrap();
        assert_eq!(primary_preview.report.entries.len(), 1);
        assert_eq!(primary_preview.report.summary.blocked, 0);
        let legacy_preview = build_pcsx2_legacy_migration_preview(
            &staged,
            &profile(&root),
            &PathBuf::from("/games/a.iso"),
            "D6385328",
        )
        .unwrap()
        .expect("a legacy migration was staged");
        assert_eq!(legacy_preview.report.entries.len(), 1);
        assert_eq!(legacy_preview.report.summary.blocked, 0);

        let history_root = outer.join("history");
        let backup_root = outer.join("backups");

        let primary_plan = build_shared_transaction_plan(
            &primary_preview.report,
            "fixture",
            "pcsx2-managed-pnach",
            &primary_preview.staged.staging_root,
        )
        .unwrap();
        let primary_result = execute_shared_apply(
            &primary_plan,
            &SharedApplyOptions {
                dry_run: false,
                confirmation: Some(SharedApplyConfirmation {
                    plan_id: primary_plan.plan_id.clone(),
                    general_approved: true,
                    replacement_approved: true,
                }),
                operation_id: "migrate-primary".to_string(),
                timestamp_unix_seconds: 1_700_000_000,
                current_context: primary_plan.context.clone(),
                history_root: history_root.clone(),
                backup_root: backup_root.clone(),
            },
        );
        assert_eq!(
            primary_result.journal.status,
            SharedApplyStatus::Success,
            "{:#?}",
            primary_result.journal.entries
        );
        let primary_journal_path = primary_result.journal_path.as_ref().unwrap();

        let legacy_plan = build_shared_transaction_plan(
            &legacy_preview.report,
            "fixture",
            "pcsx2-managed-pnach",
            &legacy_preview.staged.staging_root,
        )
        .unwrap();
        let legacy_result = execute_shared_apply(
            &legacy_plan,
            &SharedApplyOptions {
                dry_run: false,
                confirmation: Some(SharedApplyConfirmation {
                    plan_id: legacy_plan.plan_id.clone(),
                    general_approved: true,
                    replacement_approved: true,
                }),
                operation_id: "migrate-legacy-cleanup".to_string(),
                timestamp_unix_seconds: 1_700_000_001,
                current_context: legacy_plan.context.clone(),
                history_root: history_root.clone(),
                backup_root: backup_root.clone(),
            },
        );
        assert_eq!(
            legacy_result.journal.status,
            SharedApplyStatus::Success,
            "{:#?}",
            legacy_result.journal.entries
        );
        let legacy_journal_path = legacy_result.journal_path.as_ref().unwrap();

        assert!(new_path.exists());
        let new_contents = fs::read_to_string(&new_path).unwrap();
        assert!(new_contents.contains("managed block: health"));
        assert!(new_contents.contains("managed block: ammo"));
        let legacy_contents = fs::read_to_string(&legacy_path).unwrap();
        assert!(!legacy_contents.contains("ArchiveFS managed block"));

        // Undo restores *both* files to their exact original states via
        // their own independent journals: the new file is removed (it
        // didn't exist before), the legacy file's managed block reappears
        // exactly as it was.
        let legacy_rollback_preview =
            preview_shared_rollback(legacy_journal_path, &root, &backup_root);
        assert!(
            legacy_rollback_preview.available,
            "{:#?}",
            legacy_rollback_preview
        );
        let legacy_rollback = execute_shared_rollback(
            &legacy_rollback_preview,
            &SharedRollbackOptions {
                confirmation: SharedRollbackConfirmation {
                    preview_id: legacy_rollback_preview.preview_id.clone(),
                    approved: true,
                },
                rollback_operation_id: "migrate-legacy-undo".to_string(),
                timestamp_unix_seconds: 1_700_000_100,
                history_root: history_root.clone(),
                backup_root: backup_root.clone(),
            },
        );
        assert_eq!(legacy_rollback.status, SharedApplyStatus::Success);
        assert_eq!(fs::read(&legacy_path).unwrap(), legacy_original);

        let primary_rollback_preview =
            preview_shared_rollback(primary_journal_path, &root, &backup_root);
        assert!(primary_rollback_preview.available);
        let primary_rollback = execute_shared_rollback(
            &primary_rollback_preview,
            &SharedRollbackOptions {
                confirmation: SharedRollbackConfirmation {
                    preview_id: primary_rollback_preview.preview_id.clone(),
                    approved: true,
                },
                rollback_operation_id: "migrate-primary-undo".to_string(),
                timestamp_unix_seconds: 1_700_000_101,
                history_root,
                backup_root,
            },
        );
        assert_eq!(primary_rollback.status, SharedApplyStatus::Success);
        assert!(!new_path.exists());
        let _ = fs::remove_dir_all(outer);
    }
}

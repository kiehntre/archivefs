//! Materializes a selected, merged PNACH into private staging and feeds the
//! existing shared preview/transaction pipeline. This module never writes to
//! the live PCSX2 profile.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use super::pcsx2::normalize_crc;
use super::pcsx2_identity::{Pcsx2GameIdentity, pcsx2_cheats_directory};
use super::pcsx2_local::Pcsx2Profile;
use super::pcsx2_pnach::{
    MAX_MANAGED_PNACH_BYTES, ManagedPnachCheat, merge_managed_pnach_cheats, parse_pnach_document,
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
}

pub fn stage_pcsx2_pnach(
    staging_root: &Path,
    profile: &Pcsx2Profile,
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
    let file_name = pcsx2_crc_filename(crc)?;
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
    let contents = merge_managed_pnach_cheats(&document, selected).map_err(|failure| {
        error(
            Pcsx2InstallPlanErrorKind::DocumentUnsafe,
            Some(&destination_path),
            failure.to_string(),
        )
    })?;
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
    let path = staging_root.join(&file_name);
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
        file.write_all(&contents)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if let Err(failure) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error(
            Pcsx2InstallPlanErrorKind::StagingUnavailable,
            Some(&path),
            format!("merged PNACH could not be staged atomically: {failure}"),
        ));
    }
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
    })
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
    let expected_name = pcsx2_crc_filename(crc)?;
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
            "staged PNACH no longer matches the selected game's CRC",
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
    Ok(Pcsx2InstallPreview {
        plain_summary: format!(
            "Ready to review {} selected cheat{}",
            request.staged.selected_cheat_ids.len(),
            if request.staged.selected_cheat_ids.len() == 1 {
                ""
            } else {
                "s"
            }
        ),
        technical_details: vec![
            format!("Verified CRC: {crc}"),
            format!("Destination: {}", request.staged.destination_path.display()),
            format!("Staged SHA-256: {}", request.staged.digest),
        ],
        report,
        staged: request.staged.clone(),
    })
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
}

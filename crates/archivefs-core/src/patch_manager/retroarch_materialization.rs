//! Read-only bridge from an immutable trusted RetroArch snapshot to shared preview.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::CatalogueGameEvidence;
use super::cheat_catalogue::{
    CheatMatchConfidence, build_cheat_availability_report, load_cheat_catalogue_snapshot,
};
use super::cheat_sources::{CheatSourceFreshness, inspect_retroarch_cheat_source_snapshot};
use super::shared_preview::{
    PreviewAdapter, PreviewIdentity, PreviewIdentityKind, PreviewIdentityState,
    PreviewMatchStrength, PreviewSourceItem, SharedPreviewError, SharedPreviewReport,
    SharedPreviewRequest, build_shared_preview,
};
use crate::emulator_environment::HostReadOnlyFilesystem;

pub const RETROARCH_MAX_MATERIALIZED_ENTRIES: usize = 64;

#[derive(Debug, Clone)]
pub struct RetroArchMaterializationRequest {
    pub snapshot_root: PathBuf,
    pub expected_snapshot_id: String,
    pub source_id: String,
    pub selected_archive: PathBuf,
    pub archive_display_name: String,
    pub archive_normalized_name: String,
    pub platform: String,
    pub region: Option<String>,
    pub destination_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetroArchMaterializedSource {
    pub snapshot_id: String,
    pub snapshot_root: PathBuf,
    pub catalogue_root: PathBuf,
    pub source_path: PathBuf,
    pub source_digest: String,
    pub destination_relative_path: PathBuf,
    pub match_strength: PreviewMatchStrength,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchMaterializedPreview {
    pub snapshot_id: String,
    pub snapshot_root: PathBuf,
    pub catalogue_root: PathBuf,
    pub sources: Vec<RetroArchMaterializedSource>,
    pub preview: SharedPreviewReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetroArchMaterializationErrorKind {
    SnapshotIdentityMismatch,
    SnapshotUnavailable,
    SnapshotStale,
    SnapshotIncomplete,
    SnapshotPathLossy,
    SourceMissing,
    SourceEscapesSnapshot,
    SourceNotInManifest,
    SourceDigestMismatch,
    NoEligibleMatch,
    ResourceLimitReached,
    PreviewFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetroArchMaterializationError {
    pub kind: RetroArchMaterializationErrorKind,
    pub path: Option<PathBuf>,
    pub detail: String,
}

impl std::fmt::Display for RetroArchMaterializationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.detail)
    }
}

impl std::error::Error for RetroArchMaterializationError {}

pub fn materialize_retroarch_shared_preview(
    request: &RetroArchMaterializationRequest,
) -> Result<RetroArchMaterializedPreview, RetroArchMaterializationError> {
    let snapshot_id = request
        .snapshot_root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            error(
                RetroArchMaterializationErrorKind::SnapshotPathLossy,
                Some(&request.snapshot_root),
                "snapshot identity cannot be represented exactly",
            )
        })?;
    if snapshot_id != request.expected_snapshot_id {
        return Err(error(
            RetroArchMaterializationErrorKind::SnapshotIdentityMismatch,
            Some(&request.snapshot_root),
            "selected snapshot no longer matches the approved content identity",
        ));
    }
    let inspection =
        inspect_retroarch_cheat_source_snapshot(&request.snapshot_root).map_err(|failure| {
            let kind = if failure.code == "catalogue_manifest_mismatch" {
                RetroArchMaterializationErrorKind::SourceDigestMismatch
            } else {
                RetroArchMaterializationErrorKind::SnapshotUnavailable
            };
            error(kind, Some(&request.snapshot_root), &failure.to_string())
        })?;
    if inspection.source.source_id != request.source_id {
        return Err(error(
            RetroArchMaterializationErrorKind::SnapshotIdentityMismatch,
            Some(&request.snapshot_root),
            "snapshot source identifier changed",
        ));
    }
    if inspection.freshness != CheatSourceFreshness::Fresh {
        return Err(error(
            RetroArchMaterializationErrorKind::SnapshotStale,
            Some(&request.snapshot_root),
            "trusted catalogue snapshot is stale; apply is blocked without fetching automatically",
        ));
    }
    if !inspection.setup_usable {
        return Err(error(
            RetroArchMaterializationErrorKind::SnapshotIncomplete,
            Some(&request.snapshot_root),
            "trusted catalogue snapshot validation is incomplete",
        ));
    }
    let manifest = inspection.manifest.ok_or_else(|| {
        error(
            RetroArchMaterializationErrorKind::SnapshotIncomplete,
            Some(&request.snapshot_root),
            "trusted catalogue snapshot has no verified manifest",
        )
    })?;
    if !manifest.validation_complete || manifest.archive_sha256 != request.expected_snapshot_id {
        return Err(error(
            RetroArchMaterializationErrorKind::SnapshotIncomplete,
            Some(&request.snapshot_root),
            "trusted catalogue manifest is incomplete or bound to another snapshot",
        ));
    }
    let encoded_catalogue = inspection.current_catalogue_path.ok_or_else(|| {
        error(
            RetroArchMaterializationErrorKind::SnapshotUnavailable,
            Some(&request.snapshot_root),
            "trusted catalogue root is unavailable",
        )
    })?;
    if encoded_catalogue.lossy {
        return Err(error(
            RetroArchMaterializationErrorKind::SnapshotPathLossy,
            Some(&request.snapshot_root),
            "trusted catalogue root cannot be reconstructed losslessly",
        ));
    }
    let catalogue_root = PathBuf::from(encoded_catalogue.display);
    let snapshot =
        load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, &request.source_id, &catalogue_root);
    if !snapshot.complete {
        return Err(error(
            RetroArchMaterializationErrorKind::SnapshotIncomplete,
            Some(&catalogue_root),
            "catalogue parsing is incomplete; no source is materialized",
        ));
    }
    let evidence = CatalogueGameEvidence {
        archive_id: 1,
        is_present: true,
        display_name: request.archive_display_name.clone(),
        normalized_name: request.archive_normalized_name.clone(),
        platform: Some(request.platform.clone()),
        region: request.region.clone(),
        serial: None,
        executable_crc: None,
    };
    let availability = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &[evidence],
        None,
        Some(&request.destination_root),
    );
    let mut sources = Vec::new();
    for entry in availability.entries {
        if !entry.game.parsing_complete
            || entry.game_match.candidates.len() != 1
            || entry.game_match.candidates[0].archive_id != 1
            || !entry.staging_candidate
            || !matches!(
                entry.game_match.confidence,
                CheatMatchConfidence::Exact | CheatMatchConfidence::Strong
            )
        {
            continue;
        }
        if sources.len() >= RETROARCH_MAX_MATERIALIZED_ENTRIES {
            return Err(error(
                RetroArchMaterializationErrorKind::ResourceLimitReached,
                Some(&catalogue_root),
                "materialized source-entry limit reached",
            ));
        }
        if entry.game.source_file_path.lossy {
            return Err(error(
                RetroArchMaterializationErrorKind::SnapshotPathLossy,
                None,
                "catalogue source path cannot be reconstructed losslessly",
            ));
        }
        let source_path = PathBuf::from(&entry.game.source_file_path.display);
        let relative = source_path.strip_prefix(&catalogue_root).map_err(|_| {
            error(
                RetroArchMaterializationErrorKind::SourceEscapesSnapshot,
                Some(&source_path),
                "catalogue source escapes the verified catalogue root",
            )
        })?;
        if relative.as_os_str().is_empty()
            || source_path.strip_prefix(&request.snapshot_root).is_err()
        {
            return Err(error(
                RetroArchMaterializationErrorKind::SourceEscapesSnapshot,
                Some(&source_path),
                "catalogue source is outside the immutable snapshot",
            ));
        }
        let relative_text = relative
            .to_str()
            .ok_or_else(|| {
                error(
                    RetroArchMaterializationErrorKind::SnapshotPathLossy,
                    Some(&source_path),
                    "catalogue manifest path is not UTF-8",
                )
            })?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let manifest_file = manifest
            .files
            .iter()
            .find(|file| file.relative_path == relative_text)
            .ok_or_else(|| {
                error(
                    RetroArchMaterializationErrorKind::SourceNotInManifest,
                    Some(&source_path),
                    "catalogue source is absent from the verified snapshot manifest",
                )
            })?;
        let source_digest = entry.staging_plan.source_file_hash.clone().ok_or_else(|| {
            error(
                RetroArchMaterializationErrorKind::SourceDigestMismatch,
                Some(&source_path),
                "catalogue parser did not retain a source digest",
            )
        })?;
        if !source_digest.eq_ignore_ascii_case(&manifest_file.sha256) {
            return Err(error(
                RetroArchMaterializationErrorKind::SourceDigestMismatch,
                Some(&source_path),
                "catalogue source digest disagrees with the verified snapshot manifest",
            ));
        }
        let destination = entry
            .staging_plan
            .proposed_destination_path
            .as_ref()
            .ok_or_else(|| {
                error(
                    RetroArchMaterializationErrorKind::NoEligibleMatch,
                    Some(&source_path),
                    "eligible catalogue match has no exact destination",
                )
            })?;
        if destination.lossy {
            return Err(error(
                RetroArchMaterializationErrorKind::SnapshotPathLossy,
                None,
                "destination cannot be reconstructed losslessly",
            ));
        }
        let destination_path = PathBuf::from(&destination.display);
        let destination_relative_path = destination_path
            .strip_prefix(&request.destination_root)
            .map_err(|_| {
                error(
                    RetroArchMaterializationErrorKind::NoEligibleMatch,
                    Some(&destination_path),
                    "catalogue destination escapes the selected profile root",
                )
            })?
            .to_path_buf();
        let match_strength = match entry.game_match.confidence {
            CheatMatchConfidence::Exact => PreviewMatchStrength::VerifiedExact,
            CheatMatchConfidence::Strong => PreviewMatchStrength::Strong,
            _ => unreachable!(),
        };
        sources.push(RetroArchMaterializedSource {
            snapshot_id: request.expected_snapshot_id.clone(),
            snapshot_root: request.snapshot_root.clone(),
            catalogue_root: catalogue_root.clone(),
            source_path,
            source_digest,
            destination_relative_path,
            match_strength,
        });
    }
    if sources.is_empty() {
        return Err(error(
            RetroArchMaterializationErrorKind::NoEligibleMatch,
            Some(&catalogue_root),
            "no exact or approved strong trusted-catalogue match is eligible",
        ));
    }
    sources.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    let identity_value = format!("{}:archive:1", request.expected_snapshot_id);
    let preview = build_shared_preview(&SharedPreviewRequest {
        adapter: PreviewAdapter::RetroArch,
        selected_archive: request.selected_archive.clone(),
        platform: Some(request.platform.clone()),
        identity: PreviewIdentity {
            kind: PreviewIdentityKind::RetroArchCatalogueMatch,
            state: PreviewIdentityState::Verified,
            value: Some(identity_value),
            archive_path: request.selected_archive.clone(),
            revision: None,
        },
        destination_root: request.destination_root.clone(),
        source_items: sources
            .iter()
            .map(|source| PreviewSourceItem {
                adapter: PreviewAdapter::RetroArch,
                source_path: source.source_path.clone(),
                expected_source_digest: Some(source.source_digest.clone()),
                destination_relative_paths: vec![source.destination_relative_path.clone()],
                match_strength: source.match_strength,
            })
            .collect(),
    })
    .map_err(|failure| preview_error(failure, &catalogue_root))?;
    Ok(RetroArchMaterializedPreview {
        snapshot_id: request.expected_snapshot_id.clone(),
        snapshot_root: request.snapshot_root.clone(),
        catalogue_root,
        sources,
        preview,
    })
}

fn preview_error(error_value: SharedPreviewError, path: &Path) -> RetroArchMaterializationError {
    error(
        RetroArchMaterializationErrorKind::PreviewFailed,
        Some(path),
        &error_value.to_string(),
    )
}

fn error(
    kind: RetroArchMaterializationErrorKind,
    path: Option<&Path>,
    detail: &str,
) -> RetroArchMaterializationError {
    RetroArchMaterializationError {
        kind,
        path: path.map(Path::to_path_buf),
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::patch_manager::{CheatSourceManifest, CheatSourceManifestFile};

    struct Fixture {
        root: PathBuf,
        snapshot: PathBuf,
        destination: PathBuf,
        source: PathBuf,
        manifest: PathBuf,
        id: String,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn fixture(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "archivefs-retroarch-materialization-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let id = "a".repeat(64);
        let source_root = root.join("libretro-buildbot-cheats");
        let snapshot = source_root.join("snapshots").join(&id);
        let destination = root.join("destination");
        let relative = Path::new("Atari - 2600").join("Frogger (USA).cht");
        let source = snapshot.join(&relative);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::create_dir_all(source_root.join("manifests")).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let bytes = b"cheats = 1\ncheat0_desc = \"Infinite lives\"\ncheat0_code = \"ABCD\"\ncheat0_enable = false\n";
        fs::write(&source, bytes).unwrap();
        let manifest = CheatSourceManifest {
            format_version: 1,
            source_id: "libretro-buildbot-cheats".into(),
            source_url: "https://example.invalid/catalogue.zip".into(),
            pinned_version: None,
            fetched_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            downloaded_bytes: bytes.len() as u64,
            extracted_bytes: bytes.len() as u64,
            archive_entry_count: 1,
            archive_sha256: id.clone(),
            response_content_type: None,
            response_etag: None,
            response_last_modified: None,
            catalogue_file_count: 1,
            valid_cheat_count: 1,
            malformed_cheat_count: 0,
            skipped_entry_count: 0,
            discovered_platforms: vec!["Atari - 2600".into()],
            validation_complete: true,
            warnings: vec![],
            catalogue_relative_path: String::new(),
            cache_relative_path: format!("snapshots/{id}"),
            files: vec![CheatSourceManifestFile {
                relative_path: "Atari - 2600/Frogger (USA).cht".into(),
                size: bytes.len() as u64,
                sha256: sha256(bytes),
            }],
        };
        let manifest_path = source_root.join("manifests").join(format!("{id}.json"));
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        Fixture {
            root,
            snapshot,
            destination,
            source,
            manifest: manifest_path,
            id,
        }
    }

    fn request(fixture: &Fixture) -> RetroArchMaterializationRequest {
        RetroArchMaterializationRequest {
            snapshot_root: fixture.snapshot.clone(),
            expected_snapshot_id: fixture.id.clone(),
            source_id: "libretro-buildbot-cheats".into(),
            selected_archive: fixture.root.join("Frogger (USA).zip"),
            archive_display_name: "Frogger (USA)".into(),
            archive_normalized_name: "frogger usa".into(),
            platform: "Atari2600".into(),
            region: Some("USA".into()),
            destination_root: fixture.destination.clone(),
        }
    }

    #[test]
    fn exact_trusted_entry_materializes_without_copying_source() {
        let fixture = fixture("exact");
        let before = fs::metadata(&fixture.source).unwrap().len();
        let result = materialize_retroarch_shared_preview(&request(&fixture)).unwrap();
        assert_eq!(result.sources.len(), 1);
        assert_eq!(result.sources[0].source_path, fixture.source);
        assert_eq!(fs::metadata(&fixture.source).unwrap().len(), before);
        assert_eq!(result.preview.summary.blocked, 0);
    }

    #[test]
    fn digest_mismatch_and_stale_snapshot_fail_closed() {
        let changed_fixture = fixture("digest-stale");
        fs::write(&changed_fixture.source, b"changed").unwrap();
        let digest_error =
            materialize_retroarch_shared_preview(&request(&changed_fixture)).unwrap_err();
        assert_eq!(
            digest_error.kind,
            RetroArchMaterializationErrorKind::SourceDigestMismatch
        );

        let fixture = fixture("stale");
        let mut manifest: CheatSourceManifest =
            serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
        manifest.fetched_at_unix_seconds = 0;
        fs::write(&fixture.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let stale = materialize_retroarch_shared_preview(&request(&fixture)).unwrap_err();
        assert_eq!(stale.kind, RetroArchMaterializationErrorKind::SnapshotStale);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_source_is_refused_and_missing_source_does_not_fetch() {
        use std::os::unix::fs::symlink;

        let linked_fixture = fixture("symlink");
        let original = linked_fixture.source.with_extension("original");
        fs::rename(&linked_fixture.source, &original).unwrap();
        symlink(&original, &linked_fixture.source).unwrap();
        assert!(materialize_retroarch_shared_preview(&request(&linked_fixture)).is_err());

        let fixture = fixture("missing");
        fs::remove_file(&fixture.source).unwrap();
        let missing = materialize_retroarch_shared_preview(&request(&fixture)).unwrap_err();
        assert_eq!(
            missing.kind,
            RetroArchMaterializationErrorKind::SourceDigestMismatch
        );
        assert!(!fixture.source.exists());
    }
}

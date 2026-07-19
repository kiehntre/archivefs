//! Bounded, read-only inventory of existing RetroArch cheat and soft-patch artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::emulator_environment::retroarch::{PathPurpose, ProfileRef, RetroArchEnvironmentReport};
use crate::emulator_environment::{
    BoundedListResult, BoundedReadResult, EncodedPath, FsProbe, ReadOnlyHostFilesystem,
    os_str_bytes,
};

use super::retroarch::{
    DestinationKind, PlaylistEvidence, PlaylistMatchConfidence, RetroArchAdvisoryEntry,
};

pub const RETROARCH_ARTIFACT_INVENTORY_FORMAT_VERSION: u32 = 1;
pub const MAX_CHEAT_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_PATCH_METADATA_BYTES: usize = 1024 * 1024;
pub const MAX_ARTIFACTS_PER_DIRECTORY: usize = 8192;
pub const MAX_TOTAL_ARTIFACTS: usize = 8192;
pub const MAX_DIRECTORIES_PER_PROFILE: usize = 8192;
pub const MAX_CHEAT_ENTRIES_PER_FILE: usize = 16384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Cheat,
    SoftPatchIps,
    SoftPatchBps,
    SoftPatchUps,
    SoftPatchXdelta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAssociationConfidence {
    Exact,
    Strong,
    Weak,
    Ambiguous,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactConflictState {
    Empty,
    Matched,
    Occupied,
    Duplicate,
    Conflicting,
    Orphaned,
    Ambiguous,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactDiagnostic {
    pub code: &'static str,
    pub severity: ArtifactDiagnosticSeverity,
    pub profile: Option<ProfileRef>,
    pub path: Option<EncodedPath>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactCatalogueGame {
    pub archive_id: i64,
    pub display_name: String,
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactPlaylistEvidence {
    pub playlist_file: EncodedPath,
    pub playlist_name: String,
    pub entry_index: u32,
    pub confidence: PlaylistMatchConfidence,
    pub core_stem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactAssociation {
    pub confidence: ArtifactAssociationConfidence,
    /// Stable evidence identifiers, sorted and deduplicated.
    pub evidence: Vec<&'static str>,
    pub catalogue_games: Vec<ArtifactCatalogueGame>,
    pub playlist_evidence: Vec<ArtifactPlaylistEvidence>,
    pub core_stems: Vec<String>,
    pub expected_destinations: Vec<EncodedPath>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheatFileSummary {
    pub description: Option<String>,
    pub declared_cheat_count: Option<u32>,
    pub parsed_cheat_entries: usize,
    pub enabled_cheat_entries: usize,
    pub any_cheats_enabled: bool,
    pub malformed_lines: Vec<u32>,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchArtifactFinding {
    /// `None` for profile-independent soft-patch siblings.
    pub profile: Option<ProfileRef>,
    pub artifact_kind: ArtifactKind,
    pub path: EncodedPath,
    pub filename: EncodedPath,
    pub size_bytes: Option<u64>,
    pub probe: FsProbe,
    pub symlink: bool,
    pub association: ArtifactAssociation,
    pub occupies_expected_destination: bool,
    pub conflict_state: ArtifactConflictState,
    pub cheat_summary: Option<CheatFileSummary>,
    pub diagnostics: Vec<ArtifactDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchArtifactDestination {
    pub profile: Option<ProfileRef>,
    pub artifact_kind: ArtifactKind,
    pub path: EncodedPath,
    pub catalogue_games: Vec<ArtifactCatalogueGame>,
    pub probe: FsProbe,
    pub size_bytes: Option<u64>,
    pub state: ArtifactConflictState,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchArtifactSummary {
    pub artifacts_found: usize,
    pub cheat_files: usize,
    pub soft_patch_files: usize,
    pub expected_destinations: usize,
    pub empty_destinations: usize,
    pub occupied_destinations: usize,
    pub duplicate_artifacts: usize,
    pub conflicting_artifacts: usize,
    pub orphaned_artifacts: usize,
    pub ambiguous_artifacts: usize,
    pub unsupported_artifacts: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchArtifactInventory {
    pub format_version: u32,
    pub read_only: bool,
    pub complete: bool,
    pub findings: Vec<RetroArchArtifactFinding>,
    pub destinations: Vec<RetroArchArtifactDestination>,
    pub diagnostics: Vec<ArtifactDiagnostic>,
    pub summary: RetroArchArtifactSummary,
}

#[derive(Clone)]
struct ExpectedArtifact {
    profile: Option<ProfileRef>,
    kind: ArtifactKind,
    path: PathBuf,
    game: ArtifactCatalogueGame,
    core_stem: Option<String>,
    playlist_evidence: Vec<ArtifactPlaylistEvidence>,
}

#[derive(Clone)]
struct RawArtifact {
    profile: Option<ProfileRef>,
    kind: ArtifactKind,
    path: PathBuf,
    file_name: OsString,
    size_bytes: Option<u64>,
    probe: FsProbe,
}

pub(crate) fn build_retroarch_artifact_inventory(
    filesystem: &dyn ReadOnlyHostFilesystem,
    environment: &RetroArchEnvironmentReport,
    entries: &[RetroArchAdvisoryEntry],
) -> RetroArchArtifactInventory {
    let (expected, mut diagnostics, mut complete) = collect_expected_artifacts(entries);
    let mut raw = BTreeMap::<(Option<ProfileRef>, Vec<u8>), RawArtifact>::new();

    scan_patch_directories(
        filesystem,
        &expected,
        &mut raw,
        &mut diagnostics,
        &mut complete,
    );
    scan_cheat_directories(
        filesystem,
        environment,
        &mut raw,
        &mut diagnostics,
        &mut complete,
    );
    add_exact_expected_artifacts(filesystem, &expected, &mut raw, &mut complete);

    let mut findings = raw
        .into_values()
        .map(|artifact| build_finding(filesystem, artifact, &expected))
        .collect::<Vec<_>>();
    mark_duplicates(&mut findings);
    findings.sort_by(finding_order);

    let destinations = build_destinations(filesystem, &expected);
    let summary = summarize(&findings, &destinations);
    RetroArchArtifactInventory {
        format_version: RETROARCH_ARTIFACT_INVENTORY_FORMAT_VERSION,
        read_only: true,
        complete,
        findings,
        destinations,
        diagnostics,
        summary,
    }
}

fn collect_expected_artifacts(
    entries: &[RetroArchAdvisoryEntry],
) -> (Vec<ExpectedArtifact>, Vec<ArtifactDiagnostic>, bool) {
    let mut expected = Vec::new();
    let mut diagnostics = Vec::new();
    let mut complete = true;
    for entry in entries {
        let game = ArtifactCatalogueGame {
            archive_id: entry.archive_id,
            display_name: entry.display_name.clone(),
            platform: entry.platform.clone(),
        };
        for destination in &entry.soft_patch_candidates {
            let Some(kind) = kind_from_destination(destination.kind, destination.path.as_ref())
            else {
                continue;
            };
            let Some(path) = lossless_path(destination.path.as_ref()) else {
                complete = false;
                diagnostics.push(ArtifactDiagnostic {
                    code: "expected_patch_destination_lossy_path_not_inventoried",
                    severity: ArtifactDiagnosticSeverity::Warning,
                    profile: None,
                    path: destination.path.clone(),
                });
                continue;
            };
            expected.push(ExpectedArtifact {
                profile: None,
                kind,
                path,
                game: game.clone(),
                core_stem: None,
                playlist_evidence: Vec::new(),
            });
        }
        for outcome in &entry.profile_outcomes {
            let Some(path) = lossless_path(outcome.per_game_cheat_file.path.as_ref()) else {
                if outcome.per_game_cheat_file.path.is_some() {
                    complete = false;
                    diagnostics.push(ArtifactDiagnostic {
                        code: "expected_cheat_destination_lossy_path_not_inventoried",
                        severity: ArtifactDiagnosticSeverity::Warning,
                        profile: Some(outcome.profile),
                        path: outcome.per_game_cheat_file.path.clone(),
                    });
                }
                continue;
            };
            expected.push(ExpectedArtifact {
                profile: Some(outcome.profile),
                kind: ArtifactKind::Cheat,
                path,
                game: game.clone(),
                core_stem: outcome.matched_core_stem.clone(),
                playlist_evidence: outcome
                    .playlist_evidence
                    .iter()
                    .map(compact_playlist_evidence)
                    .collect(),
            });
        }
    }
    expected.sort_by(expected_order);
    (expected, diagnostics, complete)
}

fn compact_playlist_evidence(evidence: &PlaylistEvidence) -> ArtifactPlaylistEvidence {
    use super::retroarch::CoreAssociation;
    let core_stem = match &evidence.core_association {
        CoreAssociation::LinkedByCorePath { core_stem }
        | CoreAssociation::LinkedByCoreName { core_stem } => Some(core_stem.clone()),
        _ => None,
    };
    ArtifactPlaylistEvidence {
        playlist_file: evidence.playlist_file.clone(),
        playlist_name: evidence.playlist_name.clone(),
        entry_index: evidence.entry_index,
        confidence: evidence.confidence,
        core_stem,
    }
}

fn kind_from_destination(
    destination_kind: DestinationKind,
    path: Option<&EncodedPath>,
) -> Option<ArtifactKind> {
    match destination_kind {
        DestinationKind::PerGameCheatFile => Some(ArtifactKind::Cheat),
        DestinationKind::SoftPatchSibling => {
            path.and_then(|path| artifact_kind_for_path(Path::new(&path.display)))
        }
        DestinationKind::CheatDatabaseRoot | DestinationKind::Unsupported => None,
    }
}

fn artifact_kind_for_path(path: &Path) -> Option<ArtifactKind> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "cht" => Some(ArtifactKind::Cheat),
        "ips" => Some(ArtifactKind::SoftPatchIps),
        "bps" => Some(ArtifactKind::SoftPatchBps),
        "ups" => Some(ArtifactKind::SoftPatchUps),
        "xdelta" => Some(ArtifactKind::SoftPatchXdelta),
        _ => None,
    }
}

fn lossless_path(path: Option<&EncodedPath>) -> Option<PathBuf> {
    let path = path?;
    (!path.lossy).then(|| PathBuf::from(&path.display))
}

fn scan_patch_directories(
    filesystem: &dyn ReadOnlyHostFilesystem,
    expected: &[ExpectedArtifact],
    raw: &mut BTreeMap<(Option<ProfileRef>, Vec<u8>), RawArtifact>,
    diagnostics: &mut Vec<ArtifactDiagnostic>,
    complete: &mut bool,
) {
    let directories = expected
        .iter()
        .filter(|item| item.kind != ArtifactKind::Cheat)
        .filter_map(|item| item.path.parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>();
    if directories.len() > MAX_DIRECTORIES_PER_PROFILE {
        *complete = false;
        diagnostics.push(ArtifactDiagnostic {
            code: "patch_directory_limit_reached",
            severity: ArtifactDiagnosticSeverity::Warning,
            profile: None,
            path: None,
        });
    }
    for directory in directories.into_iter().take(MAX_DIRECTORIES_PER_PROFILE) {
        match filesystem.list_dir_bounded(&directory, MAX_ARTIFACTS_PER_DIRECTORY) {
            BoundedListResult::Ok(entries) => {
                for entry in entries {
                    let path = directory.join(&entry.file_name);
                    let Some(kind) = artifact_kind_for_path(&path) else {
                        continue;
                    };
                    if kind == ArtifactKind::Cheat {
                        continue;
                    }
                    insert_raw(
                        raw,
                        RawArtifact {
                            profile: None,
                            kind,
                            path,
                            file_name: entry.file_name,
                            size_bytes: entry.size_bytes,
                            probe: entry.probe,
                        },
                        complete,
                    );
                }
            }
            result => {
                *complete = false;
                diagnostics.push(list_diagnostic("patch_directory", None, &directory, result));
            }
        }
    }
}

fn scan_cheat_directories(
    filesystem: &dyn ReadOnlyHostFilesystem,
    environment: &RetroArchEnvironmentReport,
    raw: &mut BTreeMap<(Option<ProfileRef>, Vec<u8>), RawArtifact>,
    diagnostics: &mut Vec<ArtifactDiagnostic>,
    complete: &mut bool,
) {
    for profile in &environment.profiles {
        let profile_ref = ProfileRef {
            profile_kind: profile.profile_kind,
            scope: profile.scope,
        };
        let Some(finding) = profile
            .paths
            .iter()
            .find(|finding| finding.purpose == PathPurpose::Cheats)
        else {
            continue;
        };
        let Some(encoded_root) = finding.resolved_path.as_ref() else {
            continue;
        };
        if encoded_root.lossy {
            *complete = false;
            diagnostics.push(ArtifactDiagnostic {
                code: "cheat_root_lossy_path_not_scanned",
                severity: ArtifactDiagnosticSeverity::Warning,
                profile: Some(profile_ref),
                path: Some(encoded_root.clone()),
            });
            continue;
        }
        let root = PathBuf::from(&encoded_root.display);
        if filesystem.probe(&root) != FsProbe::PresentDirectory {
            continue;
        }

        let mut pending = vec![root];
        let mut visited = BTreeSet::<PathBuf>::new();
        let mut directory_limit_reported = false;
        while let Some(directory) = pending.pop() {
            if !visited.insert(directory.clone()) {
                continue;
            }
            if visited.len() > MAX_DIRECTORIES_PER_PROFILE {
                *complete = false;
                diagnostics.push(ArtifactDiagnostic {
                    code: "cheat_directory_limit_reached",
                    severity: ArtifactDiagnosticSeverity::Warning,
                    profile: Some(profile_ref),
                    path: Some(EncodedPath::from_path(&directory)),
                });
                break;
            }
            match filesystem.list_dir_bounded(&directory, MAX_ARTIFACTS_PER_DIRECTORY) {
                BoundedListResult::Ok(mut entries) => {
                    entries.sort_by(|left, right| {
                        os_str_bytes(&left.file_name).cmp(os_str_bytes(&right.file_name))
                    });
                    for entry in entries {
                        let path = directory.join(&entry.file_name);
                        if entry.probe == FsProbe::PresentDirectory {
                            if visited.len() + pending.len() < MAX_DIRECTORIES_PER_PROFILE {
                                pending.push(path);
                            } else if !directory_limit_reported {
                                *complete = false;
                                directory_limit_reported = true;
                                diagnostics.push(ArtifactDiagnostic {
                                    code: "cheat_directory_limit_reached",
                                    severity: ArtifactDiagnosticSeverity::Warning,
                                    profile: Some(profile_ref),
                                    path: Some(EncodedPath::from_path(&directory)),
                                });
                            }
                            continue;
                        }
                        if artifact_kind_for_path(&path) != Some(ArtifactKind::Cheat) {
                            continue;
                        }
                        insert_raw(
                            raw,
                            RawArtifact {
                                profile: Some(profile_ref),
                                kind: ArtifactKind::Cheat,
                                path,
                                file_name: entry.file_name,
                                size_bytes: entry.size_bytes,
                                probe: entry.probe,
                            },
                            complete,
                        );
                    }
                    pending.sort_by(|left, right| {
                        os_str_bytes(right.as_os_str()).cmp(os_str_bytes(left.as_os_str()))
                    });
                }
                result => {
                    *complete = false;
                    diagnostics.push(list_diagnostic(
                        "cheat_directory",
                        Some(profile_ref),
                        &directory,
                        result,
                    ));
                }
            }
        }
    }
}

fn list_diagnostic(
    prefix: &'static str,
    profile: Option<ProfileRef>,
    path: &Path,
    result: BoundedListResult,
) -> ArtifactDiagnostic {
    let suffix = match result {
        BoundedListResult::TooLarge => "listing_too_large",
        BoundedListResult::NotFound => "missing",
        BoundedListResult::WrongType => "wrong_type",
        BoundedListResult::Symlink => "symlink_not_followed",
        BoundedListResult::Inaccessible => "inaccessible",
        BoundedListResult::IoError => "io_error",
        BoundedListResult::Ok(_) => unreachable!(),
    };
    let code = match (prefix, suffix) {
        ("patch_directory", "listing_too_large") => "patch_directory_listing_too_large",
        ("patch_directory", "missing") => "patch_directory_missing",
        ("patch_directory", "wrong_type") => "patch_directory_wrong_type",
        ("patch_directory", "symlink_not_followed") => "patch_directory_symlink_not_followed",
        ("patch_directory", "inaccessible") => "patch_directory_inaccessible",
        ("patch_directory", "io_error") => "patch_directory_io_error",
        ("cheat_directory", "listing_too_large") => "cheat_directory_listing_too_large",
        ("cheat_directory", "missing") => "cheat_directory_missing",
        ("cheat_directory", "wrong_type") => "cheat_directory_wrong_type",
        ("cheat_directory", "symlink_not_followed") => "cheat_directory_symlink_not_followed",
        ("cheat_directory", "inaccessible") => "cheat_directory_inaccessible",
        ("cheat_directory", "io_error") => "cheat_directory_io_error",
        _ => "artifact_directory_error",
    };
    ArtifactDiagnostic {
        code,
        severity: ArtifactDiagnosticSeverity::Warning,
        profile,
        path: Some(EncodedPath::from_path(path)),
    }
}

fn insert_raw(
    raw: &mut BTreeMap<(Option<ProfileRef>, Vec<u8>), RawArtifact>,
    artifact: RawArtifact,
    complete: &mut bool,
) {
    if raw.len() >= MAX_TOTAL_ARTIFACTS {
        *complete = false;
        return;
    }
    let key = (
        artifact.profile,
        os_str_bytes(artifact.path.as_os_str()).to_vec(),
    );
    raw.entry(key).or_insert(artifact);
}

fn add_exact_expected_artifacts(
    filesystem: &dyn ReadOnlyHostFilesystem,
    expected: &[ExpectedArtifact],
    raw: &mut BTreeMap<(Option<ProfileRef>, Vec<u8>), RawArtifact>,
    complete: &mut bool,
) {
    for item in expected {
        let probe = filesystem.probe(&item.path);
        if probe == FsProbe::Missing {
            continue;
        }
        let Some(file_name) = item.path.file_name() else {
            continue;
        };
        let artifact = RawArtifact {
            profile: item.profile,
            kind: item.kind,
            path: item.path.clone(),
            file_name: file_name.to_os_string(),
            size_bytes: filesystem.size_no_follow(&item.path),
            probe,
        };
        let key = (
            artifact.profile,
            os_str_bytes(artifact.path.as_os_str()).to_vec(),
        );
        if !raw.contains_key(&key) && raw.len() >= MAX_TOTAL_ARTIFACTS {
            *complete = false;
            continue;
        }
        raw.entry(key).or_insert(artifact);
    }
}

fn build_finding(
    filesystem: &dyn ReadOnlyHostFilesystem,
    artifact: RawArtifact,
    expected: &[ExpectedArtifact],
) -> RetroArchArtifactFinding {
    let exact = expected
        .iter()
        .filter(|item| {
            item.profile == artifact.profile
                && item.kind == artifact.kind
                && item.path == artifact.path
        })
        .collect::<Vec<_>>();
    let (candidates, confidence, evidence, occupies_expected_destination) = if !exact.is_empty() {
        (
            exact,
            ArtifactAssociationConfidence::Exact,
            vec!["exact_expected_destination"],
            true,
        )
    } else {
        weaker_candidates(&artifact, expected)
    };

    let mut diagnostics = Vec::new();
    let cheat_summary = if artifact.kind == ArtifactKind::Cheat {
        read_cheat_summary(filesystem, &artifact, &mut diagnostics)
    } else {
        None
    };
    if artifact.probe == FsProbe::Symlink {
        diagnostics.push(ArtifactDiagnostic {
            code: "artifact_symlink_not_followed",
            severity: ArtifactDiagnosticSeverity::Warning,
            profile: artifact.profile,
            path: Some(EncodedPath::from_path(&artifact.path)),
        });
    }
    if artifact.kind != ArtifactKind::Cheat
        && artifact
            .size_bytes
            .is_some_and(|size| size > MAX_PATCH_METADATA_BYTES as u64)
    {
        diagnostics.push(ArtifactDiagnostic {
            code: "patch_file_metadata_only_size_limit_exceeded",
            severity: ArtifactDiagnosticSeverity::Info,
            profile: artifact.profile,
            path: Some(EncodedPath::from_path(&artifact.path)),
        });
    }

    let mut games = candidates
        .iter()
        .map(|item| item.game.clone())
        .collect::<Vec<_>>();
    games.sort_by_key(|game| game.archive_id);
    games.dedup_by_key(|game| game.archive_id);
    let mut core_stems = candidates
        .iter()
        .filter_map(|item| item.core_stem.clone())
        .collect::<Vec<_>>();
    core_stems.sort();
    core_stems.dedup();
    let mut playlist_evidence = candidates
        .iter()
        .flat_map(|item| item.playlist_evidence.clone())
        .collect::<Vec<_>>();
    playlist_evidence.sort_by(|left, right| {
        left.playlist_file
            .display
            .cmp(&right.playlist_file.display)
            .then_with(|| left.entry_index.cmp(&right.entry_index))
    });
    playlist_evidence.dedup_by(|left, right| {
        left.playlist_file == right.playlist_file && left.entry_index == right.entry_index
    });
    let mut expected_destinations = candidates
        .iter()
        .map(|item| EncodedPath::from_path(&item.path))
        .collect::<Vec<_>>();
    expected_destinations.sort_by(|left, right| left.display.cmp(&right.display));
    expected_destinations.dedup_by(|left, right| left == right);

    let conflict_state = if matches!(artifact.probe, FsProbe::Inaccessible | FsProbe::IoError) {
        ArtifactConflictState::Unsupported
    } else if artifact.probe != FsProbe::PresentFile {
        ArtifactConflictState::Conflicting
    } else {
        match confidence {
            ArtifactAssociationConfidence::Exact | ArtifactAssociationConfidence::Strong => {
                if games.len() == 1 {
                    ArtifactConflictState::Matched
                } else {
                    ArtifactConflictState::Ambiguous
                }
            }
            ArtifactAssociationConfidence::Weak => {
                if games.len() == 1 {
                    ArtifactConflictState::Matched
                } else {
                    ArtifactConflictState::Ambiguous
                }
            }
            ArtifactAssociationConfidence::Ambiguous => ArtifactConflictState::Ambiguous,
            ArtifactAssociationConfidence::Unsupported => ArtifactConflictState::Orphaned,
        }
    };

    RetroArchArtifactFinding {
        profile: artifact.profile,
        artifact_kind: artifact.kind,
        path: EncodedPath::from_path(&artifact.path),
        filename: EncodedPath::from_os_string(&artifact.file_name),
        size_bytes: artifact.size_bytes,
        probe: artifact.probe,
        symlink: artifact.probe == FsProbe::Symlink,
        association: ArtifactAssociation {
            confidence,
            evidence,
            catalogue_games: games,
            playlist_evidence,
            core_stems,
            expected_destinations,
        },
        occupies_expected_destination,
        conflict_state,
        cheat_summary,
        diagnostics,
    }
}

fn weaker_candidates<'a>(
    artifact: &RawArtifact,
    expected: &'a [ExpectedArtifact],
) -> (
    Vec<&'a ExpectedArtifact>,
    ArtifactAssociationConfidence,
    Vec<&'static str>,
    bool,
) {
    let same_profile_kind = expected
        .iter()
        .filter(|item| item.profile == artifact.profile && item.kind == artifact.kind)
        .collect::<Vec<_>>();
    let same_filename = same_profile_kind
        .iter()
        .copied()
        .filter(|item| item.path.file_name() == Some(artifact.file_name.as_os_str()))
        .collect::<Vec<_>>();

    if artifact.kind == ArtifactKind::Cheat {
        let parent_stem = artifact
            .path
            .parent()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str);
        let core_and_name = same_filename
            .iter()
            .copied()
            .filter(|item| item.core_stem.as_deref() == parent_stem)
            .collect::<Vec<_>>();
        if !core_and_name.is_empty() {
            let confidence = if unique_game_count(&core_and_name) == 1 {
                ArtifactAssociationConfidence::Strong
            } else {
                ArtifactAssociationConfidence::Ambiguous
            };
            return (
                core_and_name,
                confidence,
                vec!["exact_core_and_filename"],
                false,
            );
        }
    }

    if !same_filename.is_empty() {
        let confidence = if unique_game_count(&same_filename) == 1 {
            ArtifactAssociationConfidence::Weak
        } else {
            ArtifactAssociationConfidence::Ambiguous
        };
        return (same_filename, confidence, vec!["filename_only"], false);
    }

    // Case-insensitive filename matching is allowed only as weak evidence
    // and only for valid UTF-8 names. The real byte path remains identity.
    let casefold = artifact.file_name.to_str().map(str::to_ascii_lowercase);
    let casefold_matches = casefold
        .as_deref()
        .map(|needle| {
            same_profile_kind
                .iter()
                .copied()
                .filter(|item| {
                    item.path
                        .file_name()
                        .and_then(OsStr::to_str)
                        .is_some_and(|name| name.to_ascii_lowercase() == needle)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !casefold_matches.is_empty() {
        let confidence = if unique_game_count(&casefold_matches) == 1 {
            ArtifactAssociationConfidence::Weak
        } else {
            ArtifactAssociationConfidence::Ambiguous
        };
        return (
            casefold_matches,
            confidence,
            vec!["normalized_filename_only"],
            false,
        );
    }

    (
        Vec::new(),
        ArtifactAssociationConfidence::Unsupported,
        Vec::new(),
        false,
    )
}

fn unique_game_count(candidates: &[&ExpectedArtifact]) -> usize {
    candidates
        .iter()
        .map(|item| item.game.archive_id)
        .collect::<BTreeSet<_>>()
        .len()
}

fn read_cheat_summary(
    filesystem: &dyn ReadOnlyHostFilesystem,
    artifact: &RawArtifact,
    diagnostics: &mut Vec<ArtifactDiagnostic>,
) -> Option<CheatFileSummary> {
    if artifact.probe != FsProbe::PresentFile {
        return None;
    }
    let bytes = match filesystem.read_bounded(&artifact.path, MAX_CHEAT_FILE_BYTES) {
        BoundedReadResult::Ok(bytes) => bytes,
        result => {
            let code = match result {
                BoundedReadResult::TooLarge => "cheat_file_too_large",
                BoundedReadResult::NotFound => "cheat_file_disappeared",
                BoundedReadResult::WrongType => "cheat_file_wrong_type",
                BoundedReadResult::Symlink => "cheat_file_symlink_not_followed",
                BoundedReadResult::Inaccessible => "cheat_file_inaccessible",
                BoundedReadResult::IoError => "cheat_file_io_error",
                BoundedReadResult::Ok(_) => unreachable!(),
            };
            diagnostics.push(ArtifactDiagnostic {
                code,
                severity: ArtifactDiagnosticSeverity::Warning,
                profile: artifact.profile,
                path: Some(EncodedPath::from_path(&artifact.path)),
            });
            return None;
        }
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        diagnostics.push(ArtifactDiagnostic {
            code: "cheat_file_invalid_utf8",
            severity: ArtifactDiagnosticSeverity::Warning,
            profile: artifact.profile,
            path: Some(EncodedPath::from_path(&artifact.path)),
        });
        return None;
    };
    Some(parse_cheat_summary(text))
}

fn parse_cheat_summary(text: &str) -> CheatFileSummary {
    let mut declared_cheat_count = None;
    let mut entries = BTreeSet::<usize>::new();
    let mut enabled = BTreeSet::<usize>::new();
    let mut description = None;
    let mut malformed_lines = Vec::new();
    let mut complete = true;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            malformed_lines.push(line_number);
            continue;
        };
        let key = raw_key.trim();
        let value = unquote(raw_value.trim());
        if key == "cheats" {
            match value.parse::<u32>() {
                Ok(count) => declared_cheat_count = Some(count),
                Err(_) => malformed_lines.push(line_number),
            }
            continue;
        }
        let Some(remainder) = key.strip_prefix("cheat") else {
            continue;
        };
        let digit_count = remainder.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 || !remainder[digit_count..].starts_with('_') {
            malformed_lines.push(line_number);
            continue;
        }
        let Ok(entry_index) = remainder[..digit_count].parse::<usize>() else {
            malformed_lines.push(line_number);
            continue;
        };
        if entry_index >= MAX_CHEAT_ENTRIES_PER_FILE {
            complete = false;
            continue;
        }
        entries.insert(entry_index);
        let field = &remainder[digit_count + 1..];
        if field == "desc" && description.is_none() && !value.is_empty() {
            description = Some(value.to_string());
        }
        if field == "enable" && value.eq_ignore_ascii_case("true") {
            enabled.insert(entry_index);
        }
    }
    if declared_cheat_count.is_some_and(|count| count as usize != entries.len()) {
        complete = false;
    }
    if !malformed_lines.is_empty() {
        complete = false;
    }
    CheatFileSummary {
        description,
        declared_cheat_count,
        parsed_cheat_entries: entries.len(),
        enabled_cheat_entries: enabled.len(),
        any_cheats_enabled: !enabled.is_empty(),
        malformed_lines,
        complete,
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

fn mark_duplicates(findings: &mut [RetroArchArtifactFinding]) {
    let mut counts = BTreeMap::<(Option<ProfileRef>, ArtifactKind, i64), usize>::new();
    for finding in findings.iter() {
        for game in &finding.association.catalogue_games {
            *counts
                .entry((finding.profile, finding.artifact_kind, game.archive_id))
                .or_default() += 1;
        }
    }
    for finding in findings {
        if matches!(
            finding.conflict_state,
            ArtifactConflictState::Matched | ArtifactConflictState::Occupied
        ) && finding.association.catalogue_games.iter().any(|game| {
            counts
                .get(&(finding.profile, finding.artifact_kind, game.archive_id))
                .copied()
                .unwrap_or_default()
                > 1
        }) {
            finding.conflict_state = ArtifactConflictState::Duplicate;
        }
    }
}

fn build_destinations(
    filesystem: &dyn ReadOnlyHostFilesystem,
    expected: &[ExpectedArtifact],
) -> Vec<RetroArchArtifactDestination> {
    let mut grouped = BTreeMap::<(Option<ProfileRef>, ArtifactKind, PathBuf), Vec<_>>::new();
    for item in expected {
        grouped
            .entry((item.profile, item.kind, item.path.clone()))
            .or_default()
            .push(item.game.clone());
    }
    grouped
        .into_iter()
        .map(|((profile, kind, path), mut games)| {
            games.sort_by_key(|game| game.archive_id);
            games.dedup_by_key(|game| game.archive_id);
            let probe = filesystem.probe(&path);
            let state = if games.len() > 1 {
                ArtifactConflictState::Ambiguous
            } else {
                match probe {
                    FsProbe::Missing => ArtifactConflictState::Empty,
                    FsProbe::PresentFile => ArtifactConflictState::Occupied,
                    FsProbe::PresentDirectory
                    | FsProbe::Symlink
                    | FsProbe::WrongType
                    | FsProbe::Inaccessible
                    | FsProbe::IoError => ArtifactConflictState::Conflicting,
                }
            };
            RetroArchArtifactDestination {
                profile,
                artifact_kind: kind,
                path: EncodedPath::from_path(&path),
                catalogue_games: games,
                probe,
                size_bytes: filesystem.size_no_follow(&path),
                state,
            }
        })
        .collect()
}

fn summarize(
    findings: &[RetroArchArtifactFinding],
    destinations: &[RetroArchArtifactDestination],
) -> RetroArchArtifactSummary {
    let count_state = |state| {
        findings
            .iter()
            .filter(|finding| finding.conflict_state == state)
            .count()
    };
    RetroArchArtifactSummary {
        artifacts_found: findings.len(),
        cheat_files: findings
            .iter()
            .filter(|finding| finding.artifact_kind == ArtifactKind::Cheat)
            .count(),
        soft_patch_files: findings
            .iter()
            .filter(|finding| finding.artifact_kind != ArtifactKind::Cheat)
            .count(),
        expected_destinations: destinations.len(),
        empty_destinations: destinations
            .iter()
            .filter(|destination| destination.state == ArtifactConflictState::Empty)
            .count(),
        occupied_destinations: destinations
            .iter()
            .filter(|destination| destination.state == ArtifactConflictState::Occupied)
            .count(),
        duplicate_artifacts: count_state(ArtifactConflictState::Duplicate),
        conflicting_artifacts: count_state(ArtifactConflictState::Conflicting),
        orphaned_artifacts: count_state(ArtifactConflictState::Orphaned),
        ambiguous_artifacts: count_state(ArtifactConflictState::Ambiguous),
        unsupported_artifacts: count_state(ArtifactConflictState::Unsupported),
    }
}

fn expected_order(left: &ExpectedArtifact, right: &ExpectedArtifact) -> std::cmp::Ordering {
    left.profile
        .cmp(&right.profile)
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| os_str_bytes(left.path.as_os_str()).cmp(os_str_bytes(right.path.as_os_str())))
        .then_with(|| left.game.archive_id.cmp(&right.game.archive_id))
}

fn finding_order(
    left: &RetroArchArtifactFinding,
    right: &RetroArchArtifactFinding,
) -> std::cmp::Ordering {
    left.profile
        .cmp(&right.profile)
        .then_with(|| left.artifact_kind.cmp(&right.artifact_kind))
        .then_with(|| left.path.display.cmp(&right.path.display))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct PatchBodyMustNotBeRead;

    impl ReadOnlyHostFilesystem for PatchBodyMustNotBeRead {
        fn probe(&self, _path: &Path) -> FsProbe {
            FsProbe::PresentFile
        }

        fn probe_executable(&self, _path: &Path) -> crate::emulator_environment::ExecutableProbe {
            crate::emulator_environment::ExecutableProbe::NotExecutable
        }

        fn read_bounded(&self, _path: &Path, _max_bytes: usize) -> BoundedReadResult {
            panic!("soft-patch bodies must never be read")
        }

        fn list_dir_bounded(&self, _path: &Path, _max_entries: usize) -> BoundedListResult {
            BoundedListResult::NotFound
        }

        fn probe_regular_file_executable_bit(&self, _path: &Path) -> Option<bool> {
            None
        }
    }

    #[test]
    fn cheat_summary_reports_bounded_non_executable_metadata() {
        let summary = parse_cheat_summary(
            "cheats = 2\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n\
             cheat1_desc = \"Moon Jump\"\ncheat1_enable = true\n",
        );
        assert_eq!(summary.description.as_deref(), Some("Infinite Lives"));
        assert_eq!(summary.declared_cheat_count, Some(2));
        assert_eq!(summary.parsed_cheat_entries, 2);
        assert_eq!(summary.enabled_cheat_entries, 1);
        assert!(summary.any_cheats_enabled);
        assert!(summary.complete);
    }

    #[test]
    fn patch_findings_never_read_payload_bytes() {
        let artifact = RawArtifact {
            profile: None,
            kind: ArtifactKind::SoftPatchIps,
            path: PathBuf::from("/content/game.ips"),
            file_name: OsString::from("game.ips"),
            size_bytes: Some(12),
            probe: FsProbe::PresentFile,
        };
        let finding = build_finding(&PatchBodyMustNotBeRead, artifact, &[]);
        assert!(finding.cheat_summary.is_none());
        assert_eq!(finding.conflict_state, ArtifactConflictState::Orphaned);
    }

    #[test]
    fn inaccessible_and_wrong_type_artifacts_remain_distinct() {
        let build = |probe| {
            build_finding(
                &PatchBodyMustNotBeRead,
                RawArtifact {
                    profile: None,
                    kind: ArtifactKind::SoftPatchIps,
                    path: PathBuf::from("/content/game.ips"),
                    file_name: OsString::from("game.ips"),
                    size_bytes: None,
                    probe,
                },
                &[],
            )
        };
        assert_eq!(
            build(FsProbe::Inaccessible).conflict_state,
            ArtifactConflictState::Unsupported
        );
        assert_eq!(
            build(FsProbe::WrongType).conflict_state,
            ArtifactConflictState::Conflicting
        );
    }

    #[test]
    fn artifact_finding_json_has_the_exact_v1_key_set() {
        let finding = build_finding(
            &PatchBodyMustNotBeRead,
            RawArtifact {
                profile: None,
                kind: ArtifactKind::SoftPatchIps,
                path: PathBuf::from("/content/game.ips"),
                file_name: OsString::from("game.ips"),
                size_bytes: Some(12),
                probe: FsProbe::PresentFile,
            },
            &[],
        );
        let json = serde_json::to_value(finding).unwrap();
        let mut keys = json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "artifact_kind",
                "association",
                "cheat_summary",
                "conflict_state",
                "diagnostics",
                "filename",
                "occupies_expected_destination",
                "path",
                "probe",
                "profile",
                "size_bytes",
                "symlink",
            ]
        );
    }

    #[test]
    fn cheat_summary_preserves_malformed_line_numbers_and_count_mismatch() {
        let summary = parse_cheat_summary("cheats = 2\nnot an assignment\ncheat0_desc = \"A\"\n");
        assert_eq!(summary.malformed_lines, vec![2]);
        assert_eq!(summary.parsed_cheat_entries, 1);
        assert!(!summary.complete);
    }

    #[test]
    fn only_reviewed_preview_extensions_are_inventory_kinds() {
        assert_eq!(
            artifact_kind_for_path(Path::new("game.CHT")),
            Some(ArtifactKind::Cheat)
        );
        assert_eq!(
            artifact_kind_for_path(Path::new("game.xdelta")),
            Some(ArtifactKind::SoftPatchXdelta)
        );
        assert_eq!(artifact_kind_for_path(Path::new("game.ppf")), None);
    }

    #[test]
    fn public_inventory_enums_use_stable_lower_snake_case_names() {
        assert_eq!(
            serde_json::to_string(&ArtifactKind::SoftPatchXdelta).unwrap(),
            "\"soft_patch_xdelta\""
        );
        assert_eq!(
            serde_json::to_string(&ArtifactAssociationConfidence::Ambiguous).unwrap(),
            "\"ambiguous\""
        );
        assert_eq!(
            serde_json::to_string(&ArtifactConflictState::Orphaned).unwrap(),
            "\"orphaned\""
        );
    }

    #[test]
    fn cheat_entry_limit_marks_summary_incomplete() {
        let summary = parse_cheat_summary(&format!(
            "cheats = 1\ncheat{}_desc = \"outside bound\"\n",
            MAX_CHEAT_ENTRIES_PER_FILE
        ));
        assert_eq!(summary.parsed_cheat_entries, 0);
        assert!(!summary.complete);
    }
}

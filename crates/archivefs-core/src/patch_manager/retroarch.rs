//! Read-only RetroArch cheat/patch destination preview.
//!
//! This is the second concrete emulator preview built on top of
//! `patch_manager`, after PCSX2's `preview_pcsx2_metadata`. It deliberately
//! does **not** implement [`super::EmulatorAdapter`] and does **not**
//! produce an [`super::AdvisoryPatchPlan`]: that trait and type are shaped
//! around PCSX2's specific model (`"PS2"` platform filtering baked into
//! `patch_manager::mod`, exactly one `data_root` per installation
//! candidate, exactly one hypothetical relative path per metadata record)
//! and none of that fits RetroArch, which has no single patch/cheat root,
//! several purpose-tagged directories per installation, and a
//! core-selection ambiguity axis PCSX2 has no analogue for at all (see
//! `docs/PATCH_CHEAT_MANAGER_DESIGN.md`'s "Known RetroArch gaps" list).
//! Forcing RetroArch through that trait would either weaken it for PCSX2
//! or silently misrepresent RetroArch; this module is the "separate,
//! narrowly-scoped advisory type" the design review anticipated instead.
//!
//! **No network call of any kind is made here.** Unlike PCSX2 - which has
//! one reviewed, compiled-in upstream metadata endpoint
//! (`patch_manager::BUILT_IN_SOURCE_URL`) - no RetroArch metadata source
//! (upstream cheat database, licensing, or source policy) has been
//! reviewed or approved. `HttpsMetadataFetcher::fetch` continues to accept
//! only that one PCSX2 URL and is not used, imported, or extended by this
//! module. Consequently there is no external "record" to match a
//! catalogue game against the way PCSX2 matches a `.pnach` filename's
//! serial/CRC to a catalogue row: **the catalogue game itself is the only
//! input**, and every entry below is produced only from data already on
//! local disk (the RetroArch installation environment already discovered
//! by `emulator_environment::retroarch`, and the read-only ArchiveFS
//! catalogue). See `docs/RETROARCH_PATCH_PREVIEW.md` for the full design
//! record, including the primary RetroArch source citations the cheat/
//! patch destination conventions below are based on.
//!
//! Because there is no external record, there is also no PCSX2-style
//! "game identity" matching problem here (a catalogue row is never
//! ambiguous with *itself*). The one genuine ambiguity axis RetroArch has
//! that PCSX2 does not is **core selection**: RetroArch's own per-game
//! cheat file path is scoped by which core loaded the content
//! (`cheat_manager_get_game_specific_filename`'s `core_name` component),
//! and ArchiveFS has no reliable way to know which installed core a user
//! would actually pick. This module resolves that only when it is
//! genuinely unambiguous - exactly one installed core's own `.info`
//! metadata (already inventoried by `emulator_environment::retroarch`,
//! not re-derived here) declares the catalogue archive's file extension as
//! supported - and reports `AmbiguousCore`/`UnsupportedNoCore` rather than
//! guessing otherwise.
//!
//! `serial`/`executable_crc`-based exact identity (PCSX2's strongest tier)
//! is deliberately **not** modeled here at all, even opportunistically:
//! unlike PCSX2 (which at least has an upstream record to compare those
//! fields against, even though the catalogue side is unpopulated in
//! production today), RetroArch has no second record to compare a
//! catalogue row's identity fields against in this milestone, so a
//! same-row "exact match" would be vacuous. `no_identity_tier_is_used_for_retroarch_matching`
//! in the test module below locks in that this stays true even if a
//! catalogue row is ever populated with those fields.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::emulator_environment::retroarch::{
    ContentPathKind, CoreInfoFinding, DiscoveryEnvironment, PathFinding, PathPurpose, PlaylistCrc,
    ProfileRef, ResolutionState, RetroArchEnvironmentReport, RetroArchPlaylistEntry,
    RetroArchProfile, discover_retroarch_environment,
};
use crate::emulator_environment::{EncodedPath, FsProbe, ReadOnlyHostFilesystem};
use crate::{Database, PersistedArchive};

use super::{PatchManagerError, Result};

pub const RETROARCH_ADVISORY_FORMAT_VERSION: u32 = 1;
pub const MAX_CATALOGUE_STRING_BYTES: usize = 4 * 1024;

/// RetroArch's own soft-patch sibling extensions, in RetroArch's own
/// try-order when no per-content core option overrides it - verified
/// against `libretro/RetroArch`'s `tasks/task_patch.c` (`patch_content`:
/// `try_ips_patch`, then `try_bps_patch`, then `try_ups_patch`, then
/// `try_xdelta_patch`) and `runloop.c` (`runloop_path_fill_names`, which
/// derives `name.ups`/`name.bps`/`name.ips`/`name.xdelta` as
/// `<content-basename-without-extension>.<ext>` in the content's own
/// directory).
const SOFT_PATCH_EXTENSIONS: [&str; 4] = ["ips", "bps", "ups", "xdelta"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationKind {
    /// The bare `cheat_database_path` root for one profile - not scoped to
    /// any core or game. Reused directly from the already-resolved
    /// `PathPurpose::Cheats` finding; never re-derived.
    CheatDatabaseRoot,
    /// `<cheat_database_path>/<core_library_name>/<content_basename>.cht` -
    /// verified against `cheat_manager.c`'s
    /// `cheat_manager_get_game_specific_filename`. Produced only when
    /// exactly one installed core's `supported_extensions` matches the
    /// catalogue archive's own file extension.
    PerGameCheatFile,
    /// `<content-dir>/<content-basename-without-extension>.{ips,bps,ups,xdelta}` -
    /// verified against `runloop.c`'s `runloop_path_fill_names`. Lives next
    /// to the archive itself, not under any RetroArch-owned directory, and
    /// is therefore profile-independent.
    SoftPatchSibling,
    /// No destination could be proposed; see `unsupported_reason`.
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposedDestination {
    pub kind: DestinationKind,
    /// `None` only when `kind == Unsupported`.
    pub path: Option<EncodedPath>,
    /// `None` only when `kind == Unsupported`. Byte-safe like `path` - a
    /// non-UTF-8 filename is still rendered honestly, never treated as a
    /// matching identity (see the module doc comment and
    /// `content_extension_is_none_when_extension_is_not_utf8`).
    pub file_name: Option<EncodedPath>,
    /// A stable, fixed identifier for how this destination was computed -
    /// never free-text prose. One of `"cheat_database_path_config_key"`,
    /// `"core_supported_extensions_single_match"`,
    /// `"content_basename_soft_patch_sibling"`, or `"unsupported"`.
    pub derivation: &'static str,
    /// Whether this destination's parent directory currently exists.
    /// `None` when not applicable (`Unsupported`, or `CheatDatabaseRoot`,
    /// whose parent is the profile's own config directory and is not
    /// separately re-probed here).
    pub parent_exists: Option<bool>,
    /// Whether this exact destination path already exists on disk.
    pub destination_exists: Option<bool>,
    /// `true` exactly when `destination_exists == Some(true)` - a real,
    /// pre-existing object at this hypothetical path. Advisory only: nothing
    /// here ever reads, replaces, or removes it.
    pub conflict: bool,
    /// A stable, fixed identifier explaining why `kind == Unsupported`.
    /// `None` for every other `kind`.
    pub unsupported_reason: Option<&'static str>,
}

impl ProposedDestination {
    fn unsupported(reason: &'static str) -> Self {
        Self {
            kind: DestinationKind::Unsupported,
            path: None,
            file_name: None,
            derivation: "unsupported",
            parent_exists: None,
            destination_exists: None,
            conflict: false,
            unsupported_reason: Some(reason),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreMatchDisposition {
    /// Exactly one installed core in this profile declares the archive's
    /// file extension as supported.
    ExactCore,
    /// Two or more installed cores in this profile declare the same
    /// extension as supported; no single per-game cheat destination can be
    /// proposed.
    AmbiguousCore,
    /// No installed core in this profile declares the archive's extension
    /// as supported (or no core has a readable `.info`).
    UnsupportedNoCore,
    /// The catalogue archive has no usable (or non-UTF-8) file extension,
    /// so no core-extension comparison could even be attempted.
    UnsupportedNoContentExtension,
    /// This profile's `cheat_database_path` key is configured but this
    /// milestone declines to resolve it (colon-alias or plain relative
    /// value), or was never configured/left empty (runtime default
    /// unknown) - see `emulator_environment::retroarch::ResolutionState`.
    UnsupportedCheatsPathUnresolved,
    /// This profile's `cheat_database_path` resolved to a path, but that
    /// path does not currently exist as a directory.
    UnsupportedCheatsPathMissing,
}

/// How confidently a RetroArch playlist entry is believed to refer to a
/// given catalogue archive. Deliberately a distinct vocabulary from
/// PCSX2's `MatchConfidence` (`NoMatch`/`Uncertain`/`Probable`/`Exact`):
/// the evidence categories genuinely differ (core-selection and content-
/// path identity, not upstream-metadata-to-catalogue matching), and
/// reusing PCSX2's enum would misrepresent what each value means here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistMatchConfidence {
    /// No usable evidence at all.
    Unsupported,
    /// Only a normalized label/filename matched, with no corroborating
    /// platform evidence.
    Weak,
    /// A normalized basename matched *and* the catalogue archive has a
    /// known platform (corroborating context), or an archive-member
    /// path's outer archive portion matched exactly (inner member
    /// unverified).
    Strong,
    /// The playlist entry's own content path matched a catalogue
    /// archive's real path exactly, byte-for-byte.
    Exact,
    /// Two or more catalogue archives tied at the best available
    /// confidence for this entry; no single archive was chosen.
    Ambiguous,
}

/// How a playlist entry's `core_path`/`core_name` relates to this
/// profile's actually-installed cores. Verified against `cheat_manager.c`
/// (core identity is the loaded core's own `library_name`, i.e. its
/// filename stem) and `playlist.c`'s `playlist_entry_has_core` (`DETECT`
/// is a real sentinel meaning "no specific core", not a core name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreAssociation {
    /// `core_path`'s own filename stem matches an installed core exactly -
    /// preferred over `core_name`, since a core's filename/stem is a
    /// stable identity while `core_path` is installation-specific and
    /// `core_name` is just a display string. Verified core stem derivation
    /// mirrors `emulator_environment::retroarch`'s own `_libretro.so`
    /// suffix stripping.
    LinkedByCorePath { core_stem: String },
    /// `core_path` did not correspond to any installed core (stale - the
    /// playlist was written on a different machine or a core was
    /// removed/reinstalled elsewhere), but `core_name` matched exactly one
    /// installed core's own declared `display_name`.
    LinkedByCoreName { core_stem: String },
    /// `core_path` and/or `core_name` is the literal `"DETECT"` sentinel -
    /// RetroArch itself treats this as "no specific core", never a name to
    /// look up.
    Detect,
    /// `core_name` matched 2+ installed cores' declared `display_name`
    /// (two different cores can share a display name); no single core
    /// identity can be attributed.
    AmbiguousCoreName { candidate_stems: Vec<String> },
    /// `core_path`/`core_name` were present and not `DETECT`, but neither
    /// corresponds to any installed core in this profile.
    NoInstalledCoreMatch,
    /// The entry has no usable `core_path` or `core_name` at all.
    NoCoreEvidence,
}

/// One playlist entry's evidence about a specific catalogue archive - see
/// `docs/RETROARCH_PLAYLISTS.md` for the full matching-tier record.
#[derive(Debug, Clone, Serialize)]
pub struct PlaylistEvidence {
    pub playlist_file: EncodedPath,
    pub playlist_name: String,
    pub entry_index: u32,
    pub entry_label: Option<String>,
    /// The single catalogue archive this evidence names, when
    /// unambiguous. `None` when `confidence == Ambiguous`.
    pub matched_archive_id: Option<i64>,
    /// Every tied catalogue archive ID, populated only when
    /// `confidence == Ambiguous` - never silently resolved to one.
    pub ambiguous_archive_ids: Vec<i64>,
    pub confidence: PlaylistMatchConfidence,
    /// A stable, fixed identifier for which evidence tier produced this
    /// result - never free-text prose. One of `"exact_content_path"`,
    /// `"archive_path_member_unverified"`, `"normalized_basename"`,
    /// `"label_or_filename_only"`.
    pub evidence_basis: &'static str,
    pub content_path_kind: ContentPathKind,
    pub database_name: Option<String>,
    pub crc: PlaylistCrc,
    pub core_association: CoreAssociation,
}

/// Which mechanism produced `RetroArchProfileOutcome::matched_core_stem` -
/// an additive, purely informational field (see
/// `docs/RETROARCH_PATCH_PREVIEW.md`'s JSON-compatibility note): existing
/// consumers that only look at `disposition`/`matched_core_stem` see no
/// change in meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreSelectionSource {
    /// The pre-existing mechanism: exactly one installed core's
    /// `supported_extensions` matched the archive's own file extension.
    ExtensionMatch,
    /// A playlist entry's own `core_path`/`core_name` evidence linked
    /// exactly one installed core, upgrading what extension-matching alone
    /// left `AmbiguousCore` or `UnsupportedNoCore`. Never used to override
    /// an extension-based result that was already `ExactCore`.
    PlaylistEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchProfileOutcome {
    pub profile: ProfileRef,
    pub disposition: CoreMatchDisposition,
    /// Populated only when `disposition == ExactCore`.
    pub matched_core_stem: Option<String>,
    /// Every installed core stem considered a candidate - the single match
    /// for `ExactCore`, every tied stem for `AmbiguousCore`, empty
    /// otherwise. Sorted for determinism.
    pub candidate_core_stems: Vec<String>,
    /// Populated only when `matched_core_stem.is_some()`.
    pub selected_core_source: Option<CoreSelectionSource>,
    /// Every playlist entry (across every playlist in this profile) whose
    /// best evidence names this archive - possibly empty, and possibly
    /// more than one if multiple playlists reference the same content.
    /// Sorted deterministically (see `docs/RETROARCH_PLAYLISTS.md`).
    pub playlist_evidence: Vec<PlaylistEvidence>,
    pub cheat_database_root: ProposedDestination,
    pub per_game_cheat_file: ProposedDestination,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchAdvisoryEntry {
    pub archive_id: i64,
    pub display_name: String,
    pub normalized_name: String,
    pub platform: Option<String>,
    /// Lowercase file extension of the catalogue archive's own file (e.g.
    /// `"zip"`), or `None` if the archive has no extension or its
    /// extension is not valid UTF-8. This is the *archive's own* container
    /// extension (ArchiveFS tracks Zip/SevenZip/Rar archives only) - not an
    /// inner compressed entry's extension; see
    /// `docs/RETROARCH_PATCH_PREVIEW.md`'s "Non-goals" for why inner-entry
    /// inspection is out of scope here.
    pub content_extension: Option<String>,
    /// Always 4 entries (ips/bps/ups/xdelta) when the archive's own path
    /// is usable, in RetroArch's own try-order; a single `Unsupported`
    /// entry otherwise. Profile-independent - see `DestinationKind::SoftPatchSibling`.
    pub soft_patch_candidates: Vec<ProposedDestination>,
    /// Exactly 3, one per profile, in the same fixed native/Flatpak-user/
    /// Flatpak-system order as `RetroArchEnvironmentReport::profiles`.
    pub profile_outcomes: Vec<RetroArchProfileOutcome>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchAdvisorySummary {
    pub catalogue_archives: usize,
    pub exact_core_profile_outcomes: usize,
    pub ambiguous_core_profile_outcomes: usize,
    pub unsupported_profile_outcomes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetroArchAdvisoryPlan {
    pub format_version: u32,
    pub plan_id: String,
    pub executable: bool,
    pub environment: RetroArchEnvironmentReport,
    /// One entry per **present** catalogue archive (mirrors
    /// `CatalogueGameEvidence::is_present`/PCSX2's own treatment of
    /// already-missing rows), sorted by `archive_id` for determinism.
    pub entries: Vec<RetroArchAdvisoryEntry>,
    pub summary: RetroArchAdvisorySummary,
}

pub fn preview_retroarch_patch_and_cheat_destinations(
    filesystem: &dyn ReadOnlyHostFilesystem,
    environment: &DiscoveryEnvironment,
    catalogue_path: &Path,
) -> Result<RetroArchAdvisoryPlan> {
    let report = discover_retroarch_environment(filesystem, environment)
        .map_err(|error| PatchManagerError::Discovery(error.to_string()))?;
    let archives = load_retroarch_catalogue_archives_read_only(catalogue_path)?;
    Ok(build_retroarch_advisory_plan(filesystem, report, archives))
}

/// Mirrors `patch_manager::load_catalogue_evidence_read_only`'s exact
/// open/read/close/validate pattern, but keeps the full [`PersistedArchive`]
/// rather than the narrower `CatalogueGameEvidence` projection: unlike
/// PCSX2 (whose destination lives entirely inside the PCSX2 installation),
/// a RetroArch soft-patch sibling destination is computed from the
/// archive's own path, which `CatalogueGameEvidence` does not carry.
fn load_retroarch_catalogue_archives_read_only(path: &Path) -> Result<Vec<PersistedArchive>> {
    let database = Database::open_read_only(path)
        .map_err(|error| PatchManagerError::Catalogue(error.to_string()))?;
    let archives = database
        .load_archives()
        .map_err(|error| PatchManagerError::Catalogue(error.to_string()))?;
    database
        .close()
        .map_err(|error| PatchManagerError::Catalogue(error.to_string()))?;
    for archive in &archives {
        validate_catalogue_string("catalogue display name", &archive.display_name)?;
        validate_catalogue_string("catalogue normalized name", &archive.normalized_name)?;
        if let Some(platform) = &archive.platform {
            validate_catalogue_string("catalogue platform", platform)?;
        }
    }
    Ok(archives)
}

fn validate_catalogue_string(field: &str, value: &str) -> Result<()> {
    if value.len() > MAX_CATALOGUE_STRING_BYTES {
        return Err(PatchManagerError::UnsupportedMetadata(format!(
            "{field} exceeds the {MAX_CATALOGUE_STRING_BYTES}-byte string limit"
        )));
    }
    if value.contains('\0') {
        return Err(PatchManagerError::MalformedMetadata(format!(
            "{field} contains a NUL byte"
        )));
    }
    Ok(())
}

pub fn build_retroarch_advisory_plan(
    filesystem: &dyn ReadOnlyHostFilesystem,
    environment: RetroArchEnvironmentReport,
    archives: Vec<PersistedArchive>,
) -> RetroArchAdvisoryPlan {
    let mut present = archives
        .into_iter()
        .filter(|archive| archive.last_verified_missing_at.is_none())
        .collect::<Vec<_>>();
    present.sort_by_key(|archive| archive.id);

    let playlist_evidence_by_archive = build_playlist_evidence_by_archive(&environment, &present);

    let entries = present
        .into_iter()
        .map(|archive| {
            build_entry(
                filesystem,
                &environment,
                archive,
                &playlist_evidence_by_archive,
            )
        })
        .collect::<Vec<_>>();

    let summary = RetroArchAdvisorySummary {
        catalogue_archives: entries.len(),
        exact_core_profile_outcomes: count_dispositions(&entries, CoreMatchDisposition::ExactCore),
        ambiguous_core_profile_outcomes: count_dispositions(
            &entries,
            CoreMatchDisposition::AmbiguousCore,
        ),
        unsupported_profile_outcomes: entries
            .iter()
            .flat_map(|entry| &entry.profile_outcomes)
            .filter(|outcome| {
                matches!(
                    outcome.disposition,
                    CoreMatchDisposition::UnsupportedNoCore
                        | CoreMatchDisposition::UnsupportedNoContentExtension
                        | CoreMatchDisposition::UnsupportedCheatsPathUnresolved
                        | CoreMatchDisposition::UnsupportedCheatsPathMissing
                )
            })
            .count(),
    };

    let plan_id = compute_plan_id(&environment, &entries);

    RetroArchAdvisoryPlan {
        format_version: RETROARCH_ADVISORY_FORMAT_VERSION,
        plan_id,
        executable: false,
        environment,
        entries,
        summary,
    }
}

fn count_dispositions(entries: &[RetroArchAdvisoryEntry], target: CoreMatchDisposition) -> usize {
    entries
        .iter()
        .flat_map(|entry| &entry.profile_outcomes)
        .filter(|outcome| outcome.disposition == target)
        .count()
}

fn build_entry(
    filesystem: &dyn ReadOnlyHostFilesystem,
    environment: &RetroArchEnvironmentReport,
    archive: PersistedArchive,
    playlist_evidence_by_archive: &BTreeMap<(ProfileRef, i64), Vec<PlaylistEvidence>>,
) -> RetroArchAdvisoryEntry {
    let content_extension = content_extension(&archive.relative_path);
    let content_stem = archive.relative_path.file_stem().map(OsStr::to_os_string);
    let soft_patch_candidates = soft_patch_candidates(filesystem, &archive.absolute_path);
    let profile_outcomes = environment
        .profiles
        .iter()
        .map(|profile| {
            let profile_ref = ProfileRef {
                profile_kind: profile.profile_kind,
                scope: profile.scope,
            };
            let playlist_evidence = playlist_evidence_by_archive
                .get(&(profile_ref, archive.id))
                .cloned()
                .unwrap_or_default();
            build_profile_outcome(
                filesystem,
                profile,
                content_extension.as_deref(),
                content_stem.as_deref(),
                playlist_evidence,
            )
        })
        .collect();

    RetroArchAdvisoryEntry {
        archive_id: archive.id,
        display_name: archive.display_name,
        normalized_name: archive.normalized_name,
        platform: archive.platform,
        content_extension,
        soft_patch_candidates,
        profile_outcomes,
    }
}

/// Lowercase file extension of `relative_path`, or `None` if it has none or
/// its extension is not valid UTF-8. Only ever used as a *matching* key
/// (compared case-insensitively against a core's `supported_extensions`) -
/// never rendered as an approved identity by itself. Deliberately narrower
/// than `EncodedPath`: a non-UTF-8 extension cannot be safely compared, so
/// it is treated as unknown for matching purposes rather than lossily
/// guessed.
fn content_extension(relative_path: &Path) -> Option<String> {
    relative_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
}

/// `<content-dir>/<content-basename-without-extension>.{ips,bps,ups,xdelta}`,
/// always in that order - see `SOFT_PATCH_EXTENSIONS`. Built from real
/// `PathBuf`/`OsStr` operations (never a lossy string) so a non-UTF-8
/// archive path still produces a correct destination; only the rendered
/// `EncodedPath`/`file_name` are lossy-safe display forms.
fn soft_patch_candidates(
    filesystem: &dyn ReadOnlyHostFilesystem,
    absolute_path: &Path,
) -> Vec<ProposedDestination> {
    let (Some(parent), Some(stem)) = (absolute_path.parent(), absolute_path.file_stem()) else {
        return vec![ProposedDestination::unsupported(
            "archive_path_has_no_parent_or_stem",
        )];
    };
    let parent_probe = filesystem.probe(parent);
    let parent_exists = parent_probe == FsProbe::PresentDirectory;

    SOFT_PATCH_EXTENSIONS
        .iter()
        .map(|extension| {
            let mut file_name = stem.to_os_string();
            file_name.push(".");
            file_name.push(extension);
            let destination_path = parent.join(&file_name);
            let destination_probe = filesystem.probe(&destination_path);
            let destination_exists = matches!(
                destination_probe,
                FsProbe::PresentFile | FsProbe::Symlink | FsProbe::WrongType
            );
            ProposedDestination {
                kind: DestinationKind::SoftPatchSibling,
                path: Some(EncodedPath::from_path(&destination_path)),
                file_name: Some(EncodedPath::from_os_string(&file_name)),
                derivation: "content_basename_soft_patch_sibling",
                parent_exists: Some(parent_exists),
                destination_exists: Some(destination_exists),
                conflict: destination_exists,
                unsupported_reason: None,
            }
        })
        .collect()
}

fn build_profile_outcome(
    filesystem: &dyn ReadOnlyHostFilesystem,
    profile: &RetroArchProfile,
    content_extension: Option<&str>,
    content_stem: Option<&OsStr>,
    playlist_evidence: Vec<PlaylistEvidence>,
) -> RetroArchProfileOutcome {
    let profile_ref = ProfileRef {
        profile_kind: profile.profile_kind,
        scope: profile.scope,
    };
    let cheats_finding = profile
        .paths
        .iter()
        .find(|finding| finding.purpose == PathPurpose::Cheats);

    let cheats_root = resolve_cheats_root(cheats_finding);
    let cheat_database_root = cheats_root.destination.clone();

    let mut reasons = Vec::new();
    let (
        disposition,
        matched_core_stem,
        candidate_core_stems,
        per_game_cheat_file,
        selected_core_source,
    ) = match (content_extension, &cheats_root.raw_path) {
        (None, _) => {
            reasons.push(
                "the catalogue archive has no usable file extension for core matching".to_string(),
            );
            (
                CoreMatchDisposition::UnsupportedNoContentExtension,
                None,
                Vec::new(),
                ProposedDestination::unsupported("content_extension_unknown"),
                None,
            )
        }
        (Some(_), None) => {
            let reason = cheats_root
                .unresolved_reason
                .unwrap_or("cheats_path_unresolved");
            reasons.push(format!(
                "this profile's cheat database path is not usable: {reason}"
            ));
            let disposition = if reason == "cheats_path_missing" {
                CoreMatchDisposition::UnsupportedCheatsPathMissing
            } else {
                CoreMatchDisposition::UnsupportedCheatsPathUnresolved
            };
            (
                disposition,
                None,
                Vec::new(),
                ProposedDestination::unsupported(reason),
                None,
            )
        }
        (Some(extension), Some(raw_root)) => {
            let candidates = matching_core_stems(profile, extension);
            match candidates.len() {
                0 => {
                    reasons.push(
                            "no installed core in this profile declares this file extension as supported"
                                .to_string(),
                        );
                    let upgrade = upgrade_via_playlist_evidence(&playlist_evidence);
                    match upgrade {
                        Some(core_stem) => {
                            reasons.push(format!(
                                "upgraded by playlist evidence linking installed core {core_stem}"
                            ));
                            let destination = per_game_cheat_destination(
                                filesystem,
                                raw_root,
                                &core_stem,
                                content_stem,
                            );
                            (
                                CoreMatchDisposition::ExactCore,
                                Some(core_stem.clone()),
                                vec![core_stem],
                                destination,
                                Some(CoreSelectionSource::PlaylistEvidence),
                            )
                        }
                        None => (
                            CoreMatchDisposition::UnsupportedNoCore,
                            None,
                            Vec::new(),
                            ProposedDestination::unsupported(
                                "no_installed_core_supports_extension",
                            ),
                            None,
                        ),
                    }
                }
                1 => {
                    let core_stem = candidates[0].clone();
                    reasons.push(format!(
                            "exactly one installed core ({core_stem}) declares this file extension as supported"
                        ));
                    let destination =
                        per_game_cheat_destination(filesystem, raw_root, &core_stem, content_stem);
                    (
                        CoreMatchDisposition::ExactCore,
                        Some(core_stem),
                        candidates,
                        destination,
                        Some(CoreSelectionSource::ExtensionMatch),
                    )
                }
                _ => {
                    reasons.push(format!(
                            "{} installed cores in this profile declare this file extension as supported; no single destination can be proposed",
                            candidates.len()
                        ));
                    let upgrade = upgrade_via_playlist_evidence(&playlist_evidence);
                    match upgrade {
                        Some(core_stem) => {
                            reasons.push(format!(
                                "upgraded by playlist evidence linking installed core {core_stem}"
                            ));
                            let destination = per_game_cheat_destination(
                                filesystem,
                                raw_root,
                                &core_stem,
                                content_stem,
                            );
                            (
                                CoreMatchDisposition::ExactCore,
                                Some(core_stem.clone()),
                                vec![core_stem],
                                destination,
                                Some(CoreSelectionSource::PlaylistEvidence),
                            )
                        }
                        None => (
                            CoreMatchDisposition::AmbiguousCore,
                            None,
                            candidates,
                            ProposedDestination::unsupported(
                                "multiple_installed_cores_support_extension",
                            ),
                            None,
                        ),
                    }
                }
            }
        }
    };

    RetroArchProfileOutcome {
        profile: profile_ref,
        disposition,
        matched_core_stem,
        candidate_core_stems,
        selected_core_source,
        playlist_evidence,
        cheat_database_root,
        per_game_cheat_file,
        reasons,
    }
}

/// Returns the single installed core stem to upgrade to, if and only if
/// every piece of `Strong`-or-better playlist evidence for this archive in
/// this profile agrees on exactly one linked installed core. Any
/// disagreement, any evidence below `Strong`, or no evidence at all
/// yields `None` - upgrading is only ever a *strengthening* of an already
/// blocked result, never a guess.
fn upgrade_via_playlist_evidence(playlist_evidence: &[PlaylistEvidence]) -> Option<String> {
    let mut linked_stems: Vec<&str> = playlist_evidence
        .iter()
        .filter(|evidence| {
            matches!(
                evidence.confidence,
                PlaylistMatchConfidence::Strong | PlaylistMatchConfidence::Exact
            )
        })
        .filter_map(|evidence| match &evidence.core_association {
            CoreAssociation::LinkedByCorePath { core_stem }
            | CoreAssociation::LinkedByCoreName { core_stem } => Some(core_stem.as_str()),
            _ => None,
        })
        .collect();
    linked_stems.sort_unstable();
    linked_stems.dedup();
    match linked_stems.len() {
        1 => Some(linked_stems[0].to_string()),
        _ => None,
    }
}

/// Resolved cheats-root state for one profile: the `ProposedDestination` to
/// display, plus (only when genuinely usable) the real `PathBuf` needed to
/// safely compute a further-nested per-game cheat destination.
/// `raw_path` is deliberately `None` whenever `destination.path` is `None`
/// *or* lossy - reconstructing a further path from a lossy display string
/// would silently risk probing/joining the wrong bytes (see the module doc
/// comment's "Do not use lossy strings as identity" rationale).
struct CheatsRoot {
    destination: ProposedDestination,
    raw_path: Option<PathBuf>,
    unresolved_reason: Option<&'static str>,
}

fn resolve_cheats_root(cheats_finding: Option<&PathFinding>) -> CheatsRoot {
    let Some(finding) = cheats_finding else {
        return CheatsRoot {
            destination: ProposedDestination::unsupported("cheats_path_unresolved"),
            raw_path: None,
            unresolved_reason: Some("cheats_path_unresolved"),
        };
    };
    if finding.resolution != ResolutionState::ConfiguredResolved {
        return CheatsRoot {
            destination: ProposedDestination::unsupported("cheats_path_unresolved"),
            raw_path: None,
            unresolved_reason: Some("cheats_path_unresolved"),
        };
    }
    let Some(resolved) = &finding.resolved_path else {
        return CheatsRoot {
            destination: ProposedDestination::unsupported("cheats_path_unresolved"),
            raw_path: None,
            unresolved_reason: Some("cheats_path_unresolved"),
        };
    };
    let exists = finding.probe == Some(FsProbe::PresentDirectory);
    let destination = ProposedDestination {
        kind: DestinationKind::CheatDatabaseRoot,
        path: Some(resolved.clone()),
        file_name: None,
        derivation: "cheat_database_path_config_key",
        parent_exists: None,
        destination_exists: Some(exists),
        conflict: false,
        unsupported_reason: None,
    };
    if !exists {
        return CheatsRoot {
            destination,
            raw_path: None,
            unresolved_reason: Some("cheats_path_missing"),
        };
    }
    // Only safe to build a further-nested path when the resolved display
    // string is a lossless (non-lossy) rendering of the real path.
    let raw_path = if resolved.lossy {
        None
    } else {
        Some(PathBuf::from(&resolved.display))
    };
    let unresolved_reason = if raw_path.is_none() {
        Some("cheats_path_not_utf8")
    } else {
        None
    };
    CheatsRoot {
        destination,
        raw_path,
        unresolved_reason,
    }
}

/// Every installed core stem (sorted, deduplicated) whose `.info` metadata
/// declares `extension` as a supported extension - compared
/// case-insensitively, since real filesystem extensions vary case more
/// often than a core's own declared list does.
fn matching_core_stems(profile: &RetroArchProfile, extension: &str) -> Vec<String> {
    let mut stems = profile
        .cores
        .iter()
        .filter_map(|core| match &core.info {
            CoreInfoFinding::Found {
                supported_extensions,
                ..
            } if supported_extensions
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(extension)) =>
            {
                Some(core.core_stem.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    stems.sort();
    stems.dedup();
    stems
}

/// Read-only, in-memory lookup built once per plan from the present
/// catalogue archives - never touches the filesystem itself. Shared
/// across every profile/playlist/entry being matched, since the
/// catalogue does not vary per RetroArch profile.
struct ArchiveLookup<'a> {
    by_absolute_path: HashMap<&'a Path, i64>,
    by_basename: HashMap<&'a OsStr, Vec<i64>>,
    has_platform: HashMap<i64, bool>,
    normalized_name: HashMap<i64, &'a str>,
}

fn build_archive_lookup(archives: &[PersistedArchive]) -> ArchiveLookup<'_> {
    let mut by_absolute_path = HashMap::new();
    let mut by_basename: HashMap<&OsStr, Vec<i64>> = HashMap::new();
    let mut has_platform = HashMap::new();
    let mut normalized_name = HashMap::new();
    for archive in archives {
        by_absolute_path.insert(archive.absolute_path.as_path(), archive.id);
        if let Some(name) = archive.absolute_path.file_name() {
            by_basename.entry(name).or_default().push(archive.id);
        }
        has_platform.insert(archive.id, archive.platform.is_some());
        normalized_name.insert(archive.id, archive.normalized_name.as_str());
    }
    for candidates in by_basename.values_mut() {
        candidates.sort_unstable();
    }
    ArchiveLookup {
        by_absolute_path,
        by_basename,
        has_platform,
        normalized_name,
    }
}

enum MatchOutcome {
    None,
    Single {
        archive_id: i64,
        confidence: PlaylistMatchConfidence,
        basis: &'static str,
    },
    Ambiguous {
        archive_ids: Vec<i64>,
        basis: &'static str,
    },
}

/// Lowercased, alphanumeric-only normalization used only for the weakest
/// ("label-only") evidence tier - deliberately the same *technique*
/// `patch_manager::mod`'s own `normalize_title` already uses for PCSX2's
/// title-matching tier, not shared code (that function is private to a
/// different module, and this milestone's evidence categories are
/// otherwise unrelated to PCSX2's).
fn normalize_for_label_match(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Matches one playlist entry against every present catalogue archive,
/// strongest evidence first - see `docs/RETROARCH_PLAYLISTS.md` for the
/// full tier record. Never invents evidence ArchiveFS does not have: tier
/// 2 ("archive path plus exact inner member identity") from the design
/// review can never reach `Exact` here, because ArchiveFS has no inner-
/// member identity to verify against - an archive-member path's outer
/// archive match tops out at `Strong`, explicitly incomplete.
fn match_entry_to_archives(
    lookup: &ArchiveLookup<'_>,
    entry: &RetroArchPlaylistEntry,
) -> MatchOutcome {
    let content_path = &entry.content_path;
    match content_path.kind {
        ContentPathKind::Filesystem => {
            if let Some(raw) = &content_path.raw
                && let Some(&archive_id) = lookup.by_absolute_path.get(Path::new(raw.as_str()))
            {
                return MatchOutcome::Single {
                    archive_id,
                    confidence: PlaylistMatchConfidence::Exact,
                    basis: "exact_content_path",
                };
            }
        }
        ContentPathKind::ArchiveMember => {
            if let Some(archive_path) = &content_path.archive_path
                && let Some(&archive_id) = lookup
                    .by_absolute_path
                    .get(Path::new(archive_path.as_str()))
            {
                return MatchOutcome::Single {
                    archive_id,
                    confidence: PlaylistMatchConfidence::Strong,
                    basis: "archive_path_member_unverified",
                };
            }
        }
        ContentPathKind::Relative | ContentPathKind::Empty | ContentPathKind::Missing => {}
    }

    let basename_source = match content_path.kind {
        ContentPathKind::ArchiveMember => content_path.archive_path.as_deref(),
        ContentPathKind::Filesystem | ContentPathKind::Relative => content_path.raw.as_deref(),
        ContentPathKind::Empty | ContentPathKind::Missing => None,
    };
    if let Some(basename) = basename_source.and_then(|source| Path::new(source).file_name())
        && let Some(candidates) = lookup.by_basename.get(basename)
    {
        match candidates.len() {
            0 => {}
            1 => {
                let archive_id = candidates[0];
                let confidence = if lookup
                    .has_platform
                    .get(&archive_id)
                    .copied()
                    .unwrap_or(false)
                {
                    PlaylistMatchConfidence::Strong
                } else {
                    PlaylistMatchConfidence::Weak
                };
                return MatchOutcome::Single {
                    archive_id,
                    confidence,
                    basis: "normalized_basename",
                };
            }
            _ => {
                return MatchOutcome::Ambiguous {
                    archive_ids: candidates.clone(),
                    basis: "normalized_basename",
                };
            }
        }
    }

    // Last resort: label-only evidence, only when nothing path-shaped was
    // usable at all.
    if let Some(label) = &entry.label {
        let normalized_label = normalize_for_label_match(label);
        if !normalized_label.is_empty() {
            let mut candidates: Vec<i64> = lookup
                .normalized_name
                .iter()
                .filter(|(_, name)| **name == normalized_label)
                .map(|(id, _)| *id)
                .collect();
            candidates.sort_unstable();
            match candidates.len() {
                0 => {}
                1 => {
                    return MatchOutcome::Single {
                        archive_id: candidates[0],
                        confidence: PlaylistMatchConfidence::Weak,
                        basis: "label_or_filename_only",
                    };
                }
                _ => {
                    return MatchOutcome::Ambiguous {
                        archive_ids: candidates,
                        basis: "label_or_filename_only",
                    };
                }
            }
        }
    }

    MatchOutcome::None
}

/// `core_path`'s own filename stem, stripped of the verified
/// `_libretro.so` suffix - mirrors
/// `emulator_environment::retroarch`'s own core-stem derivation exactly,
/// so the two sides of the comparison use identical rules.
fn core_path_to_stem(core_path: &str) -> Option<String> {
    let file_name = Path::new(core_path).file_name()?.to_str()?;
    file_name.strip_suffix("_libretro.so").map(str::to_string)
}

/// Links one playlist entry's `core_path`/`core_name` evidence to an
/// installed core in `profile`, preferring the stable filename/stem
/// identity (`core_path`) over the volatile display-name identity
/// (`core_name`) - see [`CoreAssociation`] and
/// `docs/RETROARCH_PLAYLISTS.md`.
fn associate_core(entry: &RetroArchPlaylistEntry, profile: &RetroArchProfile) -> CoreAssociation {
    let is_detect = |value: &Option<String>| value.as_deref() == Some("DETECT");
    if entry.core_path.is_none() && entry.core_name.is_none() {
        return CoreAssociation::NoCoreEvidence;
    }
    if is_detect(&entry.core_path) || is_detect(&entry.core_name) {
        return CoreAssociation::Detect;
    }
    if let Some(core_path) = &entry.core_path
        && let Some(stem) = core_path_to_stem(core_path)
        && profile.cores.iter().any(|core| core.core_stem == stem)
    {
        return CoreAssociation::LinkedByCorePath { core_stem: stem };
    }
    if let Some(core_name) = &entry.core_name {
        let mut candidates: Vec<String> = profile
            .cores
            .iter()
            .filter_map(|core| match &core.info {
                CoreInfoFinding::Found {
                    display_name: Some(name),
                    ..
                } if name == core_name => Some(core.core_stem.clone()),
                _ => None,
            })
            .collect();
        candidates.sort();
        match candidates.len() {
            0 => {}
            1 => {
                return CoreAssociation::LinkedByCoreName {
                    core_stem: candidates[0].clone(),
                };
            }
            _ => {
                return CoreAssociation::AmbiguousCoreName {
                    candidate_stems: candidates,
                };
            }
        }
    }
    CoreAssociation::NoInstalledCoreMatch
}

/// Builds every playlist entry's best evidence about the present
/// catalogue archives, grouped by `(profile, archive_id)` for cheap
/// lookup while building each archive's advisory entry. Pure, read-only,
/// in-memory - no filesystem access; the environment report and archive
/// list are already fully loaded by the time this runs.
fn build_playlist_evidence_by_archive(
    environment: &RetroArchEnvironmentReport,
    archives: &[PersistedArchive],
) -> BTreeMap<(ProfileRef, i64), Vec<PlaylistEvidence>> {
    let lookup = build_archive_lookup(archives);
    let mut by_archive: BTreeMap<(ProfileRef, i64), Vec<PlaylistEvidence>> = BTreeMap::new();

    for profile in &environment.profiles {
        let profile_ref = ProfileRef {
            profile_kind: profile.profile_kind,
            scope: profile.scope,
        };
        for playlist in &profile.playlists.playlists {
            for entry in &playlist.entries {
                let outcome = match_entry_to_archives(&lookup, entry);
                let core_association = associate_core(entry, profile);
                let (archive_ids, confidence, basis) = match outcome {
                    MatchOutcome::None => continue,
                    MatchOutcome::Single {
                        archive_id,
                        confidence,
                        basis,
                    } => (vec![archive_id], confidence, basis),
                    MatchOutcome::Ambiguous { archive_ids, basis } => {
                        (archive_ids, PlaylistMatchConfidence::Ambiguous, basis)
                    }
                };
                let ambiguous = confidence == PlaylistMatchConfidence::Ambiguous;
                for archive_id in &archive_ids {
                    let evidence = PlaylistEvidence {
                        playlist_file: playlist.file_path.clone(),
                        playlist_name: playlist.playlist_name.clone(),
                        entry_index: entry.entry_index,
                        entry_label: entry.label.clone(),
                        matched_archive_id: if ambiguous { None } else { Some(*archive_id) },
                        ambiguous_archive_ids: if ambiguous {
                            archive_ids.clone()
                        } else {
                            Vec::new()
                        },
                        confidence,
                        evidence_basis: basis,
                        content_path_kind: entry.content_path.kind,
                        database_name: entry.database_name.clone(),
                        crc: entry.crc.clone(),
                        core_association: core_association.clone(),
                    };
                    by_archive
                        .entry((profile_ref, *archive_id))
                        .or_default()
                        .push(evidence);
                }
            }
        }
    }

    for evidence_list in by_archive.values_mut() {
        evidence_list.sort_by(|left, right| {
            left.playlist_file
                .display
                .cmp(&right.playlist_file.display)
                .then_with(|| left.entry_index.cmp(&right.entry_index))
        });
    }

    by_archive
}

/// `<cheat_database_root>/<core_stem>/<content_stem>.cht` - verified
/// against `cheat_manager.c`'s `cheat_manager_get_game_specific_filename`
/// (`fill_pathname_join_special(path_cheat_database, core_name)`, then join
/// `game_name`, the content's own basename with its extension replaced by
/// `.cht`). Only ever called when the cheats root is a genuinely usable,
/// non-lossy path and a content stem is available.
fn per_game_cheat_destination(
    filesystem: &dyn ReadOnlyHostFilesystem,
    cheats_root: &Path,
    core_stem: &str,
    content_stem: Option<&OsStr>,
) -> ProposedDestination {
    let Some(content_stem) = content_stem else {
        return ProposedDestination::unsupported("content_extension_unknown");
    };
    let core_dir = cheats_root.join(core_stem);
    let core_dir_exists = filesystem.probe(&core_dir) == FsProbe::PresentDirectory;
    let mut file_name = content_stem.to_os_string();
    file_name.push(".cht");
    let destination_path = core_dir.join(&file_name);
    let destination_probe = filesystem.probe(&destination_path);
    let destination_exists = matches!(
        destination_probe,
        FsProbe::PresentFile | FsProbe::Symlink | FsProbe::WrongType
    );
    ProposedDestination {
        kind: DestinationKind::PerGameCheatFile,
        path: Some(EncodedPath::from_path(&destination_path)),
        file_name: Some(EncodedPath::from_os_string(&file_name)),
        derivation: "core_supported_extensions_single_match",
        parent_exists: Some(core_dir_exists),
        destination_exists: Some(destination_exists),
        conflict: destination_exists,
        unsupported_reason: None,
    }
}

fn compute_plan_id(
    environment: &RetroArchEnvironmentReport,
    entries: &[RetroArchAdvisoryEntry],
) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"archivefs-retroarch-advisory-plan-v1");
    hash_field(&mut hasher, &environment.format_version.to_le_bytes());
    for profile in &environment.profiles {
        hash_field(&mut hasher, profile_kind_tag(profile.profile_kind));
        hash_field(&mut hasher, profile_scope_tag(profile.scope));
        hash_field(
            &mut hasher,
            profile.config_directory.path.display.as_bytes(),
        );
        hash_field(&mut hasher, fs_probe_tag(profile.config_directory.probe));
        for path in &profile.paths {
            hash_field(&mut hasher, path_purpose_tag(path.purpose));
            hash_field(&mut hasher, resolution_state_tag(path.resolution));
            hash_optional_string(
                &mut hasher,
                path.resolved_path
                    .as_ref()
                    .map(|value| value.display.as_str()),
            );
        }
        for core in &profile.cores {
            hash_field(&mut hasher, core.full_path.display.as_bytes());
            hash_field(&mut hasher, core.core_stem.as_bytes());
        }
        hash_optional_string(
            &mut hasher,
            profile
                .playlists
                .directory
                .as_ref()
                .map(|value| value.display.as_str()),
        );
        for playlist in &profile.playlists.playlists {
            hash_field(&mut hasher, playlist.file_path.display.as_bytes());
            for playlist_entry in &playlist.entries {
                hash_field(&mut hasher, &playlist_entry.entry_index.to_le_bytes());
                hash_optional_string(&mut hasher, playlist_entry.content_path.raw.as_deref());
                hash_optional_string(&mut hasher, playlist_entry.core_path.as_deref());
                hash_optional_string(&mut hasher, playlist_entry.core_name.as_deref());
            }
        }
    }
    for entry in entries {
        hash_field(&mut hasher, &entry.archive_id.to_le_bytes());
        hash_optional_string(&mut hasher, entry.content_extension.as_deref());
        for destination in &entry.soft_patch_candidates {
            hash_destination(&mut hasher, destination);
        }
        for outcome in &entry.profile_outcomes {
            hash_field(&mut hasher, profile_kind_tag(outcome.profile.profile_kind));
            hash_field(&mut hasher, profile_scope_tag(outcome.profile.scope));
            hash_field(&mut hasher, disposition_tag(outcome.disposition));
            hash_optional_string(&mut hasher, outcome.matched_core_stem.as_deref());
            for stem in &outcome.candidate_core_stems {
                hash_field(&mut hasher, stem.as_bytes());
            }
            hash_field(
                &mut hasher,
                core_selection_source_tag(outcome.selected_core_source),
            );
            for evidence in &outcome.playlist_evidence {
                hash_field(&mut hasher, evidence.playlist_file.display.as_bytes());
                hash_field(&mut hasher, &evidence.entry_index.to_le_bytes());
                hash_optional_i64(&mut hasher, evidence.matched_archive_id);
                for archive_id in &evidence.ambiguous_archive_ids {
                    hash_field(&mut hasher, &archive_id.to_le_bytes());
                }
                hash_field(
                    &mut hasher,
                    playlist_match_confidence_tag(evidence.confidence),
                );
                hash_field(&mut hasher, evidence.evidence_basis.as_bytes());
                hash_core_association(&mut hasher, &evidence.core_association);
            }
            hash_destination(&mut hasher, &outcome.cheat_database_root);
            hash_destination(&mut hasher, &outcome.per_game_cheat_file);
        }
    }
    encode_hex(&hasher.finalize())
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hash_field(hasher, b"some");
            hash_field(hasher, &value.to_le_bytes());
        }
        None => hash_field(hasher, b"none"),
    }
}

fn hash_core_association(hasher: &mut Sha256, association: &CoreAssociation) {
    match association {
        CoreAssociation::LinkedByCorePath { core_stem } => {
            hash_field(hasher, b"linked_by_core_path");
            hash_field(hasher, core_stem.as_bytes());
        }
        CoreAssociation::LinkedByCoreName { core_stem } => {
            hash_field(hasher, b"linked_by_core_name");
            hash_field(hasher, core_stem.as_bytes());
        }
        CoreAssociation::Detect => hash_field(hasher, b"detect"),
        CoreAssociation::AmbiguousCoreName { candidate_stems } => {
            hash_field(hasher, b"ambiguous_core_name");
            for stem in candidate_stems {
                hash_field(hasher, stem.as_bytes());
            }
        }
        CoreAssociation::NoInstalledCoreMatch => hash_field(hasher, b"no_installed_core_match"),
        CoreAssociation::NoCoreEvidence => hash_field(hasher, b"no_core_evidence"),
    }
}

fn core_selection_source_tag(source: Option<CoreSelectionSource>) -> &'static [u8] {
    match source {
        Some(CoreSelectionSource::ExtensionMatch) => b"extension_match",
        Some(CoreSelectionSource::PlaylistEvidence) => b"playlist_evidence",
        None => b"none",
    }
}

fn playlist_match_confidence_tag(confidence: PlaylistMatchConfidence) -> &'static [u8] {
    match confidence {
        PlaylistMatchConfidence::Unsupported => b"unsupported",
        PlaylistMatchConfidence::Weak => b"weak",
        PlaylistMatchConfidence::Strong => b"strong",
        PlaylistMatchConfidence::Exact => b"exact",
        PlaylistMatchConfidence::Ambiguous => b"ambiguous",
    }
}

fn hash_destination(hasher: &mut Sha256, destination: &ProposedDestination) {
    hash_field(hasher, destination_kind_tag(destination.kind));
    hash_optional_string(
        hasher,
        destination
            .path
            .as_ref()
            .map(|value| value.display.as_str()),
    );
    hash_field(hasher, destination.derivation.as_bytes());
    hash_optional_bool(hasher, destination.parent_exists);
    hash_optional_bool(hasher, destination.destination_exists);
    hash_field(hasher, &[destination.conflict as u8]);
    hash_optional_string(hasher, destination.unsupported_reason);
}

fn hash_optional_bool(hasher: &mut Sha256, value: Option<bool>) {
    match value {
        Some(true) => hash_field(hasher, b"true"),
        Some(false) => hash_field(hasher, b"false"),
        None => hash_field(hasher, b"none"),
    }
}

fn hash_optional_string(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hash_field(hasher, b"some");
            hash_field(hasher, value.as_bytes());
        }
        None => hash_field(hasher, b"none"),
    }
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn profile_kind_tag(kind: crate::emulator_environment::retroarch::ProfileKind) -> &'static [u8] {
    use crate::emulator_environment::retroarch::ProfileKind;
    match kind {
        ProfileKind::Native => b"native",
        ProfileKind::Flatpak => b"flatpak",
    }
}

fn profile_scope_tag(scope: crate::emulator_environment::retroarch::ProfileScope) -> &'static [u8] {
    use crate::emulator_environment::retroarch::ProfileScope;
    match scope {
        ProfileScope::User => b"user",
        ProfileScope::System => b"system",
    }
}

fn fs_probe_tag(probe: FsProbe) -> &'static [u8] {
    match probe {
        FsProbe::PresentFile => b"present_file",
        FsProbe::PresentDirectory => b"present_directory",
        FsProbe::Missing => b"missing",
        FsProbe::Symlink => b"symlink",
        FsProbe::WrongType => b"wrong_type",
        FsProbe::Inaccessible => b"inaccessible",
        FsProbe::IoError => b"io_error",
    }
}

fn path_purpose_tag(purpose: PathPurpose) -> &'static [u8] {
    match purpose {
        PathPurpose::System => b"system",
        PathPurpose::Cores => b"cores",
        PathPurpose::CoreInfo => b"core_info",
        PathPurpose::Saves => b"saves",
        PathPurpose::SaveStates => b"save_states",
        PathPurpose::Playlists => b"playlists",
        PathPurpose::Shaders => b"shaders",
        PathPurpose::Overlays => b"overlays",
        PathPurpose::Thumbnails => b"thumbnails",
        PathPurpose::JoypadAutoconfig => b"joypad_autoconfig",
        PathPurpose::Database => b"database",
        PathPurpose::Cheats => b"cheats",
    }
}

fn resolution_state_tag(state: ResolutionState) -> &'static [u8] {
    match state {
        ResolutionState::ConfiguredResolved => b"configured_resolved",
        ResolutionState::ConfiguredUnresolved => b"configured_unresolved",
        ResolutionState::RuntimeDefaultUnknown => b"runtime_default_unknown",
        ResolutionState::NoReadableConfig => b"no_readable_config",
    }
}

fn destination_kind_tag(kind: DestinationKind) -> &'static [u8] {
    match kind {
        DestinationKind::CheatDatabaseRoot => b"cheat_database_root",
        DestinationKind::PerGameCheatFile => b"per_game_cheat_file",
        DestinationKind::SoftPatchSibling => b"soft_patch_sibling",
        DestinationKind::Unsupported => b"unsupported",
    }
}

fn disposition_tag(disposition: CoreMatchDisposition) -> &'static [u8] {
    match disposition {
        CoreMatchDisposition::ExactCore => b"exact_core",
        CoreMatchDisposition::AmbiguousCore => b"ambiguous_core",
        CoreMatchDisposition::UnsupportedNoCore => b"unsupported_no_core",
        CoreMatchDisposition::UnsupportedNoContentExtension => b"unsupported_no_content_extension",
        CoreMatchDisposition::UnsupportedCheatsPathUnresolved => {
            b"unsupported_cheats_path_unresolved"
        }
        CoreMatchDisposition::UnsupportedCheatsPathMissing => b"unsupported_cheats_path_missing",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::emulator_environment::HostReadOnlyFilesystem;
    use crate::emulator_environment::retroarch::{
        ConfigFileFinding, ConfigReadOutcome, CoreFinding, DirectoryProbeFinding, Evidence,
        ProfileKind, ProfileScope,
    };

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "archivefs-retroarch-patch-preview-{label}-{}-{}",
            std::process::id(),
            label.len()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn archive(
        id: i64,
        relative: &str,
        absolute: &Path,
        platform: Option<&str>,
    ) -> PersistedArchive {
        PersistedArchive {
            id,
            source_folder_id: 1,
            relative_path: PathBuf::from(relative),
            absolute_path: absolute.to_path_buf(),
            archive_kind: "zip".to_string(),
            display_name: relative.to_string(),
            normalized_name: relative.to_ascii_lowercase(),
            size_bytes: None,
            modified_time_unix_seconds: None,
            platform: platform.map(str::to_string),
            platform_source: None,
            last_known_health: "present".to_string(),
            last_seen_at: "2026-01-01T00:00:00Z".to_string(),
            last_verified_missing_at: None,
        }
    }

    fn found_core(stem: &str, extensions: &[&str]) -> CoreFinding {
        CoreFinding {
            file_name: EncodedPath::from_path(Path::new(&format!("{stem}_libretro.so"))),
            full_path: EncodedPath::from_path(Path::new(&format!("/cores/{stem}_libretro.so"))),
            core_stem: stem.to_string(),
            info: CoreInfoFinding::Found {
                display_name: Some(stem.to_string()),
                display_version: None,
                system_name: None,
                supported_extensions: extensions.iter().map(|value| value.to_string()).collect(),
            },
        }
    }

    fn resolved_cheats_finding(path: &Path, exists: bool) -> PathFinding {
        PathFinding {
            purpose: PathPurpose::Cheats,
            config_key: "cheat_database_path",
            configured_value: Some(path.display().to_string()),
            resolution: ResolutionState::ConfiguredResolved,
            resolved_path: Some(EncodedPath::from_path(path)),
            probe: Some(if exists {
                FsProbe::PresentDirectory
            } else {
                FsProbe::Missing
            }),
        }
    }

    fn unresolved_cheats_finding() -> PathFinding {
        PathFinding {
            purpose: PathPurpose::Cheats,
            config_key: "cheat_database_path",
            configured_value: None,
            resolution: ResolutionState::RuntimeDefaultUnknown,
            resolved_path: None,
            probe: None,
        }
    }

    fn profile(
        profile_kind: ProfileKind,
        scope: ProfileScope,
        cheats: PathFinding,
        cores: Vec<CoreFinding>,
    ) -> RetroArchProfile {
        RetroArchProfile {
            profile_kind,
            scope,
            evidence: Evidence {
                executables: Vec::new(),
                flatpak_metadata_found: false,
                config_directory_found: true,
                config_file_found: true,
            },
            config_directory: DirectoryProbeFinding {
                path: EncodedPath::from_path(Path::new("/config")),
                probe: FsProbe::PresentDirectory,
            },
            config_file: ConfigFileFinding {
                path: EncodedPath::from_path(Path::new("/config/retroarch.cfg")),
                probe: FsProbe::PresentFile,
                read: ConfigReadOutcome::Parsed {
                    malformed_lines: Vec::new(),
                    include_detected: false,
                    complete: true,
                },
            },
            paths: vec![cheats],
            cores,
            playlists: crate::emulator_environment::retroarch::RetroArchPlaylistInventory {
                directory: None,
                playlists: Vec::new(),
                diagnostics: Vec::new(),
                complete: true,
            },
            diagnostics: Vec::new(),
        }
    }

    fn report_with_profiles(profiles: Vec<RetroArchProfile>) -> RetroArchEnvironmentReport {
        RetroArchEnvironmentReport {
            format_version: 1,
            profiles,
            diagnostics: Vec::new(),
        }
    }

    fn one_profile_report(
        cheats: PathFinding,
        cores: Vec<CoreFinding>,
    ) -> RetroArchEnvironmentReport {
        report_with_profiles(vec![profile(
            ProfileKind::Native,
            ProfileScope::User,
            cheats,
            cores,
        )])
    }

    // ---- Playlist matching fixtures ----

    fn filesystem_content_path(
        raw: &str,
    ) -> crate::emulator_environment::retroarch::PlaylistContentPath {
        crate::emulator_environment::retroarch::PlaylistContentPath {
            raw: Some(raw.to_string()),
            kind: ContentPathKind::Filesystem,
            archive_path: None,
            archive_member_path: None,
        }
    }

    fn archive_member_content_path(
        archive_path: &str,
        member_path: &str,
    ) -> crate::emulator_environment::retroarch::PlaylistContentPath {
        crate::emulator_environment::retroarch::PlaylistContentPath {
            raw: Some(format!("{archive_path}#{member_path}")),
            kind: ContentPathKind::ArchiveMember,
            archive_path: Some(archive_path.to_string()),
            archive_member_path: Some(member_path.to_string()),
        }
    }

    fn no_path() -> crate::emulator_environment::retroarch::PlaylistContentPath {
        crate::emulator_environment::retroarch::PlaylistContentPath {
            raw: None,
            kind: ContentPathKind::Missing,
            archive_path: None,
            archive_member_path: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn playlist_entry(
        entry_index: u32,
        content_path: crate::emulator_environment::retroarch::PlaylistContentPath,
        label: Option<&str>,
        core_path: Option<&str>,
        core_name: Option<&str>,
        crc: PlaylistCrc,
        database_name: Option<&str>,
    ) -> RetroArchPlaylistEntry {
        RetroArchPlaylistEntry {
            entry_index,
            content_path,
            label: label.map(str::to_string),
            core_path: core_path.map(str::to_string),
            core_name: core_name.map(str::to_string),
            crc,
            database_name: database_name.map(str::to_string),
            subsystem_ident: None,
            subsystem_name: None,
        }
    }

    fn playlist(
        name: &str,
        entries: Vec<RetroArchPlaylistEntry>,
    ) -> crate::emulator_environment::retroarch::RetroArchPlaylist {
        crate::emulator_environment::retroarch::RetroArchPlaylist {
            file_path: EncodedPath::from_path(Path::new(&format!("/playlists/{name}.lpl"))),
            playlist_name: name.to_string(),
            version: Some("1.5".to_string()),
            default_core_path: None,
            default_core_name: None,
            entries,
            diagnostics: Vec::new(),
            complete: true,
        }
    }

    fn profile_with_playlists(
        cheats: PathFinding,
        cores: Vec<CoreFinding>,
        playlists: Vec<crate::emulator_environment::retroarch::RetroArchPlaylist>,
    ) -> RetroArchProfile {
        let mut built = profile(ProfileKind::Native, ProfileScope::User, cheats, cores);
        built.playlists = crate::emulator_environment::retroarch::RetroArchPlaylistInventory {
            directory: Some(EncodedPath::from_path(Path::new("/playlists"))),
            playlists,
            diagnostics: Vec::new(),
            complete: true,
        };
        built
    }

    fn one_profile_report_with_playlists(
        cheats: PathFinding,
        cores: Vec<CoreFinding>,
        playlists: Vec<crate::emulator_environment::retroarch::RetroArchPlaylist>,
    ) -> RetroArchEnvironmentReport {
        report_with_profiles(vec![profile_with_playlists(cheats, cores, playlists)])
    }

    // ---- Destinations: soft-patch sibling ----

    #[test]
    fn soft_patch_candidates_use_the_verified_retroarch_try_order_and_naming() {
        let root = temp_root("softpatch-order");
        let archive_path = root.join("Chrono Trigger (USA).sfc.zip");
        fs::write(&archive_path, b"content").unwrap();
        let filesystem = HostReadOnlyFilesystem;

        let destinations = soft_patch_candidates(&filesystem, &archive_path);

        assert_eq!(destinations.len(), 4);
        let expected_names = [
            "Chrono Trigger (USA).sfc.ips",
            "Chrono Trigger (USA).sfc.bps",
            "Chrono Trigger (USA).sfc.ups",
            "Chrono Trigger (USA).sfc.xdelta",
        ];
        for (destination, expected) in destinations.iter().zip(expected_names) {
            assert_eq!(destination.kind, DestinationKind::SoftPatchSibling);
            assert_eq!(destination.file_name.as_ref().unwrap().display, expected);
            assert!(
                destination
                    .path
                    .as_ref()
                    .unwrap()
                    .display
                    .ends_with(expected)
            );
            assert_eq!(destination.parent_exists, Some(true));
            assert_eq!(destination.destination_exists, Some(false));
            assert!(!destination.conflict);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn soft_patch_candidate_reports_conflict_when_a_sibling_already_exists() {
        let root = temp_root("softpatch-conflict");
        let archive_path = root.join("game.zip");
        fs::write(&archive_path, b"content").unwrap();
        fs::write(root.join("game.ips"), b"patch").unwrap();
        let filesystem = HostReadOnlyFilesystem;

        let destinations = soft_patch_candidates(&filesystem, &archive_path);

        let ips = &destinations[0];
        assert_eq!(ips.file_name.as_ref().unwrap().display, "game.ips");
        assert_eq!(ips.destination_exists, Some(true));
        assert!(ips.conflict);
        let bps = &destinations[1];
        assert_eq!(bps.destination_exists, Some(false));
        assert!(!bps.conflict);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn soft_patch_candidates_do_not_create_or_modify_anything() {
        let root = temp_root("softpatch-nowrite");
        let archive_path = root.join("game.zip");
        fs::write(&archive_path, b"content").unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let before = fs::read_dir(&root).unwrap().count();

        let _ = soft_patch_candidates(&filesystem, &archive_path);

        let after = fs::read_dir(&root).unwrap().count();
        assert_eq!(before, after);
        let _ = fs::remove_dir_all(root);
    }

    // ---- Core matching / disposition ----

    #[test]
    fn exactly_one_matching_core_yields_exact_core_and_per_game_destination() {
        let root = temp_root("exact-core");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
        );
        let game = archive(
            1,
            "Chrono Trigger.sfc.zip",
            &root.join("Chrono Trigger.sfc.zip"),
            Some("SNES"),
        );

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        assert_eq!(plan.entries.len(), 1);
        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(outcome.disposition, CoreMatchDisposition::ExactCore);
        assert_eq!(outcome.matched_core_stem.as_deref(), Some("snes9x"));
        assert_eq!(outcome.candidate_core_stems, vec!["snes9x".to_string()]);
        assert_eq!(
            outcome.per_game_cheat_file.kind,
            DestinationKind::PerGameCheatFile
        );
        let expected_path = cheats_dir.join("snes9x").join("Chrono Trigger.sfc.cht");
        assert_eq!(
            outcome.per_game_cheat_file.path.as_ref().unwrap().display,
            expected_path.to_string_lossy()
        );
        assert_eq!(outcome.per_game_cheat_file.destination_exists, Some(false));
        assert_eq!(outcome.per_game_cheat_file.parent_exists, Some(false));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn two_matching_cores_yield_ambiguous_core_and_no_single_destination() {
        let root = temp_root("ambiguous-core");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![
                found_core("mame2003_plus", &["zip"]),
                found_core("fbneo", &["zip"]),
            ],
        );
        let game = archive(1, "sfa3.zip", &root.join("sfa3.zip"), Some("Arcade"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(outcome.disposition, CoreMatchDisposition::AmbiguousCore);
        assert_eq!(outcome.matched_core_stem, None);
        assert_eq!(
            outcome.candidate_core_stems,
            vec!["fbneo".to_string(), "mame2003_plus".to_string()]
        );
        assert_eq!(
            outcome.per_game_cheat_file.kind,
            DestinationKind::Unsupported
        );
        assert_eq!(
            outcome.per_game_cheat_file.unsupported_reason,
            Some("multiple_installed_cores_support_extension")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn zero_matching_cores_yield_unsupported_no_core() {
        let root = temp_root("no-core");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["sfc", "smc"])],
        );
        let game = archive(1, "game.nes.zip", &root.join("game.nes.zip"), Some("NES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(outcome.disposition, CoreMatchDisposition::UnsupportedNoCore);
        assert_eq!(
            outcome.per_game_cheat_file.unsupported_reason,
            Some("no_installed_core_supports_extension")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn no_content_extension_yields_unsupported_no_content_extension() {
        let root = temp_root("no-extension");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["sfc", "smc"])],
        );
        let game = archive(1, "GAMEFILE", &root.join("GAMEFILE"), None);

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let entry = &plan.entries[0];
        assert_eq!(entry.content_extension, None);
        let outcome = &entry.profile_outcomes[0];
        assert_eq!(
            outcome.disposition,
            CoreMatchDisposition::UnsupportedNoContentExtension
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unresolved_cheats_path_blocks_both_destinations_before_core_matching() {
        let root = temp_root("cheats-unresolved");
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            unresolved_cheats_finding(),
            vec![found_core("snes9x", &["sfc"])],
        );
        let game = archive(1, "game.sfc.zip", &root.join("game.sfc.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(
            outcome.disposition,
            CoreMatchDisposition::UnsupportedCheatsPathUnresolved
        );
        assert_eq!(
            outcome.cheat_database_root.kind,
            DestinationKind::Unsupported
        );
        assert_eq!(
            outcome.per_game_cheat_file.kind,
            DestinationKind::Unsupported
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_cheats_directory_is_distinguished_from_unresolved() {
        let root = temp_root("cheats-missing");
        let missing_cheats_dir = root.join("does-not-exist");
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&missing_cheats_dir, false),
            vec![found_core("snes9x", &["sfc"])],
        );
        let game = archive(1, "game.sfc.zip", &root.join("game.sfc.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(
            outcome.disposition,
            CoreMatchDisposition::UnsupportedCheatsPathMissing
        );
        // The bare root destination is still shown (hypothetical), just
        // marked as not currently existing - distinct from "unresolved".
        assert_eq!(
            outcome.cheat_database_root.kind,
            DestinationKind::CheatDatabaseRoot
        );
        assert_eq!(outcome.cheat_database_root.destination_exists, Some(false));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn absent_catalogue_rows_are_excluded_entirely() {
        let root = temp_root("missing-row");
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(unresolved_cheats_finding(), Vec::new());
        let mut missing = archive(1, "game.zip", &root.join("game.zip"), Some("SNES"));
        missing.last_verified_missing_at = Some("2026-01-01T00:00:00Z".to_string());

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![missing]);

        assert!(plan.entries.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    // ---- Case-insensitive extension matching ----

    #[test]
    fn core_extension_matching_is_case_insensitive() {
        let root = temp_root("case-insensitive");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["ZIP"])],
        );
        let game = archive(1, "game.sfc.zip", &root.join("game.sfc.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        assert_eq!(
            plan.entries[0].profile_outcomes[0].disposition,
            CoreMatchDisposition::ExactCore
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- Determinism ----

    #[test]
    fn plan_id_is_deterministic_across_repeated_calls() {
        let root = temp_root("determinism");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let build = || {
            let report = one_profile_report(
                resolved_cheats_finding(&cheats_dir, true),
                vec![found_core("snes9x", &["zip"])],
            );
            let game = archive(1, "game.sfc.zip", &root.join("game.sfc.zip"), Some("SNES"));
            build_retroarch_advisory_plan(&filesystem, report, vec![game])
        };

        let first = build();
        let second = build();

        assert_eq!(first.plan_id, second.plan_id);
        assert!(!first.plan_id.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plan_entries_are_sorted_by_archive_id_regardless_of_input_order() {
        let root = temp_root("shuffled-order");
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(unresolved_cheats_finding(), Vec::new());
        let archives = vec![
            archive(3, "c.zip", &root.join("c.zip"), None),
            archive(1, "a.zip", &root.join("a.zip"), None),
            archive(2, "b.zip", &root.join("b.zip"), None),
        ];

        let plan = build_retroarch_advisory_plan(&filesystem, report, archives);

        let ids: Vec<i64> = plan.entries.iter().map(|entry| entry.archive_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plan_id_is_stable_regardless_of_input_archive_order() {
        let root = temp_root("plan-id-order");
        let filesystem = HostReadOnlyFilesystem;
        let ordered = vec![
            archive(1, "a.zip", &root.join("a.zip"), None),
            archive(2, "b.zip", &root.join("b.zip"), None),
        ];
        let shuffled = vec![
            archive(2, "b.zip", &root.join("b.zip"), None),
            archive(1, "a.zip", &root.join("a.zip"), None),
        ];

        let plan_a = build_retroarch_advisory_plan(
            &filesystem,
            one_profile_report(unresolved_cheats_finding(), Vec::new()),
            ordered,
        );
        let plan_b = build_retroarch_advisory_plan(
            &filesystem,
            one_profile_report(unresolved_cheats_finding(), Vec::new()),
            shuffled,
        );

        assert_eq!(plan_a.plan_id, plan_b.plan_id);
        let _ = fs::remove_dir_all(root);
    }

    // ---- Safety: no writes, no execution, no network ----

    #[test]
    fn preview_makes_no_filesystem_writes_and_no_migration() {
        let root = temp_root("no-write-e2e");
        let home = root.join("home");
        fs::create_dir_all(home.join(".config")).unwrap();
        let catalogue_path = root.join("library.sqlite3");
        Database::open_or_create(&catalogue_path)
            .unwrap()
            .close()
            .unwrap();
        let before_database = fs::read(&catalogue_path).unwrap();
        let before_entries = tree_entries(&root);
        let schema_before = Database::open_read_only(&catalogue_path)
            .unwrap()
            .schema_version()
            .unwrap();

        let filesystem = HostReadOnlyFilesystem;
        let environment = DiscoveryEnvironment {
            home: Some(home.clone().into_os_string()),
            xdg_config_home: Some(home.join(".config").into_os_string()),
            path: None,
            user_flatpak_root: home.join(".local/share/flatpak"),
            system_flatpak_root: root.join("var-lib-flatpak-does-not-exist"),
        };

        let plan = preview_retroarch_patch_and_cheat_destinations(
            &filesystem,
            &environment,
            &catalogue_path,
        )
        .unwrap();

        assert!(!plan.executable);
        assert!(plan.entries.is_empty());
        assert_eq!(tree_entries(&root), before_entries);
        assert_eq!(fs::read(&catalogue_path).unwrap(), before_database);
        let schema_after = Database::open_read_only(&catalogue_path)
            .unwrap()
            .schema_version()
            .unwrap();
        assert_eq!(schema_before, schema_after);
        let _ = fs::remove_dir_all(root);
    }

    // ---- Honesty: no identity tier is used for RetroArch matching ----

    #[test]
    fn no_identity_tier_is_used_for_retroarch_matching() {
        // Even if a future catalogue scanner ever populates `serial`/
        // `executable_crc`-shaped fields on an archive row, this module has
        // no external record to compare them against (see the module doc
        // comment) and must never invent a same-row "exact identity"
        // match. `PersistedArchive` does not expose such fields at all
        // today, so this test instead locks in the structural claim: the
        // core-matching disposition is driven only by installed-core
        // `supported_extensions` evidence, never by any identity string on
        // the archive row itself. Two archives with identical extensions
        // but totally different names/platforms get identical dispositions.
        let root = temp_root("no-identity-tier");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
        );
        let a = archive(
            1,
            "Totally Different Name.sfc.zip",
            &root.join("a.sfc.zip"),
            Some("SNES"),
        );
        let b = archive(
            2,
            "SLUS-00000 Unrelated.sfc.zip",
            &root.join("b.sfc.zip"),
            Some("PS1"),
        );

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![a, b]);

        assert_eq!(
            plan.entries[0].profile_outcomes[0].disposition,
            plan.entries[1].profile_outcomes[0].disposition
        );
        assert_eq!(
            plan.entries[0].profile_outcomes[0].disposition,
            CoreMatchDisposition::ExactCore
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- JSON contract ----

    #[test]
    fn json_uses_stable_lower_snake_case_enum_names() {
        let root = temp_root("json-enum-names");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
        );
        let game = archive(1, "game.sfc.zip", &root.join("game.sfc.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);
        let json = serde_json::to_value(&plan).unwrap();

        assert_eq!(
            json["entries"][0]["profile_outcomes"][0]["disposition"],
            "exact_core"
        );
        assert_eq!(
            json["entries"][0]["profile_outcomes"][0]["per_game_cheat_file"]["kind"],
            "per_game_cheat_file"
        );
        assert_eq!(json["format_version"], RETROARCH_ADVISORY_FORMAT_VERSION);
        assert_eq!(json["executable"], false);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn json_destination_key_set_is_stable() {
        let root = temp_root("json-destination-keys");
        let archive_path = root.join("game.zip");
        fs::write(&archive_path, b"content").unwrap();
        let filesystem = HostReadOnlyFilesystem;

        let destinations = soft_patch_candidates(&filesystem, &archive_path);
        let json = serde_json::to_value(&destinations[0]).unwrap();

        let mut keys: Vec<String> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "conflict",
                "derivation",
                "destination_exists",
                "file_name",
                "kind",
                "parent_exists",
                "path",
                "unsupported_reason",
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- Non-UTF-8 paths ----

    #[test]
    fn soft_patch_sibling_destination_handles_a_non_utf8_archive_path() {
        use std::os::unix::ffi::OsStringExt;

        let root = temp_root("non-utf8-archive");
        let mut raw_name = b"bad-\xFF-name.zip".to_vec();
        let mut file_name_bytes = Vec::new();
        file_name_bytes.append(&mut raw_name);
        let archive_path = root.join(std::ffi::OsString::from_vec(file_name_bytes));
        fs::write(&archive_path, b"content").unwrap();
        let filesystem = HostReadOnlyFilesystem;

        let destinations = soft_patch_candidates(&filesystem, &archive_path);

        // Real byte-preserving path construction still succeeds and still
        // produces exactly 4 candidates in the verified try-order, even
        // though the archive's own name is not valid UTF-8 - only the
        // *display* form is lossy, never the identity used to build the
        // path or probe the filesystem.
        assert_eq!(destinations.len(), 4);
        assert!(destinations[0].path.as_ref().unwrap().lossy);
        assert!(destinations[0].file_name.as_ref().unwrap().lossy);
        assert!(
            destinations[0]
                .file_name
                .as_ref()
                .unwrap()
                .display
                .ends_with(".ips")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn non_utf8_cheats_root_blocks_further_path_construction_instead_of_reconstructing_lossily() {
        use std::os::unix::ffi::OsStringExt;

        let root = temp_root("non-utf8-cheats-root");
        let mut raw_name = b"cheats-\xFF-dir".to_vec();
        let mut dir_name_bytes = Vec::new();
        dir_name_bytes.append(&mut raw_name);
        let cheats_dir = root.join(std::ffi::OsString::from_vec(dir_name_bytes));
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
        );
        let game = archive(1, "game.zip", &root.join("game.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        // The bare root destination is still shown - it is exactly the
        // already-resolved `EncodedPath` from environment discovery,
        // rendered honestly with `lossy: true` rather than refused.
        assert!(outcome.cheat_database_root.path.as_ref().unwrap().lossy);
        // But no further per-game path was built from it: doing so would
        // require reconstructing a `PathBuf` from the lossy display
        // string, silently risking the wrong bytes.
        assert_eq!(
            outcome.per_game_cheat_file.kind,
            DestinationKind::Unsupported
        );
        assert_eq!(
            outcome.per_game_cheat_file.unsupported_reason,
            Some("cheats_path_not_utf8")
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- Playlist matching ----

    #[test]
    fn exact_content_path_match_yields_exact_confidence() {
        let root = temp_root("playlist-exact-path");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let archive_path = root.join("game.zip");
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    filesystem_content_path(&archive_path.to_string_lossy()),
                    Some("Game"),
                    None,
                    None,
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let game = archive(1, "game.zip", &archive_path, Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(outcome.playlist_evidence.len(), 1);
        let evidence = &outcome.playlist_evidence[0];
        assert_eq!(evidence.confidence, PlaylistMatchConfidence::Exact);
        assert_eq!(evidence.evidence_basis, "exact_content_path");
        assert_eq!(evidence.matched_archive_id, Some(1));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archive_member_path_matches_outer_archive_at_strong_not_exact() {
        let root = temp_root("playlist-archive-member");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let archive_path = root.join("game.zip");
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    archive_member_content_path(&archive_path.to_string_lossy(), "game.sfc"),
                    None,
                    None,
                    None,
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let game = archive(1, "game.zip", &archive_path, Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let evidence = &plan.entries[0].profile_outcomes[0].playlist_evidence[0];
        // Explicitly *not* Exact: ArchiveFS never has the inner member's
        // own identity to verify against, so this evidence is incomplete
        // even though the outer archive path matched exactly.
        assert_eq!(evidence.confidence, PlaylistMatchConfidence::Strong);
        assert_eq!(evidence.evidence_basis, "archive_path_member_unverified");
        assert_eq!(evidence.content_path_kind, ContentPathKind::ArchiveMember);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exact_core_name_association_links_an_installed_core() {
        let core = associate_core(
            &playlist_entry(
                0,
                no_path(),
                None,
                Some("/some/other/machines/path/snes9x_libretro.so"),
                Some("snes9x"),
                PlaylistCrc::Missing,
                None,
            ),
            &profile(
                ProfileKind::Native,
                ProfileScope::User,
                unresolved_cheats_finding(),
                vec![found_core("snes9x", &["zip"])],
            ),
        );
        assert_eq!(
            core,
            CoreAssociation::LinkedByCorePath {
                core_stem: "snes9x".to_string()
            }
        );
    }

    #[test]
    fn stale_core_path_falls_back_to_matching_core_name() {
        let mut core_with_display_name = found_core("snes9x", &["zip"]);
        core_with_display_name.info = CoreInfoFinding::Found {
            display_name: Some("Snes9x - Current".to_string()),
            display_version: None,
            system_name: None,
            supported_extensions: vec!["zip".to_string()],
        };
        let core = associate_core(
            &playlist_entry(
                0,
                no_path(),
                None,
                Some("/this/core/path/no/longer/exists_libretro.so"),
                Some("Snes9x - Current"),
                PlaylistCrc::Missing,
                None,
            ),
            &profile(
                ProfileKind::Native,
                ProfileScope::User,
                unresolved_cheats_finding(),
                vec![core_with_display_name],
            ),
        );
        assert_eq!(
            core,
            CoreAssociation::LinkedByCoreName {
                core_stem: "snes9x".to_string()
            }
        );
    }

    #[test]
    fn missing_installed_core_yields_no_installed_core_match() {
        let core = associate_core(
            &playlist_entry(
                0,
                no_path(),
                None,
                Some("/cores/unrelated_libretro.so"),
                Some("Totally Unrelated Core"),
                PlaylistCrc::Missing,
                None,
            ),
            &profile(
                ProfileKind::Native,
                ProfileScope::User,
                unresolved_cheats_finding(),
                vec![found_core("snes9x", &["zip"])],
            ),
        );
        assert_eq!(core, CoreAssociation::NoInstalledCoreMatch);
    }

    #[test]
    fn detect_core_is_recognized_as_no_specific_core() {
        let core = associate_core(
            &playlist_entry(
                0,
                no_path(),
                None,
                Some("DETECT"),
                Some("DETECT"),
                PlaylistCrc::Missing,
                None,
            ),
            &profile(
                ProfileKind::Native,
                ProfileScope::User,
                unresolved_cheats_finding(),
                vec![found_core("snes9x", &["zip"])],
            ),
        );
        assert_eq!(core, CoreAssociation::Detect);
    }

    #[test]
    fn ambiguous_core_is_resolved_by_agreeing_playlist_evidence() {
        let root = temp_root("playlist-upgrade-ambiguous");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let archive_path = root.join("sfa3.zip");
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![
                found_core("mame2003_plus", &["zip"]),
                found_core("fbneo", &["zip"]),
            ],
            vec![playlist(
                "Arcade",
                vec![playlist_entry(
                    0,
                    filesystem_content_path(&archive_path.to_string_lossy()),
                    Some("Street Fighter Alpha 3"),
                    Some("/cores/fbneo_libretro.so"),
                    Some("fbneo"),
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let game = archive(1, "sfa3.zip", &archive_path, Some("Arcade"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(outcome.disposition, CoreMatchDisposition::ExactCore);
        assert_eq!(outcome.matched_core_stem.as_deref(), Some("fbneo"));
        assert_eq!(
            outcome.selected_core_source,
            Some(CoreSelectionSource::PlaylistEvidence)
        );
        assert_eq!(
            outcome.per_game_cheat_file.kind,
            DestinationKind::PerGameCheatFile
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn extension_based_exact_core_is_never_overridden_by_playlist_evidence() {
        let root = temp_root("playlist-no-downgrade");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let archive_path = root.join("game.zip");
        // Playlist evidence names a *different*, uninstalled core - if the
        // upgrade path were wrongly applied to already-resolved outcomes,
        // this would corrupt an already-correct extension-based result.
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    filesystem_content_path(&archive_path.to_string_lossy()),
                    None,
                    Some("/cores/some_other_core_libretro.so"),
                    Some("Some Other Core"),
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let game = archive(1, "game.zip", &archive_path, Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(outcome.disposition, CoreMatchDisposition::ExactCore);
        assert_eq!(outcome.matched_core_stem.as_deref(), Some("snes9x"));
        assert_eq!(
            outcome.selected_core_source,
            Some(CoreSelectionSource::ExtensionMatch)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ambiguous_catalogue_match_remains_ambiguous_and_never_upgrades_a_core() {
        let root = temp_root("playlist-ambiguous-catalogue");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        // Two different archives (different directories) share the same
        // basename - the playlist entry's basename-fallback match cannot
        // safely pick one.
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![
                found_core("mame2003_plus", &["zip"]),
                found_core("fbneo", &["zip"]),
            ],
            vec![playlist(
                "Arcade",
                vec![playlist_entry(
                    0,
                    filesystem_content_path("/some/unrelated/directory/sfa3.zip"),
                    Some("Street Fighter Alpha 3"),
                    Some("/cores/fbneo_libretro.so"),
                    Some("fbneo"),
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let first = archive(1, "sfa3.zip", &root.join("a/sfa3.zip"), Some("Arcade"));
        let second = archive(2, "sfa3.zip", &root.join("b/sfa3.zip"), Some("Arcade"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![first, second]);

        for entry in &plan.entries {
            let outcome = &entry.profile_outcomes[0];
            assert_eq!(outcome.playlist_evidence.len(), 1);
            let evidence = &outcome.playlist_evidence[0];
            assert_eq!(evidence.confidence, PlaylistMatchConfidence::Ambiguous);
            assert_eq!(evidence.matched_archive_id, None);
            assert_eq!(evidence.ambiguous_archive_ids, vec![1, 2]);
            // Ambiguous evidence must never be used to upgrade a core
            // selection, even though the entry names a real installed core.
            assert_eq!(outcome.disposition, CoreMatchDisposition::AmbiguousCore);
            assert_eq!(outcome.selected_core_source, None);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_basenames_across_source_folders_do_not_produce_a_silent_pick() {
        let root = temp_root("playlist-duplicate-basenames");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    filesystem_content_path("/nonexistent/game.zip"),
                    Some("Game"),
                    None,
                    None,
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let first = archive(1, "game.zip", &root.join("a/game.zip"), Some("SNES"));
        let second = archive(2, "game.zip", &root.join("b/game.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![first, second]);

        for entry in &plan.entries {
            let evidence = &entry.profile_outcomes[0].playlist_evidence[0];
            assert_eq!(evidence.confidence, PlaylistMatchConfidence::Ambiguous);
            assert_eq!(evidence.ambiguous_archive_ids, vec![1, 2]);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn label_only_result_remains_weak_even_with_no_path_evidence() {
        let root = temp_root("playlist-label-only");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let archive_path = root.join("game.zip");
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    no_path(),
                    Some("ChronoTrigger"),
                    None,
                    None,
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        // The `archive()` fixture derives `normalized_name` by simply
        // lowercasing `relative` - unlike the real ArchiveFS scanner's own
        // normalization, it does not strip spaces/punctuation. Using a
        // single-word name here keeps this test isolated to the
        // label-matching *tier* itself rather than exercising two
        // different normalization functions against each other.
        let game = archive(1, "ChronoTrigger", &archive_path, Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(outcome.playlist_evidence.len(), 1);
        let evidence = &outcome.playlist_evidence[0];
        assert_eq!(evidence.confidence, PlaylistMatchConfidence::Weak);
        assert_eq!(evidence.evidence_basis, "label_or_filename_only");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn database_name_strengthens_but_is_not_treated_as_proof() {
        let root = temp_root("playlist-database-strengthens");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        // Basename-only match, but the catalogue archive has a known
        // platform (the only "corroborating evidence" honestly available
        // without a db_name-to-platform mapping table - see the module
        // doc comment).
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    filesystem_content_path("/nonexistent/game.zip"),
                    Some("Game"),
                    None,
                    None,
                    PlaylistCrc::Missing,
                    Some("Nintendo - Super Nintendo Entertainment System.lpl"),
                )],
            )],
        );
        let game = archive(1, "game.zip", &root.join("game.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let evidence = &plan.entries[0].profile_outcomes[0].playlist_evidence[0];
        assert_eq!(evidence.confidence, PlaylistMatchConfidence::Strong);
        assert_eq!(
            evidence.database_name.as_deref(),
            Some("Nintendo - Super Nintendo Entertainment System.lpl")
        );
        // The database name is exposed as evidence, but confidence is
        // still capped at Strong, never Exact - it never proves identity
        // by itself.
        assert_ne!(evidence.confidence, PlaylistMatchConfidence::Exact);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn inner_crc_is_never_compared_to_outer_archive_identity() {
        // ArchiveFS has no per-archive checksum field at all
        // (`PersistedArchive` carries none), so a playlist entry's CRC -
        // which may describe an *inner* file inside an archive - can
        // never be compared against anything ArchiveFS has for the outer
        // archive. This test locks in that the verified-CRC tier
        // structurally cannot fire: matching never even inspects `crc`,
        // only `content_path`/`label`.
        let root = temp_root("playlist-inner-crc");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let archive_path = root.join("game.zip");
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    filesystem_content_path(&archive_path.to_string_lossy()),
                    None,
                    None,
                    None,
                    PlaylistCrc::Verified {
                        value: "DEADBEEF".to_string(),
                    },
                    None,
                )],
            )],
        );
        let game = archive(1, "game.zip", &archive_path, Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let evidence = &plan.entries[0].profile_outcomes[0].playlist_evidence[0];
        // The match came from the exact content path, not the CRC - the
        // CRC is exposed only as informational evidence alongside it.
        assert_eq!(evidence.evidence_basis, "exact_content_path");
        assert_eq!(
            evidence.crc,
            PlaylistCrc::Verified {
                value: "DEADBEEF".to_string()
            }
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_crc_never_produces_a_false_match() {
        let root = temp_root("playlist-malformed-crc-no-match");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    no_path(),
                    None,
                    None,
                    None,
                    PlaylistCrc::Malformed {
                        raw: "not-a-crc".to_string(),
                    },
                    None,
                )],
            )],
        );
        let game = archive(1, "game.zip", &root.join("game.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        // No content path, no label, and CRC is never used for matching -
        // this entry cannot name any archive at all.
        assert!(
            plan.entries[0].profile_outcomes[0]
                .playlist_evidence
                .is_empty()
        );
        let _ = fs::remove_dir_all(root);
    }

    // ---- Patch-preview regression: playlists must not change PCSX2-free behavior ----

    #[test]
    fn preview_output_is_unchanged_when_no_playlists_are_available() {
        let root = temp_root("playlist-regression-none");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
        );
        let game = archive(1, "game.zip", &root.join("game.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let outcome = &plan.entries[0].profile_outcomes[0];
        assert_eq!(outcome.disposition, CoreMatchDisposition::ExactCore);
        assert_eq!(outcome.matched_core_stem.as_deref(), Some("snes9x"));
        assert_eq!(
            outcome.selected_core_source,
            Some(CoreSelectionSource::ExtensionMatch)
        );
        assert!(outcome.playlist_evidence.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn playlist_evidence_cannot_create_a_destination_when_content_matching_is_ambiguous() {
        let root = temp_root("playlist-no-destination-when-ambiguous");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![
                found_core("mame2003_plus", &["zip"]),
                found_core("fbneo", &["zip"]),
            ],
            vec![playlist(
                "Arcade",
                vec![playlist_entry(
                    0,
                    filesystem_content_path("/some/unrelated/directory/sfa3.zip"),
                    Some("Street Fighter Alpha 3"),
                    Some("/cores/fbneo_libretro.so"),
                    Some("fbneo"),
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let first = archive(1, "sfa3.zip", &root.join("a/sfa3.zip"), Some("Arcade"));
        let second = archive(2, "sfa3.zip", &root.join("b/sfa3.zip"), Some("Arcade"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![first, second]);

        for entry in &plan.entries {
            let outcome = &entry.profile_outcomes[0];
            assert_eq!(
                outcome.per_game_cheat_file.kind,
                DestinationKind::Unsupported
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plan_ordering_and_id_remain_deterministic_with_playlist_evidence() {
        let root = temp_root("playlist-plan-determinism");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let build = || {
            let report = one_profile_report_with_playlists(
                resolved_cheats_finding(&cheats_dir, true),
                vec![
                    found_core("mame2003_plus", &["zip"]),
                    found_core("fbneo", &["zip"]),
                ],
                vec![playlist(
                    "Arcade",
                    vec![playlist_entry(
                        0,
                        filesystem_content_path(&root.join("sfa3.zip").to_string_lossy()),
                        Some("Street Fighter Alpha 3"),
                        Some("/cores/fbneo_libretro.so"),
                        Some("fbneo"),
                        PlaylistCrc::Missing,
                        None,
                    )],
                )],
            );
            let game = archive(1, "sfa3.zip", &root.join("sfa3.zip"), Some("Arcade"));
            build_retroarch_advisory_plan(&filesystem, report, vec![game])
        };

        let first = build();
        let second = build();
        assert_eq!(first.plan_id, second.plan_id);
        assert!(!first.plan_id.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn json_format_version_is_unchanged_by_additive_playlist_fields() {
        // Locks in the explicit format-version decision: playlist
        // evidence fields are purely additive, matching this repository's
        // documented JSON policy ("New fields may be added" -
        // docs/json-api.md's Stability Guarantees) - so `format_version`
        // stays 1, not bumped.
        let root = temp_root("playlist-format-version");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    filesystem_content_path(&root.join("game.zip").to_string_lossy()),
                    None,
                    None,
                    None,
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let game = archive(1, "game.zip", &root.join("game.zip"), Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);
        assert_eq!(plan.format_version, RETROARCH_ADVISORY_FORMAT_VERSION);
        assert_eq!(plan.format_version, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn playlist_evidence_json_key_set_is_stable() {
        let root = temp_root("playlist-evidence-json-keys");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let archive_path = root.join("game.zip");
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    filesystem_content_path(&archive_path.to_string_lossy()),
                    Some("Game"),
                    Some("/cores/snes9x_libretro.so"),
                    Some("snes9x"),
                    PlaylistCrc::Verified {
                        value: "DEADBEEF".to_string(),
                    },
                    Some("Nintendo - Super Nintendo Entertainment System.lpl"),
                )],
            )],
        );
        let game = archive(1, "game.zip", &archive_path, Some("SNES"));

        let plan = build_retroarch_advisory_plan(&filesystem, report, vec![game]);
        let json = serde_json::to_value(&plan).unwrap();
        let evidence = &json["entries"][0]["profile_outcomes"][0]["playlist_evidence"][0];

        let mut keys: Vec<String> = evidence.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "ambiguous_archive_ids",
                "confidence",
                "content_path_kind",
                "core_association",
                "crc",
                "database_name",
                "entry_index",
                "entry_label",
                "evidence_basis",
                "matched_archive_id",
                "playlist_file",
                "playlist_name",
            ]
        );
        assert_eq!(evidence["confidence"], "exact");
        assert_eq!(evidence["core_association"]["type"], "linked_by_core_path");
        let _ = fs::remove_dir_all(root);
    }

    // ---- Safety: no writes, no mutation of anything playlist-related ----

    #[test]
    fn playlist_matching_makes_no_filesystem_writes() {
        let root = temp_root("playlist-matching-no-writes");
        let cheats_dir = root.join("cheats");
        fs::create_dir_all(&cheats_dir).unwrap();
        let filesystem = HostReadOnlyFilesystem;
        let archive_path = root.join("game.zip");
        fs::write(&archive_path, b"content").unwrap();
        let report = one_profile_report_with_playlists(
            resolved_cheats_finding(&cheats_dir, true),
            vec![found_core("snes9x", &["zip"])],
            vec![playlist(
                "Test",
                vec![playlist_entry(
                    0,
                    filesystem_content_path(&archive_path.to_string_lossy()),
                    Some("Game"),
                    None,
                    None,
                    PlaylistCrc::Missing,
                    None,
                )],
            )],
        );
        let game = archive(1, "game.zip", &archive_path, Some("SNES"));
        let before = tree_entries(&root);

        let _ = build_retroarch_advisory_plan(&filesystem, report, vec![game]);

        let after = tree_entries(&root);
        assert_eq!(before, after);
    }

    fn tree_entries(root: &Path) -> Vec<PathBuf> {
        fn visit(root: &Path, current: &Path, entries: &mut Vec<PathBuf>) {
            let mut children = fs::read_dir(current)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                entries.push(child.strip_prefix(root).unwrap().to_path_buf());
                if child.is_dir() {
                    visit(root, &child, entries);
                }
            }
        }
        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries
    }
}

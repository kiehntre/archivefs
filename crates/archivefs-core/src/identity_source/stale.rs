//! Explaining a stale population.
//!
//! A real import produced 26,178 matched records and 10,081 stale ones. A single
//! number that large is not actionable: "10,081 records point at files that are
//! missing" could mean a broken mapping, a dead mount, or a library that has
//! genuinely moved on. Grouping it answers which.
//!
//! What that grouping found, on the catalogue this module was written against:
//!
//! - 79% were absent *and* flagged `missing_from_fs` by RomM itself - stale rows
//!   in RomM's own database, nothing to do with EmuWiz;
//! - 18% were orphaned symlinks whose targets had gone, the mount being up and the
//!   files simply no longer there;
//! - 2% were folder-based games that are present as directories, which the
//!   matching evidence had been calling "does not exist";
//! - 0.6% were a collection absent from this machine altogether.
//!
//! # Bounded by construction
//!
//! Every group list is truncated to [`MAX_GROUPS`] and every example list to a
//! caller's limit, with the remainder stated as a count. A diagnostic that prints
//! 10,081 paths is a diagnostic nobody reads.
//!
//! # Reads metadata, never contents
//!
//! The only filesystem access is the presence probe, which the caller supplies -
//! so a test can build a whole summary without a filesystem, and so this module
//! cannot start hashing or open a file.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use super::cache::IdentityCache;
use super::matching::LocalPresence;
use super::model::{ExternalIdentityRecord, ExternalVerification};

/// The most groups reported per dimension. Beyond this the tail is a count.
pub const MAX_GROUPS: usize = 12;

/// The most examples one group may carry, whatever a caller asks for.
pub const MAX_EXAMPLES: usize = 20;
pub const DEFAULT_EXAMPLES: usize = 3;

/// One counted group, with the share it represents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaleGroup {
    pub key: String,
    pub count: usize,
    /// How many of these RomM itself reported as missing from its own filesystem.
    /// A high number means RomM knows, and the record is stale at the source.
    pub romm_reports_missing: usize,
}

/// One example record, reduced to what a diagnostic needs. Never the whole record:
/// a summary that embeds 73 provider fields per example is a catalogue dump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaleExample {
    pub romm_game_id: String,
    pub romm_path: String,
    pub archivefs_path: Option<String>,
    pub platform: Option<String>,
    pub file_size_bytes: Option<u64>,
    pub romm_reports_missing: bool,
    pub related_files: usize,
}

/// One existence outcome, with its own examples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaleReason {
    pub code: &'static str,
    pub label: &'static str,
    pub count: usize,
    pub romm_reports_missing: usize,
    pub examples: Vec<StaleExample>,
}

/// The whole picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaleSummary {
    pub total_in_cache: usize,
    pub stale: usize,
    /// Stale records RomM itself flags as missing from its own filesystem.
    pub romm_reports_missing: usize,
    /// Stale records whose local path is present as a directory. These are not
    /// missing at all - the game is a folder.
    pub present_as_directory: usize,
    /// Stale records whose local path is a symlink that no longer resolves.
    pub dangling_symlinks: usize,
    /// Stale records with no mapping to a local path at all.
    pub unmapped: usize,
    /// Genuinely multi-file records: RomM lists more than one file. A record with
    /// exactly one entry is an ordinary single-file game listing itself, which is
    /// why this is not simply "has a file list".
    pub multi_file: usize,
    pub by_reason: Vec<StaleReason>,
    pub by_platform: Vec<StaleGroup>,
    pub by_romm_prefix: Vec<StaleGroup>,
    pub by_local_prefix: Vec<StaleGroup>,
    pub by_extension: Vec<StaleGroup>,
    /// Mappings that produced stale records, with how many each accounts for.
    pub by_mapping: Vec<StaleGroup>,
    /// Whether this population looks like ordinary library drift rather than a
    /// mapping or matching fault. Carried in the output, not just available as a
    /// method, so a typed consumer gets the conclusion and not only the counts.
    pub looks_like_library_drift: bool,
    /// Groups omitted from each list above, so a truncated list says so.
    pub platforms_not_listed: usize,
    pub romm_prefixes_not_listed: usize,
    pub local_prefixes_not_listed: usize,
    pub extensions_not_listed: usize,
}

impl StaleSummary {
    /// Builds the summary. `presence_for` is the only thing that touches a
    /// filesystem, and it is the caller's to supply.
    pub fn build(
        cache: &IdentityCache,
        mappings: &[(String, String)],
        example_limit: usize,
        presence_for: impl Fn(&Path) -> LocalPresence,
    ) -> Self {
        let example_limit = example_limit.clamp(1, MAX_EXAMPLES);
        let stale: Vec<&ExternalIdentityRecord> = cache
            .records
            .iter()
            .filter(|record| record.verification == ExternalVerification::Stale)
            .collect();

        // One probe per record, and its result reused for every grouping below.
        let observed: Vec<(&ExternalIdentityRecord, LocalPresence, bool)> = stale
            .iter()
            .map(|record| {
                let presence = match record.archivefs_path.as_deref() {
                    Some(path) => presence_for(path),
                    None => LocalPresence::Absent,
                };
                (*record, presence, reports_missing(record))
            })
            .collect();

        let mut by_reason: HashMap<&'static str, (LocalPresence, usize, usize, Vec<StaleExample>)> =
            HashMap::new();
        for (record, presence, flagged) in &observed {
            let entry = by_reason
                .entry(presence.code())
                .or_insert_with(|| (*presence, 0, 0, Vec::new()));
            entry.1 += 1;
            if *flagged {
                entry.2 += 1;
            }
            if entry.3.len() < example_limit {
                entry.3.push(example_of(record, *flagged));
            }
        }
        let mut reasons: Vec<StaleReason> = by_reason
            .into_values()
            .map(|(presence, count, flagged, examples)| StaleReason {
                code: presence.code(),
                label: presence.label(),
                count,
                romm_reports_missing: flagged,
                examples,
            })
            .collect();
        // Largest first, then by code, so two runs over the same cache agree.
        reasons.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.code.cmp(right.code))
        });

        let (by_platform, platforms_not_listed) = group(&observed, |record| {
            Some(
                record
                    .platform_candidate
                    .clone()
                    .unwrap_or_else(|| "(unrecognised platform)".to_string()),
            )
        });
        let (by_romm_prefix, romm_prefixes_not_listed) =
            group(&observed, |record| Some(prefix_of(&record.provider_path)));
        let (by_local_prefix, local_prefixes_not_listed) = group(&observed, |record| {
            record.archivefs_path.as_deref().map(|path| {
                path.parent()
                    .map(|parent| parent.display().to_string())
                    .unwrap_or_else(|| path.display().to_string())
            })
        });
        let (by_extension, extensions_not_listed) = group(&observed, |record| {
            Some(extension_of(&record.provider_path))
        });
        // Mappings are a configured list, so all of them are reported rather than
        // a truncated top few.
        let (by_mapping, _) = group_all(&observed, |record| {
            mappings
                .iter()
                .find(|(_, destination)| {
                    record
                        .archivefs_path
                        .as_deref()
                        .is_some_and(|path| path.starts_with(destination))
                })
                .map(|(source, destination)| format!("{source} -> {destination}"))
                .or_else(|| Some("(no mapping)".to_string()))
        });

        let romm_reports_missing = observed.iter().filter(|(_, _, flagged)| *flagged).count();
        let dangling_symlinks = count_presence(&observed, LocalPresence::DanglingSymlink);
        Self {
            total_in_cache: cache.records.len(),
            stale: stale.len(),
            looks_like_library_drift: is_library_drift(
                stale.len(),
                romm_reports_missing,
                dangling_symlinks,
            ),
            romm_reports_missing,
            present_as_directory: count_presence(&observed, LocalPresence::Directory),
            dangling_symlinks,
            unmapped: stale
                .iter()
                .filter(|record| record.archivefs_path.is_none())
                .count(),
            multi_file: stale
                .iter()
                .filter(|record| record.related_files.len() >= 2)
                .count(),
            by_reason: reasons,
            by_platform,
            by_romm_prefix,
            by_local_prefix,
            by_extension,
            by_mapping,
            platforms_not_listed,
            romm_prefixes_not_listed,
            local_prefixes_not_listed,
            extensions_not_listed,
        }
    }

    /// Whether the population looks like ordinary library drift rather than a
    /// mapping or matching fault.
    ///
    /// True when nearly everything is either flagged missing by RomM or is a link
    /// whose target has gone - both of which are facts about the library, not about
    /// how paths were translated.
    pub fn looks_like_drift(&self) -> bool {
        self.looks_like_library_drift
    }
}

fn count_presence(
    observed: &[(&ExternalIdentityRecord, LocalPresence, bool)],
    wanted: LocalPresence,
) -> usize {
    observed
        .iter()
        .filter(|(_, presence, _)| *presence == wanted)
        .count()
}

/// Whether RomM's own answer was that the file is missing on its side.
fn reports_missing(record: &ExternalIdentityRecord) -> bool {
    record
        .evidence
        .iter()
        .any(|line| line.contains("missing from its own filesystem"))
}

fn example_of(record: &ExternalIdentityRecord, flagged: bool) -> StaleExample {
    StaleExample {
        romm_game_id: record.provider_game_id.clone(),
        romm_path: record.provider_path.clone(),
        archivefs_path: record
            .archivefs_path
            .as_deref()
            .map(|path| path.display().to_string()),
        platform: record.platform_candidate.clone(),
        file_size_bytes: record.file_size_bytes,
        romm_reports_missing: flagged,
        related_files: record.related_files.len(),
    }
}

/// The first two components of a provider path, which is where a platform folder
/// sits in every layout seen so far.
fn prefix_of(path: &str) -> String {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').take(2).collect();
    if parts.is_empty() {
        "(empty)".to_string()
    } else {
        parts.join("/")
    }
}

fn extension_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        // A dot at the start is a hidden file, not an extension.
        Some((stem, extension)) if !stem.is_empty() && !extension.contains(' ') => {
            format!(".{}", extension.to_ascii_lowercase())
        }
        _ => "(no extension)".to_string(),
    }
}

/// Counts by a key, keeping the largest [`MAX_GROUPS`] and reporting how many were
/// left out.
fn group(
    observed: &[(&ExternalIdentityRecord, LocalPresence, bool)],
    key_of: impl Fn(&ExternalIdentityRecord) -> Option<String>,
) -> (Vec<StaleGroup>, usize) {
    let (mut all, _) = group_all(observed, key_of);
    let omitted = all.len().saturating_sub(MAX_GROUPS);
    all.truncate(MAX_GROUPS);
    (all, omitted)
}

fn group_all(
    observed: &[(&ExternalIdentityRecord, LocalPresence, bool)],
    key_of: impl Fn(&ExternalIdentityRecord) -> Option<String>,
) -> (Vec<StaleGroup>, usize) {
    let mut counts: HashMap<String, (usize, usize)> = HashMap::new();
    for (record, _, flagged) in observed {
        if let Some(key) = key_of(record) {
            let entry = counts.entry(key).or_insert((0, 0));
            entry.0 += 1;
            if *flagged {
                entry.1 += 1;
            }
        }
    }
    let mut groups: Vec<StaleGroup> = counts
        .into_iter()
        .map(|(key, (count, romm_reports_missing))| StaleGroup {
            key,
            count,
            romm_reports_missing,
        })
        .collect();
    // Largest first, ties broken on the key, so the output is deterministic.
    groups.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.key.cmp(&right.key))
    });
    (groups, 0)
}

/// Whether a stale population is explained by the library having moved on.
///
/// "Explained" means RomM itself reports the file missing, or the local path is a
/// link whose target has gone. Both are facts about the files; neither says
/// anything about how paths were translated. At nine tenths or more, looking for a
/// mapping fault is looking in the wrong place.
fn is_library_drift(stale: usize, romm_reports_missing: usize, dangling_symlinks: usize) -> bool {
    if stale == 0 {
        return true;
    }
    let explained = romm_reports_missing.saturating_add(dangling_symlinks);
    explained.saturating_mul(10) >= stale.saturating_mul(9)
}

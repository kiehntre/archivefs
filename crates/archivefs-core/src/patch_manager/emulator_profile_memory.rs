//! Remembered emulator profile persistence and selection rules for the
//! beginner Cheats & Mods workflow (currently Dolphin and Xenia).
//!
//! This is deliberately a separate on-disk file
//! (`~/.config/archivefs/emulator_profiles.toml`) rather than a new field
//! on `Config`/`config.toml`: `Config`'s save path
//! (`save_source_folder_configs_to`) unconditionally rewrites the whole
//! file from an in-memory `Vec<SourceFolderConfig>`, so bolting emulator
//! profile memory onto it would require threading a second collection
//! through every source-management call site just to avoid silently
//! dropping remembered profiles on the next source edit. A dedicated file
//! avoids that coupling while still living in the same config directory
//! and reusing the same atomic-rename write primitive
//! (`crate::atomic_write_text`) as the rest of ArchiveFS's configuration.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{ArchiveFsError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RememberedEmulatorProfile {
    /// The `CheatEmulatorAdapter` this profile is remembered for, as a
    /// stable lowercase key (e.g. `"dolphin"`, `"xenia"`) - not the GUI's
    /// display string, so renaming a label in the UI never invalidates
    /// saved memory.
    pub adapter: String,
    pub profile_id: String,
    pub root: PathBuf,
}

pub fn default_emulator_profile_memory_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("archivefs")
        .join("emulator_profiles.toml"))
}

pub fn load_remembered_emulator_profiles_default() -> Result<Vec<RememberedEmulatorProfile>> {
    load_remembered_emulator_profiles_from(default_emulator_profile_memory_path()?)
}

/// A missing file is treated as "nothing remembered yet", not an error -
/// this file is only ever created the first time a profile is remembered.
pub fn load_remembered_emulator_profiles_from(
    path: impl AsRef<Path>,
) -> Result<Vec<RememberedEmulatorProfile>> {
    let path = path.as_ref();
    match fs::read_to_string(path) {
        Ok(contents) => parse_remembered_profiles(&contents),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(ArchiveFsError::io(path.to_path_buf(), source)),
    }
}

pub fn remembered_profile_for<'a>(
    profiles: &'a [RememberedEmulatorProfile],
    adapter: &str,
) -> Option<&'a RememberedEmulatorProfile> {
    profiles.iter().find(|profile| profile.adapter == adapter)
}

/// Upserts (by `adapter`) the remembered profile and atomically rewrites
/// the file. Every other adapter's remembered profile is preserved
/// unchanged.
pub fn remember_emulator_profile_default(
    adapter: &str,
    profile_id: &str,
    root: &Path,
) -> Result<()> {
    remember_emulator_profile_to(
        default_emulator_profile_memory_path()?,
        adapter,
        profile_id,
        root,
    )
}

pub fn remember_emulator_profile_to(
    path: impl AsRef<Path>,
    adapter: &str,
    profile_id: &str,
    root: &Path,
) -> Result<()> {
    let path = path.as_ref();
    let Some(root_str) = root.to_str() else {
        return Err(ArchiveFsError::Config(format!(
            "profile root cannot be stored losslessly in the UTF-8 configuration file: {}",
            root.display()
        )));
    };
    let mut profiles = load_remembered_emulator_profiles_from(path)?;
    profiles.retain(|profile| profile.adapter != adapter);
    profiles.push(RememberedEmulatorProfile {
        adapter: adapter.to_string(),
        profile_id: profile_id.to_string(),
        root: PathBuf::from(root_str),
    });
    let contents = render_remembered_profiles(&profiles);
    crate::atomic_write_text(path, &contents)
}

/// Forgets the remembered profile for `adapter`, if any - used when the
/// remembered profile becomes invalid and the user must choose again.
pub fn forget_emulator_profile_default(adapter: &str) -> Result<()> {
    forget_emulator_profile_at(default_emulator_profile_memory_path()?, adapter)
}

pub fn forget_emulator_profile_at(path: impl AsRef<Path>, adapter: &str) -> Result<()> {
    let path = path.as_ref();
    let mut profiles = load_remembered_emulator_profiles_from(path)?;
    let before = profiles.len();
    profiles.retain(|profile| profile.adapter != adapter);
    if profiles.len() == before {
        return Ok(());
    }
    let contents = render_remembered_profiles(&profiles);
    crate::atomic_write_text(path, &contents)
}

fn parse_remembered_profiles(contents: &str) -> Result<Vec<RememberedEmulatorProfile>> {
    let mut profiles = Vec::new();
    let mut adapter: Option<String> = None;
    let mut profile_id: Option<String> = None;
    let mut root: Option<PathBuf> = None;
    let mut block_line = 0usize;

    let finish = |adapter: Option<String>,
                  profile_id: Option<String>,
                  root: Option<PathBuf>,
                  block_line: usize|
     -> Result<RememberedEmulatorProfile> {
        Ok(RememberedEmulatorProfile {
            adapter: adapter.ok_or_else(|| {
                ArchiveFsError::Config(format!(
                    "the [[emulator_profile]] block starting at line {block_line} has no adapter"
                ))
            })?,
            profile_id: profile_id.ok_or_else(|| {
                ArchiveFsError::Config(format!(
                    "the [[emulator_profile]] block starting at line {block_line} has no profile_id"
                ))
            })?,
            root: root.ok_or_else(|| {
                ArchiveFsError::Config(format!(
                    "the [[emulator_profile]] block starting at line {block_line} has no root"
                ))
            })?,
        })
    };

    let mut in_block = false;
    for (index, raw_line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[emulator_profile]]" {
            if in_block {
                profiles.push(finish(
                    adapter.take(),
                    profile_id.take(),
                    root.take(),
                    block_line,
                )?);
            }
            in_block = true;
            block_line = line_number;
            continue;
        }
        if !in_block {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} is not a key/value pair"
            )));
        };
        let value = unquote(value.trim(), line_number)?;
        match key.trim() {
            "adapter" => adapter = Some(value),
            "profile_id" => profile_id = Some(value),
            "root" => root = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    if in_block {
        profiles.push(finish(adapter, profile_id, root, block_line)?);
    }
    Ok(profiles)
}

fn unquote(value: &str, line_number: usize) -> Result<String> {
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return Err(ArchiveFsError::Config(format!(
            "line {line_number} expected a quoted string, found '{value}'"
        )));
    }
    let inner = &value[1..value.len() - 1];
    Ok(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn render_remembered_profiles(profiles: &[RememberedEmulatorProfile]) -> String {
    let mut out = String::from("# ArchiveFS remembered emulator profiles\n");
    for profile in profiles {
        out.push('\n');
        out.push_str("[[emulator_profile]]\n");
        out.push_str(&format!("adapter = {}\n", quote(&profile.adapter)));
        out.push_str(&format!("profile_id = {}\n", quote(&profile.profile_id)));
        out.push_str(&format!(
            "root = {}\n",
            quote(&profile.root.display().to_string())
        ));
    }
    out
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A single discovered emulator profile as far as selection is concerned -
/// deliberately smaller than `DolphinProfile`/`XeniaProfile` so the
/// selection rules stay adapter-agnostic and independently testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulatorProfileCandidate {
    pub profile_id: String,
    pub root: PathBuf,
    pub eligible: bool,
    /// True for a profile ArchiveFS knows is a portable install (e.g. a
    /// caller-supplied explicit root distinct from the OS-standard
    /// configuration directory) - used only to break a tie between
    /// multiple valid profiles when nothing has been remembered or
    /// explicitly chosen yet.
    pub is_portable: bool,
    pub evidence_priority: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmulatorProfileSelectReason {
    Remembered,
    ExplicitChoice,
    OnlyValidProfile,
    PortablePreferred,
    StrongestEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmulatorProfileSelection {
    Auto {
        profile_id: String,
        reason: EmulatorProfileSelectReason,
    },
    /// More than one valid profile and no remembered/explicit/portable
    /// tie-break resolved it - the beginner UI must show one concise
    /// chooser. Carries every discovered candidate (including ineligible
    /// ones) so the caller can still render diagnostics under Details.
    NeedsChoice {
        candidates: Vec<EmulatorProfileCandidate>,
    },
    /// Either nothing was discovered, or a remembered profile no longer
    /// matches anything discovered - the beginner UI must show a plain
    /// setup-needed state and ask the user to choose again, never fall
    /// back to a silent automatic guess in this case.
    SetupNeeded,
}

/// Pure selection rules shared by Dolphin and Xenia. `remembered` and
/// `session_explicit` are both profile ids; `session_explicit` is an
/// in-memory choice already made this session (e.g. via the chooser
/// dialog) that has not necessarily been persisted yet.
pub fn select_emulator_profile(
    discovered: &[EmulatorProfileCandidate],
    remembered: Option<&str>,
    session_explicit: Option<&str>,
) -> EmulatorProfileSelection {
    if let Some(remembered_id) = remembered {
        return match discovered.iter().find(|c| c.profile_id == remembered_id) {
            Some(candidate) if candidate.eligible => EmulatorProfileSelection::Auto {
                profile_id: candidate.profile_id.clone(),
                reason: EmulatorProfileSelectReason::Remembered,
            },
            _ => EmulatorProfileSelection::SetupNeeded,
        };
    }

    if let Some(explicit_id) = session_explicit
        && let Some(candidate) = discovered
            .iter()
            .find(|c| c.profile_id == explicit_id && c.eligible)
    {
        return EmulatorProfileSelection::Auto {
            profile_id: candidate.profile_id.clone(),
            reason: EmulatorProfileSelectReason::ExplicitChoice,
        };
    }

    let eligible: Vec<&EmulatorProfileCandidate> =
        discovered.iter().filter(|c| c.eligible).collect();

    if eligible.is_empty() {
        return EmulatorProfileSelection::SetupNeeded;
    }
    if eligible.len() == 1 {
        return EmulatorProfileSelection::Auto {
            profile_id: eligible[0].profile_id.clone(),
            reason: EmulatorProfileSelectReason::OnlyValidProfile,
        };
    }

    let strongest = eligible
        .iter()
        .map(|candidate| candidate.evidence_priority)
        .max()
        .unwrap_or(0);
    let strongest_candidates: Vec<_> = eligible
        .iter()
        .filter(|candidate| candidate.evidence_priority == strongest)
        .collect();
    if strongest > 0 && strongest_candidates.len() == 1 {
        return EmulatorProfileSelection::Auto {
            profile_id: strongest_candidates[0].profile_id.clone(),
            reason: EmulatorProfileSelectReason::StrongestEvidence,
        };
    }
    if strongest > 0 && strongest_candidates.len() > 1 {
        return EmulatorProfileSelection::NeedsChoice {
            candidates: discovered.to_vec(),
        };
    }

    let portable: Vec<&&EmulatorProfileCandidate> =
        eligible.iter().filter(|c| c.is_portable).collect();
    if portable.len() == 1 {
        return EmulatorProfileSelection::Auto {
            profile_id: portable[0].profile_id.clone(),
            reason: EmulatorProfileSelectReason::PortablePreferred,
        };
    }

    EmulatorProfileSelection::NeedsChoice {
        candidates: discovered.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "archivefs-emulator-profile-memory-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn candidate(id: &str, eligible: bool, portable: bool) -> EmulatorProfileCandidate {
        EmulatorProfileCandidate {
            profile_id: id.to_string(),
            root: PathBuf::from(format!("/profiles/{id}")),
            eligible,
            is_portable: portable,
            evidence_priority: 0,
        }
    }

    #[test]
    fn unique_strongest_evidence_wins_but_equal_priority_requires_choice() {
        let mut native = candidate("native", true, false);
        native.evidence_priority = 100;
        let mut running = candidate("running", true, true);
        running.evidence_priority = 400;
        assert_eq!(
            select_emulator_profile(&[native.clone(), running.clone()], None, None),
            EmulatorProfileSelection::Auto {
                profile_id: "running".to_string(),
                reason: EmulatorProfileSelectReason::StrongestEvidence,
            }
        );
        native.evidence_priority = 400;
        assert!(matches!(
            select_emulator_profile(&[native, running], None, None),
            EmulatorProfileSelection::NeedsChoice { .. }
        ));
    }

    #[test]
    fn one_valid_profile_auto_selects() {
        let discovered = vec![candidate("only", true, false)];
        let selection = select_emulator_profile(&discovered, None, None);
        assert_eq!(
            selection,
            EmulatorProfileSelection::Auto {
                profile_id: "only".to_string(),
                reason: EmulatorProfileSelectReason::OnlyValidProfile,
            }
        );
    }

    #[test]
    fn remembered_profile_wins_over_discovery_order() {
        let discovered = vec![candidate("a", true, false), candidate("b", true, false)];
        let selection = select_emulator_profile(&discovered, Some("b"), None);
        assert_eq!(
            selection,
            EmulatorProfileSelection::Auto {
                profile_id: "b".to_string(),
                reason: EmulatorProfileSelectReason::Remembered,
            }
        );
    }

    #[test]
    fn explicit_choice_wins_over_portable_guess() {
        let discovered = vec![
            candidate("portable", true, true),
            candidate("standard", true, false),
        ];
        let selection = select_emulator_profile(&discovered, None, Some("standard"));
        assert_eq!(
            selection,
            EmulatorProfileSelection::Auto {
                profile_id: "standard".to_string(),
                reason: EmulatorProfileSelectReason::ExplicitChoice,
            }
        );
    }

    #[test]
    fn multiple_valid_profiles_require_one_choice() {
        let discovered = vec![candidate("a", true, false), candidate("b", true, false)];
        let selection = select_emulator_profile(&discovered, None, None);
        assert_eq!(
            selection,
            EmulatorProfileSelection::NeedsChoice {
                candidates: discovered,
            }
        );
    }

    #[test]
    fn invalid_remembered_profile_returns_to_setup_needed() {
        let discovered = vec![candidate("current", true, false)];
        let selection = select_emulator_profile(&discovered, Some("gone"), None);
        assert_eq!(selection, EmulatorProfileSelection::SetupNeeded);
    }

    #[test]
    fn remembered_profile_that_became_ineligible_returns_to_setup_needed() {
        let discovered = vec![candidate("now-invalid", false, false)];
        let selection = select_emulator_profile(&discovered, Some("now-invalid"), None);
        assert_eq!(selection, EmulatorProfileSelection::SetupNeeded);
    }

    #[test]
    fn no_discovered_profiles_is_setup_needed() {
        let selection = select_emulator_profile(&[], None, None);
        assert_eq!(selection, EmulatorProfileSelection::SetupNeeded);
    }

    #[test]
    fn single_portable_profile_is_preferred_among_multiple_valid() {
        let discovered = vec![
            candidate("standard-1", true, false),
            candidate("portable", true, true),
            candidate("standard-2", true, false),
        ];
        let selection = select_emulator_profile(&discovered, None, None);
        assert_eq!(
            selection,
            EmulatorProfileSelection::Auto {
                profile_id: "portable".to_string(),
                reason: EmulatorProfileSelectReason::PortablePreferred,
            }
        );
    }

    #[test]
    fn ineligible_candidates_are_ignored_for_auto_selection() {
        let discovered = vec![
            candidate("blocked", false, false),
            candidate("ok", true, false),
        ];
        let selection = select_emulator_profile(&discovered, None, None);
        assert_eq!(
            selection,
            EmulatorProfileSelection::Auto {
                profile_id: "ok".to_string(),
                reason: EmulatorProfileSelectReason::OnlyValidProfile,
            }
        );
    }

    #[test]
    fn remember_and_load_round_trips_through_a_real_file() {
        let dir = test_root("round-trip");
        let path = dir.join("emulator_profiles.toml");

        remember_emulator_profile_to(&path, "dolphin", "portable-1", Path::new("/portable/User"))
            .unwrap();
        remember_emulator_profile_to(&path, "xenia", "canary-1", Path::new("/xenia/root")).unwrap();

        let profiles = load_remembered_emulator_profiles_from(&path).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            remembered_profile_for(&profiles, "dolphin").unwrap().root,
            PathBuf::from("/portable/User")
        );
        assert_eq!(
            remembered_profile_for(&profiles, "xenia")
                .unwrap()
                .profile_id,
            "canary-1"
        );

        // Re-remembering the same adapter upserts, not duplicates.
        remember_emulator_profile_to(
            &path,
            "dolphin",
            "standard-1",
            Path::new("/std/dolphin-emu"),
        )
        .unwrap();
        let profiles = load_remembered_emulator_profiles_from(&path).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            remembered_profile_for(&profiles, "dolphin")
                .unwrap()
                .profile_id,
            "standard-1"
        );
    }

    #[test]
    fn missing_memory_file_is_treated_as_nothing_remembered() {
        let dir = test_root("missing-file");
        let path = dir.join("does-not-exist.toml");
        let profiles = load_remembered_emulator_profiles_from(&path).unwrap();
        assert!(profiles.is_empty());
    }

    #[test]
    fn forgetting_a_profile_removes_only_that_adapter() {
        let dir = test_root("forget");
        let path = dir.join("emulator_profiles.toml");
        remember_emulator_profile_to(&path, "dolphin", "p1", Path::new("/d")).unwrap();
        remember_emulator_profile_to(&path, "xenia", "p2", Path::new("/x")).unwrap();

        forget_emulator_profile_at(&path, "dolphin").unwrap();

        let profiles = load_remembered_emulator_profiles_from(&path).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].adapter, "xenia");
    }
}

//! Pure canonical-filename derivation for rename proposals.
//!
//! The proposed name is derived **only** from the authoritative matched DAT
//! entry's ROM name. Nothing here invents a name, and nothing here reads or
//! writes the filesystem. The rules are:
//!
//! - a name that is not a single path component (contains `/`, `\` or NUL), is
//!   `.`/`..`, or is empty is **Blocked** - never sanitised into a traversal;
//! - characters that are invalid on this filesystem are replaced with `_` and
//!   the replacement is explained (`sanitisation_notes`), deterministically;
//! - the source file's extension is preserved unless the DAT entry's own name
//!   genuinely uses the same extension; if the extensions differ the proposal
//!   is **Unsupported**, because renaming `game.zip` to `game.iso` would
//!   silently change what the file claims to be.

use super::model::ExtensionStatus;

/// The result of deriving a canonical basename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeriveOutcome {
    /// A safe, deterministic proposed basename.
    Ok(DerivedName),
    /// The name is unusable (path traversal, empty, reserved). No proposal.
    Blocked(String),
    /// The name names a different file kind than the source (container/member
    /// mismatch). Renaming would change what the file claims to be.
    Unsupported(String),
}

/// A successfully derived proposed basename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedName {
    pub proposed_basename: String,
    pub extension_status: ExtensionStatus,
    /// Deterministic, in the order characters were replaced.
    pub sanitisation_notes: Vec<String>,
}

/// Derives the proposed basename for `rom_name` (the matched DAT entry's ROM
/// name) against the source file's current basename.
pub fn derive_proposed_basename(rom_name: &str, source_basename: &str) -> DeriveOutcome {
    let trimmed = rom_name.trim();
    if trimmed.is_empty() {
        return DeriveOutcome::Blocked("the DAT entry name is empty".to_string());
    }
    if trimmed == "." || trimmed == ".." {
        return DeriveOutcome::Blocked(
            "the DAT entry name is a reserved path component ('.' or '..')".to_string(),
        );
    }
    if trimmed.contains(['/', '\\', '\0']) {
        return DeriveOutcome::Blocked(format!(
            "the DAT entry name {trimmed:?} contains a path separator; refusing to derive a \
             name that could traverse or escape its directory"
        ));
    }

    let source_extension = extension_of(source_basename);
    let rom_extension = extension_of(trimmed);
    let extension_status = match (source_extension, rom_extension) {
        (None, None) => ExtensionStatus::Preserved,
        (Some(source), Some(rom)) if source == rom => ExtensionStatus::Preserved,
        (Some(source), Some(rom)) => {
            return DeriveOutcome::Unsupported(format!(
                "the DAT entry names a different file kind ('{rom}' vs the file's '{source}'); \
                 renaming a container or an archive member is not supported"
            ));
        }
        (Some(source), None) => {
            return DeriveOutcome::Unsupported(format!(
                "the DAT entry names no extension while the file has '{source}'; refusing to \
                 strip the file's extension"
            ));
        }
        (None, Some(rom)) => {
            return DeriveOutcome::Unsupported(format!(
                "the DAT entry names extension '{rom}' while the file has none; refusing to \
                 add an extension the file does not have"
            ));
        }
    };

    let (proposed_basename, sanitisation_notes) = sanitise(trimmed);
    if proposed_basename.is_empty() || proposed_basename == "." || proposed_basename == ".." {
        return DeriveOutcome::Blocked(
            "sanitising the DAT entry name produced no usable name".to_string(),
        );
    }

    DeriveOutcome::Ok(DerivedName {
        proposed_basename,
        extension_status,
        sanitisation_notes,
    })
}

/// The lowercased extension of a basename, without the dot.
fn extension_of(name: &str) -> Option<String> {
    let stem = name.rsplit_once('.')?;
    // A leading dot (".gitignore", "..") is not an extension.
    if stem.0.is_empty() {
        return None;
    }
    Some(stem.1.to_ascii_lowercase())
}

/// Replaces characters that are invalid on this filesystem with `_`, and
/// records each distinct replacement. Deterministic: the same name always
/// produces the same output and the same notes.
fn sanitise(name: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(name.len());
    let mut notes: Vec<String> = Vec::new();
    for ch in name.chars() {
        if is_invalid_on_current_filesystem(ch) {
            let note =
                format!("replaced {ch:?} (not allowed in a filename on this filesystem) with '_'");
            if !notes.contains(&note) {
                notes.push(note);
            }
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    (out, notes)
}

/// Characters that cannot be part of a filename on the platform this build
/// runs on. Path separators and NUL are handled earlier as blocks, not here.
#[cfg(not(windows))]
fn is_invalid_on_current_filesystem(ch: char) -> bool {
    ch.is_control()
}

#[cfg(windows)]
fn is_invalid_on_current_filesystem(ch: char) -> bool {
    ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_name_is_derived_verbatim() {
        match derive_proposed_basename("Golden Axe (Europe) (Rev 2).hdf", "goldenaxe.hdf") {
            DeriveOutcome::Ok(derived) => {
                assert_eq!(derived.proposed_basename, "Golden Axe (Europe) (Rev 2).hdf");
                assert_eq!(derived.extension_status, ExtensionStatus::Preserved);
                assert!(derived.sanitisation_notes.is_empty());
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn path_separators_are_blocked_not_sanitised() {
        for evil in [
            "../escape.bin",
            "dir/escape.bin",
            "dir\\escape.bin",
            "a\0b.bin",
        ] {
            assert!(
                matches!(
                    derive_proposed_basename(evil, "x.bin"),
                    DeriveOutcome::Blocked(_)
                ),
                "{evil:?} must be blocked"
            );
        }
    }

    #[test]
    fn empty_and_reserved_names_are_blocked() {
        for bad in ["", "   ", ".", ".."] {
            assert!(
                matches!(
                    derive_proposed_basename(bad, "x.bin"),
                    DeriveOutcome::Blocked(_)
                ),
                "{bad:?} must be blocked"
            );
        }
    }

    #[test]
    fn differing_extensions_are_unsupported() {
        match derive_proposed_basename("Game.iso", "game.zip") {
            DeriveOutcome::Unsupported(reason) => {
                assert!(reason.contains("different file kind"), "{reason}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn same_extension_case_is_preserved() {
        match derive_proposed_basename("GAME.BIN", "game.bin") {
            DeriveOutcome::Ok(derived) => {
                assert_eq!(derived.extension_status, ExtensionStatus::Preserved);
                assert_eq!(derived.proposed_basename, "GAME.BIN");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn sanitisation_is_deterministic_and_explained() {
        let control = '\u{1}';
        let name = format!("Game{control}Special.bin");
        let first = derive_proposed_basename(&name, "old.bin");
        let second = derive_proposed_basename(&name, "old.bin");
        assert_eq!(first, second, "sanitisation must be deterministic");
        let DeriveOutcome::Ok(derived) = first else {
            panic!("expected Ok");
        };
        assert_eq!(derived.proposed_basename, "Game_Special.bin");
        assert!(
            derived
                .sanitisation_notes
                .iter()
                .any(|note| note.contains("replaced")),
            "{:?}",
            derived.sanitisation_notes
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_reserved_chars_are_sanitised() {
        match derive_proposed_basename("Game:Special.bin", "old.bin") {
            DeriveOutcome::Ok(derived) => {
                assert_eq!(derived.proposed_basename, "Game_Special.bin");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_names_keep_valid_chars() {
        match derive_proposed_basename("Game:Special.bin", "old.bin") {
            DeriveOutcome::Ok(derived) => {
                assert_eq!(derived.proposed_basename, "Game:Special.bin");
                assert!(derived.sanitisation_notes.is_empty());
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn no_extension_cases_are_honest() {
        assert!(matches!(
            derive_proposed_basename("rom", "file.bin"),
            DeriveOutcome::Unsupported(_)
        ));
        assert!(matches!(
            derive_proposed_basename("rom.bin", "rom"),
            DeriveOutcome::Unsupported(_)
        ));
        match derive_proposed_basename("rom", "file") {
            DeriveOutcome::Ok(derived) => {
                assert_eq!(derived.extension_status, ExtensionStatus::Preserved);
                assert_eq!(derived.proposed_basename, "rom");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}

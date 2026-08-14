//! The candidate description the DAT policy ranks.
//!
//! A *verified candidate* is one catalogue ROM whose hashes already matched a
//! local file exactly - the audit's `ExactMultipleCandidates` / `Probable` set.
//! [`DatCandidate`] is the small slice of that entry the policy needs to
//! decide between candidates, decoupled from the full
//! [`crate::dat::model::ParsedDat`] so the ranking
//! ([`super::evaluate::rank_candidates`]) is a pure function of the candidate
//! list and the effective policy.

use serde::Serialize;

use super::model::{LanguageId, RegionId};
use super::tags::{languages_of_name, regions_of_name, revision_of_name};
use crate::dat::model::{DatGameEntry, DatRomEntry};

/// One verified catalogue candidate, with the metadata the policy ranks on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatCandidate {
    /// Which registered DAT source contributed this candidate.
    pub source_id: String,
    /// The source's priority. Lower wins, only ever against another source
    /// that participates in the same platform (see
    /// [`super::evaluate::EffectiveDatPolicy::source_ordering`]).
    pub source_priority: u32,
    /// The catalogue entry's game name.
    pub game_name: String,
    /// The catalogue entry's ROM name within the game.
    pub rom_name: String,
    /// Regions named in the entry, in tag order, deduplicated.
    pub regions: Vec<RegionId>,
    /// Languages named in the entry, in tag order, deduplicated.
    pub languages: Vec<LanguageId>,
    /// The revision marker as an integer; 0 when the entry has none.
    pub revision: u32,
    /// Whether a `(Rev …)` marker was present at all.
    pub has_revision_marker: bool,
    /// The name of the entry's parent, when the catalogue declares one
    /// (`clone_of`).
    pub parent_name: Option<String>,
}

impl DatCandidate {
    /// A one-line label for display and deterministic tie-breaking: the game
    /// name, with the ROM name only when it differs.
    pub fn label(&self) -> String {
        if self.rom_name == self.game_name {
            self.game_name.clone()
        } else {
            format!("{} ({})", self.game_name, self.rom_name)
        }
    }

    /// Whether this entry declares itself a clone of another.
    pub fn is_clone(&self) -> bool {
        self.parent_name.is_some()
    }

    /// Whether this entry is the parent of `other` (or, symmetrically, the
    /// parent any entry's `clone_of` names).
    pub fn is_parent_of(&self, other: &DatCandidate) -> bool {
        other.parent_name.as_deref() == Some(self.game_name.as_str())
    }
}

/// Builds a candidate from one game entry's ROM.
///
/// Region, language and revision metadata is read from the names by the pure
/// tag extractors; the game and ROM names are both consulted so a marker that
/// lives on the ROM (revision suffixes on disc images are common) is not
/// missed. Parentage comes from the catalogue's own `clone_of` declaration
/// when the parser captured one.
pub fn candidate_for_rom(
    game: &DatGameEntry,
    rom: &DatRomEntry,
    source_id: &str,
    source_priority: u32,
) -> DatCandidate {
    let mut regions = regions_of_name(&game.name);
    for region in regions_of_name(&rom.name) {
        if !regions.contains(&region) {
            regions.push(region);
        }
    }
    let mut languages = languages_of_name(&game.name);
    for language in languages_of_name(&rom.name) {
        if !languages.contains(&language) {
            languages.push(language);
        }
    }
    let (revision, has_revision_marker) = {
        let (rom_revision, rom_marker) = revision_of_name(&rom.name);
        if rom_marker {
            (rom_revision, true)
        } else {
            revision_of_name(&game.name)
        }
    };
    DatCandidate {
        source_id: source_id.to_string(),
        source_priority,
        game_name: game.name.clone(),
        rom_name: rom.name.clone(),
        regions,
        languages,
        revision,
        has_revision_marker,
        parent_name: game.clone_of.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::model::{DatGameEntry, DatRomEntry};

    fn rom(name: &str) -> DatRomEntry {
        DatRomEntry {
            name: name.into(),
            size_bytes: None,
            crc32: None,
            md5: None,
            sha1: None,
            sha256: None,
            status: None,
            merge: None,
            date: None,
            loadflag: None,
        }
    }

    fn game(name: &str, clone_of: Option<&str>) -> DatGameEntry {
        DatGameEntry {
            name: name.into(),
            description: None,
            roms: vec![rom("rom.bin")],
            clone_of: clone_of.map(str::to_string),
            sample_of: None,
            board: None,
            rebuild_to: None,
            year: None,
            manufacturer: None,
            source_file: None,
            comment: None,
            original_metadata: Default::default(),
            content_classification: Default::default(),
            unsupported_structure: false,
        }
    }

    #[test]
    fn candidate_reads_region_language_and_revision_from_names() {
        let game = game("Sonic (Europe) (En,Fr,De) (Rev 1)", None);
        let candidate = candidate_for_rom(&game, &game.roms[0], "src", 100);
        assert_eq!(candidate.regions, vec![RegionId::Europe]);
        assert_eq!(
            candidate.languages,
            vec![LanguageId::En, LanguageId::Fr, LanguageId::De]
        );
        assert_eq!(candidate.revision, 1);
        assert!(candidate.has_revision_marker);
        assert!(!candidate.is_clone());
    }

    #[test]
    fn candidate_carries_the_declared_parent() {
        let clone_game = game("Clone (USA) (Rev 1)", Some("Parent"));
        let clone = candidate_for_rom(&clone_game, &clone_game.roms[0], "src", 100);
        assert_eq!(clone.parent_name.as_deref(), Some("Parent"));
        assert!(clone.is_clone());
        let parent_game = game("Parent", None);
        let parent = candidate_for_rom(&parent_game, &parent_game.roms[0], "src", 100);
        assert!(parent.is_parent_of(&clone));
        assert!(!clone.is_parent_of(&parent));
    }

    #[test]
    fn a_revision_marker_on_the_rom_name_is_not_missed() {
        let game = game("Disc", None);
        let mut entry = rom("disc (Rev 2).bin");
        entry.name = "disc (Rev 2).bin".into();
        let candidate = candidate_for_rom(&game, &entry, "src", 100);
        assert_eq!(candidate.revision, 2);
        assert!(candidate.has_revision_marker);
    }
}

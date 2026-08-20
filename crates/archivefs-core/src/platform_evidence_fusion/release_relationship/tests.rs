use super::*;
use crate::dat::model::{DatFormat, DatGameEntry, DatRomEntry, DatSource, ParsedDat};

fn dat_with_games(games: Vec<DatGameEntry>) -> ParsedDat {
    ParsedDat {
        source: DatSource {
            format: DatFormat::ClrMamePro,
            ecosystem: crate::dat::model::DatEcosystem::NoIntro,
            file_path: "fixture.dat".into(),
            name: Some("fixture".into()),
            description: None,
            version: None,
            author: None,
            homepage: None,
            clrmamepro_header: None,
            entry_count: games.len(),
            rom_count: games.iter().map(|g| g.roms.len()).sum(),
            parse_warnings: Vec::new(),
        },
        games,
    }
}

fn game(name: &str, clone_of: Option<&str>, rom_name: &str) -> DatGameEntry {
    DatGameEntry {
        name: name.to_string(),
        roms: vec![DatRomEntry {
            name: rom_name.to_string(),
            size_bytes: Some(1),
            crc32: Some("aaaaaaaa".to_string()),
            ..Default::default()
        }],
        clone_of: clone_of.map(str::to_string),
        ..Default::default()
    }
}

#[test]
fn game_with_no_clone_of_is_canonical() {
    let dat = dat_with_games(vec![game("Parent Game (USA)", None, "parent.bin")]);
    let index = DatIndex::build(&dat);
    let relationship = resolve_release_relationship(&index, "Parent Game (USA)");
    assert_eq!(
        relationship,
        ReleaseRelationship::Canonical {
            game_name: "Parent Game (USA)".to_string()
        }
    );
}

#[test]
fn game_with_clone_of_is_clone_of_its_declared_parent() {
    let dat = dat_with_games(vec![
        game("Parent Game (USA)", None, "parent.bin"),
        game(
            "Parent Game (USA) (Rev 1)",
            Some("Parent Game (USA)"),
            "parent_rev1.bin",
        ),
    ]);
    let index = DatIndex::build(&dat);
    let relationship = resolve_release_relationship(&index, "Parent Game (USA) (Rev 1)");
    assert_eq!(
        relationship,
        ReleaseRelationship::CloneOf {
            game_name: "Parent Game (USA) (Rev 1)".to_string(),
            parent: "Parent Game (USA)".to_string(),
        }
    );
}

#[test]
fn unknown_game_name_falls_back_to_canonical_not_a_fabricated_parent() {
    let dat = dat_with_games(vec![game("Some Game", None, "some.bin")]);
    let index = DatIndex::build(&dat);
    let relationship = resolve_release_relationship(&index, "Never Actually In The DAT");
    assert_eq!(
        relationship,
        ReleaseRelationship::Canonical {
            game_name: "Never Actually In The DAT".to_string()
        }
    );
}

#[test]
fn lineage_root_of_canonical_is_its_own_name() {
    let relationship = ReleaseRelationship::Canonical {
        game_name: "Game A".to_string(),
    };
    assert_eq!(relationship.lineage_root(), Some("Game A"));
}

#[test]
fn lineage_root_of_clone_of_is_the_parent() {
    let relationship = ReleaseRelationship::CloneOf {
        game_name: "Game A (Rev 1)".to_string(),
        parent: "Game A".to_string(),
    };
    assert_eq!(relationship.lineage_root(), Some("Game A"));
}

#[test]
fn unknown_has_no_lineage_root() {
    assert_eq!(ReleaseRelationship::Unknown.lineage_root(), None);
}

#[test]
fn parent_and_clone_share_lineage() {
    let parent = ReleaseRelationship::Canonical {
        game_name: "Game A".to_string(),
    };
    let clone = ReleaseRelationship::CloneOf {
        game_name: "Game A (Rev 1)".to_string(),
        parent: "Game A".to_string(),
    };
    assert!(same_lineage(&parent, &clone));
}

#[test]
fn two_clones_of_the_same_parent_share_lineage() {
    let rev1 = ReleaseRelationship::CloneOf {
        game_name: "Game A (Rev 1)".to_string(),
        parent: "Game A".to_string(),
    };
    let rev2 = ReleaseRelationship::CloneOf {
        game_name: "Game A (Rev 2)".to_string(),
        parent: "Game A".to_string(),
    };
    assert!(same_lineage(&rev1, &rev2));
}

#[test]
fn unrelated_games_never_share_lineage() {
    let a = ReleaseRelationship::Canonical {
        game_name: "Game A".to_string(),
    };
    let b = ReleaseRelationship::Canonical {
        game_name: "Game B".to_string(),
    };
    assert!(!same_lineage(&a, &b));
}

#[test]
fn two_unknowns_never_share_lineage() {
    assert!(!same_lineage(
        &ReleaseRelationship::Unknown,
        &ReleaseRelationship::Unknown
    ));
}

#[test]
fn game_name_accessor_returns_none_only_for_unknown() {
    assert_eq!(ReleaseRelationship::Unknown.game_name(), None);
    assert_eq!(
        ReleaseRelationship::Canonical {
            game_name: "X".to_string()
        }
        .game_name(),
        Some("X")
    );
}

#[test]
fn group_by_lineage_groups_parent_and_clones_together() {
    let parent = ReleaseRelationship::Canonical {
        game_name: "Game A".to_string(),
    };
    let clone = ReleaseRelationship::CloneOf {
        game_name: "Game A (Rev 1)".to_string(),
        parent: "Game A".to_string(),
    };
    let unrelated = ReleaseRelationship::Canonical {
        game_name: "Game B".to_string(),
    };
    let items = vec![("a", &parent), ("b", &clone), ("c", &unrelated)];
    let groups = group_by_lineage(items);
    assert_eq!(groups.len(), 2);
    let mut game_a_group = groups.get("Game A").unwrap().clone();
    game_a_group.sort();
    assert_eq!(game_a_group, vec!["a", "b"]);
}

#[test]
fn no_relationship_data_is_ever_derived_from_a_filename_string() {
    // Structural guarantee, not just a runtime test: this module must never
    // call anything that parses a title/filename for revision hints.
    let source = include_str!("../release_relationship.rs");
    for forbidden in ["Regex", "strip_prefix(\"Rev", ".find(\"Rev"] {
        assert!(!source.contains(forbidden));
    }
}

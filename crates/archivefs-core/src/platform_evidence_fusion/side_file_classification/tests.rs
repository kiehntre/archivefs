use super::*;
use std::path::Path;

#[test]
fn known_rom_extension_is_primary_content() {
    assert_eq!(
        classify_side_file(Path::new("game.n64")),
        SideFileRole::PrimaryContent
    );
    assert_eq!(
        classify_side_file(Path::new("game.gba")),
        SideFileRole::PrimaryContent
    );
    assert_eq!(
        classify_side_file(Path::new("game.gbc")),
        SideFileRole::PrimaryContent
    );
}

#[test]
fn cue_file_is_cue_sheet() {
    assert_eq!(
        classify_side_file(Path::new("Disc 1.cue")),
        SideFileRole::CueSheet
    );
}

#[test]
fn m3u_file_is_playlist() {
    assert_eq!(
        classify_side_file(Path::new("game.m3u")),
        SideFileRole::Playlist
    );
    assert_eq!(
        classify_side_file(Path::new("game.m3u8")),
        SideFileRole::Playlist
    );
}

#[test]
fn patch_extensions_are_patch_role() {
    for ext in ["ips", "bps", "xdelta", "ppf", "ups"] {
        let path = Path::new("game").with_extension(ext);
        assert_eq!(classify_side_file(&path), SideFileRole::Patch, "{ext}");
    }
}

#[test]
fn artwork_extensions_are_artwork_role() {
    for ext in ["jpg", "jpeg", "png", "webp"] {
        let path = Path::new("cover").with_extension(ext);
        assert_eq!(classify_side_file(&path), SideFileRole::Artwork, "{ext}");
    }
}

#[test]
fn cover_jpg_is_artwork_from_extension_alone() {
    // The milestone's own worked-good example (section 8).
    assert_eq!(
        classify_side_file(Path::new("cover.jpg")),
        SideFileRole::Artwork
    );
}

#[test]
fn readme_by_extension_is_readme() {
    assert_eq!(
        classify_side_file(Path::new("game.nfo")),
        SideFileRole::Readme
    );
}

#[test]
fn readme_by_basename_is_readme() {
    assert_eq!(
        classify_side_file(Path::new("README.txt")),
        SideFileRole::Readme
    );
    assert_eq!(
        classify_side_file(Path::new("ReadMe.pdf")),
        SideFileRole::Readme
    );
}

#[test]
fn manual_by_basename_is_manual() {
    assert_eq!(
        classify_side_file(Path::new("manual.pdf")),
        SideFileRole::Manual
    );
    assert_eq!(
        classify_side_file(Path::new("Game Manual.pdf")),
        SideFileRole::Manual
    );
}

#[test]
fn save_state_extensions_are_save_or_state() {
    for ext in ["sav", "srm", "state", "ss0", "st1", "mcr"] {
        let path = Path::new("game").with_extension(ext);
        assert_eq!(
            classify_side_file(&path),
            SideFileRole::SaveOrState,
            "{ext}"
        );
    }
}

#[test]
fn metadata_extensions_are_metadata() {
    for ext in ["xml", "json", "yaml"] {
        let path = Path::new("gamelist").with_extension(ext);
        assert_eq!(classify_side_file(&path), SideFileRole::Metadata, "{ext}");
    }
}

#[test]
fn genuinely_unrecognized_extension_is_unknown_support_not_guessed() {
    assert_eq!(
        classify_side_file(Path::new("something.xyz123")),
        SideFileRole::UnknownSupport
    );
}

#[test]
fn no_extension_and_no_basename_hint_is_unknown_support() {
    assert_eq!(
        classify_side_file(Path::new("mystery_file")),
        SideFileRole::UnknownSupport
    );
}

#[test]
fn filename_alone_never_asserts_platform_or_game_identity() {
    // The milestone's own worked-bad example: a name that *looks* like a
    // platform claim must never influence the role classification beyond
    // "this has a recognized ROM extension or it doesn't" - there is no
    // platform/game field on `SideFileRole` at all, so this is really an
    // API-shape guarantee, exercised here for a name that would tempt a
    // naive filename parser.
    // ".description" is not a registered content, patch, artwork, save, or
    // metadata extension for anything - the name alone must never promote
    // it to PrimaryContent or any other confident role.
    let role = classify_side_file(Path::new(
        "mario64_nintendo64_totally_a_n64_game.description",
    ));
    assert_eq!(role, SideFileRole::UnknownSupport);
}

#[test]
fn is_primary_is_true_only_for_primary_content() {
    assert!(SideFileRole::PrimaryContent.is_primary());
    for role in [
        SideFileRole::CueSheet,
        SideFileRole::Playlist,
        SideFileRole::Patch,
        SideFileRole::Manual,
        SideFileRole::Artwork,
        SideFileRole::Readme,
        SideFileRole::Metadata,
        SideFileRole::SaveOrState,
        SideFileRole::UnknownSupport,
    ] {
        assert!(!role.is_primary(), "{role:?}");
    }
}

#[test]
fn labels_are_all_distinct() {
    let roles = [
        SideFileRole::PrimaryContent,
        SideFileRole::CueSheet,
        SideFileRole::Playlist,
        SideFileRole::Patch,
        SideFileRole::Manual,
        SideFileRole::Artwork,
        SideFileRole::Readme,
        SideFileRole::Metadata,
        SideFileRole::SaveOrState,
        SideFileRole::UnknownSupport,
    ];
    let mut labels: Vec<&str> = roles.iter().map(|r| r.label()).collect();
    let before = labels.len();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), before);
}

#[test]
fn classification_is_deterministic() {
    let path = Path::new("game.cue");
    assert_eq!(classify_side_file(path), classify_side_file(path));
}

#[test]
fn serializes_to_json() {
    let json = serde_json::to_string(&SideFileRole::Artwork).unwrap();
    assert!(json.contains("artwork"));
}

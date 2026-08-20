use super::*;

#[test]
fn cue_file_attaches_to_its_own_stem_when_all_references_are_safe() {
    let contents = "FILE \"track1.bin\" BINARY\n";
    let attachment = attach_support_file(Path::new("/roms/game.cue"), Some(contents), None);
    assert_eq!(attachment.role, SideFileRole::CueSheet);
    match attachment.association {
        SupportAssociation::Attached { set_label } => assert_eq!(set_label, "game"),
        other => panic!("expected Attached, got {other:?}"),
    }
}

#[test]
fn m3u_file_attaches_when_all_references_are_safe() {
    let contents = "Disc 1.chd\nDisc 2.chd\n";
    let attachment = attach_support_file(Path::new("/roms/game.m3u"), Some(contents), None);
    match attachment.association {
        SupportAssociation::Attached { .. } => {}
        other => panic!("expected Attached, got {other:?}"),
    }
}

#[test]
fn cue_file_with_unsafe_reference_is_unsafe_reference_not_attached() {
    let contents = "FILE \"../../etc/passwd\" BINARY\n";
    let attachment = attach_support_file(Path::new("/roms/game.cue"), Some(contents), None);
    match attachment.association {
        SupportAssociation::UnsafeReference { .. } => {}
        other => panic!("expected UnsafeReference, got {other:?}"),
    }
}

#[test]
fn cue_file_with_no_references_is_candidate_not_attached() {
    let contents = "TRACK 01 MODE2/2352\n";
    let attachment = attach_support_file(Path::new("/roms/game.cue"), Some(contents), None);
    match attachment.association {
        SupportAssociation::Candidate { .. } => {}
        other => panic!("expected Candidate, got {other:?}"),
    }
}

#[test]
fn cue_file_without_supplied_contents_is_candidate() {
    let attachment = attach_support_file(Path::new("/roms/game.cue"), None, None);
    match attachment.association {
        SupportAssociation::Candidate { .. } => {}
        other => panic!("expected Candidate, got {other:?}"),
    }
}

#[test]
fn manual_attaches_only_when_a_single_game_context_is_given() {
    let attachment = attach_support_file(Path::new("/roms/manual.pdf"), None, None);
    assert_eq!(attachment.association, SupportAssociation::Unassociated);

    let context = SingleGameContext {
        set_label: "Some Game (USA)",
    };
    let attachment2 = attach_support_file(Path::new("/roms/manual.pdf"), None, Some(&context));
    assert_eq!(
        attachment2.association,
        SupportAssociation::Attached {
            set_label: "Some Game (USA)".to_string()
        }
    );
}

#[test]
fn artwork_attaches_only_when_a_single_game_context_is_given() {
    let attachment = attach_support_file(Path::new("/roms/cover.jpg"), None, None);
    assert_eq!(attachment.association, SupportAssociation::Unassociated);

    let context = SingleGameContext {
        set_label: "Some Game (USA)",
    };
    let attachment2 = attach_support_file(Path::new("/roms/cover.jpg"), None, Some(&context));
    match attachment2.association {
        SupportAssociation::Attached { .. } => {}
        other => panic!("expected Attached, got {other:?}"),
    }
}

#[test]
fn patch_is_never_automatically_attached_even_with_a_single_game_context() {
    let context = SingleGameContext {
        set_label: "Some Game (USA)",
    };
    let attachment = attach_support_file(Path::new("/roms/patch.bps"), None, Some(&context));
    assert_eq!(attachment.role, SideFileRole::Patch);
    assert_eq!(attachment.association, SupportAssociation::Unassociated);
}

#[test]
fn readme_is_never_automatically_attached() {
    let context = SingleGameContext {
        set_label: "Some Game (USA)",
    };
    let attachment = attach_support_file(Path::new("/roms/readme.txt"), None, Some(&context));
    assert_eq!(attachment.association, SupportAssociation::Unassociated);
}

#[test]
fn unknown_support_is_never_automatically_attached() {
    let attachment = attach_support_file(Path::new("/roms/mystery.xyz"), None, None);
    assert_eq!(attachment.role, SideFileRole::UnknownSupport);
    assert_eq!(attachment.association, SupportAssociation::Unassociated);
}

#[test]
fn shared_folder_alone_never_implies_attachment() {
    // Two support files in the exact same directory as a game, with no
    // single_game_context supplied (the "just shares a folder" case
    // milestone section 24 explicitly forbids attaching on) - both must
    // stay Unassociated.
    let a = attach_support_file(Path::new("/roms/game/readme.txt"), None, None);
    let b = attach_support_file(Path::new("/roms/game/notes.txt"), None, None);
    assert_eq!(a.association, SupportAssociation::Unassociated);
    assert_eq!(b.association, SupportAssociation::Unassociated);
}

#[test]
fn attachment_is_deterministic() {
    let contents = "FILE \"track1.bin\" BINARY\n";
    let a = attach_support_file(Path::new("/roms/game.cue"), Some(contents), None);
    let b = attach_support_file(Path::new("/roms/game.cue"), Some(contents), None);
    assert_eq!(a, b);
}

#[test]
fn support_attachment_source_never_references_mutation_functions() {
    let source = include_str!("../support_attachment.rs");
    for forbidden in [
        "std::fs::rename",
        "std::fs::remove_file",
        "std::fs::remove_dir",
        "std::fs::copy",
        "std::os::unix::fs::symlink",
        "std::fs::write",
    ] {
        assert!(!source.contains(forbidden));
    }
}

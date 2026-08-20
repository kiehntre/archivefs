use super::*;

#[test]
fn cue_file_references_are_extracted_in_order() {
    let cue = r#"FILE "Final Fantasy VII (Disc 1).bin" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
FILE "Final Fantasy VII (Disc 1) (Track 2).bin" BINARY
  TRACK 02 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00
"#;
    let refs = parse_cue_file_references(Path::new("/games/psx/game.cue"), cue);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().all(|r| r.is_safe()));
    assert_eq!(
        refs[0].resolved,
        Some(PathBuf::from("/games/psx/Final Fantasy VII (Disc 1).bin"))
    );
}

#[test]
fn cue_file_reference_with_parent_traversal_is_rejected() {
    let cue = r#"FILE "../../etc/passwd" BINARY"#;
    let refs = parse_cue_file_references(Path::new("/games/psx/game.cue"), cue);
    assert_eq!(refs.len(), 1);
    assert!(!refs[0].is_safe());
    assert_eq!(refs[0].rejection, Some(ReferenceRejection::ParentTraversal));
}

#[test]
fn cue_file_reference_with_absolute_path_is_rejected() {
    let cue = r#"FILE "/etc/passwd" BINARY"#;
    let refs = parse_cue_file_references(Path::new("/games/psx/game.cue"), cue);
    assert_eq!(refs.len(), 1);
    assert!(!refs[0].is_safe());
    assert_eq!(refs[0].rejection, Some(ReferenceRejection::AbsolutePath));
}

#[test]
fn cue_file_oversized_input_is_refused_entirely() {
    let huge = "FILE \"a\" BINARY\n".repeat(10_000);
    assert!(huge.len() > MAX_PARSE_BYTES);
    let refs = parse_cue_file_references(Path::new("/games/psx/game.cue"), &huge);
    assert!(refs.is_empty());
}

#[test]
fn cue_lines_without_file_are_ignored_not_errors() {
    let cue = "TRACK 01 MODE2/2352\nINDEX 01 00:00:00\n";
    let refs = parse_cue_file_references(Path::new("/games/psx/game.cue"), cue);
    assert!(refs.is_empty());
}

#[test]
fn m3u_references_skip_comments_and_blank_lines() {
    let m3u = "#EXTM3U\n\nGame (Disc 1).chd\n# a comment\nGame (Disc 2).chd\n";
    let refs = parse_m3u_references(Path::new("/games/psx/game.m3u"), m3u);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().all(|r| r.is_safe()));
}

#[test]
fn m3u_absolute_path_reference_is_rejected() {
    let m3u = "/etc/shadow\n";
    let refs = parse_m3u_references(Path::new("/games/psx/game.m3u"), m3u);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].rejection, Some(ReferenceRejection::AbsolutePath));
}

#[test]
fn m3u_parent_traversal_reference_is_rejected() {
    let m3u = "../../../root/.ssh/id_rsa\n";
    let refs = parse_m3u_references(Path::new("/games/psx/game.m3u"), m3u);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].rejection, Some(ReferenceRejection::ParentTraversal));
}

#[test]
fn m3u_oversized_input_is_refused_entirely() {
    let huge = "game.chd\n".repeat(20_000);
    assert!(huge.len() > MAX_PARSE_BYTES);
    let refs = parse_m3u_references(Path::new("/games/psx/game.m3u"), &huge);
    assert!(refs.is_empty());
}

#[test]
fn empty_reference_line_is_rejected() {
    let m3u = "   \n";
    let refs = parse_m3u_references(Path::new("/games/psx/game.m3u"), m3u);
    // A whitespace-only line is filtered as blank before ever becoming a
    // reference, so nothing is produced for it at all.
    assert!(refs.is_empty());
}

#[test]
fn malformed_cue_file_line_missing_closing_quote_is_ignored() {
    let cue = "FILE \"unterminated.bin BINARY\n";
    let refs = parse_cue_file_references(Path::new("/games/psx/game.cue"), cue);
    assert!(refs.is_empty());
}

#[test]
fn parsing_is_deterministic() {
    let cue = r#"FILE "a.bin" BINARY
FILE "b.bin" BINARY"#;
    let path = Path::new("/games/psx/game.cue");
    assert_eq!(
        parse_cue_file_references(path, cue),
        parse_cue_file_references(path, cue)
    );
}

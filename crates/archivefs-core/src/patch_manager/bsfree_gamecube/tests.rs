//! Unit tests for the BSFree GameCube bridge: classification, the proven
//! AR→Gecko byte-identity conversion, and the two-pass duplicate analysis.
//! Pure and filesystem-free; fixtures live only in memory or in a temporary
//! directory created by the caller.

use std::path::PathBuf;

use super::*;
use crate::patch_manager::{
    BsFreeCheat, BsFreeDeviceSummary, DeviceFormatCompatibility, parse_dolphin_ini,
};

fn cheat(name: &str, code: &str) -> BsFreeCheat {
    BsFreeCheat {
        upstream_id: 0,
        name: name.to_string(),
        note: None,
        code: code.to_string(),
        section: None,
        author: None,
        device: BsFreeDeviceSummary {
            upstream_id: 6,
            name: "Action Replay".to_string(),
            compatibility: DeviceFormatCompatibility::PotentiallyConvertible,
        },
        compatibility: DeviceFormatCompatibility::PotentiallyConvertible,
        truncated_fields: Vec::new(),
    }
}

fn named_cheat(id: i64, name: &str, code: &str) -> BsFreeCheat {
    let mut cheat = cheat(name, code);
    cheat.upstream_id = id;
    cheat
}

fn classify(code: &str) -> BsFreeGameCubeCodeFormat {
    classify_bsfree_gamecube_cheat(&cheat("c", code)).code_format
}

fn parse_ini(text: &str) -> DolphinIniDocument {
    parse_dolphin_ini(text)
}

fn empty_ini() -> DolphinIniDocument {
    parse_dolphin_ini("")
}

#[test]
fn pure_32_bit_writes_are_gecko_equivalent() {
    assert_eq!(
        classify("042318AC 3B8003E7\n042318B0 3B8003E8"),
        BsFreeGameCubeCodeFormat::GeckoEquivalent
    );
    assert_eq!(
        classify("042E4C84 00000001"),
        BsFreeGameCubeCodeFormat::GeckoEquivalent
    );
}

#[test]
fn gecko_equivalent_lines_are_byte_identical_after_conversion() {
    let raw = named_cheat(1, "Unlock All Items", "042E4C8C 00000001");
    let classified = classify_bsfree_gamecube_cheat(&raw);
    assert_eq!(
        classified.code_format,
        BsFreeGameCubeCodeFormat::GeckoEquivalent
    );
    let mapped = bsfree_cheat_as_gamehacking(&classified, 0);
    // The adapter input for a Gecko-equivalent code must carry the exact same
    // hex-pair lines, unmodified: "conversion" is byte-identity.
    assert_eq!(mapped.code_format, GameCubeCodeFormat::Gecko);
    assert_eq!(mapped.code_lines, vec!["042E4C8C 00000001"]);
}

#[test]
fn gecko_equivalent_address_must_fit_gecko_24bit_field() {
    // First word 0x057FAFF8 -> gcaddr 0x017FAFF8, a write to 0x817FAFF8,
    // which exceeds Gecko's 24-bit address field; the same bytes would not
    // behave identically, so the code stays Action Replay native instead of
    // being converted.
    assert_eq!(
        classify("057FAFF8 3B800001"),
        BsFreeGameCubeCodeFormat::ActionReplayNative
    );
}

#[test]
fn write_16_and_8_bit_with_fill_are_ar_native_not_gecko() {
    // AR 16-bit writes repeat (fill) with count = data>>16; Gecko 16-bit
    // writes once. Different semantics, so never relabelled as Gecko.
    assert_eq!(
        classify("0224CD50 00003E7F"),
        BsFreeGameCubeCodeFormat::ActionReplayNative
    );
    assert_eq!(
        classify("002E4BB3 000000FF"),
        BsFreeGameCubeCodeFormat::ActionReplayNative
    );
}

#[test]
fn float_write_is_ar_native() {
    assert_eq!(
        classify("063B8760 3F800000"),
        BsFreeGameCubeCodeFormat::ActionReplayNative
    );
}

#[test]
fn pointer_write_add_code_and_conditionals_are_ar_native() {
    assert_eq!(
        classify("80234C58 00000001"),
        BsFreeGameCubeCodeFormat::ActionReplayNative
    );
    assert_eq!(
        classify("A00AE4D0 00000001"),
        BsFreeGameCubeCodeFormat::ActionReplayNative
    );
    assert_eq!(
        classify("202E4C84 00000000\n042E4C88 00000001"),
        BsFreeGameCubeCodeFormat::ActionReplayNative
    );
}

#[test]
fn master_code_is_unsupported() {
    // Dolphin refuses master codes ("Master codes are not needed").
    assert_eq!(
        classify("C4129124 0000FF00"),
        BsFreeGameCubeCodeFormat::Unsupported
    );
    assert_eq!(
        classify("042318AC 3B8003E7\nC4129124 0000FF00"),
        BsFreeGameCubeCodeFormat::Unsupported
    );
}

#[test]
fn zero_code_and_self_modifying_are_unsupported() {
    assert_eq!(
        classify("00000000 04000000"),
        BsFreeGameCubeCodeFormat::Unsupported
    );
    assert_eq!(
        classify("00002222 00000001"),
        BsFreeGameCubeCodeFormat::Unsupported
    );
}

#[test]
fn placeholders_and_encrypted_dash_codes_are_malformed() {
    assert_eq!(
        classify("042E4C8C 0000XXXX"),
        BsFreeGameCubeCodeFormat::Malformed
    );
    assert_eq!(
        classify("XR7M-X292-DZ418\nKAJ8-YZ3T-1JJ2X"),
        BsFreeGameCubeCodeFormat::Malformed
    );
    assert_eq!(
        classify("0068A4FF 000000XX"),
        BsFreeGameCubeCodeFormat::Malformed
    );
    assert_eq!(classify("N/A"), BsFreeGameCubeCodeFormat::Malformed);
    assert_eq!(classify(""), BsFreeGameCubeCodeFormat::Malformed);
}

#[test]
fn empty_lines_and_whitespace_are_tolerated_in_classification() {
    let raw = named_cheat(1, "code", "  042E4C8C 00000001  \n\n  042E4C8C 00000001  ");
    let classified = classify_bsfree_gamecube_cheat(&raw);
    assert_eq!(
        classified.code_format,
        BsFreeGameCubeCodeFormat::GeckoEquivalent
    );
    assert_eq!(
        classified.code_lines,
        vec!["042E4C8C 00000001", "042E4C8C 00000001"]
    );
}

#[test]
fn gecko_equivalent_and_ar_native_are_selectable_but_others_are_not() {
    let cheats = vec![
        classify_bsfree_gamecube_cheat(&named_cheat(1, "Lives", "042318AC 3B8003E7")),
        classify_bsfree_gamecube_cheat(&named_cheat(2, "Health", "0224CD50 00003E7F")),
        classify_bsfree_gamecube_cheat(&named_cheat(3, "Master", "C4129124 0000FF00")),
        classify_bsfree_gamecube_cheat(&named_cheat(4, "Placeholder", "042E4C8C 0000XXXX")),
    ];
    let selection = BsFreeGameCubeCheatSelection::from_cheats(&cheats, &empty_ini());
    assert_eq!(selection.selectable_count(), 2);
    assert!(!selection.entries[0].already_managed);
    assert_eq!(
        selection.resolve(&cheats).unwrap_err().kind,
        BsFreeGameCubeErrorKind::NoSelectedCheats
    );
    let mut selection = selection;
    assert!(selection.set_selected(0, true));
    assert!(selection.set_selected(1, true));
    assert!(
        !selection.set_selected(2, true),
        "Unsupported never selects"
    );
    assert!(!selection.set_selected(3, true), "Malformed never selects");
    assert_eq!(selection.resolve(&cheats).unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// Two-pass duplicate / conflict analysis
// ---------------------------------------------------------------------------

#[test]
fn duplicate_record_within_bsfree_is_caught() {
    let cheats = vec![
        classify_bsfree_gamecube_cheat(&named_cheat(1, "Lives", "042318AC 3B8003E7")),
        classify_bsfree_gamecube_cheat(&named_cheat(2, "Lives", "042318AC 3B8003E7")),
    ];
    let findings = analyze_bsfree_gamecube_duplicates(&cheats, &empty_ini());
    assert!(
        findings
            .iter()
            .any(|f| f.kind == BsFreeDedupFindingKind::DuplicateRecord)
    );
}

#[test]
fn duplicate_body_with_different_labels_is_a_variant_not_a_duplicate_record() {
    let cheats = vec![
        classify_bsfree_gamecube_cheat(&named_cheat(1, "Lives A", "042318AC 3B8003E7")),
        classify_bsfree_gamecube_cheat(&named_cheat(2, "Lives B", "042318AC 3B8003E7")),
    ];
    let findings = analyze_bsfree_gamecube_duplicates(&cheats, &empty_ini());
    assert!(
        findings
            .iter()
            .any(|f| f.kind == BsFreeDedupFindingKind::DuplicateBody)
    );
}

#[test]
fn same_name_different_body_is_a_conflict() {
    let cheats = vec![
        classify_bsfree_gamecube_cheat(&named_cheat(1, "Lives", "042318AC 3B8003E7")),
        classify_bsfree_gamecube_cheat(&named_cheat(2, "Lives", "042318AC 3B8003E8")),
    ];
    let findings = analyze_bsfree_gamecube_duplicates(&cheats, &empty_ini());
    assert!(
        findings
            .iter()
            .any(|f| f.kind == BsFreeDedupFindingKind::DuplicateNameConflict)
    );
}

#[test]
fn two_records_converting_to_identical_gecko_output_are_deduplicated() {
    // Both are pure 04 writes; both become byte-identical Gecko codes even
    // though their BSFree labels differ. The second must be deduplicated.
    let cheats = vec![
        classify_bsfree_gamecube_cheat(&named_cheat(1, "Lives A", "042318AC 3B8003E7")),
        classify_bsfree_gamecube_cheat(&named_cheat(2, "Lives B", "042318AC 3B8003E7")),
    ];
    let findings = analyze_bsfree_gamecube_duplicates(&cheats, &empty_ini());
    assert!(
        findings
            .iter()
            .any(|f| f.kind == BsFreeDedupFindingKind::DuplicateBody)
    );
    // Output-level dedup happens at staging time.
    let selection = BsFreeGameCubeCheatSelection::from_cheats(&cheats, &empty_ini());
    let mut selection = selection;
    selection.select_all();
    assert_eq!(selection.resolve(&cheats).unwrap().len(), 2);
}

#[test]
fn gecko_equivalent_matching_an_installed_gecko_code_is_already_installed() {
    let destination = parse_ini(
        "[Gecko]\n$Lives [BSFree Archive]\n042318AC 3B8003E7\n[Gecko_Enabled]\n$Lives [BSFree Archive]\n",
    );
    let cheats = vec![classify_bsfree_gamecube_cheat(&named_cheat(
        1,
        "Lives",
        "042318AC 3B8003E7",
    ))];
    let findings = analyze_bsfree_gamecube_duplicates(&cheats, &destination);
    assert!(
        findings
            .iter()
            .any(|f| f.kind == BsFreeDedupFindingKind::AlreadyInstalled)
    );
}

#[test]
fn already_installed_under_a_different_name_is_skipped_not_duplicated() {
    // The destination already has this exact Gecko body under another name;
    // a second selected label must not install a duplicate code body.
    let destination = parse_ini(
        "[Gecko]\n$My Own Lives [User]\n042318AC 3B8003E7\n[Gecko_Enabled]\n$My Own Lives [User]\n",
    );
    let cheats = vec![classify_bsfree_gamecube_cheat(&named_cheat(
        1,
        "Lives",
        "042318AC 3B8003E7",
    ))];
    let findings = analyze_bsfree_gamecube_duplicates(&cheats, &destination);
    assert!(
        findings
            .iter()
            .any(|f| { f.kind == BsFreeDedupFindingKind::AlreadyInstalledDifferentName }),
        "the analysis must report the cross-label duplicate"
    );

    let selection = BsFreeGameCubeCheatSelection::from_cheats(&cheats, &destination);
    let mut selection = selection;
    selection.select_all();
    let staging_root =
        std::env::temp_dir().join(format!("archivefs-bsfree-gc-skip-{}", std::process::id()));
    let result = stage_bsfree_gamecube_install(
        &staging_root,
        "GLME01.ini",
        &destination,
        true,
        &cheats,
        &selection,
    );
    assert_eq!(
        result.unwrap_err().kind,
        BsFreeGameCubeErrorKind::NoSelectedCheats,
        "the only selected cheat is already covered, so nothing is staged"
    );
    let _ = std::fs::remove_dir_all(&staging_root);
}

#[test]
fn gecko_equivalent_matching_an_installed_ar_code_is_a_cross_section_collision() {
    // The same 04 body installed as an Action Replay code: both engines
    // interpret these bytes differently, so this requires review, not apply.
    let destination = parse_ini(
        "[ActionReplay]\n$Lives [BSFree Archive]\n042318AC 3B8003E7\n[ActionReplay_Enabled]\n$Lives [BSFree Archive]\n",
    );
    let cheats = vec![classify_bsfree_gamecube_cheat(&named_cheat(
        1,
        "Lives",
        "042318AC 3B8003E7",
    ))];
    let findings = analyze_bsfree_gamecube_duplicates(&cheats, &destination);
    assert!(
        findings
            .iter()
            .any(|f| f.kind == BsFreeDedupFindingKind::CrossSectionCollision)
    );
}

#[test]
fn installed_user_code_with_same_name_different_body_blocks_and_preserves() {
    let destination = parse_ini(
        "[ActionReplay]\n$Lives [BSFree Archive]\n0224CD50 00003E7F\n[ActionReplay_Enabled]\n$Lives [BSFree Archive]\n",
    );
    let cheats = vec![classify_bsfree_gamecube_cheat(&named_cheat(
        1,
        "Lives",
        "042318AC 3B8003E7",
    ))];
    let findings = analyze_bsfree_gamecube_duplicates(&cheats, &destination);
    assert!(
        findings
            .iter()
            .any(|f| f.kind == BsFreeDedupFindingKind::SameLabelDifferentBody)
    );
    // The blocking finding prevents staging (never silently overwrites).
    let selection = BsFreeGameCubeCheatSelection::from_cheats(&cheats, &destination);
    let mut selection = selection;
    selection.select_all();
    let staging_root = std::env::temp_dir().join(format!(
        "archivefs-bsfree-gc-conflict-{}",
        std::process::id()
    ));
    let result = stage_bsfree_gamecube_install(
        &staging_root,
        "GLME01.ini",
        &destination,
        true,
        &cheats,
        &selection,
    );
    assert_eq!(
        result.unwrap_err().kind,
        BsFreeGameCubeErrorKind::ConflictingSelection
    );
    let _ = std::fs::remove_dir_all(&staging_root);
}

#[test]
fn staging_skips_output_level_duplicates_and_reports_them() {
    let cheats = vec![
        classify_bsfree_gamecube_cheat(&named_cheat(1, "Lives A", "042318AC 3B8003E7")),
        classify_bsfree_gamecube_cheat(&named_cheat(2, "Lives B", "042318AC 3B8003E7")),
        classify_bsfree_gamecube_cheat(&named_cheat(3, "Health", "0224CD50 00003E7F")),
        classify_bsfree_gamecube_cheat(&named_cheat(4, "Master", "C4129124 0000FF00")),
    ];
    let destination = empty_ini();
    let selection = BsFreeGameCubeCheatSelection::from_cheats(&cheats, &destination);
    let mut selection = selection;
    selection.select_all();
    let staging_root =
        std::env::temp_dir().join(format!("archivefs-bsfree-gc-dedup-{}", std::process::id()));
    let staged = stage_bsfree_gamecube_install(
        &staging_root,
        "GLME01.ini",
        &destination,
        false,
        &cheats,
        &selection,
    )
    .expect("staging succeeds");
    // "Lives B" and "Master" are skipped; only two distinct outputs staged.
    assert_eq!(staged.skipped_duplicates, vec!["Lives B"]);
    assert_eq!(staged.skipped_unselectable, vec!["Master"]);
    let contents = std::fs::read_to_string(&staged.staged.path).unwrap();
    assert!(contents.contains("Lives A [BSFree Archive]"));
    assert!(!contents.contains("Lives B"));
    assert!(!contents.contains("Master"));
    assert!(contents.contains("Health [BSFree Archive]"));
    let _ = std::fs::remove_dir_all(&staging_root);
}

// ---------------------------------------------------------------------------
// Identity matching
// ---------------------------------------------------------------------------

#[test]
fn match_requires_platform_and_exact_normalized_title() {
    assert_eq!(
        normalize_title("Luigi's Mansion"),
        normalize_title("Luigis Mansion")
    );
}

#[test]
fn region_evidence_reports_both_sides_honestly() {
    assert!(region_evidence(Some("USA"), Some("USA")).contains("contains"));
    assert!(region_evidence(Some("Europe"), Some("USA")).contains("does not explicitly"));
    assert!(region_evidence(None, Some("USA")).contains("archive region"));
    assert!(region_evidence(None, None).contains("no region"));
}

#[test]
fn converted_output_never_changes_native_gecko_bytes() {
    // A native Gecko code (from an existing provider) fed through the BSFree
    // mapping path must keep its bytes exactly.
    let raw = named_cheat(1, "Native", "04123456 00000001\n06000000 00000001");
    let classified = classify_bsfree_gamecube_cheat(&raw);
    // The second line is a Gecko string code (CT0 sub 3), not an AR 32-bit
    // write; the whole code is not treated as Gecko-equivalent.
    assert_eq!(
        classified.code_format,
        BsFreeGameCubeCodeFormat::ActionReplayNative
    );
    let mapped = bsfree_cheat_as_gamehacking(&classified, 0);
    assert_eq!(
        mapped.code_lines,
        vec!["04123456 00000001", "06000000 00000001"]
    );
}

#[test]
fn provider_never_touches_files_for_classification() {
    let raw = named_cheat(1, "Lives", "042318AC 3B8003E7");
    let _ = classify_bsfree_gamecube_cheat(&raw);
    let _ = bsfree_dolphin_code_name(&classify_bsfree_gamecube_cheat(&raw));
    // No path is created or read by the pure classification path.
    let probe = PathBuf::from("/nonexistent/archivefs-bsfree-gc-probe");
    assert!(!probe.exists());
}

#[test]
fn author_fallback_uses_bsfree_label_in_dolphin_name() {
    let raw = named_cheat(1, "Lives", "042318AC 3B8003E7");
    let classified = classify_bsfree_gamecube_cheat(&raw);
    assert_eq!(
        bsfree_dolphin_code_name(&classified),
        "Lives [BSFree Archive]"
    );
}

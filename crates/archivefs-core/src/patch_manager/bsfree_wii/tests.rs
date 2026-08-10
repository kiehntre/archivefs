//! Unit tests for the BSFree Wii bridge: classification of the verified safe
//! subset, the browse-only gate, and the shared duplicate/conflict analysis
//! (including cross-provider comparison with GameHacking Wii records).

use std::path::PathBuf;

use super::*;
use crate::patch_manager::{
    BsFreeCheat, BsFreeDeviceSummary, DeviceFormatCompatibility, GameHackingWiiCheat,
    WiiCheatSafety, WiiCodeFormat, parse_dolphin_ini,
};

fn wii_cheat(id: i64, name: &str, code: &str, device: &str) -> BsFreeCheat {
    BsFreeCheat {
        upstream_id: id,
        name: name.to_string(),
        note: None,
        code: code.to_string(),
        section: None,
        author: None,
        device: BsFreeDeviceSummary {
            upstream_id: 0,
            name: device.to_string(),
            compatibility: DeviceFormatCompatibility::PotentiallyConvertible,
        },
        compatibility: DeviceFormatCompatibility::PotentiallyConvertible,
        truncated_fields: Vec::new(),
    }
}

fn classify(id: i64, name: &str, code: &str, device: &str) -> BsFreeWiiCheat {
    classify_bsfree_wii_cheat(&wii_cheat(id, name, code, device))
}

fn empty_ini() -> DolphinIniDocument {
    parse_dolphin_ini("")
}

fn gamehacking_cheat(
    id: &str,
    name: &str,
    format: WiiCodeFormat,
    line: &str,
) -> GameHackingWiiCheat {
    GameHackingWiiCheat {
        id: id.to_string(),
        name: name.to_string(),
        author: Some("GameHacking.org".to_string()),
        description: None,
        code_format: format,
        safety: WiiCheatSafety::Installable,
        safety_warnings: Vec::new(),
        code_lines: vec![line.to_string()],
        source_game_id: 131936,
        source_url: "https://gamehacking.org/game/131936".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Classification: the verified safe subset
// ---------------------------------------------------------------------------

#[test]
fn verified_action_replay_device_with_strict_pairs_is_installable() {
    let cheat = classify(1, "Infinite Health", "042318AC 3B8003E7", "Action Replay");
    assert_eq!(cheat.code_format, BsFreeWiiCodeFormat::GeckoEquivalent);
    assert!(cheat.code_format.is_installable());
    assert_eq!(cheat.code_lines, vec!["042318AC 3B8003E7"]);
}

#[test]
fn native_action_replay_body_installs_verbatim_as_action_replay() {
    let cheat = classify(2, "Player Speed", "0224CD50 00003E7F", "Action Replay");
    assert_eq!(cheat.code_format, BsFreeWiiCodeFormat::ActionReplayNative);
    assert!(cheat.code_format.is_installable());
    let mapped = bsfree_cheat_as_wii(&cheat, 0);
    assert_eq!(mapped.code_format, WiiCodeFormat::ActionReplay);
    assert_eq!(mapped.safety, WiiCheatSafety::Installable);
    assert_eq!(mapped.code_lines, vec!["0224CD50 00003E7F"]);
}

#[test]
fn gecko_device_with_gecko_lines_is_installable_as_gecko() {
    let cheat = classify(3, "Max Items", "042318AC 3B8003E7", "Gecko");
    assert_eq!(cheat.code_format, BsFreeWiiCodeFormat::GeckoEquivalent);
    let mapped = bsfree_cheat_as_wii(&cheat, 0);
    assert_eq!(mapped.code_format, WiiCodeFormat::Gecko);
    assert_eq!(mapped.safety, WiiCheatSafety::Installable);
}

#[test]
fn encrypted_dash_codes_stay_browse_only() {
    let cheat = classify(
        4,
        "Encrypted",
        "XR7M-X292-DZ418\nKAJ8-YZ3T-1JJ2X",
        "Action Replay",
    );
    assert_eq!(cheat.code_format, BsFreeWiiCodeFormat::Malformed);
    assert!(!cheat.code_format.is_installable());
    let mapped = bsfree_cheat_as_wii(&cheat, 0);
    assert!(!mapped.safety.installable());
}

#[test]
fn master_and_self_modifying_codes_stay_browse_only() {
    let master = classify(5, "Master", "C4129124 0000FF00", "Action Replay");
    assert_eq!(master.code_format, BsFreeWiiCodeFormat::Unsupported);
    assert!(!master.code_format.is_installable());

    let named_master = classify(6, "Master Code On", "042318AC 00000001", "Action Replay");
    assert_eq!(named_master.code_format, BsFreeWiiCodeFormat::Unsupported);

    let self_modifying = classify(7, "Loop", "00002222 00000001", "Action Replay");
    assert_eq!(self_modifying.code_format, BsFreeWiiCodeFormat::Unsupported);

    let zero_code = classify(8, "Zero", "00000000 04000000", "Action Replay");
    assert_eq!(zero_code.code_format, BsFreeWiiCodeFormat::Unsupported);
}

#[test]
fn placeholders_and_free_text_stay_browse_only() {
    let placeholder = classify(8, "Unknown", "0423???? 00000001", "Action Replay");
    assert_eq!(placeholder.code_format, BsFreeWiiCodeFormat::Malformed);
    let free_text = classify(
        9,
        "Note",
        "infinite health, press R to activate",
        "Action Replay",
    );
    assert_eq!(free_text.code_format, BsFreeWiiCodeFormat::Malformed);
}

#[test]
fn an_unverified_device_is_never_installable() {
    for device in ["GameShark", "CodeBreaker", "CWCheats", "Mystery Device"] {
        let cheat = classify(10, "Lives", "042318AC 3B8003E7", device);
        assert_eq!(
            cheat.code_format,
            BsFreeWiiCodeFormat::Malformed,
            "{device} must never be promoted to installable"
        );
        assert!(!cheat.code_format.is_installable());
    }
}

// ---------------------------------------------------------------------------
// Shared duplicate/conflict analysis
// ---------------------------------------------------------------------------

#[test]
fn duplicate_bodies_within_bsfree_wii_are_detected() {
    let cheats = vec![
        classify(1, "Lives A", "042318AC 3B8003E7", "Action Replay"),
        classify(2, "Lives B", "042318AC 3B8003E7", "Action Replay"),
    ];
    let findings = analyze_bsfree_wii_duplicates(&cheats, &empty_ini());
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == BsFreeWiiDedupFindingKind::DuplicateBody),
        "{findings:?}"
    );
}

#[test]
fn same_name_different_code_is_not_deduplicated() {
    let cheats = vec![
        classify(1, "Level Select", "042318AC 00000001", "Action Replay"),
        classify(2, "Level Select", "0424CD50 00000002", "Action Replay"),
    ];
    let findings = analyze_bsfree_wii_duplicates(&cheats, &empty_ini());
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == BsFreeWiiDedupFindingKind::DuplicateNameConflict),
        "same display name with different bodies is a conflict, never a collapse: {findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.kind == BsFreeWiiDedupFindingKind::DuplicateBody),
        "different bodies must not be treated as duplicates: {findings:?}"
    );
}

#[test]
fn conflicting_memory_writes_across_bsfree_wii_are_detected_and_block() {
    let cheats = vec![
        classify(1, "Max Money", "042318AC 3B8003E7", "Action Replay"),
        classify(2, "No Money", "042318AC 00000000", "Action Replay"),
    ];
    let findings = analyze_bsfree_wii_duplicates(&cheats, &empty_ini());
    assert!(
        findings
            .iter()
            .any(|finding| { finding.kind == BsFreeWiiDedupFindingKind::ConflictingMemoryWrite }),
        "{findings:?}"
    );
    assert!(BsFreeWiiDedupFindingKind::ConflictingMemoryWrite.blocks_selection());
}

#[test]
fn cross_provider_exact_duplicate_between_bsfree_and_gamehacking_is_detected() {
    // The same byte-identical Gecko body arrives from both providers.
    let bsfree = classify(1, "Infinite Health", "042318AC 3B8003E7", "Action Replay");
    let gamehacking = gamehacking_cheat(
        "131936:42",
        "Infinite Health",
        WiiCodeFormat::Gecko,
        "042318AC 3B8003E7",
    );
    let findings = analyze_dolphin_duplicates(
        &[
            &bsfree as &dyn DolphinCheat,
            &gamehacking as &dyn DolphinCheat,
        ],
        &empty_ini(),
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == DolphinDedupFindingKind::DuplicateBody),
        "a byte-identical body from two providers is a duplicate body: {findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| { finding.kind == DolphinDedupFindingKind::ConflictingMemoryWrite }),
        "identical writes are not a conflict: {findings:?}"
    );
}

#[test]
fn cross_provider_same_name_different_body_stays_distinct() {
    let bsfree = classify(1, "Level Select", "042318AC 00000001", "Action Replay");
    let gamehacking = gamehacking_cheat(
        "131936:43",
        "Level Select",
        WiiCodeFormat::Gecko,
        "0424CD50 00000002",
    );
    let findings = analyze_dolphin_duplicates(
        &[
            &bsfree as &dyn DolphinCheat,
            &gamehacking as &dyn DolphinCheat,
        ],
        &empty_ini(),
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == DolphinDedupFindingKind::DuplicateNameConflict),
        "same name, different operations must remain separate: {findings:?}"
    );
}

#[test]
fn cross_provider_conflicting_writes_are_detected() {
    let bsfree = classify(1, "Max Money", "042318AC 3B8003E7", "Action Replay");
    let gamehacking = gamehacking_cheat(
        "131936:44",
        "Wallet Zero",
        WiiCodeFormat::Gecko,
        "042318AC 00000000",
    );
    let findings = analyze_dolphin_duplicates(
        &[
            &bsfree as &dyn DolphinCheat,
            &gamehacking as &dyn DolphinCheat,
        ],
        &empty_ini(),
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == DolphinDedupFindingKind::ConflictingMemoryWrite),
        "different providers writing different values to the same address must conflict: \
         {findings:?}"
    );
}

#[test]
fn browse_only_records_are_reported_not_installable() {
    let cheats = vec![
        classify(1, "Encrypted", "XR7M-X292-DZ418", "Action Replay"),
        classify(2, "Lives", "042318AC 3B8003E7", "Action Replay"),
    ];
    let findings = analyze_bsfree_wii_duplicates(&cheats, &empty_ini());
    let encrypted = findings
        .iter()
        .find(|finding| finding.cheat_upstream_id == 1)
        .expect("the browse-only record must produce a finding");
    assert_eq!(encrypted.kind, BsFreeWiiDedupFindingKind::NotInstallable);
}

#[test]
fn selection_can_never_select_a_browse_only_record() {
    let cheats = vec![
        classify(1, "Lives", "042318AC 3B8003E7", "Action Replay"),
        classify(2, "Encrypted", "XR7M-X292-DZ418", "Action Replay"),
        classify(3, "Master", "C4129124 0000FF00", "Action Replay"),
    ];
    let mut selection = BsFreeWiiCheatSelection::from_cheats(&cheats, &empty_ini());
    assert!(selection.set_selected(0, true), "installable cheats select");
    assert!(
        !selection.set_selected(1, true),
        "malformed cheats can never select"
    );
    assert!(
        !selection.set_selected(2, true),
        "unsupported cheats can never select"
    );
    assert_eq!(selection.selected_count(), 1);
    assert_eq!(selection.selectable_count(), 1);
}

// ---------------------------------------------------------------------------
// Staging routes into the existing Wii/Dolphin adapter
// ---------------------------------------------------------------------------

#[test]
fn stage_routes_installable_cheats_through_the_wii_adapter() {
    let staging_root =
        std::env::temp_dir().join(format!("bsfree-wii-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging_root);
    std::fs::create_dir_all(&staging_root).unwrap();

    let cheats = vec![
        classify(1, "Lives", "042318AC 3B8003E7", "Action Replay"),
        classify(2, "Encrypted", "XR7M-X292-DZ418", "Action Replay"),
    ];
    let destination = empty_ini();
    let mut selection = BsFreeWiiCheatSelection::from_cheats(&cheats, &destination);
    selection.set_selected(0, true);

    let staged = stage_bsfree_wii_install(
        &staging_root,
        "R3HX6Z.ini",
        &destination,
        false,
        &cheats,
        &selection,
    )
    .expect("staging succeeds for the installable cheat");
    assert!(
        staged
            .skipped_unselectable
            .iter()
            .any(|name| name == "Encrypted"),
        "browse-only records are skipped: {:?}",
        staged.skipped_unselectable
    );
    assert_eq!(staged.staged.affected.len(), 1);
    assert!(
        staged.staged.path.ends_with("R3HX6Z.ini"),
        "{}",
        staged.staged.path.display()
    );
    let _ = std::fs::remove_dir_all(&staging_root);
}

#[test]
fn wii_match_requires_an_exact_normalized_title() {
    // BSFree carries no emulator-stable identifier, so title equality is the
    // only signal - the caller must still require user review before Apply.
    let normalized = normalize_title("Agent Hugo: Hula Holiday");
    assert_eq!(normalized, "agenthugohulaholiday");
    // Region evidence is produced but never proves identity.
    let evidence = region_evidence(Some("PAL"), Some("PAL"));
    assert!(evidence.contains("compatible"), "{evidence}");
}

// Keep PathBuf import used for the staging fixture.
#[allow(dead_code)]
fn _unused() -> PathBuf {
    PathBuf::new()
}

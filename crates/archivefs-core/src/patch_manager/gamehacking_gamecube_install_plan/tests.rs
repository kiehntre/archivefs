use super::*;
use crate::patch_manager::gamehacking_gamecube_provider::GameCubeCodeFormat;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(label: &str) -> PathBuf {
    let unique = format!(
        "archivefs-gamecube-gamehacking-install-plan-{label}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let root = std::env::temp_dir().join(unique);
    fs::create_dir_all(&root).expect("fixture root");
    root
}

fn cheat(
    id: &str,
    name: &str,
    author: Option<&str>,
    format: GameCubeCodeFormat,
    lines: &[&str],
) -> GameHackingGameCubeCheat {
    GameHackingGameCubeCheat {
        id: id.to_string(),
        name: name.to_string(),
        author: author.map(str::to_string),
        description: None,
        code_format: format,
        code_lines: lines.iter().map(|line| line.to_string()).collect(),
        source_game_id: 1,
        source_url: "https://gamehacking.org/game/1".to_string(),
    }
}

fn ar_cheat() -> GameHackingGameCubeCheat {
    cheat(
        "1",
        "999 Cash",
        Some("Codejunkies"),
        GameCubeCodeFormat::ActionReplay,
        &["040AE4D0 3C00270F", "040AE4E8 60000000"],
    )
}

fn gecko_cheat() -> GameHackingGameCubeCheat {
    cheat(
        "2",
        "Infinite Health",
        Some("Link Master"),
        GameCubeCodeFormat::Gecko,
        &["04123456 00000001"],
    )
}

fn raw_unknown_cheat() -> GameHackingGameCubeCheat {
    cheat(
        "3",
        "Mystery Code",
        None,
        GameCubeCodeFormat::RawUnknown,
        &["0A0A0A0A 0B0B0B0B"],
    )
}

#[test]
fn raw_unknown_cheats_are_never_selectable() {
    let document = parse_dolphin_ini("");
    let cheats = vec![ar_cheat(), gecko_cheat(), raw_unknown_cheat()];
    let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &document);
    assert!(!selection.entries[2].selectable);
    assert!(!selection.set_selected(2, true));
    assert!(!selection.entries[2].selected);
    selection.select_all();
    assert_eq!(selection.selected_count(), 2);
}

#[test]
fn mixed_action_replay_and_gecko_install_routes_to_correct_sections() {
    let document = parse_dolphin_ini("");
    let cheats = vec![ar_cheat(), gecko_cheat()];
    let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &document);
    selection.select_all();

    let dir = temp_dir("case");
    let staged = stage_gamecube_gamehacking_install(
        dir.as_path(),
        "GLME01.ini",
        &document,
        false,
        &cheats,
        &selection,
    )
    .expect("install stages cleanly");

    let reparsed = parse_dolphin_ini(&staged.contents);
    assert_eq!(reparsed.action_replay_codes.len(), 1);
    assert_eq!(reparsed.gecko_codes.len(), 1);
    assert!(
        reparsed
            .action_replay_codes
            .iter()
            .any(|code| code.name == "999 Cash [Codejunkies]")
    );
    assert!(
        reparsed
            .gecko_codes
            .iter()
            .any(|code| code.name == "Infinite Health [Link Master]")
    );
    assert!(
        reparsed
            .action_replay_enabled_names
            .contains(&"999 Cash [Codejunkies]".to_string())
    );
    assert!(
        reparsed
            .gecko_enabled_names
            .contains(&"Infinite Health [Link Master]".to_string())
    );
    let managed = managed_names(&reparsed);
    assert!(managed.contains("999 Cash [Codejunkies]"));
    assert!(managed.contains("Infinite Health [Link Master]"));
}

#[test]
fn install_preserves_existing_ini_content_and_unrelated_sections() {
    let existing = "[Core]\r\nCPUThread = True\r\n\r\n[Video_Settings]\r\nAspectRatio = 0\r\n";
    let document = parse_dolphin_ini(existing);
    let cheats = vec![ar_cheat()];
    let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &document);
    selection.select_all();

    let dir = temp_dir("case");
    let staged = stage_gamecube_gamehacking_install(
        dir.as_path(),
        "GLME01.ini",
        &document,
        true,
        &cheats,
        &selection,
    )
    .expect("install stages cleanly");

    assert!(staged.contents.contains("[Core]"));
    assert!(staged.contents.contains("CPUThread = True"));
    assert!(staged.contents.contains("[Video_Settings]"));
    assert!(staged.contents.contains("AspectRatio = 0"));
}

#[test]
fn reinstalling_the_same_selection_is_idempotent() {
    let document = parse_dolphin_ini("");
    let cheats = vec![ar_cheat()];
    let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &document);
    selection.select_all();

    let dir = temp_dir("case");
    let first = stage_gamecube_gamehacking_install(
        dir.as_path(),
        "GLME01.ini",
        &document,
        false,
        &cheats,
        &selection,
    )
    .expect("first install stages cleanly");

    let reparsed = parse_dolphin_ini(&first.contents);
    let mut second_selection = GameCubeCheatSelection::from_cheats(&cheats, &reparsed);
    second_selection.select_all();
    let second = stage_gamecube_gamehacking_install(
        dir.as_path(),
        "GLME01.ini",
        &reparsed,
        true,
        &cheats,
        &second_selection,
    )
    .expect("second install stages cleanly");

    assert_eq!(first.contents, second.contents);
    let final_document = parse_dolphin_ini(&second.contents);
    assert_eq!(final_document.action_replay_codes.len(), 1);
}

#[test]
fn removal_only_touches_archivefs_managed_entries() {
    let document = parse_dolphin_ini("");
    let cheats = vec![ar_cheat(), gecko_cheat()];
    let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &document);
    selection.select_all();

    let dir = temp_dir("case");
    let installed = stage_gamecube_gamehacking_install(
        dir.as_path(),
        "GLME01.ini",
        &document,
        false,
        &cheats,
        &selection,
    )
    .expect("install stages cleanly");

    let with_user_code = installed.contents.replacen(
        "[ActionReplay]",
        "[ActionReplay]\n$My Own Code\n11111111 22222222",
        1,
    );
    let document_with_user_code = parse_dolphin_ini(&with_user_code);

    let removal = stage_gamecube_gamehacking_removal(
        dir.as_path(),
        "GLME01.ini",
        &document_with_user_code,
        true,
        &["999 Cash [Codejunkies]".to_string()],
    )
    .expect("removal stages cleanly");

    let removed_document = parse_dolphin_ini(&removal.contents);
    assert!(
        !removed_document
            .action_replay_codes
            .iter()
            .any(|code| code.name == "999 Cash [Codejunkies]")
    );
    assert!(
        removed_document
            .action_replay_codes
            .iter()
            .any(|code| code.name == "My Own Code")
    );
    assert!(
        removed_document
            .gecko_codes
            .iter()
            .any(|code| code.name == "Infinite Health [Link Master]")
    );

    let attempt_unmanaged = stage_gamecube_gamehacking_removal(
        dir.as_path(),
        "GLME01.ini",
        &document_with_user_code,
        true,
        &["My Own Code".to_string()],
    );
    assert!(attempt_unmanaged.is_err());
    assert_eq!(
        attempt_unmanaged.unwrap_err().kind,
        GameCubeInstallPlanErrorKind::NotManaged
    );
}

#[test]
fn raw_unknown_cheats_are_blocked_from_install() {
    let document = parse_dolphin_ini("");
    let cheats = vec![raw_unknown_cheat()];
    let selection = GameCubeCheatSelection::from_cheats(&cheats, &document);
    assert_eq!(selection.selectable_count(), 0);
    let result = selection.resolve(&cheats);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().kind,
        GameCubeInstallPlanErrorKind::NoSelectedCheats
    );
}

#[test]
fn destination_file_name_uses_the_exact_verified_game_id() {
    let document = parse_dolphin_ini("");
    let cheats = vec![ar_cheat()];
    let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &document);
    selection.select_all();

    let dir = temp_dir("case");
    let staged = stage_gamecube_gamehacking_install(
        dir.as_path(),
        "GLME01.ini",
        &document,
        false,
        &cheats,
        &selection,
    )
    .expect("install stages cleanly");

    assert_eq!(staged.path.file_name().unwrap(), "GLME01.ini");
}

#[test]
fn does_not_convert_gecko_to_action_replay_or_back() {
    let document = parse_dolphin_ini("");
    let cheats = vec![gecko_cheat()];
    let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &document);
    selection.select_all();

    let dir = temp_dir("case");
    let staged = stage_gamecube_gamehacking_install(
        dir.as_path(),
        "GLME01.ini",
        &document,
        false,
        &cheats,
        &selection,
    )
    .expect("install stages cleanly");

    let reparsed = parse_dolphin_ini(&staged.contents);
    assert!(reparsed.action_replay_codes.is_empty());
    assert_eq!(reparsed.gecko_codes.len(), 1);
}

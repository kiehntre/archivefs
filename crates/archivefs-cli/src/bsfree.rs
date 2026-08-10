use std::path::PathBuf;

use archivefs_core::patch_manager::{
    BsFreeCatalogue, BsFreeDownloadOptions, BsFreeGameCubeCheatSelection, BsFreeGameCubeCodeFormat,
    BsFreeGameCubeInstallPreviewRequest, BsFreeGameSearchRequest, BsFreePaths,
    HttpsCheatSourceTransport, PageRequest, ReadOnlyCheatCatalogue, SharedApplyConfirmation,
    SharedApplyOptions, SharedRollbackConfirmation, SharedRollbackOptions, bsfree_gamecube_cheats,
    build_bsfree_gamecube_install_preview, build_shared_transaction_plan,
    classify_bsfree_gamecube_cheat, default_bsfree_source_root, default_shared_backup_root,
    default_shared_history_root, download_bsfree_database, execute_shared_apply,
    execute_shared_rollback, generate_shared_operation_id, import_local_bsfree_database,
    inspect_bsfree_source, load_dolphin_destination, managed_names, parse_dolphin_ini,
    preview_shared_rollback, remove_local_bsfree_source,
    require_dolphin_managed_gamehacking_verification, set_bsfree_enabled,
    stage_bsfree_gamecube_install, validate_installed_bsfree_source,
};

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args;
    let json = take_flag(&mut args, "--json");
    let root = take_path(&mut args, "--root")?.unwrap_or(default_bsfree_source_root()?);
    let paths = BsFreePaths::at(root);
    let Some(command) = args.first().cloned() else {
        return Err("cheats source bsfree requires a command".into());
    };
    args.remove(0);
    match command.as_str() {
        "status" => {
            reject_extra(&args, "status")?;
            render(&inspect_bsfree_source(&paths)?, json)?;
        }
        "validate" => {
            reject_extra(&args, "validate")?;
            render(&validate_installed_bsfree_source(&paths)?, json)?;
        }
        "download" => {
            reject_extra(&args, "download")?;
            let result = download_bsfree_database(
                &paths,
                &BsFreeDownloadOptions::default(),
                &HttpsCheatSourceTransport::new(),
            )?;
            render(&result, json)?;
        }
        "import-local" => {
            if args.len() != 1 {
                return Err("import-local requires exactly one database path".into());
            }
            render(
                &import_local_bsfree_database(&paths, &PathBuf::from(&args[0]))?,
                json,
            )?;
        }
        "enable" | "disable" => {
            reject_extra(&args, &command)?;
            render(&set_bsfree_enabled(&paths, command == "enable")?, json)?;
        }
        "remove" => {
            let confirmed = take_flag(&mut args, "--confirm");
            reject_extra(&args, "remove")?;
            remove_local_bsfree_source(&paths, confirmed)?;
            if json {
                println!("{{\n  \"removed\": true,\n  \"provider\": \"bsfree-archive\"\n}}");
            } else {
                println!("Removed EmuWiz's local BSFree source copy only.");
            }
        }
        "systems" => {
            let page = page_options(&mut args, PageRequest::DEFAULT_GAME_LIMIT)?;
            reject_extra(&args, "systems")?;
            let catalogue = BsFreeCatalogue::open_installed(&paths)?;
            render(&catalogue.systems(page)?, json)?;
        }
        "devices" => {
            let page = page_options(&mut args, PageRequest::DEFAULT_GAME_LIMIT)?;
            reject_extra(&args, "devices")?;
            let catalogue = BsFreeCatalogue::open_installed(&paths)?;
            render(&catalogue.devices(page)?, json)?;
        }
        "search" => {
            let platform_id = take_value(&mut args, "--platform")?;
            let title = take_value(&mut args, "--title")?.unwrap_or_default();
            let version = take_value(&mut args, "--version")?;
            let device_id = take_i64(&mut args, "--device")?;
            let upstream_game_id = take_i64(&mut args, "--game-id")?;
            let page = page_options(&mut args, PageRequest::DEFAULT_GAME_LIMIT)?;
            reject_extra(&args, "search")?;
            let catalogue = BsFreeCatalogue::open_installed(&paths)?;
            render(
                &catalogue.search_games(&BsFreeGameSearchRequest {
                    platform_id,
                    title,
                    version,
                    device_id,
                    upstream_game_id,
                    page,
                })?,
                json,
            )?;
        }
        "game" => {
            if args.is_empty() {
                return Err("game requires an upstream UID".into());
            }
            let upstream_uid = args.remove(0).parse::<i64>()?;
            let page = page_options(&mut args, PageRequest::DEFAULT_CHEAT_LIMIT)?;
            reject_extra(&args, "game")?;
            let catalogue = BsFreeCatalogue::open_installed(&paths)?;
            let game = catalogue
                .game(upstream_uid)?
                .ok_or("BSFree upstream game UID was not found")?;
            let cheats = catalogue.cheats(upstream_uid, page)?;
            let gamecube = game.system.archivefs_platform_id.as_deref() == Some("GameCube");
            let installable_count = cheats
                .rows
                .iter()
                .filter(|cheat| {
                    gamecube
                        && classify_bsfree_gamecube_cheat(cheat)
                            .code_format
                            .is_installable()
                })
                .count();
            let cheat_capabilities = cheats
                .rows
                .iter()
                .map(|cheat| cheat_capability(cheat, gamecube))
                .collect::<Vec<_>>();
            #[derive(Debug, serde::Serialize)]
            struct Output<G, C> {
                provider: &'static str,
                browse_only: bool,
                install_capability: String,
                installable_cheat_count: usize,
                exact_revision_verified: bool,
                game: G,
                cheats: C,
                cheat_capabilities: Vec<CheatCapability>,
            }
            render(
                &Output {
                    provider: "BSFree Archive",
                    browse_only: installable_count == 0,
                    install_capability: if gamecube {
                        "Some GameCube codes can be installed via Dolphin (Gecko or Action Replay); \
                         unsupported formats remain browse-only"
                            .to_string()
                    } else {
                        "No EmuWiz adapter can install codes for this platform; browse-only"
                            .to_string()
                    },
                    installable_cheat_count: installable_count,
                    exact_revision_verified: false,
                    game,
                    cheats,
                    cheat_capabilities,
                },
                json,
            )?;
        }
        "gamecube-preview" => {
            gamecube_preview_or_apply(&paths, &mut args, json, false)?;
        }
        "gamecube-apply" => {
            gamecube_preview_or_apply(&paths, &mut args, json, true)?;
        }
        "gamecube-rollback" => {
            gamecube_rollback(&mut args, json)?;
        }
        _ => return Err(format!("unknown BSFree command {command:?}").into()),
    }
    Ok(())
}

/// Per-cheat installation capability, computed honestly from the classified
/// code format (GameCube) or a plain reference-only statement (everything
/// else).
#[derive(Debug, serde::Serialize)]
struct CheatCapability {
    upstream_id: i64,
    name: String,
    format: Option<BsFreeGameCubeCodeFormat>,
    installable: bool,
    capability: String,
}

fn cheat_capability(
    cheat: &archivefs_core::patch_manager::BsFreeCheat,
    gamecube: bool,
) -> CheatCapability {
    let classified = gamecube.then(|| classify_bsfree_gamecube_cheat(cheat));
    let format = classified.as_ref().map(|cheat| cheat.code_format);
    let installable = classified
        .as_ref()
        .is_some_and(|cheat| cheat.code_format.is_installable());
    let capability = match format {
        Some(BsFreeGameCubeCodeFormat::GeckoEquivalent) => {
            "installable via Dolphin (Gecko, byte-identical)".to_string()
        }
        Some(BsFreeGameCubeCodeFormat::ActionReplayNative) => {
            "installable via Dolphin (Action Replay, verbatim)".to_string()
        }
        Some(BsFreeGameCubeCodeFormat::Unsupported) => {
            "browse-only: contains an Action Replay command Dolphin refuses to run".to_string()
        }
        Some(BsFreeGameCubeCodeFormat::Malformed) => {
            "browse-only: not a well-formed hex-pair code".to_string()
        }
        None => "reference only: no EmuWiz adapter for this platform/format".to_string(),
    };
    CheatCapability {
        upstream_id: cheat.upstream_id,
        name: cheat.name.clone(),
        format,
        installable,
        capability,
    }
}

/// Shared implementation of `gamecube-preview` and `gamecube-apply`. The
/// provider (this module) supplies classified cheat data; the existing
/// GameCube GameHacking adapter and shared transaction layer produce the
/// preview and perform the mutation. Nothing here writes an emulator file
/// directly.
fn gamecube_preview_or_apply(
    paths: &BsFreePaths,
    args: &mut Vec<String>,
    json: bool,
    apply: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let archive = take_path(args, "--archive")?
        .ok_or("gamecube preview/apply requires --archive <selected-game-path>")?;
    let game_id = take_value(args, "--game-id")?
        .ok_or("gamecube preview/apply requires --game-id <six-character Game ID>")?;
    let revision = take_u16(args, "--revision")?;
    let configuration_path = take_path(args, "--configuration-path")?
        .ok_or("gamecube preview/apply requires --configuration-path <dolphin-profile-config>")?;
    let upstream_uid = take_i64(args, "--bsfree-game")?
        .ok_or("gamecube preview/apply requires --bsfree-game <upstream UID>")?;
    let staging_root = take_path(args, "--staging-root")?
        .ok_or("gamecube preview/apply requires --staging-root <managed-staging-dir>")?;
    let history_root = match take_path(args, "--history-root")? {
        Some(path) => path,
        None => default_shared_history_root().map_err(|failure| failure.detail)?,
    };
    let backup_root = match take_path(args, "--backup-root")? {
        Some(path) => path,
        None => default_shared_backup_root().map_err(|failure| failure.detail)?,
    };
    let select = take_value(args, "--select")?;
    let select_all = take_flag(args, "--select-all");
    let confirmed = take_flag(args, "--confirm");
    // Optional wrong-game guard: when the archive's display title is supplied,
    // the selected BSFree game must agree with it on platform + exact
    // normalized title before anything is staged or applied.
    let title = take_value(args, "--title")?;
    reject_extra(
        args,
        if apply {
            "gamecube-apply"
        } else {
            "gamecube-preview"
        },
    )?;

    if game_id.len() != 6
        || !game_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err("game-id must be an exact six-character Dolphin Game ID".into());
    }

    let catalogue = BsFreeCatalogue::open_installed(paths)?;
    let game = catalogue
        .game(upstream_uid)?
        .ok_or("BSFree upstream game UID was not found")?;
    if game.system.archivefs_platform_id.as_deref() != Some("GameCube") {
        return Err(format!(
            "BSFree game {:?} is not a GameCube game (system {:?}); only GameCube codes can be \
             installed via Dolphin, and this game is browse-only",
            game.name, game.system.name
        )
        .into());
    }
    if let Some(title) = title
        && !normalize_cli_title(&title).is_empty()
        && normalize_cli_title(&title) != normalize_cli_title(&game.name)
    {
        return Err(format!(
            "selected BSFree game {:?} does not match the archive title {:?}; refusing to apply \
             cheats for the wrong game. Review the search candidates and pick the exact BSFree game",
            game.name, title
        )
        .into());
    }

    let cheats = bsfree_gamecube_cheats(&catalogue, upstream_uid)?;
    let destination = load_dolphin_destination(&configuration_path, &game_id)?;
    let mut selection = BsFreeGameCubeCheatSelection::from_cheats(&cheats, &destination.document);
    if select_all {
        selection.select_all();
    } else if let Some(select) = select {
        for token in select
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            let id = token.parse::<i64>().map_err(|error| {
                format!("--select value {token:?} is not a numeric code ID: {error}")
            })?;
            let index = cheats
                .iter()
                .position(|cheat| cheat.upstream_id == id)
                .ok_or_else(|| {
                    format!("selected BSFree code ID {id} is not in this game's cheat list")
                })?;
            if !selection.set_selected(index, true) {
                return Err(format!(
                    "selected BSFree code ID {id} is not installable (browse-only format)"
                )
                .into());
            }
        }
    } else {
        return Err("gamecube preview/apply requires --select <id,id,...> or --select-all".into());
    }
    if selection.selected_count() == 0 {
        return Err("no installable cheats were selected".into());
    }

    let file_name = format!("{game_id}.ini");
    let staged = stage_bsfree_gamecube_install(
        &staging_root,
        &file_name,
        &destination.document,
        destination.existed,
        &cheats,
        &selection,
    )?;
    let preview = build_bsfree_gamecube_install_preview(&BsFreeGameCubeInstallPreviewRequest {
        selected_archive: archive.clone(),
        configuration_path: configuration_path.clone(),
        game_id: game_id.clone(),
        revision,
        staged: staged.staged.clone(),
    })?;

    let mut plan = build_shared_transaction_plan(
        &preview.report,
        "bsfree-gamecube",
        "Dolphin GameSettings",
        &staging_root,
    )
    .map_err(|failure| failure.detail)?;
    let staged_source = preview
        .report
        .entries
        .first()
        .and_then(|entry| entry.source_path.clone())
        .ok_or("preview produced no staged source")?;
    let staged_text = std::fs::read_to_string(&staged_source)?;
    require_dolphin_managed_gamehacking_verification(
        &mut plan,
        managed_names(&parse_dolphin_ini(&staged_text))
            .into_iter()
            .collect(),
    )
    .map_err(|failure| failure.detail)?;

    let operation_id = generate_shared_operation_id();
    let timestamp = now_unix_seconds();
    let will_apply = apply && confirmed;
    let options = SharedApplyOptions {
        dry_run: !will_apply,
        confirmation: will_apply.then(|| SharedApplyConfirmation {
            plan_id: plan.plan_id.clone(),
            general_approved: true,
            replacement_approved: true,
        }),
        operation_id: operation_id.clone(),
        timestamp_unix_seconds: timestamp,
        current_context: plan.context.clone(),
        history_root,
        backup_root,
    };
    let result = execute_shared_apply(&plan, &options);

    #[derive(Debug, serde::Serialize)]
    struct Output {
        provider: &'static str,
        operation: &'static str,
        applied: bool,
        game_title: String,
        bsfree_game_uid: i64,
        game_id: String,
        platform: &'static str,
        destination: String,
        selected_cheats: Vec<String>,
        findings: Vec<archivefs_core::patch_manager::BsFreeDedupFinding>,
        skipped_duplicates: Vec<String>,
        skipped_unselectable: Vec<String>,
        report: archivefs_core::patch_manager::SharedPreviewReport,
        journal_status: String,
        journal_path: Option<String>,
        operation_id: String,
    }
    render(
        &Output {
            provider: "BSFree Archive",
            operation: if will_apply { "apply" } else { "preview" },
            applied: will_apply,
            game_title: game.name.clone(),
            bsfree_game_uid: upstream_uid,
            game_id: game_id.clone(),
            platform: "GameCube",
            destination: configuration_path.display().to_string(),
            selected_cheats: selection
                .entries
                .iter()
                .filter(|entry| entry.selected)
                .map(|entry| entry.name.clone())
                .collect(),
            findings: staged.findings,
            skipped_duplicates: staged.skipped_duplicates,
            skipped_unselectable: staged.skipped_unselectable,
            report: preview.report,
            journal_status: format!("{:?}", result.journal.status),
            journal_path: result.journal_path.map(|path| path.display().to_string()),
            operation_id,
        },
        json,
    )?;
    Ok(())
}

/// `gamecube-rollback`: previews and (with `--confirm`) executes the shared
/// rollback for a BSFree GameCube apply journal.
fn gamecube_rollback(args: &mut Vec<String>, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let journal = take_path(args, "--journal")?
        .ok_or("gamecube-rollback requires --journal <operation-journal-json>")?;
    let configuration_path = take_path(args, "--configuration-path")?
        .ok_or("gamecube-rollback requires --configuration-path <dolphin-profile-config>")?;
    let backup_root = match take_path(args, "--backup-root")? {
        Some(path) => path,
        None => default_shared_backup_root().map_err(|failure| failure.detail)?,
    };
    let history_root = match take_path(args, "--history-root")? {
        Some(path) => path,
        None => default_shared_history_root().map_err(|failure| failure.detail)?,
    };
    let confirmed = take_flag(args, "--confirm");
    reject_extra(args, "gamecube-rollback")?;

    let preview = preview_shared_rollback(&journal, &configuration_path, &backup_root);
    if !preview.available && !confirmed {
        return Err(
            "rollback is not available for this journal (already rolled back, destination changed, \
             or a backup is missing); use --confirm only after reviewing the preview"
                .into(),
        );
    }
    let result = confirmed.then(|| {
        execute_shared_rollback(
            &preview,
            &SharedRollbackOptions {
                confirmation: SharedRollbackConfirmation {
                    preview_id: preview.preview_id.clone(),
                    approved: true,
                },
                rollback_operation_id: generate_shared_operation_id(),
                timestamp_unix_seconds: now_unix_seconds(),
                history_root,
                backup_root,
            },
        )
    });

    #[derive(Debug, serde::Serialize)]
    struct Output {
        provider: &'static str,
        operation: &'static str,
        journal: String,
        available: bool,
        status: String,
        entries: Vec<archivefs_core::patch_manager::SharedRollbackEntry>,
    }
    render(
        &Output {
            provider: "BSFree Archive",
            operation: "rollback",
            journal: journal.display().to_string(),
            available: preview.available,
            status: match &result {
                Some(result) => format!("{:?}", result.status),
                None => "preview (not approved); re-run with --confirm to roll back".to_string(),
            },
            entries: preview.entries,
        },
        json,
    )?;
    Ok(())
}

fn render<T: serde::Serialize + std::fmt::Debug>(
    value: &T,
    json: bool,
) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{value:#?}");
    }
    Ok(())
}

fn page_options(args: &mut Vec<String>, default: u16) -> Result<PageRequest, String> {
    let offset = take_u32(args, "--offset")?.unwrap_or(0);
    let limit = take_u16(args, "--limit")?.unwrap_or(default);
    Ok(PageRequest { offset, limit }.bounded())
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(index) = args.iter().position(|argument| argument == flag) {
        args.remove(index);
        true
    } else {
        false
    }
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    let Some(index) = args.iter().position(|argument| argument == flag) else {
        return Ok(None);
    };
    if index + 1 >= args.len() {
        return Err(format!("{flag} requires a value"));
    }
    args.remove(index);
    Ok(Some(args.remove(index)))
}

fn take_path(args: &mut Vec<String>, flag: &str) -> Result<Option<PathBuf>, String> {
    Ok(take_value(args, flag)?.map(PathBuf::from))
}

fn take_i64(args: &mut Vec<String>, flag: &str) -> Result<Option<i64>, String> {
    take_value(args, flag)?
        .map(|value| value.parse::<i64>().map_err(|error| error.to_string()))
        .transpose()
}

fn take_u32(args: &mut Vec<String>, flag: &str) -> Result<Option<u32>, String> {
    take_value(args, flag)?
        .map(|value| value.parse::<u32>().map_err(|error| error.to_string()))
        .transpose()
}

fn take_u16(args: &mut Vec<String>, flag: &str) -> Result<Option<u16>, String> {
    take_value(args, flag)?
        .map(|value| value.parse::<u16>().map_err(|error| error.to_string()))
        .transpose()
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn normalize_cli_title(value: &str) -> String {
    // Mirrors the core matcher's normalization: strip parenthesized/bracketed
    // region and edition markers before the exact comparison so "(USA)" etc.
    // do not cause a false wrong-game refusal.
    let mut without_markers = String::with_capacity(value.len());
    let mut depth = 0u8;
    for character in value.chars() {
        match character {
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => without_markers.push(character),
            _ => {}
        }
    }
    without_markers
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn reject_extra(args: &[String], command: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("BSFree {command} does not accept {args:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_flags_are_bounded_and_typed() {
        let mut args = vec![
            "--offset".to_string(),
            "10".to_string(),
            "--limit".to_string(),
            "65000".to_string(),
        ];
        let page = page_options(&mut args, 50).unwrap();
        assert_eq!(page.offset, 10);
        assert_eq!(page.limit, PageRequest::HARD_LIMIT);
        assert!(args.is_empty());
    }

    #[test]
    fn no_command_implicitly_downloads() {
        let source = include_str!("bsfree.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert_eq!(source.matches("download_bsfree_database(").count(), 1);
        assert!(source.contains("\"download\" =>"));
    }

    #[test]
    fn gamecube_title_guard_compares_normalized_titles() {
        assert_eq!(
            normalize_cli_title("Luigi's Mansion (USA)"),
            normalize_cli_title("Luigis Mansion")
        );
        assert_ne!(
            normalize_cli_title("Pokemon XD"),
            normalize_cli_title("The Sims")
        );
        assert!(
            normalize_cli_title("  !?  ").is_empty(),
            "punct-only titles never match"
        );
    }

    #[test]
    fn cheat_capability_reflects_installable_and_reference_only_codes() {
        let gamecube = archivefs_core::patch_manager::BsFreeCheat {
            upstream_id: 1,
            name: "Lives".to_string(),
            note: None,
            code: "042318AC 3B8003E7".to_string(),
            section: None,
            author: None,
            device: archivefs_core::patch_manager::BsFreeDeviceSummary {
                upstream_id: 6,
                name: "Action Replay".to_string(),
                compatibility:
                    archivefs_core::patch_manager::DeviceFormatCompatibility::PotentiallyConvertible,
            },
            compatibility:
                archivefs_core::patch_manager::DeviceFormatCompatibility::PotentiallyConvertible,
            truncated_fields: Vec::new(),
        };
        let capability = cheat_capability(&gamecube, true);
        assert!(capability.installable);
        assert!(capability.capability.contains("Dolphin"));

        let ps2 = archivefs_core::patch_manager::BsFreeCheat {
            upstream_id: 2,
            name: "Code".to_string(),
            note: None,
            code: "2A123456 00000001".to_string(),
            section: None,
            author: None,
            device: archivefs_core::patch_manager::BsFreeDeviceSummary {
                upstream_id: 9,
                name: "CodeBreaker".to_string(),
                compatibility:
                    archivefs_core::patch_manager::DeviceFormatCompatibility::PotentiallyConvertible,
            },
            compatibility:
                archivefs_core::patch_manager::DeviceFormatCompatibility::PotentiallyConvertible,
            truncated_fields: Vec::new(),
        };
        let capability = cheat_capability(&ps2, false);
        assert!(!capability.installable);
        assert_eq!(
            capability.capability,
            "reference only: no EmuWiz adapter for this platform/format"
        );
    }

    #[test]
    fn gamecube_commands_are_registered_and_reuse_the_shared_pipeline() {
        let source = include_str!("bsfree.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains("\"gamecube-preview\" =>"));
        assert!(source.contains("\"gamecube-apply\" =>"));
        assert!(source.contains("\"gamecube-rollback\" =>"));
        // The provider must route through the existing adapter + shared apply,
        // never write an emulator file directly.
        assert!(source.contains("stage_bsfree_gamecube_install("));
        assert!(source.contains("execute_shared_apply("));
        assert!(source.contains("execute_shared_rollback("));
    }
}

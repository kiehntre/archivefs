use std::path::PathBuf;

use archivefs_core::patch_manager::{
    BsFreeCatalogue, BsFreeDownloadOptions, BsFreeGameSearchRequest, BsFreePaths,
    HttpsCheatSourceTransport, PageRequest, ReadOnlyCheatCatalogue, default_bsfree_source_root,
    download_bsfree_database, import_local_bsfree_database, inspect_bsfree_source,
    remove_local_bsfree_source, set_bsfree_enabled, validate_installed_bsfree_source,
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
                println!("Removed ArchiveFS's local BSFree source copy only.");
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
            #[derive(Debug, serde::Serialize)]
            struct Output<G, C> {
                provider: &'static str,
                browse_only: bool,
                exact_revision_verified: bool,
                game: G,
                cheats: C,
            }
            render(
                &Output {
                    provider: "BSFree Archive",
                    browse_only: true,
                    exact_revision_verified: false,
                    game,
                    cheats,
                },
                json,
            )?;
        }
        _ => return Err(format!("unknown BSFree command {command:?}").into()),
    }
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
}

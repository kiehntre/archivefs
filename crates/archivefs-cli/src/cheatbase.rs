use std::path::PathBuf;

use archivefs_core::patch_manager::{
    CheatBaseCatalogue, CheatBaseDownloadOptions, CheatBaseGameSearchRequest,
    CheatBaseHashAlgorithm, CheatBasePaths, HttpsCheatSourceTransport, PageRequest,
    ReadOnlyCheatCatalogue, default_cheatbase_source_root, download_cheatbase_database,
    import_local_cheatbase_database, inspect_cheatbase_source, remove_local_cheatbase_source,
    set_cheatbase_enabled, validate_installed_cheatbase_source,
};

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args;
    let json = take_flag(&mut args, "--json");
    let root = take_value(&mut args, "--root")?
        .map(PathBuf::from)
        .unwrap_or(default_cheatbase_source_root()?);
    let paths = CheatBasePaths::at(root);
    let command = args
        .first()
        .cloned()
        .ok_or("cheats source cheatbase requires a command")?;
    args.remove(0);
    match command.as_str() {
        "status" => {
            reject_extra(&args, "status")?;
            render(&inspect_cheatbase_source(&paths)?, json)?;
        }
        "validate" => {
            reject_extra(&args, "validate")?;
            render(&validate_installed_cheatbase_source(&paths)?, json)?;
        }
        "download" => {
            reject_extra(&args, "download")?;
            render(
                &download_cheatbase_database(
                    &paths,
                    &CheatBaseDownloadOptions::default(),
                    &HttpsCheatSourceTransport::new(),
                )?,
                json,
            )?;
        }
        "import-local" => {
            if args.len() != 1 {
                return Err("import-local requires exactly one SQLite path".into());
            }
            render(
                &import_local_cheatbase_database(&paths, &PathBuf::from(&args[0]))?,
                json,
            )?;
        }
        "enable" | "disable" => {
            reject_extra(&args, &command)?;
            render(&set_cheatbase_enabled(&paths, command == "enable")?, json)?;
        }
        "remove" => {
            let confirmed = take_flag(&mut args, "--confirm");
            reject_extra(&args, "remove")?;
            remove_local_cheatbase_source(&paths, confirmed)?;
            render(
                &RemovalOutput {
                    provider: "cheatbase",
                    removed: true,
                },
                json,
            )?;
        }
        "systems" => {
            let page = page_options(&mut args, PageRequest::DEFAULT_GAME_LIMIT)?;
            reject_extra(&args, "systems")?;
            render(
                &CheatBaseCatalogue::open_installed(&paths)?.systems(page)?,
                json,
            )?;
        }
        "devices" => {
            let page = page_options(&mut args, PageRequest::DEFAULT_GAME_LIMIT)?;
            reject_extra(&args, "devices")?;
            render(
                &CheatBaseCatalogue::open_installed(&paths)?.devices(page)?,
                json,
            )?;
        }
        "search" => {
            let platform_id = take_value(&mut args, "--platform")?;
            let title = take_value(&mut args, "--title")?.unwrap_or_default();
            let region = take_value(&mut args, "--region")?;
            let upstream_release_id = take_i64(&mut args, "--release-id")?;
            let page = page_options(&mut args, PageRequest::DEFAULT_GAME_LIMIT)?;
            reject_extra(&args, "search")?;
            render(
                &CheatBaseCatalogue::open_installed(&paths)?.search_games(
                    &CheatBaseGameSearchRequest {
                        platform_id,
                        title,
                        region,
                        upstream_release_id,
                        page,
                    },
                )?,
                json,
            )?;
        }
        "lookup-hash" => {
            let algorithm = take_value(&mut args, "--algorithm")?
                .ok_or("lookup-hash requires --algorithm")?
                .parse::<CheatBaseHashAlgorithm>()?;
            let hash = take_value(&mut args, "--hash")?.ok_or("lookup-hash requires --hash")?;
            let platform = take_value(&mut args, "--platform")?;
            let page = page_options(&mut args, PageRequest::DEFAULT_GAME_LIMIT)?;
            reject_extra(&args, "lookup-hash")?;
            render(
                &CheatBaseCatalogue::open_installed(&paths)?.lookup_hash(
                    algorithm,
                    &hash,
                    platform.as_deref(),
                    page,
                )?,
                json,
            )?;
        }
        "lookup-serial" => {
            let serial =
                take_value(&mut args, "--serial")?.ok_or("lookup-serial requires --serial")?;
            let platform =
                take_value(&mut args, "--platform")?.ok_or("lookup-serial requires --platform")?;
            let region = take_value(&mut args, "--region")?;
            let page = page_options(&mut args, PageRequest::DEFAULT_GAME_LIMIT)?;
            reject_extra(&args, "lookup-serial")?;
            render(
                &CheatBaseCatalogue::open_installed(&paths)?.lookup_serial(
                    &serial,
                    &platform,
                    region.as_deref(),
                    page,
                )?,
                json,
            )?;
        }
        "game" => {
            if args.is_empty() {
                return Err("game requires an upstream release ID".into());
            }
            let release_id = args.remove(0).parse::<i64>()?;
            let page = page_options(&mut args, PageRequest::DEFAULT_CHEAT_LIMIT)?;
            reject_extra(&args, "game")?;
            let catalogue = CheatBaseCatalogue::open_installed(&paths)?;
            let game = catalogue
                .game(release_id)?
                .ok_or("CheatBase release ID was not found")?;
            let cheats = catalogue.cheats(release_id, page)?;
            render(
                &GameOutput {
                    provider: "CheatBase",
                    cheat_coverage_platforms: vec!["Nintendo DS"],
                    identity_metadata_platforms: "multiple systems",
                    browse_only: true,
                    install_supported: false,
                    revision_verified: false,
                    game,
                    cheats,
                },
                json,
            )?;
        }
        _ => return Err(format!("unknown CheatBase command {command:?}").into()),
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct RemovalOutput {
    provider: &'static str,
    removed: bool,
}
#[derive(Debug, serde::Serialize)]
struct GameOutput<G, C> {
    provider: &'static str,
    cheat_coverage_platforms: Vec<&'static str>,
    identity_metadata_platforms: &'static str,
    browse_only: bool,
    install_supported: bool,
    revision_verified: bool,
    game: G,
    cheats: C,
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
    if let Some(index) = args.iter().position(|v| v == flag) {
        args.remove(index);
        true
    } else {
        false
    }
}
fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    let Some(index) = args.iter().position(|v| v == flag) else {
        return Ok(None);
    };
    if index + 1 >= args.len() {
        return Err(format!("{flag} requires a value"));
    }
    args.remove(index);
    Ok(Some(args.remove(index)))
}
fn take_i64(args: &mut Vec<String>, flag: &str) -> Result<Option<i64>, String> {
    take_value(args, flag)?
        .map(|v| v.parse::<i64>().map_err(|e| e.to_string()))
        .transpose()
}
fn take_u32(args: &mut Vec<String>, flag: &str) -> Result<Option<u32>, String> {
    take_value(args, flag)?
        .map(|v| v.parse::<u32>().map_err(|e| e.to_string()))
        .transpose()
}
fn take_u16(args: &mut Vec<String>, flag: &str) -> Result<Option<u16>, String> {
    take_value(args, flag)?
        .map(|v| v.parse::<u16>().map_err(|e| e.to_string()))
        .transpose()
}
fn reject_extra(args: &[String], command: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("CheatBase {command} does not accept {args:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_is_bounded() {
        let mut args = vec!["--limit".to_string(), "65535".to_string()];
        assert_eq!(
            page_options(&mut args, 50).unwrap().limit,
            PageRequest::HARD_LIMIT
        );
    }
    #[test]
    fn no_command_installs_or_implicitly_downloads() {
        let source = include_str!("cheatbase.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(!source.contains("Install selected"));
        assert!(!source.contains("emulator"));
    }
    #[test]
    fn game_json_is_explicitly_browse_only() {
        let output = GameOutput {
            provider: "CheatBase",
            cheat_coverage_platforms: vec!["Nintendo DS"],
            identity_metadata_platforms: "multiple systems",
            browse_only: true,
            install_supported: false,
            revision_verified: false,
            game: "g",
            cheats: "c",
        };
        let json = serde_json::to_value(output).unwrap();
        assert_eq!(json["browse_only"], true);
        assert_eq!(json["install_supported"], false);
        assert_eq!(json["cheat_coverage_platforms"][0], "Nintendo DS");
        assert_eq!(json["identity_metadata_platforms"], "multiple systems");
    }

    #[test]
    fn release_packaging_has_no_database_input_or_database_member() {
        let script = include_str!("../../../scripts/build-release.sh");
        assert!(!script.contains("cheatbase.sqlite"));
        assert!(!script.contains("bsfree.db"));
        assert!(!script.contains("cheat-sources"));
        for member in [
            "archivefs-cli",
            "archivefs-gui",
            "install.sh",
            "README.md",
            "CHANGELOG.md",
            "LICENSE",
            "config.toml.example",
        ] {
            assert!(script.contains(member), "missing release member {member}");
        }
    }
}

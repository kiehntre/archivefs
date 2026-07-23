use std::path::PathBuf;

use archivefs_core::patch_manager::{
    CheatSourceError, CheatSourceFetchOptions, CheatSourceFetchResult, CheatSourceInspection,
    HttpsCheatSourceTransport, default_cheat_source_cache_root, fetch_retroarch_cheat_source,
    inspect_retroarch_cheat_source, inspect_retroarch_cheat_source_snapshot,
    list_retroarch_cheat_sources,
};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct SourceOptions {
    pub json: bool,
    pub force_refresh: bool,
    pub offline: bool,
    pub expected_sha256: Option<String>,
    pub cache_root: PathBuf,
    pub max_download_bytes: Option<u64>,
}

#[derive(Serialize)]
struct JsonFailure<'a> {
    schema_version: u32,
    status: &'static str,
    error: &'a CheatSourceError,
}

pub fn run_list(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let options = parse_source_options(args, false)?;
    let report = match list_retroarch_cheat_sources(&options.cache_root) {
        Ok(report) => report,
        Err(error) => return render_failure(error, options.json),
    };
    if options.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if report.entries.is_empty() {
            println!("No trusted RetroArch cheat sources are registered.");
        }
        for entry in report.entries {
            println!("{} — {}", entry.source.source_id, entry.source.display_name);
            println!(
                "  Trust: {}  Enabled: {}  Host: {}",
                entry.trust_status, entry.source.enabled, entry.source.permitted_host
            );
            println!(
                "  Cache: {:?}  Usable for setup: {}",
                entry.freshness, entry.setup_usable
            );
            if let Some(version) = entry.current_cached_version {
                println!("  Version: {version}");
            }
            if let Some(timestamp) = entry.fetched_at_unix_seconds {
                println!("  Fetched: {timestamp}");
            }
            if let Some(hash) = entry.archive_sha256 {
                println!("  Archive SHA-256: {hash}");
            }
            if let Some(count) = entry.catalogue_file_count {
                println!("  Catalogue files: {count}");
            }
            for warning in entry.warnings {
                println!("  Warning: {warning}");
            }
        }
    }
    Ok(())
}

pub fn run_fetch(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let source_id = extract_source_id(&mut args, "retroarch-cheat-source-fetch")?;
    let options = parse_source_options(args, true)?;
    match fetch_source(&source_id, &options) {
        Ok(result) => {
            render_fetch(&result, options.json)?;
            Ok(())
        }
        Err(error) => render_failure(error, options.json),
    }
}

pub fn run_inspect(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let source_id = extract_source_id(&mut args, "retroarch-cheat-source-inspect")?;
    let options = parse_source_options(args, false)?;
    let inspected = if source_id.contains(std::path::MAIN_SEPARATOR) {
        inspect_retroarch_cheat_source_snapshot(&PathBuf::from(&source_id))
    } else {
        inspect_retroarch_cheat_source(&source_id, &options.cache_root)
    };
    match inspected {
        Ok(result) => {
            render_inspection(&result, options.json)?;
            Ok(())
        }
        Err(error) => render_failure(error, options.json),
    }
}

pub fn fetch_source(
    source_id: &str,
    options: &SourceOptions,
) -> Result<CheatSourceFetchResult, CheatSourceError> {
    let core_options = CheatSourceFetchOptions {
        cache_root: options.cache_root.clone(),
        force_refresh: options.force_refresh,
        offline: options.offline,
        expected_sha256: options.expected_sha256.clone(),
        max_download_bytes: options.max_download_bytes,
        cancellation: None,
    };
    fetch_retroarch_cheat_source(source_id, &core_options, &HttpsCheatSourceTransport::new())
}

pub fn parse_source_options(
    mut args: Vec<String>,
    allow_fetch: bool,
) -> Result<SourceOptions, Box<dyn std::error::Error>> {
    let json = take_flag(&mut args, "--json");
    let force_refresh = take_flag(&mut args, "--force-refresh");
    let offline = take_flag(&mut args, "--offline");
    let expected_sha256 = take_value(&mut args, "--expected-sha256")?;
    let cache_root = take_value(&mut args, "--cache-root")?
        .map(PathBuf::from)
        .unwrap_or(default_cheat_source_cache_root()?);
    let max_download_bytes = take_value(&mut args, "--max-download-bytes")?
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|_| "--max-download-bytes requires a positive integer")?;
    if max_download_bytes == Some(0) {
        return Err("--max-download-bytes must be greater than zero".into());
    }
    if !allow_fetch
        && (force_refresh || offline || expected_sha256.is_some() || max_download_bytes.is_some())
    {
        return Err("this command accepts only --json and --cache-root <path>".into());
    }
    if !args.is_empty() {
        return Err(format!("unexpected argument: {}", args.join(" ")).into());
    }
    Ok(SourceOptions {
        json,
        force_refresh,
        offline,
        expected_sha256,
        cache_root,
        max_download_bytes,
    })
}

fn extract_source_id(
    args: &mut Vec<String>,
    command: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if args.first().is_none_or(|value| value.starts_with('-')) {
        return Err(format!("{command} requires one source ID").into());
    }
    Ok(args.remove(0))
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(index) = args.iter().position(|value| value == flag) {
        args.remove(index);
        true
    } else {
        false
    }
}
fn take_value(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if let Some(index) = args.iter().position(|value| value == flag) {
        args.remove(index);
        if index >= args.len() {
            return Err(format!("{flag} requires a value").into());
        }
        Ok(Some(args.remove(index)))
    } else {
        Ok(None)
    }
}

fn render_fetch(result: &CheatSourceFetchResult, json: bool) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(&fetch_json(result))?);
    } else {
        println!("RetroArch cheat source: {}", result.source.display_name);
        println!("  Status: {:?}", result.status);
        println!("  Catalogue: {}", result.local_catalogue_path.display);
        println!("  Archive SHA-256: {}", result.manifest.archive_sha256);
        println!("  Downloaded bytes: {}", result.manifest.downloaded_bytes);
        println!(
            "  Catalogue files: {}  Cheats: {}",
            result.manifest.catalogue_file_count, result.manifest.valid_cheat_count
        );
        println!(
            "  Validation complete: {}  Stale: {}",
            result.manifest.validation_complete, result.stale
        );
        println!("  Fetching never installs cheats.");
    }
    Ok(())
}

fn render_inspection(result: &CheatSourceInspection, json: bool) -> Result<(), serde_json::Error> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&inspection_json(result))?
        );
    } else {
        println!(
            "RetroArch cheat source: {} ({})",
            result.source.display_name, result.source.source_id
        );
        println!("  URL: {}", result.source.download_url);
        println!("  Permitted host: {}", result.source.permitted_host);
        println!(
            "  Freshness: {:?}  Setup usable: {}",
            result.freshness, result.setup_usable
        );
        if let Some(path) = &result.current_catalogue_path {
            println!("  Catalogue: {}", path.display);
        }
        if let Some(manifest) = &result.manifest {
            println!("  Fetched: {}", manifest.fetched_at_unix_seconds);
            println!("  Archive SHA-256: {}", manifest.archive_sha256);
            println!(
                "  Files: {}  Cheats: {}  Complete: {}",
                manifest.catalogue_file_count,
                manifest.valid_cheat_count,
                manifest.validation_complete
            );
        }
        if let Some(error) = &result.last_error {
            println!("  Last error: {error}");
        }
    }
    Ok(())
}

fn manifest_json(
    manifest: &archivefs_core::patch_manager::CheatSourceManifest,
) -> serde_json::Value {
    serde_json::json!({
        "format_version": manifest.format_version,
        "source_id": manifest.source_id,
        "source_url": manifest.source_url,
        "canonical_repository_url": manifest.canonical_repository_url,
        "resolved_revision": manifest.resolved_revision,
        "pinned_version": manifest.pinned_version,
        "fetched_at_unix_seconds": manifest.fetched_at_unix_seconds,
        "downloaded_bytes": manifest.downloaded_bytes,
        "extracted_bytes": manifest.extracted_bytes,
        "archive_entry_count": manifest.archive_entry_count,
        "archive_sha256": manifest.archive_sha256,
        "response_content_type": manifest.response_content_type,
        "response_etag": manifest.response_etag,
        "response_last_modified": manifest.response_last_modified,
        "catalogue_file_count": manifest.catalogue_file_count,
        "valid_cheat_count": manifest.valid_cheat_count,
        "malformed_cheat_count": manifest.malformed_cheat_count,
        "skipped_entry_count": manifest.skipped_entry_count,
        "discovered_platforms": manifest.discovered_platforms,
        "validation_complete": manifest.validation_complete,
        "warnings": manifest.warnings,
        "catalogue_relative_path": manifest.catalogue_relative_path,
        "cache_relative_path": manifest.cache_relative_path,
        "file_manifest_count": manifest.files.len(),
    })
}

fn fetch_json(result: &CheatSourceFetchResult) -> serde_json::Value {
    serde_json::json!({
        "schema_version": result.schema_version,
        "status": result.status,
        "source": result.source,
        "local_catalogue_path": result.local_catalogue_path,
        "immutable_snapshot_path": result.immutable_snapshot_path,
        "manifest": manifest_json(&result.manifest),
        "freshness": result.freshness,
        "from_cache": result.from_cache,
        "stale": result.stale,
        "warnings": result.warnings,
    })
}

fn inspection_json(result: &CheatSourceInspection) -> serde_json::Value {
    serde_json::json!({
        "schema_version": result.schema_version,
        "source": result.source,
        "cache_root": result.cache_root,
        "current_snapshot_path": result.current_snapshot_path,
        "current_catalogue_path": result.current_catalogue_path,
        "manifest": result.manifest.as_ref().map(manifest_json),
        "freshness": result.freshness,
        "last_fetch_succeeded": result.last_fetch_succeeded,
        "last_error": result.last_error,
        "setup_usable": result.setup_usable,
        "warnings": result.warnings,
    })
}

fn render_failure(error: CheatSourceError, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&JsonFailure {
                schema_version: 1,
                status: "failed",
                error: &error
            })?
        );
    }
    Err(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_options_are_strict_and_deterministic() {
        let options = parse_source_options(
            vec!["--json".into(), "--cache-root".into(), "/cache".into()],
            false,
        )
        .unwrap();
        assert!(options.json);
        assert_eq!(options.cache_root, PathBuf::from("/cache"));
        assert!(parse_source_options(vec!["--offline".into()], false).is_err());
        assert!(
            parse_source_options(vec!["--max-download-bytes".into(), "0".into()], true).is_err()
        );
    }

    #[test]
    fn source_list_json_has_a_versioned_stable_top_level() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-empty-source-list-{}",
            std::process::id()
        ));
        let value = serde_json::to_value(list_retroarch_cheat_sources(&root).unwrap()).unwrap();
        assert_eq!(
            value["schema_version"],
            archivefs_core::patch_manager::CHEAT_SOURCE_RESULT_SCHEMA_VERSION
        );
        assert!(value["entries"].is_array());
        assert_eq!(
            value["entries"][0]["source"]["source_id"],
            "libretro-buildbot-cheats"
        );
        assert_eq!(value["entries"][0]["freshness"], "missing");
        assert!(!root.exists());
    }
}

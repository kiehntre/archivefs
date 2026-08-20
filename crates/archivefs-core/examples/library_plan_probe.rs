//! Read-only library-planning probe (Mass Library Batch 10). Builds the same
//! [`archivefs_core::platform_evidence_fusion::identity_orchestrator::IdentityResult`]
//! that `cartridge_probe` builds - by calling the *same* header/content
//! observers, not a re-implementation of them - then runs it through the new
//! [`archivefs_core::platform_evidence_fusion::library_planning`] bridge and
//! prints the resulting plan via
//! [`archivefs_core::platform_evidence_fusion::library_plan_presentation::render_library_plan_text`].
//!
//! Nothing is ever written: no rename, move, copy, delete, symlink, RomM
//! write, or DB migration happens anywhere in this file. `--dest-root` only
//! feeds a hypothetical destination *preview* string built in memory.
//!
//! ```text
//! cargo run -p archivefs-core --example library_plan_probe -- <file> [--dest-root DIR] [--dat DAT_FILE]
//! cargo run -p archivefs-core --example library_plan_probe -- --dir DIR [--max N] [--dest-root DIR] [--dat DAT_FILE]
//! ```

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use archivefs_core::atari7800_header_evidence::{observe_a78_evidence, parse_a78_header};
use archivefs_core::content_evidence::ContentEvidence;
use archivefs_core::dat::index::DatIndex;
use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::parsers::parse_dat_file;
use archivefs_core::dat::rom_organisation::OrganisationMode;
use archivefs_core::disc_evidence_collector::{
    MAX_CHD_BYTES, collect_chd_evidence, collect_plain_iso_evidence,
};
use archivefs_core::gb_header_evidence::{observe_gb_evidence, parse_gb_header};
use archivefs_core::gba_header_evidence::{observe_gba_evidence, parse_gba_header};
use archivefs_core::lynx_header_evidence::{observe_lynx_evidence, parse_lynx_header};
use archivefs_core::megadrive_header_evidence::{
    observe_megadrive_evidence, parse_megadrive_header,
};
use archivefs_core::n64_byte_order::{N64ByteOrder, detect_n64_byte_order};
use archivefs_core::n64_header_evidence::{observe_n64_evidence, parse_n64_header};
use archivefs_core::nes_header_evidence::{observe_ines_evidence, parse_ines_header};
use archivefs_core::platform_evidence_fusion::dat_hash_representation::{
    ByteRepresentation, RepresentationHashes, audit_representation, compare_representations,
    hash_bytes, normalized_header_stripped_representation, normalized_n64_representation,
    normalized_smd_representation,
};
use archivefs_core::platform_evidence_fusion::duplicate_taxonomy::{
    DuplicateClass, group_duplicates,
};
use archivefs_core::platform_evidence_fusion::fuse_platform_evidence;
use archivefs_core::platform_evidence_fusion::identity_orchestrator::{
    IdentityInspectionInput, IdentityResult, inspect_identity,
};
use archivefs_core::platform_evidence_fusion::library_grouping::group_multidisc_sets;
use archivefs_core::platform_evidence_fusion::library_plan_presentation::{
    present_library_plan, render_library_plan_text,
};
use archivefs_core::platform_evidence_fusion::library_planning::{
    LibraryPlanInput, LibraryPlanningContext, no_slug_mapping, plan_library,
};
use archivefs_core::sms_gg_header_evidence::{
    find_tmr_sega_header, observe_tmr_sega_evidence, parse_tmr_sega_header,
};
use archivefs_core::snes_header_evidence::{best_snes_header_candidate, observe_snes_evidence};
use archivefs_core::ws_header_evidence::observe_ws_evidence;

const DEFAULT_MAX_ITEMS: usize = 25;

struct Args {
    single_file: Option<PathBuf>,
    dir: Option<PathBuf>,
    max_items: usize,
    dest_root: PathBuf,
    dat_path: Option<PathBuf>,
    demo_slug: bool,
}

/// `--demo-slug` now resolves through the real Batch 11 production mapping
/// ([`archivefs_core::platform_evidence_fusion::romm_platform_mapping::production_romm_slug`])
/// with no override map and no live identity cache - so this exercises
/// exactly the same vetted static table (tier 3) a real caller with no
/// connected RomM instance would see. Still opt-in via the flag, since
/// `no_slug_mapping` (the honest zero-mapping default) remains this
/// probe's own default.
fn demo_slug_for_platform(platform: &str) -> Option<String> {
    use archivefs_core::platform_evidence_fusion::romm_platform_mapping::{
        FrontendPlatformMapping, production_romm_slug,
    };
    production_romm_slug(platform, &FrontendPlatformMapping::default(), None)
}

fn parse_args() -> Option<Args> {
    let mut raw: Vec<String> = env::args().skip(1).collect();
    let mut dest_root = None;
    let mut dat_path = None;
    let mut dir = None;
    let mut max_items = DEFAULT_MAX_ITEMS;
    let mut demo_slug = false;

    let mut i = 0;
    let mut positional = Vec::new();
    while i < raw.len() {
        match raw[i].as_str() {
            "--dest-root" => {
                dest_root = raw.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--dat" => {
                dat_path = raw.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--dir" => {
                dir = raw.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--max" => {
                max_items = raw
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(DEFAULT_MAX_ITEMS);
                i += 2;
            }
            "--demo-slug" => {
                demo_slug = true;
                i += 1;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }
    raw.clear();

    Some(Args {
        demo_slug,
        single_file: positional.first().map(PathBuf::from),
        dir,
        max_items,
        dest_root: dest_root.unwrap_or_else(|| PathBuf::from("/tmp/library_plan_probe_preview")),
        dat_path,
    })
}

fn main() -> ExitCode {
    let Some(args) = parse_args() else {
        eprintln!(
            "usage: library_plan_probe <file> | --dir DIR [--max N] [--dest-root DIR] [--dat DAT_FILE] [--demo-slug]"
        );
        return ExitCode::FAILURE;
    };

    let files: Vec<PathBuf> = if let Some(dir) = &args.dir {
        let Ok(read) = std::fs::read_dir(dir) else {
            eprintln!("could not read directory {}", dir.display());
            return ExitCode::FAILURE;
        };
        let mut found: Vec<PathBuf> = read
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        found.sort();
        found.truncate(args.max_items);
        found
    } else if let Some(file) = &args.single_file {
        vec![file.clone()]
    } else {
        eprintln!(
            "usage: library_plan_probe <file> | --dir DIR [--max N] [--dest-root DIR] [--dat DAT_FILE] [--demo-slug]"
        );
        return ExitCode::FAILURE;
    };

    if files.is_empty() {
        println!(
            "No files found (scan is bounded to --max {}).",
            args.max_items
        );
        return ExitCode::SUCCESS;
    }

    let mut inputs = Vec::new();
    let mut identities: Vec<(PathBuf, IdentityResult)> = Vec::new();
    for path in &files {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        // Auto-routing (milestone section 44): the extension only chooses
        // *which parser to try* - identity itself still comes entirely from
        // the structural evidence that parser produces, never the
        // extension text itself.
        // Deliberately just ".chd"/".iso" - ".bin"/".img" are too overloaded
        // with cartridge-format use elsewhere in this same probe (a Mega
        // Drive `.bin`, for one) to route blindly; a real `.cue`+`.bin`
        // pair is a cue/m3u-grouping concern (see
        // `archivefs_core::platform_evidence_fusion::cue_m3u_parsing`), not
        // this per-file dispatch.
        let disc_evidence = match extension.as_deref() {
            Some("chd") => {
                Some(collect_chd_evidence(path).map_err(|refusal| format!("{refusal:?}")))
            }
            Some("iso") => Some(
                collect_plain_iso_evidence(path, MAX_CHD_BYTES)
                    .map_err(|refusal| format!("{refusal:?}")),
            ),
            _ => None,
        };

        if let Some(disc_result) = disc_evidence {
            match disc_result {
                Ok(evidence) => {
                    let identity = inspect_identity(IdentityInspectionInput {
                        content_evidence: evidence,
                        ..Default::default()
                    });
                    identities.push((path.clone(), identity.clone()));
                    inputs.push(LibraryPlanInput {
                        source_path: path.clone(),
                        identity,
                        set_identity: None,
                        physical_hash: None,
                        normalized_hash: None,
                    });
                }
                Err(reason) => {
                    println!(
                        "[skip] disc evidence collection refused for {}: {reason}",
                        path.display()
                    );
                }
            }
            continue;
        }

        let Ok(bytes) = std::fs::read(path) else {
            println!("[skip] could not read {}", path.display());
            continue;
        };
        let identity = build_identity(&bytes, path, args.dat_path.as_deref());
        identities.push((path.clone(), identity.clone()));
        let physical_hash = hash_bytes(&bytes, path.to_str().unwrap_or(""), "physical").sha256;
        inputs.push(LibraryPlanInput {
            source_path: path.clone(),
            identity,
            set_identity: None,
            physical_hash,
            normalized_hash: None,
        });
    }

    let slug_fn: &dyn Fn(&str) -> Option<String> = if args.demo_slug {
        &demo_slug_for_platform
    } else {
        &no_slug_mapping
    };
    let context = LibraryPlanningContext {
        destination_root: &args.dest_root,
        mode: OrganisationMode::MoveRealFile,
        slug_for_platform: slug_fn,
        generation: 1,
    };
    let report = plan_library(&inputs, &context);

    println!(
        "Scanned {} file(s). Ready={} NeedsReview={} Ambiguous={} Conflict={} Unknown={} Unsupported={}",
        report.items.len(),
        report.ready,
        report.needs_review,
        report.ambiguous,
        report.conflict,
        report.unknown,
        report.unsupported,
    );
    println!(
        "RomM: mapped={} unmapped={} ({})",
        report.romm_mapped,
        report.romm_unmapped,
        if args.demo_slug {
            "using the real Batch 11 production static table (--demo-slug)"
        } else {
            "no mapping requested - pass --demo-slug for the production static table"
        }
    );
    let duplicate_groups = group_duplicates(&inputs);
    let multidisc_sets = group_multidisc_sets(&inputs);
    println!(
        "Duplicate groups: {} ({} exact physical, {} exact normalized, {} same DAT release/dump, \
         {} possible)",
        duplicate_groups.len(),
        duplicate_groups
            .iter()
            .filter(|g| g.classification == DuplicateClass::ExactPhysicalDuplicate)
            .count(),
        duplicate_groups
            .iter()
            .filter(|g| g.classification == DuplicateClass::ExactNormalizedDuplicate)
            .count(),
        duplicate_groups
            .iter()
            .filter(|g| matches!(
                g.classification,
                DuplicateClass::SameDatRelease | DuplicateClass::SameGameDifferentDump
            ))
            .count(),
        duplicate_groups
            .iter()
            .filter(|g| g.classification == DuplicateClass::PossibleDuplicate)
            .count(),
    );
    println!("Multi-disc sets: {}", multidisc_sets.len());
    println!();

    for (item, (path, identity)) in report.items.iter().zip(identities.iter()) {
        println!("==================================================");
        println!("{}", path.display());
        let presentation = present_library_plan(item, identity);
        println!("{}", render_library_plan_text(&presentation));
    }

    ExitCode::SUCCESS
}

/// Builds an [`IdentityResult`] the same way `cartridge_probe` does: by
/// calling the existing per-format header/content observers directly and
/// fusing their output, optionally comparing against a real local DAT file.
/// No new detection logic lives here.
fn build_identity(bytes: &[u8], path: &Path, dat_path: Option<&Path>) -> IdentityResult {
    let mut evidence: Vec<ContentEvidence> = Vec::new();

    if let Some(fact) = parse_ines_header(bytes) {
        evidence.extend(observe_ines_evidence(&fact));
    }
    if let Some(fact) = best_snes_header_candidate(bytes) {
        evidence.extend(observe_snes_evidence(&fact));
    }
    if let Some(fact) = parse_gb_header(bytes) {
        evidence.extend(observe_gb_evidence(&fact));
    }
    if let Some(fact) = parse_gba_header(bytes) {
        evidence.extend(observe_gba_evidence(&fact));
    }
    if let Some(order) = detect_n64_byte_order(bytes) {
        use archivefs_core::content_detector::ContentDetector as _;
        evidence.extend(
            archivefs_core::n64_byte_order::N64ByteOrderDetector
                .detect(bytes)
                .evidence()
                .to_vec(),
        );
        if order == N64ByteOrder::Z64
            && let Some(fact) = parse_n64_header(bytes)
        {
            evidence.extend(observe_n64_evidence(&fact));
        }
    }
    if let Some(fact) = parse_megadrive_header(bytes) {
        evidence.extend(observe_megadrive_evidence(&fact));
    }
    if let Some(offset) = find_tmr_sega_header(bytes)
        && let Some(fact) = parse_tmr_sega_header(bytes, offset)
    {
        evidence.extend(observe_tmr_sega_evidence(&fact));
    }
    if let Some(fact) = parse_a78_header(bytes) {
        evidence.extend(observe_a78_evidence(&fact));
    }
    if let Some(fact) = parse_lynx_header(bytes) {
        evidence.extend(observe_lynx_evidence(&fact));
    }
    evidence.extend(observe_ws_evidence(bytes));

    let explanation = fuse_platform_evidence(evidence);

    let representation_match = dat_path.and_then(|dat_path| {
        let outcome = parse_dat_file(dat_path, DatLimits::default()).ok()?;
        let index = DatIndex::build(&outcome.dat);
        let physical_evidence = hash_bytes(bytes, dat_path.to_str().unwrap_or(""), "physical");
        let (_, physical_verdict) = audit_representation(
            &RepresentationHashes {
                representation: ByteRepresentation::Physical,
                evidence: physical_evidence,
            },
            &index,
        );
        let normalized = normalized_n64_representation(bytes, dat_path.to_str().unwrap_or(""), "n")
            .or_else(|| {
                normalized_header_stripped_representation(
                    bytes,
                    dat_path.to_str().unwrap_or(""),
                    "n",
                )
            })
            .or_else(|| normalized_smd_representation(bytes, dat_path.to_str().unwrap_or(""), "n"));
        let (identical, normalized_verdict) = match normalized {
            Some((normalized, identical)) => {
                let (_, verdict) = audit_representation(&normalized, &index);
                (identical, Some(verdict))
            }
            None => (false, None),
        };
        Some(compare_representations(
            physical_verdict,
            normalized_verdict,
            identical,
        ))
    });

    let _ = path;
    inspect_identity(IdentityInspectionInput {
        content_evidence: explanation.input_evidence,
        representation_match,
        ..Default::default()
    })
}

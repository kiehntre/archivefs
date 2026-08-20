//! Read-only cartridge-header probe, for real-corpus validation of Mass
//! Evidence Batch 4's cartridge-header observers. Reads exactly one file
//! (or a ZIP archive's members) and prints whatever structural evidence its
//! recognized header(s) produce. Nothing is ever written.
//!
//! ```text
//! cargo run -p archivefs-core --example cartridge_probe -- /path/to/rom
//! cargo run -p archivefs-core --example cartridge_probe -- /path/to/archive.zip
//! cargo run -p archivefs-core --example cartridge_probe -- /path/to/rom /path/to/some.dat
//! ```
//!
//! Batch 9: an optional second argument names a real local DAT file - when
//! given, its physical and normalized hash representations are compared
//! against the DAT via
//! [`archivefs_core::platform_evidence_fusion::dat_hash_representation`]
//! and shown through the shared identity presentation layer, exactly the
//! same pipeline `real_dat_match_scan` uses for a whole directory.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use archivefs_core::archive_member_content_evidence::{
    classify_archive_content, observe_zip_member_content,
};
use archivefs_core::atari7800_header_evidence::{observe_a78_evidence, parse_a78_header};
use archivefs_core::gb_header_evidence::{observe_gb_evidence, parse_gb_header};
use archivefs_core::gba_header_evidence::{observe_gba_evidence, parse_gba_header};
use archivefs_core::lynx_header_evidence::{observe_lynx_evidence, parse_lynx_header};
use archivefs_core::megadrive_header_evidence::{
    compute_megadrive_checksum, observe_megadrive_evidence, parse_megadrive_header,
};
use archivefs_core::n64_byte_order::detect_n64_byte_order;
use archivefs_core::n64_header_evidence::{observe_n64_evidence, parse_n64_header};
use archivefs_core::nes_header_evidence::{observe_ines_evidence, parse_ines_header};
use archivefs_core::sega32x_header_evidence::{
    observe_sega32x_candidate, observe_sega32x_evidence,
};
use archivefs_core::sms_gg_header_evidence::{
    find_tmr_sega_header, observe_tmr_sega_evidence, parse_tmr_sega_header,
};
use archivefs_core::snes_header_evidence::{best_snes_header_candidate, observe_snes_evidence};
use archivefs_core::ws_header_evidence::observe_ws_evidence;

fn main() -> ExitCode {
    let Some(path) = env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: cartridge_probe <path-to-rom-or-zip>");
        return ExitCode::FAILURE;
    };
    println!("Path: {}", path.display());

    if path.extension().and_then(|e| e.to_str()) == Some("zip") {
        return probe_zip(&path);
    }
    if path.extension().and_then(|e| e.to_str()) == Some("7z") {
        return probe_sevenz(&path);
    }

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("could not read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    println!("File size: {} bytes", bytes.len());
    let dat_path = env::args_os().nth(2).map(PathBuf::from);
    probe_bytes(&bytes, dat_path.as_deref());
    ExitCode::SUCCESS
}

fn probe_zip(path: &std::path::Path) -> ExitCode {
    match observe_zip_member_content(path) {
        Ok(observation) => {
            println!("ZIP members: {}", observation.members.len());
            for member in &observation.members {
                println!(
                    "  [{}] {} ({} bytes declared): {:?}",
                    member.member_index, member.member_name, member.declared_size, member.outcome
                );
                for fact in &member.evidence {
                    println!(
                        "      {:?} = {} ({:?}) - {}",
                        fact.kind, fact.value, fact.confidence, fact.detail
                    );
                }
            }
            println!(
                "Classification: {:?}",
                classify_archive_content(&observation)
            );
            let combined: Vec<_> = observation
                .members
                .iter()
                .flat_map(|m| m.evidence.iter().cloned())
                .collect();
            let explanation =
                archivefs_core::platform_evidence_fusion::fuse_platform_evidence(combined);
            println!(
                "Fusion outcome (all members combined): {:?}",
                explanation.outcome
            );
            println!(
                "Fusion resolved platform: {:?}",
                explanation.resolved_platform
            );
        }
        Err(error) => println!("ZIP content observation failed: {error:?}"),
    }
    ExitCode::SUCCESS
}

fn probe_sevenz(path: &std::path::Path) -> ExitCode {
    use archivefs_core::archive_member_content_evidence::observe_sevenz_member_content;
    use archivefs_core::dat::archive::limits::ArchiveLimits;
    use archivefs_core::safe_read::TrustedRoots;
    use std::sync::atomic::AtomicBool;

    let trusted = TrustedRoots::from_paths(path.parent());
    let cancel = AtomicBool::new(false);
    match observe_sevenz_member_content(path, &trusted, ArchiveLimits::default(), &cancel) {
        Ok(observation) => {
            println!("7z members: {}", observation.members.len());
            for member in &observation.members {
                println!(
                    "  [{}] {} ({} bytes declared): {:?}",
                    member.member_index, member.member_name, member.declared_size, member.outcome
                );
                for fact in &member.evidence {
                    println!(
                        "      {:?} = {} ({:?}) - {}",
                        fact.kind, fact.value, fact.confidence, fact.detail
                    );
                }
            }
            println!(
                "Classification: {:?}",
                classify_archive_content(&observation)
            );
            let combined: Vec<_> = observation
                .members
                .iter()
                .flat_map(|m| m.evidence.iter().cloned())
                .collect();
            let explanation =
                archivefs_core::platform_evidence_fusion::fuse_platform_evidence(combined);
            println!(
                "Fusion outcome (all members combined): {:?}",
                explanation.outcome
            );
            println!(
                "Fusion resolved platform: {:?}",
                explanation.resolved_platform
            );
        }
        Err(error) => println!("7z content observation failed: {error:?}"),
    }
    ExitCode::SUCCESS
}

fn probe_bytes(bytes: &[u8], dat_path: Option<&std::path::Path>) {
    let mut evidence: Vec<archivefs_core::content_evidence::ContentEvidence> = Vec::new();

    if let Some(fact) = parse_ines_header(bytes) {
        println!("iNES/NES2.0: {:?}", fact);
        let facts = observe_ines_evidence(&fact);
        println!("Evidence: {facts:?}");
        evidence.extend(facts);
    }
    if let Some(fact) = best_snes_header_candidate(bytes) {
        println!("SNES: {:?}", fact);
        let facts = observe_snes_evidence(&fact);
        println!("Evidence: {facts:?}");
        evidence.extend(facts);
    }
    if let Some(fact) = parse_gb_header(bytes) {
        println!(
            "GB/GBC: logo_valid={} checksum_valid={} title={:?} color={:?}",
            fact.logo_valid, fact.header_checksum_valid, fact.title, fact.color_support
        );
        let facts = observe_gb_evidence(&fact);
        println!("Evidence: {facts:?}");
        evidence.extend(facts);
    }
    if let Some(fact) = parse_gba_header(bytes) {
        println!(
            "GBA: fixed_value_valid={} checksum_valid={} title={:?} code={:?}",
            fact.fixed_value_valid, fact.complement_check_valid, fact.game_title, fact.game_code
        );
        let facts = observe_gba_evidence(&fact);
        println!("Evidence: {facts:?}");
        evidence.extend(facts);
    }
    if let Some(order) = detect_n64_byte_order(bytes) {
        println!("N64 byte order: {}", order.label());
        use archivefs_core::content_detector::ContentDetector as _;
        evidence.extend(
            archivefs_core::n64_byte_order::N64ByteOrderDetector
                .detect(bytes)
                .evidence()
                .to_vec(),
        );
        if order == archivefs_core::n64_byte_order::N64ByteOrder::Z64
            && let Some(fact) = parse_n64_header(bytes)
        {
            println!("N64 header: {:?}", fact);
            let facts = observe_n64_evidence(&fact);
            println!("Evidence: {facts:?}");
            evidence.extend(facts);
        } else {
            println!("(N64 header fields skipped - not canonical Z64 order in this probe)");
        }
    }
    if let Some(fact) = parse_megadrive_header(bytes) {
        println!(
            "Mega Drive: console_name={:?} recognized={} serial={:?} checksum_declared={:#06x}",
            fact.console_name, fact.console_name_recognized, fact.serial_number, fact.checksum
        );
        if let Some(computed) = compute_megadrive_checksum(bytes) {
            println!(
                "  checksum computed={:#06x} matches_declared={}",
                computed,
                computed == fact.checksum
            );
        }
        let facts = observe_megadrive_evidence(&fact);
        println!("Evidence: {facts:?}");
        evidence.extend(facts);
        let sega32x_fact = observe_sega32x_candidate(&fact);
        let sega32x_evidence = observe_sega32x_evidence(&sega32x_fact);
        if !sega32x_evidence.is_empty() {
            println!("32X evidence: {sega32x_evidence:?}");
            evidence.extend(sega32x_evidence);
        }
    }
    if let Some(offset) = find_tmr_sega_header(bytes)
        && let Some(fact) = parse_tmr_sega_header(bytes, offset)
    {
        println!("TMR SEGA: {:?}", fact);
        let facts = observe_tmr_sega_evidence(&fact);
        println!("Evidence: {facts:?}");
        evidence.extend(facts);
    }
    if let Some(fact) = parse_a78_header(bytes) {
        println!("Atari 7800: {:?}", fact);
        let facts = observe_a78_evidence(&fact);
        println!("Evidence: {facts:?}");
        evidence.extend(facts);
    }
    if let Some(fact) = parse_lynx_header(bytes) {
        println!("Lynx: {:?}", fact);
        let facts = observe_lynx_evidence(&fact);
        println!("Evidence: {facts:?}");
        evidence.extend(facts);
    }
    let ws_evidence = observe_ws_evidence(bytes);
    if !ws_evidence.is_empty() {
        println!("WonderSwan evidence: {ws_evidence:?}");
        evidence.extend(ws_evidence);
    }

    let explanation = archivefs_core::platform_evidence_fusion::fuse_platform_evidence(evidence);
    println!("Fusion outcome: {:?}", explanation.outcome);
    match explanation.resolved_platform {
        Some(platform) => println!("Fusion resolved platform: {platform}"),
        None => println!("Fusion resolved platform: N/A"),
    }
    if !explanation.conflicting_platforms.is_empty() {
        println!(
            "Fusion conflicting platforms: {:?}",
            explanation.conflicting_platforms
        );
    }
    println!("Rules fired:");
    for candidate in &explanation.fired_candidates {
        println!(
            "  {} -> {} (strong leg: {})",
            candidate.rule_id, candidate.platform, candidate.has_strong_leg
        );
    }
    let ignored_weak: Vec<_> = explanation
        .input_evidence
        .iter()
        .filter(|fact| {
            fact.confidence == archivefs_core::content_evidence::ContentEvidenceConfidence::Weak
        })
        .collect();
    if !ignored_weak.is_empty() {
        println!("Ignored weak evidence (never independently resolves):");
        for fact in &ignored_weak {
            println!("  {:?} = {}", fact.kind, fact.value);
        }
    }
    // Batch 9: route through the shared identity orchestrator/presentation
    // pipeline rather than a second, hand-built DAT comparison.
    use archivefs_core::dat::index::DatIndex;
    use archivefs_core::dat::limits::DatLimits;
    use archivefs_core::dat::parsers::parse_dat_file;
    use archivefs_core::platform_evidence_fusion::dat_hash_representation::{
        ByteRepresentation, RepresentationHashes, audit_representation, compare_representations,
        hash_bytes, normalized_header_stripped_representation, normalized_n64_representation,
        normalized_smd_representation,
    };
    use archivefs_core::platform_evidence_fusion::identity_orchestrator::{
        IdentityInspectionInput, inspect_identity,
    };
    use archivefs_core::platform_evidence_fusion::identity_presentation::{
        present_identity, render_identity_text,
    };

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

    let identity_result = inspect_identity(IdentityInspectionInput {
        content_evidence: explanation.input_evidence.clone(),
        representation_match,
        ..Default::default()
    });
    let presentation = present_identity(&identity_result);
    println!("\n--- Identity summary ---");
    println!("{}", render_identity_text(&presentation));
}

//! Read-only cartridge-header probe, for real-corpus validation of Mass
//! Evidence Batch 4's cartridge-header observers. Reads exactly one file
//! (or a ZIP archive's members) and prints whatever structural evidence its
//! recognized header(s) produce. Nothing is ever written.
//!
//! ```text
//! cargo run -p archivefs-core --example cartridge_probe -- /path/to/rom
//! cargo run -p archivefs-core --example cartridge_probe -- /path/to/archive.zip
//! ```

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

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("could not read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    println!("File size: {} bytes", bytes.len());
    probe_bytes(&bytes);
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
        }
        Err(error) => println!("ZIP content observation failed: {error:?}"),
    }
    ExitCode::SUCCESS
}

fn probe_bytes(bytes: &[u8]) {
    if let Some(fact) = parse_ines_header(bytes) {
        println!("iNES/NES2.0: {:?}", fact);
        println!("Evidence: {:?}", observe_ines_evidence(&fact));
    }
    if let Some(fact) = best_snes_header_candidate(bytes) {
        println!("SNES: {:?}", fact);
        println!("Evidence: {:?}", observe_snes_evidence(&fact));
    }
    if let Some(fact) = parse_gb_header(bytes) {
        println!(
            "GB/GBC: logo_valid={} checksum_valid={} title={:?} color={:?}",
            fact.logo_valid, fact.header_checksum_valid, fact.title, fact.color_support
        );
        println!("Evidence: {:?}", observe_gb_evidence(&fact));
    }
    if let Some(fact) = parse_gba_header(bytes) {
        println!(
            "GBA: fixed_value_valid={} checksum_valid={} title={:?} code={:?}",
            fact.fixed_value_valid, fact.complement_check_valid, fact.game_title, fact.game_code
        );
        println!("Evidence: {:?}", observe_gba_evidence(&fact));
    }
    if let Some(order) = detect_n64_byte_order(bytes) {
        println!("N64 byte order: {}", order.label());
        if order == archivefs_core::n64_byte_order::N64ByteOrder::Z64
            && let Some(fact) = parse_n64_header(bytes)
        {
            println!("N64 header: {:?}", fact);
            println!("Evidence: {:?}", observe_n64_evidence(&fact));
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
        println!("Evidence: {:?}", observe_megadrive_evidence(&fact));
        let sega32x_fact = observe_sega32x_candidate(&fact);
        let sega32x_evidence = observe_sega32x_evidence(&sega32x_fact);
        if !sega32x_evidence.is_empty() {
            println!("32X evidence: {sega32x_evidence:?}");
        }
    }
    if let Some(offset) = find_tmr_sega_header(bytes)
        && let Some(fact) = parse_tmr_sega_header(bytes, offset)
    {
        println!("TMR SEGA: {:?}", fact);
        println!("Evidence: {:?}", observe_tmr_sega_evidence(&fact));
    }
    if let Some(fact) = parse_a78_header(bytes) {
        println!("Atari 7800: {:?}", fact);
        println!("Evidence: {:?}", observe_a78_evidence(&fact));
    }
    if let Some(fact) = parse_lynx_header(bytes) {
        println!("Lynx: {:?}", fact);
        println!("Evidence: {:?}", observe_lynx_evidence(&fact));
    }
    let ws_evidence = observe_ws_evidence(bytes);
    if !ws_evidence.is_empty() {
        println!("WonderSwan evidence: {ws_evidence:?}");
    }
}

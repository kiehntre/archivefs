//! Read-only DAT-source identity probe, for real-corpus validation of
//! [`archivefs_core::dat::identity`] and, via
//! [`archivefs_core::platform_evidence_fusion::combined_identity`], the
//! Batch 7 content+DAT convergence layer. Parses exactly one DAT file
//! (Logiqx XML or ClrMamePro) and prints what platform its own catalogue
//! metadata resolves to. Nothing is ever written.
//!
//! ```text
//! cargo run -p archivefs-core --example dat_probe -- /path/to/some.dat
//! ```

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use archivefs_core::dat::identity::{gather_dat_platform_evidence, identify_dat_source};
use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::parsers::parse_dat_file;
use archivefs_core::platform_evidence_fusion::combined_identity::{
    combine_identity, dat_source_provenance,
};
use archivefs_core::platform_evidence_fusion::{FusionOutcome, ResolutionExplanation};

fn main() -> ExitCode {
    let Some(path) = env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: dat_probe <path-to-dat-file>");
        return ExitCode::FAILURE;
    };
    println!("Path: {}", path.display());

    let outcome = match parse_dat_file(&path, DatLimits::default()) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("could not parse {}: {error:?}", path.display());
            return ExitCode::FAILURE;
        }
    };
    println!("Format: {:?}", outcome.dat.source.format);
    println!("Header name: {:?}", outcome.dat.source.name);
    println!("Header description: {:?}", outcome.dat.source.description);
    println!("Entries: {}", outcome.dat.games.len());
    if !outcome.warnings.is_empty() {
        println!("Parse warnings: {}", outcome.warnings.len());
    }

    let evidence = gather_dat_platform_evidence(&outcome.dat);
    println!("DAT platform evidence:");
    for item in &evidence {
        println!(
            "  {:?} = {} ({:?}) - {}",
            item.kind, item.platform, item.confidence, item.detail
        );
    }

    let dat_identity = identify_dat_source(&outcome.dat);
    println!("DAT-source identity: {dat_identity:?}");

    if let Some(provenance) = dat_source_provenance(&dat_identity) {
        println!(
            "DAT source provenance: platform={} confidence={:?} decided-by={} machine_key={:?}",
            provenance.platform,
            provenance.confidence,
            provenance.deciding_kind_label,
            provenance.machine_key
        );
    }

    // This probe has no content-evidence pipeline of its own (that lives in
    // disc_probe/cartridge_probe) - the placeholder Unknown content
    // resolution below only demonstrates how a real caller would combine
    // its own real ResolutionExplanation with this DAT-source identity via
    // combine_identity; see combined_identity's own module documentation
    // for the full outcome table.
    let placeholder_content = ResolutionExplanation {
        outcome: FusionOutcome::Unknown,
        resolved_platform: None,
        fired_candidates: Vec::new(),
        conflicting_platforms: Vec::new(),
        input_evidence: Vec::new(),
    };
    let combined = combine_identity(&placeholder_content, &dat_identity);
    println!(
        "Combined view (content lane is a placeholder here): {:?}",
        combined.relationship
    );

    ExitCode::SUCCESS
}

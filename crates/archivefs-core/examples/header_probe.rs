//! Read-only header-normalization probe.
//!
//! Reads exactly one, explicitly supplied ROM path, checks it against every
//! recognizer in [`archivefs_core::header_normalization`], and - for every
//! candidate that matches, not only the first - strips the recognized
//! header **in memory only** so its physical and normalized SHA-256 can be
//! compared, and proves the strip is reversible. Nothing is ever written:
//! the only filesystem call in this file is the single read of the path
//! given on the command line, and the normalized/reconstructed bytes exist
//! only for as long as it takes to hash and compare them before the process
//! exits.
//!
//! All of the actual recognition/normalization logic already lives in, and
//! is already tested by, [`archivefs_core::header_normalization`]; this
//! file is nothing more than a thin, explicitly-invoked wrapper around it -
//! the same shape as the existing `n64_probe` example. No normalization
//! algorithm is duplicated or modified here.
//!
//! # Usage
//!
//! ```text
//! cargo run -p archivefs-core --example header_probe -- /path/to/game.lnx
//! ```

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use archivefs_core::dat::archive::hash::hash_member_stream;
use archivefs_core::header_normalization::{
    recognize_header_normalization, reconstruct_with_header, strip_known_header,
};
use archivefs_core::identity_source::hashing::FileFingerprint;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let path = match args.as_slice() {
        [single] => PathBuf::from(single),
        [] => {
            eprintln!("usage: header_probe <path-to-rom>");
            return ExitCode::FAILURE;
        }
        _ => {
            eprintln!("usage: header_probe <path-to-rom>  (exactly one path, no options)");
            return ExitCode::FAILURE;
        }
    };

    println!("Path: {}", path.display());

    // Metadata only, no read - the same fingerprint the crate's own hashing
    // helpers (and the existing n64_probe example) already use to detect a
    // file changing underneath them.
    let before = FileFingerprint::observe(&path);

    // The one and only filesystem access this program performs. Opens for
    // reading only; nothing is ever opened for write.
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("could not read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    println!("File size: {} bytes", bytes.len());

    let after = FileFingerprint::observe(&path);
    let confirmed_unmodified = before.is_some() && before == after;
    println!(
        "Original file modified: {}",
        if confirmed_unmodified {
            "NO"
        } else {
            "UNKNOWN (could not confirm the file was unchanged while it was being read)"
        }
    );

    let candidates = recognize_header_normalization(&bytes);
    if candidates.is_empty() {
        println!("NotRecognized");
        return ExitCode::SUCCESS;
    }

    println!("Candidates: {}", candidates.len());
    if candidates.len() > 1 {
        // Explicitly not resolved here - both a real magic-based format and
        // the weak SNES size candidate can legitimately match the same
        // bytes, and this probe reports every one of them rather than
        // silently preferring one.
        println!("(multiple candidates matched - none is preferred or discarded by this probe)");
    }

    let physical_sha256 = match hash_bytes(&bytes) {
        Ok(hex) => hex,
        Err(detail) => {
            eprintln!("failed to hash the physical bytes: {detail}");
            return ExitCode::FAILURE;
        }
    };

    let mut all_reconstructed_exactly = true;
    let mut any_strip_failed = false;

    for (index, kind) in candidates.iter().enumerate() {
        println!("--- Candidate {} of {} ---", index + 1, candidates.len());
        println!("Kind: {kind:?}");
        println!("Transform id: {}", kind.transform_id());
        println!("Header length: {} bytes", kind.header_len());
        println!("Physical SHA-256: {physical_sha256}");

        let result = match strip_known_header(&bytes, *kind) {
            Ok(result) => result,
            Err(error) => {
                any_strip_failed = true;
                eprintln!("  strip failed: {}", error.detail());
                continue;
            }
        };

        let normalized_sha256 = match hash_bytes(&result.bytes) {
            Ok(hex) => hex,
            Err(detail) => {
                any_strip_failed = true;
                eprintln!("  failed to hash the normalized bytes: {detail}");
                continue;
            }
        };
        println!("Normalized size: {} bytes", result.bytes.len());
        println!("Normalized SHA-256: {normalized_sha256}");

        let reconstructed = reconstruct_with_header(&result);
        let reconstruction_exact = reconstructed == bytes;
        println!(
            "Reconstruction exact: {}",
            if reconstruction_exact { "YES" } else { "NO" }
        );
        if !reconstruction_exact {
            all_reconstructed_exactly = false;
        }
        // `result.bytes`, `reconstructed`, and every hash above exist only
        // in memory - nothing from this loop is ever written to disk.
    }

    if any_strip_failed || !all_reconstructed_exactly {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// SHA-256 of an in-memory buffer, via the crate's existing
/// [`hash_member_stream`] (the same helper `n64_probe` already uses) rather
/// than a new hashing implementation. `&[u8]` already implements `Read`, so
/// no file or path is involved.
fn hash_bytes(data: &[u8]) -> Result<String, String> {
    hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
        .map(|hashed| hashed.hashes.sha256)
        .map_err(|error| format!("{error:?}"))
}

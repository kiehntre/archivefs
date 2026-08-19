//! Read-only N64 byte-order probe.
//!
//! Reads exactly one, explicitly supplied ROM path, detects its N64
//! byte-order header (`z64`/`v64`/`n64`), and - if recognised - normalizes
//! it to canonical `z64` order **in memory only** so its physical and
//! normalized SHA-256 can be compared. Nothing is ever written: the only
//! filesystem call in this file is the single read of the path given on the
//! command line, and the normalized bytes exist only for as long as it
//! takes to hash them before the process exits.
//!
//! All of the actual detection/normalization logic already lives in, and is
//! already tested by, [`archivefs_core::n64_byte_order`]; this file is
//! nothing more than a thin, explicitly-invoked wrapper around it plus
//! [`archivefs_core::dat::archive::hash::hash_member_stream`] (the existing
//! in-memory-capable hasher already used elsewhere in this crate) and
//! [`archivefs_core::identity_source::hashing::FileFingerprint`] (the
//! existing "did this file change while I was reading it" check).
//!
//! # Usage
//!
//! ```text
//! cargo run -p archivefs-core --example n64_probe -- /path/to/game.z64
//! ```

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use archivefs_core::dat::archive::hash::hash_member_stream;
use archivefs_core::identity_source::hashing::FileFingerprint;
use archivefs_core::n64_byte_order::{detect_n64_byte_order, normalize_to_z64};

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let path = match args.as_slice() {
        [single] => PathBuf::from(single),
        [] => {
            eprintln!("usage: n64_probe <path-to-rom>");
            return ExitCode::FAILURE;
        }
        _ => {
            eprintln!("usage: n64_probe <path-to-rom>  (exactly one path, no options)");
            return ExitCode::FAILURE;
        }
    };

    println!("Path: {}", path.display());

    // Metadata only, no read - the same fingerprint the crate's own hashing
    // helpers already use to detect a file changing underneath them.
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

    let Some(order) = detect_n64_byte_order(&bytes) else {
        println!("Detected N64 byte order: NotRecognized");
        return ExitCode::SUCCESS;
    };
    println!("Detected N64 byte order: {}", order.label());

    let physical_sha256 = match hash_bytes(&bytes) {
        Ok(hex) => hex,
        Err(detail) => {
            eprintln!("failed to hash the physical bytes: {detail}");
            return ExitCode::FAILURE;
        }
    };
    println!("Physical SHA-256: {physical_sha256}");

    let normalized = match normalize_to_z64(&bytes, order) {
        Ok(result) => result,
        Err(error) => {
            // The explicit, safe error from the existing normalizer -
            // nothing here reinterprets or papers over it. Nothing was
            // written before this point and nothing is written now.
            eprintln!("normalization failed: {}", error.detail());
            return ExitCode::FAILURE;
        }
    };
    println!("Normalization transform: {}", normalized.transform);
    println!("Source byte order: {}", order.label());
    println!("Normalized size: {} bytes", normalized.bytes.len());

    let normalized_sha256 = match hash_bytes(&normalized.bytes) {
        Ok(hex) => hex,
        Err(detail) => {
            eprintln!("failed to hash the normalized bytes: {detail}");
            return ExitCode::FAILURE;
        }
    };
    println!("Normalized SHA-256: {normalized_sha256}");
    // `normalized.bytes` is dropped here, in memory only - it was never
    // written to disk, and neither was `bytes`.

    ExitCode::SUCCESS
}

/// SHA-256 of an in-memory buffer, via the crate's existing
/// [`hash_member_stream`] (already used for archive-member hashing
/// elsewhere) rather than a new hashing implementation. `&[u8]` already
/// implements `Read`, so no file or path is involved.
fn hash_bytes(data: &[u8]) -> Result<String, String> {
    hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
        .map(|hashed| hashed.hashes.sha256)
        .map_err(|error| format!("{error:?}"))
}

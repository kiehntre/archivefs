//! Read-only CHD identity/media probe.
//!
//! Reads exactly one, explicitly supplied CHD path and prints every identity
//! and conservative media fact [`archivefs_core::chd_identity`] exposes for
//! it. Nothing is ever written: the only filesystem call in this file is the
//! single read of the path given on the command line. No hunk is ever
//! decompressed, no metadata is mutated, and no chdman/extraction/repair
//! tool is invoked.
//!
//! All of the actual parsing logic already lives in, and is already tested
//! by, [`archivefs_core::chd_identity`]; this file is nothing more than a
//! thin, explicitly-invoked wrapper around it - the same shape as the
//! existing `header_probe`/`n64_probe` examples.
//!
//! # Usage
//!
//! ```text
//! cargo run -p archivefs-core --example chd_probe -- /path/to/game.chd
//! ```

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use archivefs_core::chd_identity::{ChdMetadataFact, ChdMetadataOutcome, observe_chd_identity};
use archivefs_core::dat::archive::hash::hash_member_stream;
use archivefs_core::identity_source::hashing::FileFingerprint;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let path = match args.as_slice() {
        [single] => PathBuf::from(single),
        [] => {
            eprintln!("usage: chd_probe <path-to-chd>");
            return ExitCode::FAILURE;
        }
        _ => {
            eprintln!("usage: chd_probe <path-to-chd>  (exactly one path, no options)");
            return ExitCode::FAILURE;
        }
    };

    println!("Path: {}", path.display());

    // Metadata only, no read - the same fingerprint the crate's own hashing
    // helpers (and the existing probes) already use to detect a file
    // changing underneath them.
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

    let physical_sha256 = match hash_bytes(&bytes) {
        Ok(hex) => hex,
        Err(detail) => {
            eprintln!("failed to hash the physical bytes: {detail}");
            return ExitCode::FAILURE;
        }
    };
    println!("Physical SHA-256: {physical_sha256}");

    let observation = match observe_chd_identity(&bytes) {
        Ok(observation) => observation,
        Err(error) => {
            eprintln!("not a readable CHD v5 header: {error}");
            return ExitCode::FAILURE;
        }
    };

    println!("CHD version: {}", observation.version);
    println!("Logical bytes: {}", observation.logical_bytes);
    println!("Hunk bytes: {}", observation.hunk_bytes);
    println!("Unit bytes: {}", observation.unit_bytes);
    println!("Raw SHA-1: {}", observation.raw_sha1_hex());
    println!("Combined SHA-1: {}", observation.combined_sha1_hex());
    println!("Parent SHA-1: {}", observation.parent_sha1_hex());
    println!(
        "Parent required: {}",
        if observation.parent_required {
            "YES"
        } else {
            "NO"
        }
    );

    match &observation.metadata {
        ChdMetadataOutcome::Empty => {
            println!("Metadata entry count: 0 (meta_offset is zero - no chain declared)");
        }
        ChdMetadataOutcome::Malformed(error) => {
            println!("Metadata chain: MALFORMED - {error}");
            return ExitCode::FAILURE;
        }
        ChdMetadataOutcome::Observed(metadata) => {
            println!("Metadata entry count: {}", metadata.entries.len());
            for (index, entry) in metadata.entries.iter().enumerate() {
                let tag_text = tag_to_display(entry.tag);
                println!(
                    "  [{index}] tag={tag_text} kind={:?} flags={:#04x} length={}",
                    entry.kind, entry.flags, entry.length
                );
                match &entry.fact {
                    ChdMetadataFact::HardDiskGeometry(geometry) => {
                        println!(
                            "      hard disk geometry: CYLS:{} HEADS:{} SECS:{} BPS:{}",
                            geometry.cylinders,
                            geometry.heads,
                            geometry.sectors,
                            geometry.bytes_per_sector
                        );
                    }
                    ChdMetadataFact::CdromTrack(track) => {
                        println!(
                            "      CD-ROM track {}: type={} subtype={} frames={} pregap={:?} postgap={:?}",
                            track.track,
                            track.track_type,
                            track.subtype,
                            track.frames,
                            track.pregap,
                            track.postgap
                        );
                    }
                    ChdMetadataFact::GdromTrack(track) => {
                        println!(
                            "      GD-ROM track {}: type={} subtype={} frames={} pad={:?} pregap={:?} postgap={:?}",
                            track.track,
                            track.track_type,
                            track.subtype,
                            track.frames,
                            track.pad,
                            track.pregap,
                            track.postgap
                        );
                    }
                    ChdMetadataFact::Unparsed => {
                        println!("      (tag identity recorded; payload not interpreted)");
                    }
                }
            }

            let classes = metadata.media_classes();
            if classes.is_empty() {
                println!("Conservative media class: Unknown (no supporting metadata tag present)");
            } else {
                println!("Conservative media class: {classes:?}");
            }
        }
    }

    ExitCode::SUCCESS
}

/// A best-effort readable rendering of a raw big-endian FOURCC tag - the
/// four ASCII bytes if printable, otherwise the raw hex. Display only; never
/// used to decide anything.
fn tag_to_display(tag: u32) -> String {
    let bytes = tag.to_be_bytes();
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        format!("'{}'", String::from_utf8_lossy(&bytes))
    } else {
        format!("{tag:#010x}")
    }
}

/// SHA-256 of an in-memory buffer, via the crate's existing
/// [`hash_member_stream`] (the same helper the other probes already use)
/// rather than a new hashing implementation.
fn hash_bytes(data: &[u8]) -> Result<String, String> {
    hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
        .map(|hashed| hashed.hashes.sha256)
        .map_err(|error| format!("{error:?}"))
}

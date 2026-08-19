//! Read-only container -> media -> logical-filesystem probe.
//!
//! Reads exactly one, explicitly supplied file and walks as far down the
//! pipeline as this crate currently can:
//!
//! ```text
//! container -> media -> logical media reader -> filesystem/root tree
//!     -> boot/layout observations
//! ```
//!
//! For a plain `.iso`/`.bin` image, the whole pipeline runs: the file's own
//! bytes are the logical media, so [`archivefs_core::iso9660`] can observe
//! it directly via [`archivefs_core::logical_media::SliceMedia`].
//!
//! For a `.chd`, the full pipeline now runs too:
//! [`archivefs_core::chd_identity`] observes the CHD's own identity and
//! media facts and selects a candidate data track, then
//! [`archivefs_core::chd_logical_media`] decodes that track's sectors
//! on demand (via the `chd` crate) and exposes them as
//! [`archivefs_core::logical_media::LogicalMedia`], which
//! [`archivefs_core::iso9660`] can observe exactly as it would a plain
//! image. Any step that this crate does not yet support (a parent-required
//! CHD, a non-track-1/non-zero-pregap data track, an unsupported track
//! type) is reported explicitly rather than guessed at - see
//! [`archivefs_core::chd_logical_media`]'s module documentation for the
//! exact scope limits.
//!
//! Nothing is ever written. The only filesystem call in this file is the
//! single read of the path given on the command line. No platform is ever
//! printed - every line here is a container, media, or filesystem fact.
//!
//! # Usage
//!
//! ```text
//! cargo run -p archivefs-core --example disc_probe -- /path/to/game.iso
//! cargo run -p archivefs-core --example disc_probe -- /path/to/game.chd
//! ```

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use archivefs_core::chd_identity::{
    ChdMetadataOutcome, looks_like_chd, observe_chd_identity, select_candidate_data_track,
};
use archivefs_core::chd_logical_media::{ChdLogicalMediaError, open_chd_track_logical_media};
use archivefs_core::dat::archive::hash::hash_member_stream;
use archivefs_core::identity_source::hashing::FileFingerprint;
use archivefs_core::iso9660::{
    DiscFilesystemObservation, INTERESTING_ROOT_PATHS, find_path, looks_like_iso9660,
    observe_iso9660,
};
use archivefs_core::logical_media::{LogicalMedia, SliceMedia};

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let path = match args.as_slice() {
        [single] => PathBuf::from(single),
        [] => {
            eprintln!("usage: disc_probe <path-to-iso-or-chd>");
            return ExitCode::FAILURE;
        }
        _ => {
            eprintln!("usage: disc_probe <path-to-iso-or-chd>  (exactly one path, no options)");
            return ExitCode::FAILURE;
        }
    };

    println!("Path: {}", path.display());

    let before = FileFingerprint::observe(&path);

    // The one and only filesystem access this program performs.
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
        "Original modified: {}",
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

    if looks_like_chd(&bytes) {
        return probe_chd(&bytes);
    }
    if looks_like_iso9660(&SliceMedia(&bytes)) {
        return probe_iso9660(&bytes);
    }

    println!("Container: Unknown (neither CHD magic nor ISO9660 CD001 recognised)");
    ExitCode::SUCCESS
}

fn probe_chd(bytes: &[u8]) -> ExitCode {
    println!("Container: CHD");

    let observation = match observe_chd_identity(bytes) {
        Ok(observation) => observation,
        Err(error) => {
            eprintln!("CHD header did not parse: {error}");
            return ExitCode::FAILURE;
        }
    };

    match &observation.metadata {
        ChdMetadataOutcome::Empty => println!("Media: Unknown (no metadata chain)"),
        ChdMetadataOutcome::Malformed(error) => {
            println!("Media: Unknown (metadata chain malformed: {error})");
        }
        ChdMetadataOutcome::Observed(metadata) => {
            let classes = metadata.media_classes();
            println!(
                "Media: {}",
                if classes.is_empty() {
                    "Unknown".to_string()
                } else {
                    format!("{classes:?}")
                }
            );

            match select_candidate_data_track(metadata) {
                Some(candidate) => println!(
                    "Data track: track {} (type={}, media={:?}) - conservative metadata-only selection, audio tracks excluded",
                    candidate.track, candidate.track_type, candidate.media_class
                ),
                None => println!(
                    "Data track: none identified (all-audio, or no CD/GD-ROM track metadata)"
                ),
            }
        }
    }

    let media = match open_chd_track_logical_media(bytes) {
        Ok(media) => {
            println!("Logical reader: OK");
            media
        }
        Err(error) => {
            println!("Logical reader: {}", logical_reader_outcome(&error));
            println!("Filesystem: Unknown (no logical reader available)");
            print_interesting_paths_unavailable();
            return ExitCode::SUCCESS;
        }
    };

    if !looks_like_iso9660(&media) {
        println!(
            "Filesystem: Unknown (logical data does not begin with an ISO9660 CD001 identifier)"
        );
        print_interesting_paths_unavailable();
        return ExitCode::SUCCESS;
    }

    match observe_iso9660(&media) {
        Ok(observation) => print_iso9660_observation(&media, &observation),
        Err(error) => {
            println!("Filesystem: Unsupported (ISO9660 structure did not parse: {error})");
            print_interesting_paths_unavailable();
        }
    }

    ExitCode::SUCCESS
}

fn logical_reader_outcome(error: &ChdLogicalMediaError) -> String {
    match error {
        ChdLogicalMediaError::NeedsParent { parent_sha1 } => {
            format!(
                "NeedsParent (combined SHA-1 {})",
                parent_sha1
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        }
        ChdLogicalMediaError::NoDataTrack
        | ChdLogicalMediaError::UnsupportedTrackType { .. }
        | ChdLogicalMediaError::UnsupportedTrackPosition { .. }
        | ChdLogicalMediaError::UnsupportedPregap { .. } => format!("Unsupported ({error})"),
        ChdLogicalMediaError::Header(_) | ChdLogicalMediaError::Codec { .. } => {
            format!("Malformed ({error})")
        }
    }
}

fn print_interesting_paths_unavailable() {
    println!("Volume ID: N/A");
    println!("Root entries: N/A");
    for path in INTERESTING_ROOT_PATHS {
        println!("  {path}: N/A");
    }
}

fn print_iso9660_observation<M: LogicalMedia>(media: &M, observation: &DiscFilesystemObservation) {
    println!("Filesystem: ISO9660");
    println!("Volume ID: {}", observation.volume_identifier);
    println!("Root entries: {}", observation.root_entries.len());
    for entry in &observation.root_entries {
        println!(
            "  {} ({}, size={})",
            entry.original_name,
            if entry.is_directory { "dir" } else { "file" },
            entry.size
        );
    }
    for path in INTERESTING_ROOT_PATHS {
        let exists = matches!(find_path(media, observation, path), Ok(Some(_)));
        println!("  {path}: {}", if exists { "YES" } else { "NO" });
    }
}

fn probe_iso9660(bytes: &[u8]) -> ExitCode {
    println!("Container: raw logical image (plain ISO9660 byte stream)");
    println!("Media: N/A (no CHD container to report media facts for)");
    println!("Data track(s): N/A (single logical byte stream, not a multi-track container)");

    let media = SliceMedia(bytes);
    let observation = match observe_iso9660(&media) {
        Ok(observation) => observation,
        Err(error) => {
            eprintln!("ISO9660 structure did not parse: {error}");
            return ExitCode::FAILURE;
        }
    };

    print_iso9660_observation(&media, &observation);
    ExitCode::SUCCESS
}

/// SHA-256 of an in-memory buffer, via the crate's existing
/// [`hash_member_stream`] (the same helper the other probes already use)
/// rather than a new hashing implementation.
fn hash_bytes(data: &[u8]) -> Result<String, String> {
    hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
        .map(|hashed| hashed.hashes.sha256)
        .map_err(|error| format!("{error:?}"))
}

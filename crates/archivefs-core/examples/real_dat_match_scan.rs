use std::env;
use std::path::PathBuf;

use archivefs_core::dat::index::DatIndex;
use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::parsers::parse_dat_file;
use archivefs_core::platform_evidence_fusion::dat_hash_representation::{
    ByteRepresentation, RepresentationHashes, audit_representation, hash_bytes,
    normalized_header_stripped_representation, normalized_n64_representation,
    normalized_smd_representation,
};

fn main() {
    let dat_path = PathBuf::from(env::args_os().nth(1).unwrap());
    let rom_dir = PathBuf::from(env::args_os().nth(2).unwrap());
    let limit: usize = env::args_os()
        .nth(3)
        .and_then(|s| s.to_str().map(|s| s.to_owned()))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20000);

    let outcome = parse_dat_file(&dat_path, DatLimits::default()).unwrap();
    let index = DatIndex::build(&outcome.dat);
    println!(
        "DAT: {} entries={} sha1_count={} md5_count={} crc32_count={}",
        dat_path.display(),
        outcome.dat.games.len(),
        index.sha1_count(),
        index.md5_count(),
        index.crc32_count()
    );

    let mut checked = 0usize;
    let mut matched = 0usize;
    for entry in std::fs::read_dir(&rom_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if checked >= limit {
            break;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        checked += 1;
        let evidence = hash_bytes(
            &bytes,
            path.to_str().unwrap_or(""),
            &entry.file_name().to_string_lossy(),
        );
        let (_, verdict) = audit_representation(
            &RepresentationHashes {
                representation: ByteRepresentation::Physical,
                evidence,
            },
            &index,
        );
        if verdict.is_confident() {
            matched += 1;
            println!("PHYSICAL MATCH: {} -> {:?}", path.display(), verdict);
        }

        // Try any wired normalization too.
        for (label, result) in [
            (
                "n64",
                normalized_n64_representation(&bytes, path.to_str().unwrap_or(""), "n"),
            ),
            (
                "header-strip",
                normalized_header_stripped_representation(&bytes, path.to_str().unwrap_or(""), "n"),
            ),
            (
                "smd",
                normalized_smd_representation(&bytes, path.to_str().unwrap_or(""), "n"),
            ),
        ] {
            if let Some((normalized, identical)) = result {
                let (_, normalized_verdict) = audit_representation(&normalized, &index);
                if normalized_verdict.is_confident() {
                    println!(
                        "NORMALIZED MATCH ({label}, identical_to_physical={identical}): {} -> {:?}",
                        path.display(),
                        normalized_verdict
                    );
                }
            }
        }
    }
    println!("Checked {checked} files, {matched} physical matches");
}

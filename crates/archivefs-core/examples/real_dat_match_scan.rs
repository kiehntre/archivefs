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
        checked += 1;

        if path.extension().and_then(|e| e.to_str()) == Some("zip") {
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            let Ok(mut archive) = zip::ZipArchive::new(file) else {
                continue;
            };
            for i in 0..archive.len() {
                let Ok(mut member) = archive.by_index(i) else {
                    continue;
                };
                if member.is_dir() || member.size() > 64 * 1024 * 1024 {
                    continue;
                }
                let mut bytes = Vec::with_capacity(member.size() as usize);
                if std::io::Read::read_to_end(&mut member, &mut bytes).is_err() {
                    continue;
                }
                let member_name = member.name().to_string();
                if check_and_report(&bytes, &path, &member_name, &index) {
                    matched += 1;
                }
            }
            continue;
        }

        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if check_and_report(&bytes, &path, &entry.file_name().to_string_lossy(), &index) {
            matched += 1;
        }
    }
    println!("Checked {checked} files, {matched} physical matches");
}

/// Hashes `bytes` (from a bare file or a ZIP member) and reports any
/// physical or normalized DAT match. Returns whether a physical match was
/// found.
fn check_and_report(bytes: &[u8], path: &std::path::Path, label: &str, index: &DatIndex) -> bool {
    let evidence = hash_bytes(bytes, path.to_str().unwrap_or(""), label);
    let (_, verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence,
        },
        index,
    );
    let is_match = verdict.is_confident();
    if is_match {
        println!(
            "PHYSICAL MATCH: {} [{label}] -> {:?}",
            path.display(),
            verdict
        );
    }

    for (norm_label, result) in [
        (
            "n64",
            normalized_n64_representation(bytes, path.to_str().unwrap_or(""), label),
        ),
        (
            "header-strip",
            normalized_header_stripped_representation(bytes, path.to_str().unwrap_or(""), label),
        ),
        (
            "smd",
            normalized_smd_representation(bytes, path.to_str().unwrap_or(""), label),
        ),
    ] {
        if let Some((normalized, identical)) = result {
            let (_, normalized_verdict) = audit_representation(&normalized, index);
            if normalized_verdict.is_confident() {
                println!(
                    "NORMALIZED MATCH ({norm_label}, identical_to_physical={identical}): {} [{label}] -> {:?}",
                    path.display(),
                    normalized_verdict
                );
            }
        }
    }
    is_match
}

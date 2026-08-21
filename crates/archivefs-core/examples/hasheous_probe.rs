//! Batch 20 live, narrowly-scoped probe: hash a file locally, send only the
//! hash to Hasheous, and print the lineage-aware result.
//!
//! Deliberately takes NO arbitrary path argument by default - the default
//! run uses a hardcoded, tiny, in-memory synthetic byte buffer, so running
//! this example with no arguments makes zero filesystem access and zero
//! assumption about what is on this machine.
//!
//! ```text
//! cargo run -p archivefs-core --example hasheous_probe
//! cargo run -p archivefs-core --example hasheous_probe -- --file /path/to/rom
//! ```
//!
//! `--file <path>` is the one opt-in exception: it reads that file
//! READ-ONLY, hashes it locally, and only the resulting hash values are
//! sent over the network - never the path, filename, or byte content. No
//! mutation of any kind is ever performed on the named file.

use std::sync::atomic::AtomicBool;

use archivefs_core::identity_source::hasheous::{
    HasheousClient, HasheousConfig, HasheousHashSet, HasheousLookupOutcome, UreqTransport,
    now_unix, observations_from_hash_lookup,
};
use archivefs_core::platform_evidence_fusion::evidence_lineage::{
    Representation, render_evidence_summary,
};
use archivefs_core::safe_read::TrustedRoots;

fn hash_of_bytes(bytes: &[u8]) -> (String, String, String) {
    use md5::Md5;
    use sha1::Sha1;
    use sha1::digest::Digest;

    let crc = archivefs_core::identity_source::hashing::Crc32::of(bytes);
    let mut md5_hasher = Md5::new();
    md5_hasher.update(bytes);
    let md5_hex = hex_of(&md5_hasher.finalize());
    let mut sha1_hasher = Sha1::new();
    sha1_hasher.update(bytes);
    let sha1_hex = hex_of(&sha1_hasher.finalize());
    (crc, md5_hex, sha1_hex)
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let file_arg = std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[0] == "--file")
        .map(|pair| pair[1].clone());

    let (crc, md5_hex, sha1_hex, representation, source_desc) = match &file_arg {
        Some(path) => {
            let path = std::path::PathBuf::from(path);
            let trusted = TrustedRoots::from_paths([path.parent().unwrap_or(&path)]);
            let hashes = archivefs_core::identity_source::hashing::hash_file(&path, &trusted, None)
                .expect("hash the file read-only");
            (
                hashes.crc32,
                hashes.md5,
                hashes.sha1,
                Representation::PhysicalFile,
                "(user-supplied file, hashed locally, path not sent)".to_string(),
            )
        }
        None => {
            let bytes: Vec<u8> = (0u8..64).collect();
            let (crc, md5_hex, sha1_hex) = hash_of_bytes(&bytes);
            (
                crc,
                md5_hex,
                sha1_hex,
                Representation::PhysicalFile,
                "(64-byte synthetic in-memory fixture)".to_string(),
            )
        }
    };

    println!("Source: {source_desc}");
    println!("CRC32:  {crc}");
    println!("MD5:    {md5_hex}");
    println!("SHA1:   {sha1_hex}");
    println!();

    let config = HasheousConfig {
        enabled: true,
        ..HasheousConfig::default()
    };
    let transport = UreqTransport::default();
    let client = HasheousClient::new(&config, &transport);
    let hash_set = HasheousHashSet {
        crc: Some(crc),
        md5: Some(md5_hex),
        sha1: Some(sha1_hex.clone()),
        sha256: None,
    };
    let cancel = AtomicBool::new(false);

    match client.lookup(&hash_set, Some(&cancel)) {
        Ok(HasheousLookupOutcome::NoMatch) => {
            println!("Hasheous: no match (a valid, neutral result).");
        }
        Ok(HasheousLookupOutcome::Found(response)) => {
            let observations = observations_from_hash_lookup(
                &response,
                representation,
                &sha1_hex,
                Some(now_unix()),
            );
            println!("Hasheous: {} observation(s) found.\n", observations.len());
            for observation in &observations {
                println!(
                    "  channel={:?} upstream_source={:?} claim={:?} strength={:?} lineage={:?}",
                    observation.provenance.channel,
                    observation.provenance.upstream_source,
                    observation.claim,
                    observation.claim_strength,
                    observation.provenance.lineage,
                );
            }
            println!("\n{}", render_evidence_summary(&observations));
        }
        Err(error) => {
            println!("Hasheous request failed: {}", error.detail());
        }
    }
}

use super::*;
use crate::dat::model::{DatEcosystem, DatFormat, DatGameEntry, DatRomEntry, DatSource, ParsedDat};

fn index_with_entries(entries: &[(&str, &KnownFileEvidence)]) -> DatIndex {
    let games = entries
        .iter()
        .map(|(game_name, evidence)| DatGameEntry {
            name: (*game_name).to_string(),
            description: None,
            roms: vec![DatRomEntry {
                name: format!("{game_name}.bin"),
                size_bytes: evidence.size_bytes,
                crc32: evidence.crc32.clone(),
                md5: evidence.md5.clone(),
                sha1: evidence.sha1.clone(),
                sha256: evidence.sha256.clone(),
                status: None,
                merge: None,
                date: None,
                loadflag: None,
                ..Default::default()
            }],
            clone_of: None,
            sample_of: None,
            board: None,
            rebuild_to: None,
            year: None,
            manufacturer: None,
            source_file: None,
            comment: None,
            original_metadata: Default::default(),
            content_classification: Default::default(),
            unsupported_structure: false,
            ..Default::default()
        })
        .collect();
    let dat = ParsedDat {
        source: DatSource {
            format: DatFormat::Logiqx,
            ecosystem: DatEcosystem::GenericLogiqx,
            file_path: "test.dat".into(),
            name: Some("Test".into()),
            description: None,
            version: None,
            author: None,
            homepage: None,
            clrmamepro_header: None,
            entry_count: entries.len(),
            rom_count: entries.len(),
            parse_warnings: Vec::new(),
        },
        games,
    };
    DatIndex::build(&dat)
}

// ------------------------------------------------------------------
// ByteRepresentation (section 3)
// ------------------------------------------------------------------

#[test]
fn physical_is_physical() {
    assert!(ByteRepresentation::Physical.is_physical());
    assert!(!ByteRepresentation::Physical.is_normalized());
}

#[test]
fn normalized_is_normalized() {
    let rep = ByteRepresentation::Normalized {
        transform: "n64_byte_order",
    };
    assert!(rep.is_normalized());
    assert!(!rep.is_physical());
}

#[test]
fn archive_member_is_neither_physical_nor_normalized() {
    let rep = ByteRepresentation::ArchiveMember {
        member_name: "game.nes".to_string(),
    };
    assert!(!rep.is_physical());
    assert!(!rep.is_normalized());
}

#[test]
fn representations_with_different_transforms_are_not_equal() {
    assert_ne!(
        ByteRepresentation::Normalized {
            transform: "n64_byte_order"
        },
        ByteRepresentation::Normalized {
            transform: "smd_deinterleave"
        }
    );
}

#[test]
fn representation_is_debuggable() {
    let rep = ByteRepresentation::Physical;
    assert!(format!("{rep:?}").contains("Physical"));
}

// ------------------------------------------------------------------
// hash_bytes / observe_representation (section 4)
// ------------------------------------------------------------------

#[test]
fn hash_bytes_computes_all_four_algorithms() {
    let evidence = hash_bytes(b"hello world", "path", "file");
    assert!(evidence.crc32.is_some());
    assert!(evidence.md5.is_some());
    assert!(evidence.sha1.is_some());
    assert!(evidence.sha256.is_some());
}

#[test]
fn hash_bytes_records_the_byte_length() {
    let evidence = hash_bytes(b"hello world", "path", "file");
    assert_eq!(evidence.size_bytes, Some(11));
}

#[test]
fn hash_bytes_sha256_matches_a_known_vector() {
    // Empty input's SHA-256 is a well-known published constant.
    let evidence = hash_bytes(b"", "path", "file");
    assert_eq!(
        evidence.sha256.as_deref(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
}

#[test]
fn hash_bytes_is_deterministic() {
    assert_eq!(
        hash_bytes(b"same bytes", "a", "b").crc32,
        hash_bytes(b"same bytes", "c", "d").crc32
    );
}

#[test]
fn different_bytes_produce_different_hashes() {
    let a = hash_bytes(b"payload one", "a", "a");
    let b = hash_bytes(b"payload two", "a", "a");
    assert_ne!(a.sha256, b.sha256);
    assert_ne!(a.crc32, b.crc32);
}

#[test]
fn observe_representation_pairs_the_hash_with_its_representation() {
    let observed = observe_representation(ByteRepresentation::Physical, b"data", "p", "f");
    assert_eq!(observed.representation, ByteRepresentation::Physical);
    assert!(observed.evidence.sha256.is_some());
}

// ------------------------------------------------------------------
// audit_representation
// ------------------------------------------------------------------

#[test]
fn audit_representation_returns_the_same_representation_it_was_given() {
    let evidence = hash_bytes(b"unmatched bytes", "p", "f");
    let index = index_with_entries(&[]);
    let observed = RepresentationHashes {
        representation: ByteRepresentation::Normalized {
            transform: "n64_byte_order",
        },
        evidence,
    };
    let (representation, verdict) = audit_representation(&observed, &index);
    assert_eq!(
        representation,
        ByteRepresentation::Normalized {
            transform: "n64_byte_order"
        }
    );
    assert_eq!(verdict, AuditVerdict::NotInDat);
}

// ------------------------------------------------------------------
// compare_representations / RepresentationMatchOutcome (sections 6, 7, 8, 13, 14)
// ------------------------------------------------------------------

#[test]
fn physical_only_confident_is_physical_only() {
    let physical = hash_bytes(b"physical payload", "p", "f");
    let index = index_with_entries(&[("Game", &physical)]);
    let (_, verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: physical,
        },
        &index,
    );
    assert!(verdict.is_confident());
    let outcome = compare_representations(verdict, None, false);
    assert!(matches!(
        outcome,
        RepresentationMatchOutcome::PhysicalOnly { .. }
    ));
    assert!(outcome.is_confident());
    assert!(!outcome.is_conflict());
}

#[test]
fn no_normalized_and_no_physical_match_is_no_match() {
    let physical = hash_bytes(b"totally unmatched", "p", "f");
    let index = index_with_entries(&[]);
    let (_, verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: physical,
        },
        &index,
    );
    let outcome = compare_representations(verdict, None, false);
    assert_eq!(outcome, RepresentationMatchOutcome::NoMatch);
    assert!(!outcome.is_confident());
}

#[test]
fn normalized_only_confident_when_physical_bytes_never_match() {
    // This is the milestone's headline scenario (section 12): physical
    // bytes do not match anything, normalized bytes do.
    let normalized_bytes = b"canonical normalized payload";
    let normalized_evidence = hash_bytes(normalized_bytes, "p", "f");
    let index = index_with_entries(&[("Normalized Game", &normalized_evidence)]);

    let physical_evidence = hash_bytes(b"physically different byte order", "p", "f");
    let (_, physical_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: physical_evidence,
        },
        &index,
    );
    let (_, normalized_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Normalized {
                transform: "n64_byte_order",
            },
            evidence: normalized_evidence,
        },
        &index,
    );
    assert!(!physical_verdict.is_confident());
    assert!(normalized_verdict.is_confident());

    let outcome = compare_representations(physical_verdict, Some(normalized_verdict), false);
    assert!(matches!(
        outcome,
        RepresentationMatchOutcome::NormalizedOnly { .. }
    ));
}

#[test]
fn both_agree_when_both_confidently_match_the_same_game() {
    let bytes = b"identical payload used both ways";
    let evidence = hash_bytes(bytes, "p", "f");
    let index = index_with_entries(&[("Same Game", &evidence)]);

    let (_, physical_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: evidence.clone(),
        },
        &index,
    );
    let (_, normalized_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Normalized {
                transform: "z64_identity",
            },
            evidence,
        },
        &index,
    );
    let outcome = compare_representations(physical_verdict, Some(normalized_verdict), true);
    match outcome {
        RepresentationMatchOutcome::BothAgree {
            identical_bytes, ..
        } => {
            assert!(identical_bytes);
        }
        other => panic!("expected BothAgree, got {other:?}"),
    }
}

#[test]
fn both_agree_marks_non_identical_bytes_when_they_genuinely_differ() {
    // Two representations that are NOT byte-identical but both happen to
    // resolve (via their own hash) to the same DAT game entry - a rarer
    // but legitimate case (e.g. two different valid dumps of the same
    // release). identical_bytes must be false here.
    let physical_bytes = b"physical byte stream A";
    let normalized_bytes = b"normalized byte stream B is different";
    let physical_evidence = hash_bytes(physical_bytes, "p", "f");
    let normalized_evidence = hash_bytes(normalized_bytes, "p", "f");
    // Two distinct DAT game entries sharing the same name - the real shape
    // a release with multiple valid dumps would take in a catalogue.
    let index = index_with_entries(&[
        ("Shared Game", &physical_evidence),
        ("Shared Game", &normalized_evidence),
    ]);
    let (_, physical_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: physical_evidence,
        },
        &index,
    );
    let (_, normalized_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Normalized {
                transform: "smd_deinterleave",
            },
            evidence: normalized_evidence,
        },
        &index,
    );
    assert!(physical_verdict.is_confident());
    assert!(normalized_verdict.is_confident());
    let outcome = compare_representations(physical_verdict, Some(normalized_verdict), false);
    match outcome {
        RepresentationMatchOutcome::BothAgree {
            identical_bytes, ..
        } => {
            assert!(!identical_bytes);
        }
        other => panic!("expected BothAgree, got {other:?}"),
    }
}

#[test]
fn disagree_when_physical_and_normalized_name_different_games_never_a_winner() {
    let physical_bytes = b"belongs to game one entirely";
    let normalized_bytes = b"belongs to game two entirely, unrelated";
    let physical_evidence = hash_bytes(physical_bytes, "p", "f");
    let normalized_evidence = hash_bytes(normalized_bytes, "p", "f");
    let index = index_with_entries(&[
        ("Game One", &physical_evidence),
        ("Game Two", &normalized_evidence),
    ]);

    let (_, physical_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: physical_evidence,
        },
        &index,
    );
    let (_, normalized_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Normalized {
                transform: "n64_byte_order",
            },
            evidence: normalized_evidence,
        },
        &index,
    );
    let outcome = compare_representations(physical_verdict, Some(normalized_verdict), false);
    assert!(outcome.is_conflict());
    match outcome {
        RepresentationMatchOutcome::Disagree {
            physical_verdict,
            normalized_verdict,
        } => {
            assert!(matches!(physical_verdict, AuditVerdict::Exact { .. }));
            assert!(matches!(normalized_verdict, AuditVerdict::Exact { .. }));
        }
        other => panic!("expected Disagree, got {other:?}"),
    }
}

#[test]
fn disagree_never_silently_prefers_normalized() {
    let physical_bytes = b"physical identity A payload";
    let normalized_bytes = b"normalized identity B payload, unrelated entirely";
    let physical_evidence = hash_bytes(physical_bytes, "p", "f");
    let normalized_evidence = hash_bytes(normalized_bytes, "p", "f");
    let index = index_with_entries(&[
        ("Physical Game", &physical_evidence),
        ("Normalized Game", &normalized_evidence),
    ]);
    let (_, physical_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: physical_evidence,
        },
        &index,
    );
    let (_, normalized_verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Normalized {
                transform: "smd_deinterleave",
            },
            evidence: normalized_evidence,
        },
        &index,
    );
    let outcome = compare_representations(physical_verdict, Some(normalized_verdict), false);
    // The outcome must retain BOTH verdicts - never collapse to one.
    match outcome {
        RepresentationMatchOutcome::Disagree {
            physical_verdict,
            normalized_verdict,
        } => {
            assert_ne!(physical_verdict, normalized_verdict);
        }
        other => panic!("expected Disagree, got {other:?}"),
    }
}

#[test]
fn probable_crc_only_match_never_upgrades_to_a_confident_outcome() {
    // A Probable (CRC32-only) verdict on one side, nothing on the other -
    // must fall through to NoMatch, not PhysicalOnly/NormalizedOnly.
    let evidence = hash_bytes(b"crc only scenario", "p", "f");
    let mut crc_only = KnownFileEvidence::new("p", "f");
    crc_only.crc32 = evidence.crc32.clone();
    crc_only.size_bytes = evidence.size_bytes;
    let index = index_with_entries(&[("Some Game", &evidence)]);
    let (_, verdict) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: crc_only,
        },
        &index,
    );
    assert!(matches!(verdict, AuditVerdict::Probable { .. }));
    assert!(!verdict.is_confident());
    let outcome = compare_representations(verdict, None, false);
    assert_eq!(outcome, RepresentationMatchOutcome::NoMatch);
}

// ------------------------------------------------------------------
// Determinism
// ------------------------------------------------------------------

#[test]
fn compare_representations_is_deterministic() {
    let physical = hash_bytes(b"determinism check", "p", "f");
    let index = index_with_entries(&[("Game", &physical)]);
    let (_, verdict_a) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: physical.clone(),
        },
        &index,
    );
    let (_, verdict_b) = audit_representation(
        &RepresentationHashes {
            representation: ByteRepresentation::Physical,
            evidence: physical,
        },
        &index,
    );
    assert_eq!(
        compare_representations(verdict_a.clone(), None, false),
        compare_representations(verdict_b, None, false)
    );
}

// ------------------------------------------------------------------
// No action authority
// ------------------------------------------------------------------

#[test]
fn dat_hash_representation_source_never_references_mutation_modules() {
    let source = include_str!("../dat_hash_representation.rs");
    for forbidden in [
        "crate::repair",
        "rename_plan",
        "rename_apply",
        "std::fs::remove",
        "std::fs::rename",
        "std::fs::write",
    ] {
        assert!(
            !source.contains(forbidden),
            "dat_hash_representation.rs unexpectedly references {forbidden:?}"
        );
    }
}

// ------------------------------------------------------------------
// normalized_n64_representation (section 5)
// ------------------------------------------------------------------

fn synthetic_z64_payload() -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    bytes[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
    bytes
}

#[test]
fn n64_v64_yields_a_normalized_representation_that_differs_from_physical() {
    let z64 = synthetic_z64_payload();
    let v64 =
        crate::n64_byte_order::denormalize_from_z64(&z64, crate::n64_byte_order::N64ByteOrder::V64)
            .unwrap();
    let (normalized, identical) = normalized_n64_representation(&v64, "p", "f").unwrap();
    assert!(!identical);
    assert_eq!(
        normalized.evidence.sha256,
        hash_bytes(&z64, "p", "f").sha256
    );
    assert_eq!(
        normalized.representation,
        ByteRepresentation::Normalized {
            transform: crate::n64_byte_order::N64_BYTE_ORDER_TRANSFORM_ID
        }
    );
}

#[test]
fn n64_z64_yields_a_normalized_representation_identical_to_physical() {
    let z64 = synthetic_z64_payload();
    let (normalized, identical) = normalized_n64_representation(&z64, "p", "f").unwrap();
    assert!(identical);
    assert_eq!(
        normalized.evidence.sha256,
        hash_bytes(&z64, "p", "f").sha256
    );
}

#[test]
fn n64_unrecognized_bytes_yield_no_normalized_representation() {
    assert!(normalized_n64_representation(b"too short", "p", "f").is_none());
}

// ------------------------------------------------------------------
// normalized_header_stripped_representation (section 5)
// ------------------------------------------------------------------

fn synthetic_lynx_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 64 + 2048];
    bytes[0..4].copy_from_slice(b"LYNX");
    for (i, byte) in bytes[64..].iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    bytes
}

#[test]
fn lynx_yields_a_normalized_representation_that_differs_from_physical() {
    let physical = synthetic_lynx_bytes();
    let (normalized, identical) =
        normalized_header_stripped_representation(&physical, "p", "f").unwrap();
    assert!(!identical);
    assert_ne!(
        normalized.evidence.sha256,
        hash_bytes(&physical, "p", "f").sha256
    );
}

#[test]
fn unrecognized_bytes_yield_no_header_stripped_representation() {
    let plain = vec![0u8; 128];
    assert!(normalized_header_stripped_representation(&plain, "p", "f").is_none());
}

// ------------------------------------------------------------------
// normalized_smd_representation (section 5)
// ------------------------------------------------------------------

fn synthetic_smd_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 512 + 16384];
    for (i, byte) in bytes[512..].iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    bytes
}

#[test]
fn smd_yields_a_normalized_representation_that_differs_from_physical() {
    let physical = synthetic_smd_bytes();
    let (normalized, identical) = normalized_smd_representation(&physical, "p", "f").unwrap();
    assert!(!identical);
    assert_ne!(
        normalized.evidence.sha256,
        hash_bytes(&physical, "p", "f").sha256
    );
}

#[test]
fn too_short_bytes_yield_no_smd_representation() {
    assert!(normalized_smd_representation(b"short", "p", "f").is_none());
}

// ------------------------------------------------------------------
// Real corpus (run manually this session - see the crate-level module
// documentation for the results; not embedded here since /mnt/games/roms
// is outside this repository and not present on another machine or CI).
// ------------------------------------------------------------------

#[test]
fn real_corpus_validation_is_documented_not_embedded() {
    // Structural placeholder: the real N64 v64/z64 and Lynx cross-checks
    // for this exact pairing (normalized_n64_representation /
    // normalized_header_stripped_representation) were run manually this
    // session against real corpus files and are reported in the final
    // report and in this module's own doc comment - never re-run here as
    // an automated #[test], since that would silently pass or fail
    // depending on whether /mnt/games/roms happens to exist on whatever
    // machine runs `cargo test`.
}

//! Tests for read-only pairwise duplicate-content proof.
//!
//! Every fixture lives in a fresh `tempfile::tempdir()`; nothing here ever
//! touches a real library. The mid-proof "race" tests do not race anything:
//! they call the crate-private `prove_duplicate_content_checkpointed` and
//! inject the mutation synchronously, on the same thread, from inside the
//! checkpoint callback that fires at the exact point in the proof under
//! test. This makes the mutation land deterministically every run, and lets
//! each test assert both that the mutation actually happened and that the
//! production proof never returned `Ok` for it - no thread, no sleep, no
//! scheduler-dependent retry loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::repair::proposal::{DeferredActionKind, RepairAction, RepairEvidenceKind};
use crate::safe_read::TrustedRoots;

use super::*;

fn trusted() -> TrustedRoots {
    TrustedRoots::none()
}

fn write(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write fixture");
    path
}

// --- same bytes, different paths -> proven duplicate ------------------------

#[test]
fn same_bytes_different_paths_are_a_proven_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"identical payload");
    let b = write(dir.path(), "b.bin", b"identical payload");

    let mut cache = DuplicateHashCache::new();
    let proof = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap();

    assert_eq!(proof.path_a, a);
    assert_eq!(proof.path_b, b);
    assert_eq!(proof.size_bytes, b"identical payload".len() as u64);
    assert_eq!(proof.algorithm, HashAlgorithm::Sha1);
    assert_eq!(proof.hash.len(), 40, "a sha1 hex digest");
    assert_eq!(
        proof.classification,
        DuplicatePairClassification::DistinctObjects
    );
    assert!(proof.is_distinct_object_duplicate());
}

// --- same name, different bytes -> not duplicate -----------------------------

#[test]
fn same_filename_in_different_directories_with_different_bytes_is_not_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let dir_a = dir.path().join("a");
    let dir_b = dir.path().join("b");
    std::fs::create_dir(&dir_a).unwrap();
    std::fs::create_dir(&dir_b).unwrap();
    // Same basename, different directories, different content and size.
    let a = write(&dir_a, "game.bin", b"short");
    let b = write(&dir_b, "game.bin", b"a much longer different payload");

    let mut cache = DuplicateHashCache::new();
    let error = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap_err();
    assert!(
        matches!(error, DuplicateProofRefusal::SizeMismatch { .. }),
        "{error:?}"
    );
}

// --- same size, different bytes -> not duplicate ------------------------------

#[test]
fn same_size_different_bytes_is_not_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"0123456789");
    let b = write(dir.path(), "b.bin", b"abcdefghij");
    assert_eq!(
        std::fs::metadata(&a).unwrap().len(),
        std::fs::metadata(&b).unwrap().len()
    );

    let mut cache = DuplicateHashCache::new();
    let error = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap_err();
    assert_eq!(error, DuplicateProofRefusal::HashMismatch);
}

// --- different names, same bytes -> still provable if directly compared ------

#[test]
fn different_names_same_bytes_is_still_provable() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(
        dir.path(),
        "totally-different-name.rom",
        b"same content here",
    );
    let b = write(
        dir.path(),
        "another-name-entirely.bin",
        b"same content here",
    );

    let mut cache = DuplicateHashCache::new();
    let proof = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap();
    assert_eq!(
        proof.classification,
        DuplicatePairClassification::DistinctObjects
    );
}

// --- hard-link pair classified explicitly -------------------------------------

#[cfg(unix)]
#[test]
fn a_hard_linked_pair_is_classified_as_the_same_object() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"shared content");
    let b = dir.path().join("b.bin");
    std::fs::hard_link(&a, &b).unwrap();

    let mut cache = DuplicateHashCache::new();
    let proof = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap();

    assert_eq!(
        proof.classification,
        DuplicatePairClassification::SameObject
    );
    assert!(!proof.is_distinct_object_duplicate());
    let evidence = proof.evidence();
    assert!(evidence.detail.contains("same filesystem object"));
}

// --- deterministic mid-proof mutation: replaced object -----------------------

/// `a` is swapped for a pre-existing, distinct-inode object (same size,
/// content unchanged) at the [`ProofCheckpoint::AfterHashingA`] checkpoint -
/// i.e. strictly *after* `a` has already been hashed (or served from cache)
/// but *before* `b` is even looked at. This is the exact window a cross-call
/// stale-cache bug or an insufficiently strong post-hash revalidation would
/// miss.
#[test]
fn a_file_replaced_with_a_distinct_object_after_it_was_hashed_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let payload = b"identical payload, sixteen".to_vec();
    let a = write(dir.path(), "a.bin", &payload);
    let b = write(dir.path(), "b.bin", &payload);
    let replacement = write(dir.path(), "replacement.bin", &payload);

    let mut mutation_happened = false;
    let mut cache = DuplicateHashCache::new();
    let result =
        prove_duplicate_content_checkpointed(&a, &b, &mut cache, &trusted(), None, &mut |cp| {
            if cp == ProofCheckpoint::AfterHashingA {
                std::fs::rename(&replacement, &a).unwrap();
                mutation_happened = true;
            }
        });

    assert!(
        mutation_happened,
        "the checkpoint that should have fired never did"
    );
    let error = result.expect_err("a replacement after hashing must never be proven a duplicate");
    assert!(
        matches!(
            error,
            DuplicateProofRefusal::ContentModifiedDuringProof { .. }
        ),
        "{error:?}"
    );
    // The replacement genuinely landed: `a` now has the replacement's
    // identity, proving the mutation was not a no-op.
    assert!(std::fs::symlink_metadata(&replacement).is_err());
}

// --- deterministic mid-proof mutation: in-place same-size rewrite ------------

/// `a` is rewritten in place (same path, same inode, same size, different
/// bytes) at [`ProofCheckpoint::AfterHashingA`] - after its digest was
/// already produced, while `b` has not yet been hashed. Same inode and size
/// mean `identity_matches` alone would never catch this; only the
/// full-precision mtime folded into [`ProofObjectKey`] can.
#[test]
fn a_file_modified_in_place_after_it_was_hashed_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let payload = b"identical payload, sixteen".to_vec();
    let mutated_payload = b"replaced-payload, sixteen!".to_vec();
    assert_eq!(payload.len(), mutated_payload.len(), "same size, required");
    let a = write(dir.path(), "a.bin", &payload);
    let b = write(dir.path(), "b.bin", &payload);

    let identity_before = capture_identity(&a).unwrap();

    let mut mutation_happened = false;
    let mut cache = DuplicateHashCache::new();
    let result =
        prove_duplicate_content_checkpointed(&a, &b, &mut cache, &trusted(), None, &mut |cp| {
            if cp == ProofCheckpoint::AfterHashingA {
                std::fs::write(&a, &mutated_payload).unwrap();
                mutation_happened = true;
            }
        });

    assert!(
        mutation_happened,
        "the checkpoint that should have fired never did"
    );
    let identity_after = capture_identity(&a).unwrap();
    #[cfg(unix)]
    assert_eq!(
        identity_before.ino, identity_after.ino,
        "the rewrite must be genuinely in-place (same inode), not a swap"
    );
    assert_eq!(
        identity_before.size_bytes, identity_after.size_bytes,
        "the rewrite must be genuinely same-size"
    );
    assert_eq!(
        std::fs::read(&a).unwrap(),
        mutated_payload,
        "the mutation actually took effect"
    );

    let error = result.expect_err("an in-place same-size rewrite must never be proven a duplicate");
    assert!(
        matches!(
            error,
            DuplicateProofRefusal::ContentModifiedDuringProof { .. }
        ),
        "{error:?}"
    );
}

// --- sub-second in-place modification --------------------------------------

/// A variant of the in-place rewrite above, phrased around the specific
/// hostile finding: a same-inode, same-size rewrite whose modification time,
/// truncated to whole seconds, could plausibly collide with the original -
/// only a full-precision mtime comparison is guaranteed to separate them.
/// The checkpoint makes this deterministic rather than depending on the two
/// writes actually landing in the same wall-clock second.
#[test]
fn a_same_inode_same_size_sub_second_modification_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let payload = vec![0_u8; 4096];
    let mut mutated_payload = payload.clone();
    mutated_payload[0] = 1;
    let a = write(dir.path(), "a.bin", &payload);
    let b = write(dir.path(), "b.bin", &payload);

    let mut cache = DuplicateHashCache::new();
    let result =
        prove_duplicate_content_checkpointed(&a, &b, &mut cache, &trusted(), None, &mut |cp| {
            if cp == ProofCheckpoint::AfterHashingA {
                // Rewrite via an open file handle at the same path (no
                // rename/no new inode): the most adversarial in-place case, and
                // the fastest possible wall-clock gap between old and new mtime.
                use std::io::{Seek, SeekFrom, Write};
                let mut file = std::fs::OpenOptions::new().write(true).open(&a).unwrap();
                file.seek(SeekFrom::Start(0)).unwrap();
                file.write_all(&mutated_payload).unwrap();
                file.flush().unwrap();
            }
        });

    let error = result.expect_err("a sub-second in-place rewrite must never be proven a duplicate");
    assert!(
        matches!(
            error,
            DuplicateProofRefusal::ContentModifiedDuringProof { .. }
        ),
        "{error:?}"
    );
    assert_eq!(std::fs::read(&a).unwrap(), mutated_payload);
}

// --- pairwise cache interaction: stale cache after inode replacement --------

/// A hashes successfully in an A-B pair (its digest lands in the shared
/// [`DuplicateHashCache`]). `a` is then replaced - same size, mtime forced
/// back to exactly what it was before the swap, so both the coarse
/// whole-second key *and* a naively-precise-but-unverified mtime could
/// plausibly coincide - with a different-inode, different-content object.
/// A-C is then evaluated against the *same* cache instance
/// [`prove_duplicate_group`] would share across a whole candidate group. The
/// result for A-C must never be `Ok`: a stale digest for `a`'s old content
/// must never be silently reused for what is now a different object.
#[test]
fn a_stale_cache_entry_after_inode_replacement_is_never_reused() {
    let dir = tempfile::tempdir().unwrap();
    let shared_payload = b"shared-twenty-bytes!".to_vec();
    let different_payload = b"different-twenty-byt".to_vec();
    assert_eq!(shared_payload.len(), different_payload.len());

    let a = write(dir.path(), "a.bin", &shared_payload);
    let b = write(dir.path(), "b.bin", &shared_payload);
    // `c` matches `a`'s *original* content - if a stale cache entry for `a`
    // were reused, A-C would falsely prove a duplicate.
    let c = write(dir.path(), "c.bin", &shared_payload);
    let replacement = write(dir.path(), "replacement.bin", &different_payload);

    let mtime_before = std::fs::symlink_metadata(&a).unwrap().modified().unwrap();

    let mut cache = DuplicateHashCache::new();

    // A-B: proves fine, and caches A's digest.
    let ab = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None);
    assert!(ab.is_ok(), "{ab:?}");

    // Replace `a` with a distinct object (different inode), same size,
    // mtime forced back to what it was before the swap - the exact
    // adversarial case the cache key must not be fooled by.
    std::fs::rename(&replacement, &a).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&a)
        .unwrap()
        .set_modified(mtime_before)
        .unwrap();
    assert_eq!(
        std::fs::symlink_metadata(&a).unwrap().modified().unwrap(),
        mtime_before,
        "the replacement's mtime was forced to collide with the original"
    );

    // A-C, sharing the same cache A-B populated.
    let ac = prove_duplicate_content(&a, &c, &mut cache, &trusted(), None);
    assert!(
        ac.is_err(),
        "a stale cache entry for a's old content must never be reused for its replacement: {ac:?}"
    );
}

/// The same scenario, but driven through [`prove_duplicate_group`] exactly
/// as a real candidate-group analysis would call it - proving the fix holds
/// at the public, group-level API a caller actually uses, not only when the
/// cache is threaded through by hand.
#[test]
fn prove_duplicate_group_never_reuses_a_stale_digest_across_pairs() {
    let dir = tempfile::tempdir().unwrap();
    let shared_payload = b"shared-twenty-bytes!".to_vec();
    let different_payload = b"different-twenty-byt".to_vec();
    assert_eq!(shared_payload.len(), different_payload.len());

    let a = write(dir.path(), "a.bin", &shared_payload);
    let b = write(dir.path(), "b.bin", &shared_payload);
    let c = write(dir.path(), "c.bin", &shared_payload);

    // First pass: everything genuinely matches.
    let results = prove_duplicate_group(&[a.clone(), b.clone(), c.clone()], &trusted(), None);
    assert!(
        results.iter().all(|(_, _, result)| result.is_ok()),
        "{results:?}"
    );

    // Now replace `a`, forcing its mtime to collide with what it was.
    let mtime_before = std::fs::symlink_metadata(&a).unwrap().modified().unwrap();
    let replacement = write(dir.path(), "replacement.bin", &different_payload);
    std::fs::rename(&replacement, &a).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&a)
        .unwrap()
        .set_modified(mtime_before)
        .unwrap();

    let results = prove_duplicate_group(&[a.clone(), b.clone(), c.clone()], &trusted(), None);
    for (path_a, path_b, result) in &results {
        let touches_a = path_a == &a || path_b == &a;
        if touches_a {
            assert!(
                result.is_err(),
                "any pair touching the replaced `a` must refuse, never reuse a stale digest: \
                 {path_a:?} vs {path_b:?}: {result:?}"
            );
        } else {
            assert!(result.is_ok(), "b vs c is untouched and must still prove");
        }
    }
}

// --- ordinary unchanged duplicate still proves successfully -----------------

#[test]
fn an_ordinary_unchanged_duplicate_still_proves_successfully() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"nothing hostile happens here");
    let b = write(dir.path(), "b.bin", b"nothing hostile happens here");

    let mut cache = DuplicateHashCache::new();
    let proof = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap();
    assert_eq!(
        proof.classification,
        DuplicatePairClassification::DistinctObjects
    );

    // A repeated proof against the same, still-unchanged files, sharing the
    // cache, must also still succeed - the fix must not make ordinary reuse
    // impossible.
    let proof_again = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap();
    assert_eq!(proof, proof_again);
}

// --- unreadable/truncated file -> refuse -----------------------------------------

#[test]
fn a_missing_file_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("does-not-exist.bin");
    let b = write(dir.path(), "b.bin", b"content");

    let mut cache = DuplicateHashCache::new();
    let error = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap_err();
    assert!(
        matches!(error, DuplicateProofRefusal::NotReadable { .. }),
        "{error:?}"
    );
}

#[test]
fn a_directory_is_not_a_regular_file_and_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("subdir");
    std::fs::create_dir(&a).unwrap();
    let b = write(dir.path(), "b.bin", b"content");

    let mut cache = DuplicateHashCache::new();
    let error = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap_err();
    assert!(
        matches!(error, DuplicateProofRefusal::NotReadable { .. }),
        "{error:?}"
    );
}

// --- cancellation -> refuse cleanly -----------------------------------------------

#[test]
fn cancellation_refuses_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"content");
    let b = write(dir.path(), "b.bin", b"content");

    let mut cache = DuplicateHashCache::new();
    let cancel = AtomicBool::new(true);
    let error = prove_duplicate_content(&a, &b, &mut cache, &trusted(), Some(&cancel)).unwrap_err();
    assert_eq!(error, DuplicateProofRefusal::Cancelled);
}

// --- same path given twice --------------------------------------------------------

#[test]
fn the_same_path_given_twice_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"content");

    let mut cache = DuplicateHashCache::new();
    let error = prove_duplicate_content(&a, &a, &mut cache, &trusted(), None).unwrap_err();
    assert_eq!(error, DuplicateProofRefusal::SamePath);
}

// --- evidence serializes/deserializes ---------------------------------------------

#[test]
fn duplicate_content_proof_round_trips_through_json() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"identical payload");
    let b = write(dir.path(), "b.bin", b"identical payload");

    let mut cache = DuplicateHashCache::new();
    let proof = prove_duplicate_content(&a, &b, &mut cache, &trusted(), None).unwrap();

    let json = serde_json::to_string(&proof).unwrap();
    let decoded: DuplicateContentProof = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, proof);

    let evidence = proof.evidence();
    assert_eq!(evidence.kind, RepairEvidenceKind::DuplicateContent);
    assert!(evidence.detail.contains(&a.display().to_string()));
    assert!(evidence.detail.contains(&b.display().to_string()));
    let evidence_json = serde_json::to_string(&evidence).unwrap();
    assert!(
        serde_json::from_str::<crate::repair::proposal::RepairEvidence>(&evidence_json).is_ok()
    );
}

// --- DeleteDuplicate remains Deferred and non-executable --------------------------

#[test]
fn delete_duplicate_remains_deferred_and_non_executable() {
    let action = RepairAction::Deferred(DeferredActionKind::DeleteDuplicate);
    assert!(!action.is_executable());
    assert!(action.destination().is_none());
}

// --- candidate generation / grouping -----------------------------------------------

#[test]
fn candidate_pairs_generates_every_unordered_pair() {
    let paths: Vec<PathBuf> = ["a.bin", "b.bin", "c.bin"]
        .iter()
        .map(PathBuf::from)
        .collect();
    let pairs = candidate_pairs(&paths);
    assert_eq!(pairs.len(), 3);
    assert!(pairs.contains(&(paths[0].clone(), paths[1].clone())));
    assert!(pairs.contains(&(paths[0].clone(), paths[2].clone())));
    assert!(pairs.contains(&(paths[1].clone(), paths[2].clone())));
}

#[test]
fn candidate_pairs_of_a_single_path_is_empty() {
    let paths = vec![PathBuf::from("a.bin")];
    assert!(candidate_pairs(&paths).is_empty());
}

#[test]
fn prove_duplicate_group_proves_every_pair_and_reuses_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"same content across the group");
    let b = write(dir.path(), "b.bin", b"same content across the group");
    let c = write(dir.path(), "c.bin", b"same content across the group");

    let results = prove_duplicate_group(&[a, b, c], &trusted(), None);
    assert_eq!(results.len(), 3, "three unordered pairs from three paths");
    for (path_a, path_b, result) in &results {
        let proof = result
            .as_ref()
            .unwrap_or_else(|error| panic!("{path_a:?} vs {path_b:?}: {error}"));
        assert_eq!(
            proof.classification,
            DuplicatePairClassification::DistinctObjects
        );
    }
}

#[test]
fn prove_duplicate_group_never_falsely_groups_a_mismatched_member() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", b"same content across the group");
    let b = write(dir.path(), "b.bin", b"same content across the group");
    // Same filename convention, but genuinely different content: the group
    // itself (as a filename/platform candidate list) is only an
    // optimization - it must never be trusted as the classification.
    let d = write(dir.path(), "d.bin", b"an entirely different payload");

    let results = prove_duplicate_group(&[a.clone(), b.clone(), d.clone()], &trusted(), None);
    assert_eq!(results.len(), 3);
    for (path_a, path_b, result) in &results {
        if (path_a == &a && path_b == &b) || (path_a == &b && path_b == &a) {
            assert!(result.is_ok(), "a and b are genuinely identical");
        } else {
            assert!(
                result.is_err(),
                "d must never be classified a duplicate of {path_a:?}/{path_b:?} by filename alone"
            );
        }
    }
}

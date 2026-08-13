//! Bounded, read-only 7z member reading (POST-ALPHA-1.1, experimental).
//!
//! Implements [`super::ArchiveMemberSource`] for `.7z` using `sevenz-rust2`
//! (Apache-2.0, pure Rust), the maintained fork of the abandoned
//! `sevenz-rust`. Everything is in-process and read-only.
//!
//! # Hostile-input ordering
//!
//! Before `sevenz-rust2` is ever constructed, a dedicated pre-decoder probe
//! (`super::sevenz_preflight`) parses just enough of the 7z header to enforce
//! hard resource ceilings: next-header size, file/folder/coder/pack-stream
//! counts, LZMA/LZMA2 dictionary declarations, per-folder solid-decode budget,
//! decompression ratio, and the cumulative logical-byte budget. Archives that
//! declare hostile values — including any archive with an **encoded header**
//! (whose expansion cannot be bounded) — are refused **before** any large
//! allocation inside `sevenz-rust2`, and cancellation is checked during the
//! probe. The probe is deliberately not a complete decoder; it extracts only
//! the resource-relevant facts.
//!
//! After preflight passes, `ArchiveReader::new` runs with every
//! attacker-controlled allocation already bounded by the configured limits
//! (its next-header buffer is at most `max_header_bytes`; its file/block/
//! coder vectors are at most `max_members`; its decoder dictionaries are at
//! most `max_dictionary_bytes`). A belt-and-braces revalidation re-checks the
//! same limits against `sevenz-rust2`'s parsed `Archive`, and member hashing
//! re-enforces per-member and cumulative budgets while streaming.
//!
//! # Upstream advisories and the API surface this module may use
//!
//! This module previously used `sevenz-rust` 0.6.1, which carries two open
//! RUSTSEC entries (RUSTSEC-2026-0245, a path-traversal in the `decompress*`
//! convenience functions' shared `decompress_impl` body, and RUSTSEC-2026-0246,
//! unmaintained). It now uses the maintained fork `sevenz-rust2`, which has
//! neither.
//!
//! The extract-to-disk convenience helpers that carried the traversal defect
//! live behind `sevenz-rust2`'s `util` feature, and this crate enables **no**
//! features in the production build, so those functions are not compiled at
//! all. Independently of that, this module never extracts to disk: it only
//! uses `Archive`, `ArchiveReader` and `for_each_entries`, and hashes member
//! bytes in memory. Nothing in this crate may call `sevenz_rust2::decompress`,
//! `decompress_file`, `decompress_with_password` or any other `util` entry
//! point.
//!
//! The pre-decoder probe (`super::sevenz_preflight`) is retained unchanged.
//! Its hostile-input refusals are a defence-in-depth property of *this*
//! module, not a workaround for one upstream release: it bounds allocation
//! before any third-party parser sees attacker-controlled counts, and it
//! refuses malformed coder graphs regardless of which fork is underneath.
//!
//! # Cancellation granularity
//!
//! Cancellation is cooperative and is observed:
//!
//! - before and during the header probe (per read chunk and in every
//!   count-driven parse loop);
//! - between members;
//! - inside a member, before every 256 KiB chunk of *decoded output* (the
//!   hashing and draining loops).
//!
//! It is **not** observed inside a single `Read::read` call into
//! `sevenz-rust2`: that call is a plain blocking function with no yield point,
//! so the smallest uninterruptible unit is "decode until the next chunk of
//! output is available". A folder whose packed stream decodes very slowly into
//! very little output therefore stalls cancellation for as long as it takes to
//! consume that folder's packed bytes — bounded by the archive's own size and
//! by the compression-ratio, dictionary and solid-decode ceilings, but not by
//! the cancellation flag. `ArchiveReader::new` and the per-folder decoder-stack
//! construction are likewise uninterruptible; both are bounded (at most
//! `max_header_bytes` of header parsing and `max_aggregate_decoder_memory_bytes`
//! of allocation) and neither decodes member data.
//!
//! # Nested members
//!
//! Nested-archive members are surfaced with metadata but never recursively
//! opened; they **count toward the cumulative logical-byte budget** even
//! though their bytes are only drained, not hashed.
//!
//! # Zero production callers
//!
//! Nothing in shipped code calls this module. It is experimental scaffolding
//! exercised only by focused tests (`docs/research/SEVEN_Z_RAR_ARCHIVE_
//! VERIFICATION_RESEARCH.md` §12, slice 7Z-1).

use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use sevenz_rust2::Error as SevenZError;
use sevenz_rust2::{Archive, ArchiveEntry, ArchiveReader, Password};

use crate::safe_read::{TrustedRoots, open_bounded_read};

use super::hash::{MemberStreamError, hash_member_stream};
use super::limits::{ARCHIVE_HASH_CHUNK_BYTES, ArchiveLimits};
use super::{
    ArchiveMemberEvidence, ArchiveMemberSource, ArchiveMemberSourceError, ArchiveMemberStatus,
};
use crate::dat::archive::sevenz_preflight::{PreflightRefusal, preflight_sevenz};

/// File extensions treated as "nested archive" member names. Members with one
/// of these extensions are surfaced with metadata but never opened.
const NESTED_ARCHIVE_EXTENSIONS: &[&str] = &["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst"];

// Test-only count of `ArchiveReader::new` constructions, so hostile-input
// tests can prove the preflight refused an archive before the upstream decoder
// was ever reached.
//
// Thread-local, not global: the test harness runs tests in parallel and
// `construct_reader` always runs on the calling test's own thread, so a shared
// counter would let one test's successful open perturb another test's
// before/after comparison.
#[cfg(test)]
thread_local! {
    static READER_NEW_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Reads this thread's `construct_reader` call count.
#[cfg(test)]
fn reader_new_calls() -> usize {
    READER_NEW_CALLS.with(std::cell::Cell::get)
}

/// Precomputed, deterministic member metadata (stream-bearing files in the
/// archive's own order).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MemberMeta {
    name: String,
    logical_size: u64,
    is_nested: bool,
}

/// A bounded, read-only `.7z` archive source.
pub struct SevenZArchiveSource {
    archive: String,
    reader: ArchiveReader<std::fs::File>,
    members: Vec<MemberMeta>,
    limits: ArchiveLimits,
}

impl std::fmt::Debug for SevenZArchiveSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SevenZArchiveSource")
            .field("archive", &self.archive)
            .field("members", &self.members)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl SevenZArchiveSource {
    /// Opens `path` under `trusted`, preflights the header, and validates the
    /// archive up to decode.
    ///
    /// The pre-decoder probe runs first and refuses hostile archives before
    /// `sevenz-rust2` performs any attacker-controlled allocation. `cancel` is
    /// checked during the probe and can be shared with the later `verify_all`
    /// pass.
    pub fn open(
        path: &Path,
        trusted: &TrustedRoots,
        limits: ArchiveLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, ArchiveMemberSourceError> {
        if is_multivolume_name(path) {
            return Err(ArchiveMemberSourceError::Unsupported {
                detail: "multi-volume 7z archives are not supported".to_string(),
            });
        }
        let safe =
            open_bounded_read(path, trusted).map_err(|error| ArchiveMemberSourceError::Open {
                detail: format!("read policy refused the archive: {error:?}"),
            })?;
        let len = safe.len();
        let mut file = safe.into_file();

        // Hard resource limits BEFORE sevenz-rust2 sees the archive.
        preflight_sevenz(&mut file, len, &limits, cancel).map_err(map_preflight_refusal)?;

        // The probe left the file position at the end of the next header.
        // `sevenz-rust2`'s `Archive::read` seeks to the start itself, but the
        // rewind is kept so this module never depends on that internal detail
        // (`sevenz-rust` 0.6.1 read the signature from the current position).
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))
            .map_err(|error| ArchiveMemberSourceError::Open {
                detail: format!("could not rewind the archive: {error:?}"),
            })?;

        let reader = construct_reader(file);
        let reader = reader.map_err(|error| classify_error(&error))?;

        let members = collect_members(reader.archive());
        if members.len() > limits.max_members {
            return Err(ArchiveMemberSourceError::RefusedLimits {
                reason: "member count",
            });
        }
        validate_archive_structure(reader.archive(), &limits)?;

        Ok(Self {
            archive: path.display().to_string(),
            reader,
            members,
            limits,
        })
    }
}

/// Constructs the upstream reader, incrementing a test-only counter so
/// hostile-input tests can assert the preflight refused an archive before the
/// decoder was reached.
///
/// This is the **only** construction site for `ArchiveReader` in this crate,
/// and it always pins the reader to a single decode thread.
/// `ArchiveReader::new` otherwise defaults its thread count to
/// `std::thread::available_parallelism()`, which would hand multi-threaded
/// LZMA2 decode a pool of worker threads: that breaks the sequential
/// resource model this module is built on (one bounded decode at a time,
/// cancellation observed between output chunks, memory ceilings reasoned
/// about for one decoder stack rather than N). Never call
/// `ArchiveReader::new` elsewhere, and never raise this count.
///
/// The password is always empty: encrypted members are refused, never
/// decrypted.
fn construct_reader(file: std::fs::File) -> Result<ArchiveReader<std::fs::File>, SevenZError> {
    #[cfg(test)]
    {
        READER_NEW_CALLS.with(|calls| calls.set(calls.get() + 1));
    }
    let mut reader = ArchiveReader::new(file, Password::empty())?;
    reader.set_thread_count(1);
    Ok(reader)
}

/// Maps a preflight refusal onto the archive-source error taxonomy.
fn map_preflight_refusal(refusal: PreflightRefusal) -> ArchiveMemberSourceError {
    match refusal {
        PreflightRefusal::Cancelled => ArchiveMemberSourceError::Cancelled,
        PreflightRefusal::EncodedHeader => ArchiveMemberSourceError::Unsupported {
            detail: "encoded 7z header is not supported (its expansion cannot be bounded)"
                .to_string(),
        },
        PreflightRefusal::NextHeaderTooLarge { .. }
        | PreflightRefusal::TooManyFiles { .. }
        | PreflightRefusal::TooManyFolders { .. }
        | PreflightRefusal::TooManyPackStreams { .. }
        | PreflightRefusal::CoderChainTooLong { .. }
        | PreflightRefusal::PropertyBlobTooLarge { .. }
        | PreflightRefusal::DictionaryTooLarge { .. }
        | PreflightRefusal::AggregateDecoderMemoryExceeded { .. }
        | PreflightRefusal::MemberSizeExceeded { .. }
        | PreflightRefusal::SolidDecodeBudgetExceeded { .. }
        | PreflightRefusal::CompressionRatioExceeded { .. }
        | PreflightRefusal::LogicalBytesExceeded { .. }
        | PreflightRefusal::MemberCountExceeded { .. }
        | PreflightRefusal::ArithmeticOverflow => ArchiveMemberSourceError::RefusedLimits {
            reason: refusal_reason(&refusal),
        },
        PreflightRefusal::BadSignature
        | PreflightRefusal::UnsupportedVersion { .. }
        | PreflightRefusal::Truncated
        | PreflightRefusal::Malformed { .. } => ArchiveMemberSourceError::Corrupt {
            detail: format!("{refusal:?}"),
        },
        PreflightRefusal::Io(detail) => ArchiveMemberSourceError::Corrupt { detail },
    }
}

fn refusal_reason(refusal: &PreflightRefusal) -> &'static str {
    match refusal {
        PreflightRefusal::NextHeaderTooLarge { .. } => "next header size",
        PreflightRefusal::TooManyFiles { .. }
        | PreflightRefusal::TooManyFolders { .. }
        | PreflightRefusal::TooManyPackStreams { .. } => "structural count",
        PreflightRefusal::CoderChainTooLong { .. } => "coder chain",
        PreflightRefusal::PropertyBlobTooLarge { .. } => "property blob",
        PreflightRefusal::DictionaryTooLarge { .. } => "dictionary",
        PreflightRefusal::AggregateDecoderMemoryExceeded { .. } => "aggregate decoder memory",
        PreflightRefusal::MemberSizeExceeded { .. } => "member size",
        PreflightRefusal::SolidDecodeBudgetExceeded { .. } => "solid decode budget",
        PreflightRefusal::CompressionRatioExceeded { .. } => "compression ratio",
        PreflightRefusal::LogicalBytesExceeded { .. } => "total logical budget",
        PreflightRefusal::MemberCountExceeded { .. } => "member count",
        PreflightRefusal::ArithmeticOverflow => "arithmetic overflow",
        PreflightRefusal::Cancelled
        | PreflightRefusal::EncodedHeader
        | PreflightRefusal::BadSignature
        | PreflightRefusal::UnsupportedVersion { .. }
        | PreflightRefusal::Truncated
        | PreflightRefusal::Malformed { .. }
        | PreflightRefusal::Io(_) => "preflight",
    }
}

impl ArchiveMemberSource for SevenZArchiveSource {
    fn archive_format(&self) -> &'static str {
        "7z"
    }

    fn member_count(&self) -> usize {
        self.members.len()
    }

    fn verify_all(
        &mut self,
        cancel: &AtomicBool,
        visit: &mut dyn FnMut(ArchiveMemberEvidence) -> Result<bool, ArchiveMemberSourceError>,
    ) -> Result<(), ArchiveMemberSourceError> {
        let members: Vec<MemberMeta> = self.members.clone();
        let limits = self.limits;
        let archive = self.archive.as_str();
        let reader = &mut self.reader;
        let mut cursor: usize = 0;
        let mut total_consumed: u64 = 0;
        let mut internal_error: Option<ArchiveMemberSourceError> = None;

        let result = reader.for_each_entries(|entry, stream| {
            let Some(meta) = members.get(cursor).cloned() else {
                // Directory/empty entries (has_stream == false) are visited
                // after every stream member and never match a cursor member.
                return Ok(true);
            };
            // KNOWN LIMITATION: `sevenz-rust2`'s stream map assigns a block's
            // file range as `[block_first_file_index, +num_unpack_sub_streams)`,
            // so an empty-stream entry sitting *between* two stream members of
            // the same folder shifts that window and hides the last member.
            // The name check below turns that into a fail-closed `Corrupt`
            // refusal rather than a silent mis-attribution, but it means such
            // an archive is reported corrupt even though it is well-formed.
            if meta.name != entry.name() {
                internal_error = Some(ArchiveMemberSourceError::Corrupt {
                    detail: format!("member order mismatch at index {cursor}"),
                });
                return Ok(false);
            }
            let index = cursor;
            cursor += 1;

            // Empty members contribute nothing to any budget.
            if meta.logical_size == 0 {
                return match visit(evidence(
                    archive,
                    &meta,
                    index,
                    ArchiveMemberStatus::EmptyFile,
                    None,
                )) {
                    Ok(true) => Ok(true),
                    Ok(false) => Ok(false),
                    Err(error) => {
                        internal_error = Some(error);
                        Ok(false)
                    }
                };
            }

            // The cumulative logical-byte budget covers every stream member -
            // including nested ones, which are drained, never hashed.
            let Some(consumed_after) = total_consumed.checked_add(meta.logical_size) else {
                internal_error = Some(ArchiveMemberSourceError::RefusedLimits {
                    reason: "total logical budget",
                });
                return Ok(false);
            };
            if consumed_after > limits.max_archive_logical_bytes {
                let outcome = visit(evidence(
                    archive,
                    &meta,
                    index,
                    ArchiveMemberStatus::RefusedLimits {
                        reason: "total logical budget",
                    },
                    None,
                ));
                return match outcome {
                    Ok(_) => Ok(false),
                    Err(error) => {
                        internal_error = Some(error);
                        Ok(false)
                    }
                };
            }
            // NOTE: `consumed_after` is a guard only. The member is charged
            // exactly once: drained nested members by their declared size after
            // draining, ordinary hashed members by their actual bytes after
            // hashing. Pre-committing the declared size here would double-count
            // ordinary members (the second charge happens below).

            if meta.is_nested {
                if meta.logical_size > limits.max_member_logical_bytes {
                    let outcome = visit(evidence(
                        archive,
                        &meta,
                        index,
                        ArchiveMemberStatus::RefusedLimits {
                            reason: "member size",
                        },
                        None,
                    ));
                    return match outcome {
                        Ok(_) => Ok(false),
                        Err(error) => {
                            internal_error = Some(error);
                            return Ok(false);
                        }
                    };
                }
                // Drain (bounded) so a solid block stays aligned; never hash or
                // open the nested archive. Its bytes already count toward the
                // cumulative budget above.
                if let Err(error) = drain_member(stream, meta.logical_size, cancel) {
                    internal_error = Some(match error {
                        MemberStreamError::Cancelled => ArchiveMemberSourceError::Cancelled,
                        MemberStreamError::TooLarge { .. } => {
                            ArchiveMemberSourceError::RefusedLimits {
                                reason: "member size",
                            }
                        }
                        MemberStreamError::Io(detail) => {
                            ArchiveMemberSourceError::Corrupt { detail }
                        }
                    });
                    return Ok(false);
                }
                // Charge the drained nested member exactly once by its declared
                // size (its bytes are never hashed, but they were processed).
                match total_consumed.checked_add(meta.logical_size) {
                    Some(value) => total_consumed = value,
                    None => {
                        internal_error = Some(ArchiveMemberSourceError::RefusedLimits {
                            reason: "total logical budget",
                        });
                        return Ok(false);
                    }
                }
                return match visit(evidence(
                    archive,
                    &meta,
                    index,
                    ArchiveMemberStatus::NestedArchive,
                    None,
                )) {
                    Ok(true) => Ok(true),
                    Ok(false) => Ok(false),
                    Err(error) => {
                        internal_error = Some(error);
                        Ok(false)
                    }
                };
            }

            if meta.logical_size > limits.max_member_logical_bytes {
                let outcome = visit(evidence(
                    archive,
                    &meta,
                    index,
                    ArchiveMemberStatus::RefusedLimits {
                        reason: "member size",
                    },
                    None,
                ));
                return match outcome {
                    Ok(_) => Ok(false),
                    Err(error) => {
                        internal_error = Some(error);
                        Ok(false)
                    }
                };
            }

            // Bound the hash by the *declared* logical size (never larger than
            // the per-member ceiling, which was checked just above) rather than
            // by the ceiling alone: the cumulative budget was reserved against
            // the declared size, so a decoder that produced more bytes than the
            // header promised must be refused, not silently charged.
            match hash_member_stream(stream, meta.logical_size, cancel) {
                Ok(hashed) => {
                    total_consumed = total_consumed.saturating_add(hashed.bytes_read);
                    // A short read is EOF, not success: the decoder ran out of
                    // input before the header's declared logical size was
                    // satisfied. The hashes cover only the bytes that did
                    // arrive, so they identify a prefix of the member, not the
                    // member — reporting them as Verified would let a truncated
                    // stream claim a clean bill of health. (An over-long stream
                    // cannot reach here: the hasher is bounded by the declared
                    // size and refuses with TooLarge below.)
                    if hashed.bytes_read != meta.logical_size {
                        let outcome = visit(evidence(
                            archive,
                            &meta,
                            index,
                            ArchiveMemberStatus::Corrupt {
                                detail: format!(
                                    "decoded {} bytes of the {} declared",
                                    hashed.bytes_read, meta.logical_size
                                ),
                            },
                            None,
                        ));
                        return match outcome {
                            Ok(_) => Ok(false),
                            Err(error) => {
                                internal_error = Some(error);
                                Ok(false)
                            }
                        };
                    }
                    match visit(evidence(
                        archive,
                        &meta,
                        index,
                        ArchiveMemberStatus::Verified,
                        Some(hashed.hashes),
                    )) {
                        Ok(true) => Ok(true),
                        Ok(false) => Ok(false),
                        Err(error) => {
                            internal_error = Some(error);
                            Ok(false)
                        }
                    }
                }
                Err(MemberStreamError::Cancelled) => {
                    internal_error = Some(ArchiveMemberSourceError::Cancelled);
                    Ok(false)
                }
                Err(MemberStreamError::TooLarge { .. }) => {
                    let outcome = visit(evidence(
                        archive,
                        &meta,
                        index,
                        ArchiveMemberStatus::RefusedLimits {
                            reason: "member size",
                        },
                        None,
                    ));
                    match outcome {
                        Ok(_) => Ok(false),
                        Err(error) => {
                            internal_error = Some(error);
                            Ok(false)
                        }
                    }
                }
                Err(MemberStreamError::Io(detail)) => {
                    let outcome = visit(evidence(
                        archive,
                        &meta,
                        index,
                        ArchiveMemberStatus::Corrupt { detail },
                        None,
                    ));
                    match outcome {
                        Ok(_) => Ok(false),
                        Err(error) => {
                            internal_error = Some(error);
                            Ok(false)
                        }
                    }
                }
            }
        });

        if let Some(error) = internal_error {
            return Err(error);
        }
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                // A folder-level decode failure (encrypted/unsupported/corrupt)
                // that fired before any member of that folder was handed out:
                // attribute it to the next unvisited member, or surface as a
                // source error if there is none.
                if cursor < members.len() {
                    let status = match classify_error(&error) {
                        ArchiveMemberSourceError::Encrypted => ArchiveMemberStatus::Encrypted,
                        ArchiveMemberSourceError::Unsupported { detail } => {
                            ArchiveMemberStatus::UnsupportedCodec { method: detail }
                        }
                        ArchiveMemberSourceError::Corrupt { detail } => {
                            ArchiveMemberStatus::Corrupt { detail }
                        }
                        ArchiveMemberSourceError::RefusedLimits { reason } => {
                            ArchiveMemberStatus::RefusedLimits { reason }
                        }
                        other => return Err(other),
                    };
                    visit(self.evidence(cursor, status))?;
                    return Ok(());
                }
                Err(classify_error(&error))
            }
        }
    }
}

impl SevenZArchiveSource {
    fn evidence(&self, index: usize, status: ArchiveMemberStatus) -> ArchiveMemberEvidence {
        let meta = &self.members[index];
        ArchiveMemberEvidence {
            archive: self.archive.clone(),
            name: meta.name.clone(),
            index,
            logical_size: meta.logical_size,
            is_nested_archive: meta.is_nested,
            status,
            hashes: None,
        }
    }
}

/// Maps a `sevenz-rust2` error onto our fail-closed refusal taxonomy.
fn classify_error(error: &SevenZError) -> ArchiveMemberSourceError {
    use sevenz_rust2::Error as E;
    match error {
        E::PasswordRequired | E::MaybeBadPassword(_) => ArchiveMemberSourceError::Encrypted,
        E::UnsupportedCompressionMethod(method) if method.contains("AES") => {
            ArchiveMemberSourceError::Encrypted
        }
        E::UnsupportedCompressionMethod(method) => ArchiveMemberSourceError::Unsupported {
            detail: format!("unsupported compression method: {method}"),
        },
        E::MaxMemLimited { .. } => ArchiveMemberSourceError::RefusedLimits {
            reason: "dictionary",
        },
        E::Other(message) if message.contains("Dictionary") => {
            ArchiveMemberSourceError::RefusedLimits {
                reason: "dictionary",
            }
        }
        E::UnsupportedVersion { .. } | E::Unsupported(_) | E::ExternalUnsupported => {
            ArchiveMemberSourceError::Unsupported {
                detail: format!("{error:?}"),
            }
        }
        E::BadSignature(_)
        | E::ChecksumVerificationFailed
        | E::NextHeaderCrcMismatch
        | E::Io(..)
        | E::FileOpen(..)
        | E::Other(_)
        | E::BadTerminatedStreamsInfo(_)
        | E::BadTerminatedUnpackInfo
        | E::BadTerminatedPackInfo(_)
        | E::BadTerminatedSubStreamsInfo
        | E::BadTerminatedHeader(_)
        // `FileNotFound` is only produced by `ArchiveReader::read_file`, which
        // this module never calls; treat it as corrupt rather than letting a
        // future refactor fall through to something more permissive.
        | E::FileNotFound => ArchiveMemberSourceError::Corrupt {
            detail: format!("{error:?}"),
        },
    }
}

/// Stream-bearing members in archive order.
fn collect_members(archive: &Archive) -> Vec<MemberMeta> {
    archive
        .files
        .iter()
        .filter(|file| file.has_stream)
        .map(|file| MemberMeta {
            name: file.name.clone(),
            logical_size: file.size,
            is_nested: is_nested_name(&file.name),
        })
        .collect()
}

/// Whether a member name looks like a nested archive (by extension only).
fn is_nested_name(name: &str) -> bool {
    let Some(extension) = Path::new(name).extension() else {
        return false;
    };
    let extension = extension.to_string_lossy().to_ascii_lowercase();
    NESTED_ARCHIVE_EXTENSIONS.contains(&extension.as_str())
}

/// Whether a filename looks like a multi-volume 7z (`name.7z.001`, …).
fn is_multivolume_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let Some(index) = name.rfind(".7z.") else {
        return false;
    };
    let suffix = &name[index + 4..];
    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
}

/// Enforces structure-level limits from header metadata, before any decode.
///
/// Uses only `sevenz-rust2`'s public surface (`Archive`, `StreamMap`,
/// `pack_sizes()`, `files`): the `Block`/`Coder` internals are not public, so
/// per-block sizes and ratios are derived from the pack-stream ranges and the
/// files each block owns.
///
/// (`sevenz-rust2` renamed `sevenz-rust`'s "folder" concept to "block"; the
/// on-disk structure is identical, so the checks below are unchanged.)
fn validate_archive_structure(
    archive: &Archive,
    limits: &ArchiveLimits,
) -> Result<(), ArchiveMemberSourceError> {
    validate_structure_parts(
        archive.pack_sizes(),
        archive.stream_map.block_first_pack_stream_index(),
        &archive.files,
        &archive.stream_map.file_block_index,
        limits,
    )
}

/// The body of [`validate_archive_structure`], over borrowed slices.
///
/// `sevenz-rust2` made `pack_sizes` and `block_first_pack_stream_index`
/// read-only accessors rather than public fields, so a hand-built hostile
/// `Archive` can no longer be constructed in a test. Taking the same data as
/// slices keeps the ratio and solid-budget checks directly testable without
/// changing what they check.
fn validate_structure_parts(
    pack_sizes: &[u64],
    block_first_stream: &[usize],
    files: &[ArchiveEntry],
    file_block_index: &[Option<usize>],
    limits: &ArchiveLimits,
) -> Result<(), ArchiveMemberSourceError> {
    let folder_count = block_first_stream.len();
    for folder_index in 0..folder_count {
        let pack_start = block_first_stream[folder_index];
        let pack_end = block_first_stream
            .get(folder_index + 1)
            .copied()
            .unwrap_or(pack_sizes.len());
        let pack_slice = pack_sizes.get(pack_start..pack_end).ok_or_else(|| {
            ArchiveMemberSourceError::Corrupt {
                detail: "folder pack stream range out of bounds".to_string(),
            }
        })?;
        // Checked summation of the folder's compressed bytes.
        let mut pack: u64 = 0;
        for &size in pack_slice {
            pack = pack
                .checked_add(size)
                .ok_or(ArchiveMemberSourceError::RefusedLimits {
                    reason: "arithmetic overflow",
                })?;
        }

        // Files owned by this folder: their declared sizes sum to the folder's
        // logical (unpacked) size.
        let (files_in_folder, unpack) = files
            .iter()
            .enumerate()
            .filter(|(index, file)| {
                file.has_stream
                    && file_block_index.get(*index).copied().flatten() == Some(folder_index)
            })
            .fold((0_usize, 0_u64), |(count, sum), (_, file)| {
                (count + 1, sum.saturating_add(file.size))
            });

        // Compression ratio, without lossy integer division, with explicit
        // zero-compressed-size handling: any unpacked bytes from zero
        // compressed bytes is unbounded amplification.
        if unpack > 0 && (pack == 0 || ratio_exceeded(unpack, pack, limits.max_compression_ratio)) {
            return Err(ArchiveMemberSourceError::RefusedLimits {
                reason: "compression ratio",
            });
        }
        // Solid (multi-member) block budget: decoding member K costs every
        // member before it in the block, so a huge solid block is refused up
        // front.
        if files_in_folder > 1 && unpack > limits.max_solid_decode_bytes {
            return Err(ArchiveMemberSourceError::RefusedLimits {
                reason: "solid decode budget",
            });
        }
    }
    Ok(())
}

/// `unpack > pack * ratio`, computed without lossy integer division. An
/// overflow in the multiplication is treated as exceeding the limit.
fn ratio_exceeded(unpack: u64, pack: u64, ratio: u64) -> bool {
    match pack.checked_mul(ratio) {
        Some(limit) => unpack > limit,
        None => true,
    }
}

/// Reads and discards `limit` bytes from `reader`, checking cancellation per
/// chunk. Used to keep a solid block aligned for nested members.
fn drain_member<R: Read>(
    reader: R,
    limit: u64,
    cancel: &AtomicBool,
) -> Result<(), MemberStreamError> {
    let mut reader = reader;
    let mut buffer = vec![0_u8; ARCHIVE_HASH_CHUNK_BYTES];
    let mut total: u64 = 0;
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(MemberStreamError::Cancelled);
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| MemberStreamError::Io(error.kind().to_string()))?;
        if read == 0 {
            return Ok(());
        }
        total = total.saturating_add(read as u64);
        if total > limit {
            return Err(MemberStreamError::TooLarge { limit });
        }
    }
}

fn evidence(
    archive: &str,
    meta: &MemberMeta,
    index: usize,
    status: ArchiveMemberStatus,
    hashes: Option<super::ArchiveMemberHashes>,
) -> ArchiveMemberEvidence {
    ArchiveMemberEvidence {
        archive: archive.to_string(),
        name: meta.name.clone(),
        index,
        logical_size: meta.logical_size,
        is_nested_archive: meta.is_nested,
        status,
        hashes,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::vec_init_then_push)]
    //! Focused tests for the experimental 7z reader. Fixtures are generated
    //! in-test with the `sevenz-rust2` writer (a dev-dependency feature) into
    //! a temp directory; no copyrighted data and no external tools are used.

    use super::*;
    use crate::safe_read::TrustedRoots;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::tempdir;

    use sevenz_rust2::{ArchiveWriter, EncoderMethod};

    static TEST_NO_CANCEL: AtomicBool = AtomicBool::new(false);

    fn trusted_for(root: &std::path::Path) -> TrustedRoots {
        TrustedRoots::from_paths(std::iter::once(root))
    }

    /// Builds a stream-bearing fixture entry. Only the three fields the reader
    /// cares about are set; everything else keeps its default.
    #[allow(clippy::field_reassign_with_default)]
    fn fixture_entry(name: &str, size: u64) -> ArchiveEntry {
        let mut entry = ArchiveEntry::new();
        entry.name = name.to_string();
        entry.has_stream = true;
        entry.size = size;
        entry
    }

    /// Writes a 7z with the given entries, all in one non-solid archive.
    fn make_archive(dir: &std::path::Path, files: &[(&str, &str)]) -> PathBuf {
        let archive_path = dir.join("archive.7z");
        let mut writer = ArchiveWriter::new(std::fs::File::create(&archive_path).unwrap()).unwrap();
        for (name, contents) in files {
            let entry = fixture_entry(name, contents.len() as u64);
            let bytes = contents.as_bytes();
            writer
                .push_archive_entry(entry, Some(std::io::Cursor::new(bytes)))
                .unwrap();
        }
        writer.finish().unwrap();
        archive_path
    }

    /// Writes a password-encrypted 7z with a **plain** header and AES-encrypted
    /// content, so the encryption surfaces at decode time.
    ///
    /// `ArchiveWriter` defaults `encrypt_header` to `true` and honours it
    /// whenever a content method is AES, which produces a `kEncodedHeader` the
    /// preflight refuses outright (its expansion cannot be bounded). That
    /// refusal is real and is pinned by
    /// `encrypted_header_archive_is_refused_before_decode`; this fixture turns
    /// header encryption off so the *member-level* Encrypted refusal is still
    /// exercised too.
    fn make_encrypted_archive(dir: &std::path::Path, contents: &str) -> PathBuf {
        let archive_path = dir.join("encrypted.7z");
        let mut writer = ArchiveWriter::new(std::fs::File::create(&archive_path).unwrap()).unwrap();
        writer.set_encrypt_header(false);
        writer.set_content_methods(vec![
            sevenz_rust2::encoder_options::AesEncoderOptions::new(Password::from("secret")).into(),
            EncoderMethod::LZMA2.into(),
        ]);
        let entry = fixture_entry("game.rom", contents.len() as u64);
        writer
            .push_archive_entry(entry, Some(std::io::Cursor::new(contents.as_bytes())))
            .unwrap();
        writer.finish().unwrap();
        archive_path
    }

    fn collect(
        source: &mut SevenZArchiveSource,
    ) -> Result<Vec<ArchiveMemberEvidence>, ArchiveMemberSourceError> {
        let cancel = AtomicBool::new(false);
        let mut out = Vec::new();
        source.verify_all(&cancel, &mut |evidence| {
            out.push(evidence);
            Ok(true)
        })?;
        Ok(out)
    }

    #[test]
    fn one_supported_member_hashes_correctly() {
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("game.rom", "hello world")]);
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        assert_eq!(source.member_count(), 1);
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 1);
        assert!(evidence[0].is_verified());
        let hashes = evidence[0].hashes.as_ref().unwrap();
        assert_eq!(hashes.md5, "5eb63bbbe01eeed093cb22bb8f5acdc3");
        assert_eq!(hashes.sha1, "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed");
    }

    #[test]
    fn multiple_members_enumerate_deterministically() {
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("b.bin", "bbb"), ("a.rom", "aaa")]);
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        assert_eq!(source.member_count(), 2);
        let first = collect(&mut source).unwrap();
        let names: Vec<_> = first.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["b.bin", "a.rom"]);

        let mut source2 =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        let second = collect(&mut source2).unwrap();
        assert_eq!(
            first, second,
            "member order and evidence must be deterministic"
        );
    }

    #[test]
    fn solid_archive_within_budget_hashes_every_member() {
        // The sevenz-rust2 writer packs multi-file headers, which the preflight
        // refuses (encoded-header expansion is unbounded). A hand-built solid
        // archive with a plain header and COPY-compressed members is the legal
        // fixture for the positive solid path.
        let dir = tempdir().unwrap();
        let path = write_solid_copy_archive(
            dir.path(),
            &[
                ("one.bin", b"first member payload".to_vec()),
                ("two.bin", b"second member payload".to_vec()),
            ],
        );
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        assert_eq!(source.member_count(), 2);
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().all(|e| e.is_verified()));
    }

    #[test]
    fn oversized_member_is_refused_and_verification_stops() {
        // The per-member ceiling is enforced by the pre-decoder probe from the
        // parsed sub-stream sizes, before any decoder is constructed.
        let dir = tempdir().unwrap();
        let big = "x".repeat(4096);
        let path = make_archive(dir.path(), &[("small.bin", "ok"), ("big.bin", &big)]);
        let trusted = trusted_for(dir.path());
        let limits = ArchiveLimits {
            max_member_logical_bytes: 1024,
            ..ArchiveLimits::default()
        };
        let err = SevenZArchiveSource::open(&path, &trusted, limits, &TEST_NO_CANCEL).unwrap_err();
        assert_eq!(
            err,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "member size"
            }
        );
    }

    #[test]
    fn total_decode_budget_is_refused() {
        // The pre-decoder probe enforces the cumulative logical-byte budget
        // before sevenz-rust2 is ever constructed, so the archive is refused at
        // open time.
        let dir = tempdir().unwrap();
        let path = make_archive(
            dir.path(),
            &[("one.bin", "payload one"), ("two.bin", "payload two")],
        );
        let trusted = trusted_for(dir.path());
        let limits = ArchiveLimits {
            max_archive_logical_bytes: 16,
            ..ArchiveLimits::default()
        };
        let err = SevenZArchiveSource::open(&path, &trusted, limits, &TEST_NO_CANCEL).unwrap_err();
        assert_eq!(
            err,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "total logical budget"
            }
        );
    }

    #[test]
    fn dictionary_memory_error_maps_to_refusal() {
        use sevenz_rust2::Error as E;
        let mapped = classify_error(&E::MaxMemLimited {
            max_kb: 1024,
            actaul_kb: 4096,
        });
        assert_eq!(
            mapped,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "dictionary"
            }
        );
        let mapped = classify_error(&E::Other("Dictionary larger than 4GiB maximum size".into()));
        assert_eq!(
            mapped,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "dictionary"
            }
        );
    }

    #[test]
    fn encrypted_error_variants_map_to_encrypted() {
        use sevenz_rust2::Error as E;
        assert_eq!(
            classify_error(&E::PasswordRequired),
            ArchiveMemberSourceError::Encrypted
        );
        assert_eq!(
            classify_error(&E::UnsupportedCompressionMethod("AES256SHA256".into())),
            ArchiveMemberSourceError::Encrypted
        );
        assert_eq!(
            classify_error(&E::UnsupportedCompressionMethod("ZSTD".into())),
            ArchiveMemberSourceError::Unsupported {
                detail: "unsupported compression method: ZSTD".to_string()
            }
        );
    }

    #[test]
    fn compression_ratio_limit_is_refused() {
        // A tiny pack with a huge unpack: a block-shaped declaration of
        // pack 1 byte, unpack 2 GiB. `sevenz-rust2` made `pack_sizes` and
        // `block_first_pack_stream_index` read-only accessors, so the same
        // declaration is handed to the check as slices instead of being
        // pushed into a hand-built `Archive`.
        let files = vec![fixture_entry("bomb.bin", 2 * 1024 * 1024 * 1024)];
        let limits = ArchiveLimits {
            max_compression_ratio: 1000,
            ..ArchiveLimits::default()
        };
        let err = validate_structure_parts(&[1], &[0], &files, &[Some(0)], &limits).unwrap_err();
        assert_eq!(
            err,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "compression ratio"
            }
        );
    }

    #[test]
    fn solid_decode_budget_is_refused() {
        // Two files sharing one block with a total above the solid budget.
        // Pack large enough that the ratio check passes (6 GiB / 64 MiB = 96)
        // so the solid-budget check is the one that fires.
        let size = 3 * 1024 * 1024 * 1024;
        let files = vec![fixture_entry("a.bin", size), fixture_entry("b.bin", size)];
        let limits = ArchiveLimits {
            max_solid_decode_bytes: 2 * 1024 * 1024 * 1024,
            ..ArchiveLimits::default()
        };
        let err = validate_structure_parts(
            &[64 * 1024 * 1024],
            &[0],
            &files,
            &[Some(0), Some(0)],
            &limits,
        )
        .unwrap_err();
        assert_eq!(
            err,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "solid decode budget"
            }
        );
    }

    #[test]
    fn encrypted_member_is_refused_and_stops() {
        // Plain header, AES-encrypted content: the archive opens and the
        // encryption surfaces at decode time as a per-member refusal. The
        // encrypted-*header* path is covered by
        // `encrypted_header_archive_is_refused_before_decode`, and the error
        // mapping itself by `encrypted_error_variants_map_to_encrypted`.
        let dir = tempdir().unwrap();
        let path = make_encrypted_archive(dir.path(), "secret payload");
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].status, ArchiveMemberStatus::Encrypted);
        assert!(evidence[0].hashes.is_none());
    }

    #[test]
    fn corrupt_archive_is_refused() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("corrupt.7z");
        std::fs::write(&path, b"not a 7z archive at all").unwrap();
        let trusted = trusted_for(dir.path());
        let err =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap_err();
        assert!(matches!(err, ArchiveMemberSourceError::Corrupt { .. }));
    }

    #[test]
    fn multi_volume_name_is_refused_before_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.7z.001");
        let trusted = trusted_for(dir.path());
        let err =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap_err();
        assert!(matches!(err, ArchiveMemberSourceError::Unsupported { .. }));
    }

    #[test]
    fn nested_archive_member_is_surfaced_but_not_opened() {
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("inner.zip", "zip bytes")]);
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].status, ArchiveMemberStatus::NestedArchive);
        assert!(evidence[0].is_nested_archive);
        assert!(evidence[0].hashes.is_none());
    }

    #[test]
    fn cancellation_stops_verification() {
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("a.bin", "aaa"), ("b.bin", "bbb")]);
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        let cancel = AtomicBool::new(true);
        let mut seen = 0;
        let result = source.verify_all(&cancel, &mut |_evidence| {
            seen += 1;
            Ok(true)
        });
        assert_eq!(result, Err(ArchiveMemberSourceError::Cancelled));
        // Cancellation is checked before any read, so no member completed.
        assert_eq!(seen, 0);
    }

    #[test]
    fn zero_filesystem_writes() {
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("a.bin", "aaa"), ("b.bin", "bbb")]);
        let trusted = trusted_for(dir.path());
        let snapshot = |root: &std::path::Path| -> Vec<(String, u64, u64)> {
            std::fs::read_dir(root)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    let meta = std::fs::symlink_metadata(entry.path()).unwrap();
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        meta.len(),
                        meta.modified()
                            .unwrap()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos() as u64,
                    )
                })
                .collect()
        };
        let before = snapshot(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        collect(&mut source).unwrap();
        let after = snapshot(dir.path());
        assert_eq!(
            before, after,
            "verification must create, write, or modify nothing"
        );
    }

    #[test]
    fn trusted_roots_are_enforced() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let real = make_archive(outside.path(), &[("a.bin", "aaa")]);
        let link = dir.path().join("link.7z");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let trusted = TrustedRoots::from_paths(std::iter::once(dir.path()));
        let err =
            SevenZArchiveSource::open(&link, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap_err();
        assert!(
            matches!(err, ArchiveMemberSourceError::Open { .. }),
            "a symlink resolving outside the trusted roots must be refused"
        );
    }

    #[test]
    fn member_count_limit_is_refused() {
        // The pre-decoder probe caps both the declared file count (which
        // bounds sevenz-rust2's files-vector allocation) and the stream-member
        // count against `max_members`, refusing at open time.
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("a.bin", "aaa"), ("b.bin", "bbb")]);
        let trusted = trusted_for(dir.path());
        let limits = ArchiveLimits {
            max_members: 1,
            ..ArchiveLimits::default()
        };
        let err = SevenZArchiveSource::open(&path, &trusted, limits, &TEST_NO_CANCEL).unwrap_err();
        assert!(
            matches!(err, ArchiveMemberSourceError::RefusedLimits { .. }),
            "an archive whose declared or stream-member count exceeds the limit must be refused"
        );
    }

    #[test]
    fn empty_stream_member_is_surfaced() {
        let dir = tempdir().unwrap();
        let archive_path = dir.path().join("empty.7z");
        let mut writer = ArchiveWriter::new(std::fs::File::create(&archive_path).unwrap()).unwrap();
        let entry = fixture_entry("empty.bin", 0);
        writer
            .push_archive_entry(entry, Some(std::io::Cursor::new(Vec::<u8>::new())))
            .unwrap();
        writer.finish().unwrap();
        let trusted = trusted_for(dir.path());
        let mut source = SevenZArchiveSource::open(
            &archive_path,
            &trusted,
            ArchiveLimits::default(),
            &TEST_NO_CANCEL,
        )
        .unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].status, ArchiveMemberStatus::EmptyFile);
    }

    // ------------------------------------------------------------------
    // Hand-built 7z fixtures for hostile-input tests. The sevenz-rust2
    // writer packs multi-file headers (encoded), which the preflight
    // deliberately refuses; hostile tests therefore craft the exact bytes.
    // ------------------------------------------------------------------

    const SIG: [u8; 6] = [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C];
    const K_HEADER: u8 = 0x01;
    const K_MAIN_STREAMS_INFO: u8 = 0x04;
    const K_FILES_INFO: u8 = 0x05;
    const K_PACK_INFO: u8 = 0x06;
    const K_UNPACK_INFO: u8 = 0x07;
    const K_SUB_STREAMS_INFO: u8 = 0x08;
    const K_SIZE: u8 = 0x09;
    const K_END: u8 = 0x00;
    const K_FOLDER: u8 = 0x0B;
    const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
    const K_NUM_UNPACK_STREAM: u8 = 0x0D;
    const K_NAME: u8 = 0x11;

    /// 7z variable-length integer (mirrors the 7z format's encoding).
    ///
    /// `n` bytes encode `7n` bits: the first byte has `n - 1` leading 1 bits
    /// as a length prefix, its low `8 - n` bits are the value's highest bits,
    /// and the `n - 1` continuation bytes hold the low bytes. The maximum
    /// (0xFF first byte + 8 continuation bytes) encodes the full u64.
    fn uvarint(value: u64, out: &mut Vec<u8>) {
        let bits = (64 - value.leading_zeros()).max(1) as u64;
        let mut n = bits.div_ceil(7);
        n = n.min(9);
        let first_low_bits = 8usize.saturating_sub(n as usize);
        let prefix: u8 = if n >= 2 {
            0xFF_u8 << (9 - n as usize)
        } else {
            0
        };
        let shift = 8 * (n as usize - 1);
        let low_mask: u64 = if first_low_bits >= 64 {
            u64::MAX
        } else {
            (1_u64 << first_low_bits) - 1
        };
        let first_value: u64 = if shift >= 64 { 0 } else { value >> shift };
        out.push(prefix | ((first_value as u8) & (low_mask as u8)));
        for i in 0..(n as usize - 1) {
            out.push(((value >> (8 * i)) & 0xff) as u8);
        }
    }

    /// Writes a complete 7z file whose next-header bytes are `next_header`,
    /// placed immediately after the 32-byte file header. Both CRCs are
    /// computed so `sevenz-rust2` (when the preflight lets it run) accepts the
    /// file; the preflight validates the start-header CRC itself.
    fn write_archive_bytes(
        dir: &std::path::Path,
        name: &str,
        packed: &[u8],
        next_header: &[u8],
    ) -> PathBuf {
        let mut file = Vec::new();
        file.extend_from_slice(&SIG);
        file.push(0);
        file.push(2);
        file.extend_from_slice(&[0; 4]); // start-header CRC, patched below
        file.extend_from_slice(&(packed.len() as u64).to_le_bytes()); // next_header_offset
        file.extend_from_slice(&(next_header.len() as u64).to_le_bytes()); // next_header_size
        let mut next_crc = crate::identity_source::hashing::Crc32::new();
        next_crc.update(next_header);
        file.extend_from_slice(&next_crc.finish().to_le_bytes());
        file.extend_from_slice(packed);
        file.extend_from_slice(next_header);
        let mut start_crc = crate::identity_source::hashing::Crc32::new();
        start_crc.update(&file[12..32]);
        file[8..12].copy_from_slice(&start_crc.finish().to_le_bytes());
        let path = dir.join(name);
        std::fs::write(&path, &file).unwrap();
        path
    }

    fn preflight(
        path: &std::path::Path,
        limits: &ArchiveLimits,
    ) -> Result<
        crate::dat::archive::sevenz_preflight::SevenZPreflightInfo,
        crate::dat::archive::sevenz_preflight::PreflightRefusal,
    > {
        let len = std::fs::metadata(path).unwrap().len();
        let mut file = std::fs::File::open(path).unwrap();
        crate::dat::archive::sevenz_preflight::preflight_sevenz(
            &mut file,
            len,
            limits,
            &TEST_NO_CANCEL,
        )
    }

    /// A plain header with one COPY folder of `unpack` bytes and one pack
    /// stream of `pack` bytes (packed data is empty; the probe never decodes).
    fn header_one_copy_folder(pack: u64, unpack: u64) -> Vec<u8> {
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h); // pack_pos
        uvarint(1, &mut h); // num_pack_streams
        h.push(K_SIZE);
        uvarint(pack, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h); // num_folders
        h.push(0); // external
        uvarint(1, &mut h); // num_coders
        h.push(0x01); // coder bits: id_size=1, simple, no props
        h.push(0x00); // COPY id
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(unpack, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        h
    }

    /// A header with one folder whose coder is LZMA or LZMA2 with the given
    /// property bytes (dictionary lives in the properties).
    fn header_with_coder(method_id: &[u8], props: &[u8]) -> Vec<u8> {
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(1, &mut h);
        h.push(K_SIZE);
        uvarint(0, &mut h); // pack size 0
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(1, &mut h); // num_coders
        h.push(0x01 | 0x20 | (method_id.len() as u8)); // id_size | has_attrs
        h.extend_from_slice(method_id);
        uvarint(props.len() as u64, &mut h);
        h.extend_from_slice(props);
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h); // one stream file, consistent with one sub-stream
        h.push(K_END);
        h.push(K_END);
        h
    }

    /// A plain solid (one folder, several sub-streams) COPY archive whose
    /// member bytes are stored uncompressed in the packed stream. Used for
    /// the positive solid path, which the sevenz-rust2 writer cannot produce
    /// with a plain header.
    fn write_solid_copy_archive(dir: &std::path::Path, files: &[(&str, Vec<u8>)]) -> PathBuf {
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h); // pack_pos
        uvarint(1, &mut h); // num_pack_streams
        h.push(K_SIZE);
        let total: usize = files.iter().map(|(_, c)| c.len()).sum();
        uvarint(total as u64, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h); // num_folders (solid)
        h.push(0);
        uvarint(1, &mut h); // num_coders
        h.push(0x01);
        h.push(0x00); // COPY
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(total as u64, &mut h);
        h.push(K_END);
        h.push(K_SUB_STREAMS_INFO);
        h.push(K_NUM_UNPACK_STREAM);
        uvarint(files.len() as u64, &mut h);
        h.push(K_SIZE);
        for (index, (_, c)) in files.iter().enumerate() {
            if index < files.len() - 1 {
                uvarint(c.len() as u64, &mut h);
            }
        }
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(files.len() as u64, &mut h);
        h.push(K_NAME);
        let mut names = vec![0_u8]; // external = 0
        for (name, _) in files {
            for unit in name.encode_utf16() {
                names.extend_from_slice(&unit.to_le_bytes());
            }
            names.extend_from_slice(&[0, 0]);
        }
        uvarint(names.len() as u64, &mut h);
        h.extend_from_slice(&names);
        h.push(K_END);
        h.push(K_END);

        let packed: Vec<u8> = files.iter().flat_map(|(_, c)| c.clone()).collect();
        write_archive_bytes(dir, "solid_copy.7z", &packed, &h)
    }

    /// A plain single-member COPY archive. `payload` is stored verbatim as the
    /// (only) pack stream, while `declared_size` is what the header claims the
    /// member decompresses to — the two are allowed to disagree, which is how
    /// a truncated member is expressed: COPY hands the decoder exactly
    /// `payload.len()` bytes, so decode hits EOF short of `declared_size`.
    fn write_copy_member_archive(
        dir: &std::path::Path,
        file_name: &str,
        member: &str,
        payload: &[u8],
        declared_size: u64,
    ) -> PathBuf {
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h); // pack_pos
        uvarint(1, &mut h); // num_pack_streams
        h.push(K_SIZE);
        uvarint(payload.len() as u64, &mut h); // the real stored size
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h); // num_folders
        h.push(0); // external
        uvarint(1, &mut h); // num_coders
        h.push(0x01); // simple, id_size = 1
        h.push(0x00); // COPY
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(declared_size, &mut h); // the *claimed* logical size
        h.push(K_END);
        h.push(K_SUB_STREAMS_INFO);
        h.push(K_NUM_UNPACK_STREAM);
        uvarint(1, &mut h); // one sub-stream, so it inherits the folder size
        h.push(K_SIZE); // no explicit sizes: the last (only) one is implied
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_NAME);
        let mut names = vec![0_u8]; // external = 0
        for unit in member.encode_utf16() {
            names.extend_from_slice(&unit.to_le_bytes());
        }
        names.extend_from_slice(&[0, 0]);
        uvarint(names.len() as u64, &mut h);
        h.extend_from_slice(&names);
        h.push(K_END);
        h.push(K_END);
        write_archive_bytes(dir, file_name, payload, &h)
    }

    #[test]
    fn truncated_member_is_corrupt_not_verified() {
        // The header declares a 20-byte member but only 10 bytes are stored,
        // so decode reaches EOF halfway through. The hashes computed over that
        // prefix are not evidence about the member, and must never be reported
        // as a verification.
        let dir = tempdir().unwrap();
        let path = write_copy_member_archive(
            dir.path(),
            "truncated.7z",
            "game.rom",
            b"0123456789",
            20, // declared logical size, twice what is stored
        );
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 1);
        assert!(
            !evidence[0].is_verified(),
            "a member whose decode ended early must not be verified"
        );
        assert!(
            matches!(evidence[0].status, ArchiveMemberStatus::Corrupt { .. }),
            "unexpected status: {:?}",
            evidence[0].status
        );
        assert!(
            evidence[0].hashes.is_none(),
            "a partial hash must never be handed out as evidence"
        );
    }

    #[test]
    fn exactly_sized_copy_member_is_verified() {
        // The happy path the truncation check must not break: the same fixture
        // shape with an honest declared size verifies, and its hashes are the
        // hashes of the stored bytes.
        let dir = tempdir().unwrap();
        let path = write_copy_member_archive(
            dir.path(),
            "exact.7z",
            "game.rom",
            b"hello world",
            11, // declared size matches the stored bytes exactly
        );
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 1);
        assert!(evidence[0].is_verified());
        let hashes = evidence[0].hashes.as_ref().unwrap();
        assert_eq!(hashes.md5, "5eb63bbbe01eeed093cb22bb8f5acdc3");
        assert_eq!(hashes.sha1, "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed");
    }

    #[test]
    fn hostile_lzma_dictionary_rejected_before_decode() {
        let dir = tempdir().unwrap();
        // LZMA id + a 2 GiB dictionary declaration in coder properties.
        let header = header_with_coder(&[0x03, 0x01, 0x01], &[0, 0, 0, 0, 0x80]);
        let path = write_archive_bytes(dir.path(), "dict.7z", &[], &header);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::DictionaryTooLarge {
                dictionary: 2_147_483_648,
                limit: ArchiveLimits::default().max_dictionary_bytes,
            }
        );
        assert_eq!(
            reader_new_calls(),
            before,
            "preflight must refuse before sevenz-rust2 constructs a decoder"
        );
    }

    #[test]
    fn hostile_lzma2_dictionary_rejected_before_decode() {
        let dir = tempdir().unwrap();
        // LZMA2 id + dict bits 40 → 0xFFFFFFFF (4 GiB - 1).
        let header = header_with_coder(&[0x21], &[40]);
        let path = write_archive_bytes(dir.path(), "dict2.7z", &[], &header);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::DictionaryTooLarge {
                dictionary: 0xFFFF_FFFF,
                limit: ArchiveLimits::default().max_dictionary_bytes,
            }
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn hostile_next_header_size_rejected_without_large_allocation() {
        let dir = tempdir().unwrap();
        // Start header claims a 100 MiB next header; the actual header is tiny.
        // The start-header and next-header CRCs are computed correctly so the
        // probe reaches the size check (a CRC mismatch would refuse earlier).
        let header = header_one_copy_folder(1, 1);
        let mut file = Vec::new();
        file.extend_from_slice(&SIG);
        file.push(0);
        file.push(2);
        file.extend_from_slice(&[0; 4]); // start-header CRC placeholder
        file.extend_from_slice(&0_u64.to_le_bytes()); // next_header_offset
        file.extend_from_slice(&(100_u64 * 1024 * 1024).to_le_bytes()); // hostile size
        let mut next_crc = crate::identity_source::hashing::Crc32::new();
        next_crc.update(&header);
        file.extend_from_slice(&next_crc.finish().to_le_bytes());
        file.extend_from_slice(&header);
        let mut start_crc = crate::identity_source::hashing::Crc32::new();
        start_crc.update(&file[12..32]);
        file[8..12].copy_from_slice(&start_crc.finish().to_le_bytes());
        let path = dir.path().join("big.7z");
        std::fs::write(&path, &file).unwrap();
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::NextHeaderTooLarge {
                size: 100 * 1024 * 1024,
                limit: ArchiveLimits::default().max_header_bytes,
            }
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn excessive_file_count_refused() {
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(0x1000_0000, &mut h); // hostile num_files
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "many.7z", &[], &h);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::TooManyFiles { .. }
        ));
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn excessive_folder_count_refused() {
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(0x1000_0000, &mut h); // hostile num_folders
        h.push(0);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "folders.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::TooManyFolders { .. }
        ));
    }

    #[test]
    fn excessive_coder_count_refused() {
        // The per-folder coder-chain ceiling is far below the generic
        // structural ceiling; a hostile chain length is refused up front.
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(0x1000_0000, &mut h); // hostile num_coders
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "coders.7z", &[], &h);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::CoderChainTooLong { .. }
        ));
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn encoded_header_is_refused() {
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(0x17); // K_ENCODED_HEADER
        h.extend_from_slice(&[0; 8]);
        let path = write_archive_bytes(dir.path(), "encoded.7z", &[], &h);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::EncodedHeader
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn encoded_header_archive_open_is_unsupported() {
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(0x17);
        h.extend_from_slice(&[0; 8]);
        let path = write_archive_bytes(dir.path(), "encoded.7z", &[], &h);
        let trusted = trusted_for(dir.path());
        let err =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap_err();
        assert!(matches!(err, ArchiveMemberSourceError::Unsupported { .. }));
    }

    #[test]
    fn compressed_size_sum_overflow_is_refused() {
        let dir = tempdir().unwrap();
        // One folder with two packed streams summing past u64::MAX.
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(2, &mut h);
        h.push(K_SIZE);
        uvarint(u64::MAX, &mut h);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        // One non-simple COPY coder with num_in=2, num_out=1 → two packed streams.
        uvarint(1, &mut h);
        h.push(0x11); // bits: id_size=1, not simple, no props
        h.push(0x00); // COPY
        uvarint(2, &mut h); // num_in
        uvarint(1, &mut h); // num_out
        // no bind pairs (num_out - 1 = 0); num_packed = 2 - 0 = 2 → read indices
        uvarint(0, &mut h);
        uvarint(1, &mut h);
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "overflow.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::ArithmeticOverflow
        );
    }

    #[test]
    fn ratio_exactly_at_limit_is_accepted() {
        let dir = tempdir().unwrap();
        let header = header_one_copy_folder(10, 10 * 1000);
        // The ten declared packed bytes are actually stored: the packed region
        // must exist in the file, or the region check refuses it before the
        // ratio is ever considered.
        let path = write_archive_bytes(dir.path(), "ratio_ok.7z", &[0_u8; 10], &header);
        let info = preflight(&path, &ArchiveLimits::default()).unwrap();
        assert_eq!(info.member_count, 1);
    }

    #[test]
    fn ratio_one_unit_beyond_limit_is_refused() {
        let dir = tempdir().unwrap();
        let header = header_one_copy_folder(10, 10 * 1000 + 1);
        let path = write_archive_bytes(dir.path(), "ratio_over.7z", &[], &header);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::CompressionRatioExceeded { .. }
        ));
    }

    #[test]
    fn zero_compressed_size_is_refused_for_nonzero_unpack() {
        let dir = tempdir().unwrap();
        let header = header_one_copy_folder(0, 1);
        let path = write_archive_bytes(dir.path(), "zeropack.7z", &[], &header);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::CompressionRatioExceeded { .. }
        ));
    }

    #[test]
    fn nested_members_count_toward_the_logical_budget() {
        // The nested member's bytes are never hashed, but its declared size
        // counts toward the cumulative logical budget: the archive is refused
        // at open because the total (nested 8 + normal 3 = 11) exceeds 10.
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("a.zip", "AAAAAAAA"), ("b.rom", "BBB")]);
        let trusted = trusted_for(dir.path());
        let limits = ArchiveLimits {
            max_archive_logical_bytes: 10,
            ..ArchiveLimits::default()
        };
        let err = SevenZArchiveSource::open(&path, &trusted, limits, &TEST_NO_CANCEL).unwrap_err();
        assert_eq!(
            err,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "total logical budget"
            }
        );
    }

    #[test]
    fn cancellation_during_real_decode_stops_verification() {
        use std::sync::Arc;
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("a.bin", "aaa"), ("b.bin", "bbb")]);
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_flag = cancel.clone();
        let mut seen = 0;
        let result = source.verify_all(&cancel, &mut |evidence| {
            if evidence.is_verified() {
                // Set mid-decode: the first member really decoded and hashed
                // before the flag fires; the second member stops on its first
                // chunk check.
                cancel_flag.store(true, Ordering::Relaxed);
            }
            seen += 1;
            Ok(true)
        });
        assert_eq!(result, Err(ArchiveMemberSourceError::Cancelled));
        assert_eq!(seen, 1, "exactly the first member completed");
    }

    #[test]
    fn preflight_refused_input_never_constructs_the_reader() {
        let dir = tempdir().unwrap();
        let header = header_with_coder(&[0x21], &[40]);
        let path = write_archive_bytes(dir.path(), "hostile.7z", &[], &header);
        let trusted = trusted_for(dir.path());
        let before = reader_new_calls();
        let err =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap_err();
        assert!(matches!(
            err,
            ArchiveMemberSourceError::RefusedLimits { .. }
        ));
        assert_eq!(
            reader_new_calls(),
            before,
            "ArchiveReader::new must never run for a preflight-refused archive"
        );
    }

    // ------------------------------------------------------------------
    // Additional hostile fixtures for the Codex re-review pass.
    // ------------------------------------------------------------------

    /// A header with one folder containing `count` LZMA2 coders, each with the
    /// given dictionary bits, chained via bind pairs, with `count` sub-streams
    /// and `count` files. Used for aggregate-decoder-memory and coder-chain
    /// tests.
    fn header_with_lzma2_chain(count: usize, dict_bits: u8) -> Vec<u8> {
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(1, &mut h);
        h.push(K_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(count as u64, &mut h); // num_coders
        for _ in 0..count {
            h.push(0x21); // bits: id_size=1, simple, has_attrs
            h.push(0x21); // LZMA2 id
            uvarint(1, &mut h);
            h.push(dict_bits);
        }
        for i in 0..count.saturating_sub(1) {
            uvarint(i as u64 + 1, &mut h); // in_index
            uvarint(i as u64, &mut h); // out_index
        }
        h.push(K_CODERS_UNPACK_SIZE);
        for _ in 0..count {
            uvarint(0, &mut h);
        }
        h.push(K_END);
        h.push(K_SUB_STREAMS_INFO);
        h.push(K_NUM_UNPACK_STREAM);
        uvarint(count as u64, &mut h);
        h.push(K_SIZE);
        for _ in 0..count.saturating_sub(1) {
            uvarint(0, &mut h);
        }
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(count as u64, &mut h);
        h.push(K_END);
        h.push(K_END);
        h
    }

    /// Writes a file whose start-header or next-header CRC is deliberately
    /// wrong. `bad_start`/`bad_next` overwrite the stored CRC fields.
    fn write_archive_bad_crc(
        dir: &std::path::Path,
        name: &str,
        next_header: &[u8],
        bad_start: bool,
        bad_next: bool,
    ) -> PathBuf {
        let mut file = Vec::new();
        file.extend_from_slice(&SIG);
        file.push(0);
        file.push(2);
        file.extend_from_slice(&[0; 4]); // start crc placeholder
        file.extend_from_slice(&0_u64.to_le_bytes()); // offset
        file.extend_from_slice(&(next_header.len() as u64).to_le_bytes());
        let mut next_crc = crate::identity_source::hashing::Crc32::new();
        next_crc.update(next_header);
        let next_value = if bad_next {
            next_crc.finish().wrapping_add(1)
        } else {
            next_crc.finish()
        };
        file.extend_from_slice(&next_value.to_le_bytes());
        file.extend_from_slice(next_header);
        let mut start_crc = crate::identity_source::hashing::Crc32::new();
        start_crc.update(&file[12..32]);
        let start_value = if bad_start {
            start_crc.finish().wrapping_add(1)
        } else {
            start_crc.finish()
        };
        file[8..12].copy_from_slice(&start_value.to_le_bytes());
        let path = dir.join(name);
        std::fs::write(&path, &file).unwrap();
        path
    }

    #[test]
    fn aggregate_decoder_memory_budget_is_enforced() {
        // Three individually valid 1 GiB LZMA2 dictionaries in one folder sum
        // to 3 GiB, exceeding the 2 GiB aggregate decoder-memory budget.
        let dir = tempdir().unwrap();
        let header = header_with_lzma2_chain(3, 36); // 36 → 1 GiB each
        let path = write_archive_bytes(dir.path(), "agg.7z", &[], &header);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::AggregateDecoderMemoryExceeded {
                bytes: 3 * 1024 * 1024 * 1024,
                limit: ArchiveLimits::default().max_aggregate_decoder_memory_bytes,
            }
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn aggregate_decoder_memory_within_budget_is_accepted() {
        // Two 512 MiB dictionaries sum to 1 GiB ≤ the 2 GiB budget.
        let dir = tempdir().unwrap();
        let header = header_with_lzma2_chain(2, 34); // 34 → 512 MiB each
        let path = write_archive_bytes(dir.path(), "agg_ok.7z", &[], &header);
        let info = preflight(&path, &ArchiveLimits::default()).unwrap();
        assert_eq!(info.member_count, 2);
    }

    #[test]
    fn coder_chain_ceiling_is_enforced() {
        // 17 coders in one folder exceeds the 16-coder chain ceiling.
        let dir = tempdir().unwrap();
        let header = header_with_lzma2_chain(17, 36);
        let path = write_archive_bytes(dir.path(), "chain.7z", &[], &header);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::CoderChainTooLong {
                count: 17,
                limit: ArchiveLimits::default().max_coders_per_folder,
            }
        );
    }

    #[test]
    fn zero_sized_k_name_is_refused() {
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_NAME);
        uvarint(0, &mut h); // zero-sized K_NAME property (panic-prone upstream)
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "zname.7z", &[], &h);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                detail: "zero-sized K_NAME property"
            }
        ));
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn malformed_k_name_names_count_is_refused() {
        // K_NAME declares two names but only one null-terminated name is stored.
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(2, &mut h); // two files
        h.push(K_NAME);
        let mut names = vec![0_u8];
        for unit in "a".encode_utf16() {
            names.extend_from_slice(&unit.to_le_bytes());
        }
        names.extend_from_slice(&[0, 0]);
        uvarint(names.len() as u64, &mut h);
        h.extend_from_slice(&names);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "bnames.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                detail: "K_NAME names count mismatch"
            }
        ));
    }

    #[test]
    fn truncated_files_info_is_refused() {
        // FilesInfo declares a large K_NAME property with only a few bytes.
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_NAME);
        uvarint(101, &mut h); // claims 101 bytes (100 of names); only 2 stored
        h.push(0); // external byte
        h.extend_from_slice(&[0, 0]); // only 2 bytes of names
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "trunc.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Truncated
        ));
    }

    #[test]
    fn missing_files_info_terminator_is_refused() {
        // FilesInfo properties never end with K_END; the cursor runs past the
        // bounded buffer.
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_NAME);
        uvarint(3, &mut h); // external byte + one null-terminated name
        h.push(0); // external byte
        h.push(0);
        h.push(0); // the name ("") null; no K_END follows the property
        let path = write_archive_bytes(dir.path(), "noterm.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Truncated
        ));
    }

    #[test]
    fn incorrect_start_header_crc_is_refused() {
        let dir = tempdir().unwrap();
        let header = header_one_copy_folder(1, 1);
        let path = write_archive_bad_crc(dir.path(), "badstart.7z", &header, true, false);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                detail: "start header checksum mismatch"
            }
        ));
    }

    #[test]
    fn zero_start_header_crc_is_still_validated() {
        // A stored start-header CRC of 0 must still match the computed CRC;
        // here it does not (the content's real CRC is non-zero).
        let dir = tempdir().unwrap();
        let header = header_one_copy_folder(1, 1);
        let mut file = Vec::new();
        file.extend_from_slice(&SIG);
        file.push(0);
        file.push(2);
        file.extend_from_slice(&0_u32.to_le_bytes()); // stored start CRC = 0
        file.extend_from_slice(&0_u64.to_le_bytes()); // offset
        file.extend_from_slice(&(header.len() as u64).to_le_bytes());
        let mut next_crc = crate::identity_source::hashing::Crc32::new();
        next_crc.update(&header);
        file.extend_from_slice(&next_crc.finish().to_le_bytes());
        file.extend_from_slice(&header);
        // content of the start-header block has a non-zero CRC, so stored 0 is wrong
        let path = dir.path().join("zerocrc.7z");
        std::fs::write(&path, &file).unwrap();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                detail: "start header checksum mismatch"
            }
        ));
    }

    #[test]
    fn incorrect_next_header_crc_is_refused() {
        let dir = tempdir().unwrap();
        let header = header_one_copy_folder(1, 1);
        let path = write_archive_bad_crc(dir.path(), "badnext.7z", &header, false, true);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                detail: "next header checksum mismatch"
            }
        ));
    }

    #[test]
    fn invalid_bind_pair_index_is_refused() {
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(1, &mut h);
        h.push(K_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(2, &mut h); // two COPY coders → total_out=2 → one bind pair
        h.push(0x01);
        h.push(0x00);
        h.push(0x01);
        h.push(0x00);
        uvarint(1, &mut h); // in_index 1
        uvarint(5, &mut h); // out_index 5 >= total_out=2 → invalid
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(0, &mut h);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "bindpair.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                detail: "bind pair index out of range"
            }
        ));
    }

    #[test]
    fn invalid_packed_stream_index_is_refused() {
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(1, &mut h);
        h.push(K_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        // One non-simple COPY coder with num_in=2, num_out=1 → two packed streams.
        uvarint(1, &mut h);
        h.push(0x11);
        h.push(0x00);
        uvarint(2, &mut h);
        uvarint(1, &mut h);
        uvarint(0, &mut h);
        uvarint(5, &mut h); // second packed-stream index 5 >= total_in=2 → invalid
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "packedidx.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                detail: "packed stream index out of range"
            }
        ));
    }

    #[test]
    fn inconsistent_pack_folder_consumption_is_refused() {
        let dir = tempdir().unwrap();
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h);
        uvarint(2, &mut h); // two pack streams...
        h.push(K_SIZE);
        uvarint(1, &mut h);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(1, &mut h); // ...but only one folder consuming one pack stream
        h.push(0x01);
        h.push(0x00);
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "packleft.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(matches!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                detail: "pack streams not fully consumed"
            }
        ));
    }

    #[test]
    fn runtime_cumulative_budget_near_limit_succeeds_without_double_counting() {
        // Two 10-byte members with a 20-byte cumulative budget: each member is
        // charged once, so both verify. The previous double-count bug charged
        // ordinary members twice and falsely rejected the second.
        let dir = tempdir().unwrap();
        let path = make_archive(
            dir.path(),
            &[("a.bin", "AAAAAAAAAA"), ("b.bin", "BBBBBBBBBB")],
        );
        let trusted = trusted_for(dir.path());
        let limits = ArchiveLimits {
            max_archive_logical_bytes: 20,
            ..ArchiveLimits::default()
        };
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, limits, &TEST_NO_CANCEL).unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 2);
        assert!(
            evidence.iter().all(|e| e.is_verified()),
            "both members must verify when the cumulative total exactly meets the budget"
        );
    }

    #[test]
    fn nested_and_ordinary_runtime_accounting_counts_each_once() {
        // A 5-byte nested member and a 5-byte ordinary member with a 10-byte
        // budget: nested counts once (drained), ordinary once (hashed) → both
        // pass. If nested were double-counted, the total would exceed 10.
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("a.zip", "AAAAA"), ("b.rom", "BBBBB")]);
        let trusted = trusted_for(dir.path());
        let limits = ArchiveLimits {
            max_archive_logical_bytes: 10,
            ..ArchiveLimits::default()
        };
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, limits, &TEST_NO_CANCEL).unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0].status, ArchiveMemberStatus::NestedArchive);
        assert!(evidence[1].is_verified());
    }

    #[test]
    fn deterministic_malformed_headers_never_panic() {
        // A bounded property-style pass over a valid header's truncated
        // prefixes and byte-flip mutations: the probe must return Err, never
        // panic. (A single valid solid header is the seed.)
        let dir = tempdir().unwrap();
        let valid = header_one_copy_folder(8, 8);
        // Truncated prefixes.
        for length in 0..valid.len() {
            let path = write_archive_bytes(dir.path(), "prefix.7z", &[], &valid[..length]);
            let _ = preflight(&path, &ArchiveLimits::default());
        }
        // Byte flips (deterministic): each byte XOR 0xFF, plus a few offsets.
        let mut mutated = valid.clone();
        for index in 0..valid.len() {
            let original = mutated[index];
            mutated[index] ^= 0xFF;
            let path = write_archive_bytes(dir.path(), "mut.7z", &[], &mutated);
            let _ = preflight(&path, &ArchiveLimits::default());
            mutated[index] = original;
        }
        // Boundary count values in FilesInfo (0, 1, limit, limit + 1, huge).
        for count in [0_u64, 1, 2, 4096, 4097, 0x1_0000_0000] {
            let mut h = Vec::new();
            h.push(K_HEADER);
            h.push(K_MAIN_STREAMS_INFO);
            h.push(K_PACK_INFO);
            uvarint(0, &mut h);
            uvarint(0, &mut h);
            h.push(K_END);
            h.push(K_END);
            h.push(K_FILES_INFO);
            uvarint(count, &mut h);
            h.push(K_NAME);
            uvarint(2, &mut h);
            h.push(0);
            h.push(0);
            h.push(0);
            h.push(K_END);
            h.push(K_END);
            let path = write_archive_bytes(dir.path(), "counts.7z", &[], &h);
            let _ = preflight(&path, &ArchiveLimits::default());
        }
        // Malformed varints (a truncated multi-byte varint at the end).
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        h.push(0x80); // varint continuation with no following byte
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "varint.7z", &[], &h);
        let _ = preflight(&path, &ArchiveLimits::default());
    }

    // ------------------------------------------------------------------
    // Regressions for parser-divergence and coder-graph defects originally
    // found by reading the `sevenz-rust` 0.6.1 source (see sevenz_preflight
    // docs). They are kept after the move to `sevenz-rust2`: the probe owns
    // these refusals regardless of what the upstream parser does with the
    // same bytes, so they must keep failing closed here.
    // ------------------------------------------------------------------

    const K_EMPTY_STREAM: u8 = 0x0E;
    const K_EMPTY_FILE: u8 = 0x0F;
    const K_M_TIME: u8 = 0x14;

    /// Header prologue: pack info with `pack_streams` streams of `pack_size`,
    /// then the caller appends its own unpack info.
    fn pack_info(pack_sizes: &[u64]) -> Vec<u8> {
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(0, &mut h); // pack_pos
        uvarint(pack_sizes.len() as u64, &mut h);
        h.push(K_SIZE);
        for &size in pack_sizes {
            uvarint(size, &mut h);
        }
        h.push(K_END);
        h
    }

    #[test]
    fn empty_stream_vector_is_a_plain_bit_vector_not_all_or_bits() {
        // `kEmptyStream` has NO leading all-defined byte (7zFormat.txt), and
        // both forks read it as a plain bit vector. Reading it as
        // all-or-bits made the probe see "every file is empty" for any payload
        // whose first byte is non-zero, so this archive passed the
        // stream-count reconciliation while `read_files_info` went on to index
        // `sub_streams_info.crcs[0]` on an empty vector and panic.
        let dir = tempdir().unwrap();
        let mut h = pack_info(&[0]);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h); // one folder
        h.push(0); // external
        uvarint(1, &mut h); // one coder
        h.push(0x01); // simple, id_size = 1
        h.push(0x00); // COPY
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_SUB_STREAMS_INFO);
        h.push(K_NUM_UNPACK_STREAM);
        uvarint(0, &mut h); // the folder declares zero sub-streams
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(2, &mut h); // two files
        h.push(K_EMPTY_STREAM);
        uvarint(1, &mut h);
        h.push(0x80); // file 0 empty, file 1 HAS a stream
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "emptybits.7z", &[], &h);

        // One stream-bearing file against zero declared sub-streams.
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                    detail: "file count inconsistent with stream sizes"
                }
            ),
            "unexpected refusal: {err:?}"
        );

        // End to end: the decoder must never be constructed for this archive.
        let trusted = trusted_for(dir.path());
        let before = reader_new_calls();
        let err =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default(), &TEST_NO_CANCEL)
                .unwrap_err();
        assert!(matches!(err, ArchiveMemberSourceError::Corrupt { .. }));
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn empty_file_property_without_empty_stream_is_refused() {
        // `kEmptyFile` is sized by the empty-stream count, so it is only
        // meaningful after `kEmptyStream`; upstream errors out too.
        let dir = tempdir().unwrap();
        let mut h = pack_info(&[]);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_EMPTY_FILE);
        uvarint(1, &mut h);
        h.push(0x00);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "emptyfile.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                    detail: "kEmptyStream must precede kEmptyFile/kAnti"
                }
            ),
            "unexpected refusal: {err:?}"
        );
    }

    #[test]
    fn timestamp_property_is_parsed_structurally_not_skipped_by_declared_size() {
        // both forks ignore the declared size of kMTime and parse it
        // structurally, so a lying size would desynchronise a probe that
        // skipped by size. Here the size lies (0) but the payload is a real
        // all-defined kMTime record; the probe must consume it and still find
        // the two terminators.
        let dir = tempdir().unwrap();
        let mut h = pack_info(&[1]);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(1, &mut h);
        h.push(0x01);
        h.push(0x00); // COPY
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(8, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_M_TIME);
        uvarint(0, &mut h); // declared size lies
        h.push(0x01); // all times defined
        h.push(0x00); // external = 0
        h.extend_from_slice(&[0; 8]); // one 64-bit timestamp
        h.push(K_END);
        h.push(K_END);
        // The single declared packed byte is actually stored, so the packed
        // region is physically present and only the kMTime parse is under test.
        let path = write_archive_bytes(dir.path(), "mtime.7z", &[0_u8; 1], &h);
        let info = preflight(&path, &ArchiveLimits::default()).unwrap();
        assert_eq!(info.member_count, 1);
    }

    #[test]
    fn cyclic_bind_pairs_are_refused() {
        // Bind pairs (in 2 -> out 0), (in 3 -> out 2), (in 2 -> out 3) reuse
        // input stream 2. `OrderedCoderIter` would then walk 0 -> 2 -> 3 -> 2
        // -> 3 ... forever, allocating a new decoder on every step.
        let dir = tempdir().unwrap();
        let mut h = pack_info(&[0]);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(4, &mut h); // four simple COPY coders
        for _ in 0..4 {
            h.push(0x01);
            h.push(0x00);
        }
        for (in_index, out_index) in [(2_u64, 0_u64), (3, 2), (2, 3)] {
            uvarint(in_index, &mut h);
            uvarint(out_index, &mut h);
        }
        h.push(K_CODERS_UNPACK_SIZE);
        for _ in 0..4 {
            uvarint(0, &mut h);
        }
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "cycle.7z", &[], &h);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                    detail: "duplicate bind pair input index"
                }
            ),
            "unexpected refusal: {err:?}"
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn duplicate_packed_stream_index_is_refused() {
        let dir = tempdir().unwrap();
        let mut h = pack_info(&[0, 0]);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(1, &mut h);
        h.push(0x11); // not simple, id_size = 1
        h.push(0x00); // COPY
        uvarint(2, &mut h); // num_in
        uvarint(1, &mut h); // num_out
        uvarint(0, &mut h); // packed stream index 0...
        uvarint(0, &mut h); // ...declared twice
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "duppack.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                    detail: "duplicate packed stream index"
                }
            ),
            "unexpected refusal: {err:?}"
        );
    }

    #[test]
    fn multi_output_coder_is_refused_before_the_fixed_size_arrays_overflow() {
        // One coder declaring 41 inputs and 40 outputs sends `sevenz-rust`
        // down `build_decode_stack2`, which writes `coder_used[out_index]`
        // into a `[bool; 32]` - an out-of-bounds panic for out_index >= 32.
        let dir = tempdir().unwrap();
        let mut h = pack_info(&[0, 0]);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(1, &mut h); // one coder...
        h.push(0x11); // ...not simple, id_size = 1
        h.push(0x00); // COPY
        uvarint(41, &mut h); // num_in
        uvarint(40, &mut h); // num_out
        for index in 0..39_u64 {
            uvarint(index, &mut h); // in_index
            uvarint(index, &mut h); // out_index
        }
        uvarint(39, &mut h); // packed stream indices
        uvarint(40, &mut h);
        h.push(K_CODERS_UNPACK_SIZE);
        for _ in 0..40 {
            uvarint(0, &mut h);
        }
        h.push(K_END);
        h.push(K_SUB_STREAMS_INFO);
        h.push(K_NUM_UNPACK_STREAM);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "multiout.7z", &[], &h);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                    detail: "coder with multiple output streams is unsupported"
                }
            ),
            "unexpected refusal: {err:?}"
        );
        assert_eq!(reader_new_calls(), before);
    }

    // ------------------------------------------------------------------
    // The declared packed-data region must exist in the physical file: the
    // offsets are attacker-controlled, so every step of the arithmetic is
    // checked and the region must neither run past EOF nor alias the
    // next-header region.
    // ------------------------------------------------------------------

    /// A plain header with an explicit `pack_pos` and one COPY folder per pack
    /// stream, each folder unpacking to zero bytes. Nothing but the packed
    /// region checks can fire for these fixtures: a zero unpack size skips the
    /// compression-ratio check, and one folder per stream keeps the pack
    /// streams fully consumed.
    fn header_pack_region(pack_pos: u64, pack_sizes: &[u64]) -> Vec<u8> {
        let mut h = Vec::new();
        h.push(K_HEADER);
        h.push(K_MAIN_STREAMS_INFO);
        h.push(K_PACK_INFO);
        uvarint(pack_pos, &mut h);
        uvarint(pack_sizes.len() as u64, &mut h);
        h.push(K_SIZE);
        for &size in pack_sizes {
            uvarint(size, &mut h);
        }
        h.push(K_END);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(pack_sizes.len() as u64, &mut h); // one folder per pack stream
        h.push(0); // external
        for _ in pack_sizes {
            uvarint(1, &mut h); // one coder
            h.push(0x01); // simple, id_size = 1
            h.push(0x00); // COPY
        }
        h.push(K_CODERS_UNPACK_SIZE);
        for _ in pack_sizes {
            uvarint(0, &mut h); // unpack size 0
        }
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(pack_sizes.len() as u64, &mut h);
        h.push(K_END);
        h.push(K_END);
        h
    }

    #[test]
    fn hostile_pack_pos_overflow_is_refused() {
        // `pack_pos` is added to the 32-byte signature-header size to get the
        // region's absolute start; a `u64::MAX` declaration overflows that
        // addition, which must be a refusal rather than a wrap to a small
        // (and therefore plausible-looking) offset.
        let dir = tempdir().unwrap();
        let header = header_pack_region(u64::MAX, &[0]);
        let path = write_archive_bytes(dir.path(), "packpos.7z", &[], &header);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::ArithmeticOverflow
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn hostile_cumulative_pack_size_overflow_is_refused() {
        // Two folders of one `u64::MAX` pack stream each. Neither folder's own
        // packed-size summation overflows, so this can only be caught by the
        // running total over *all* pack streams.
        let dir = tempdir().unwrap();
        let header = header_pack_region(0, &[u64::MAX, u64::MAX]);
        let path = write_archive_bytes(dir.path(), "packsum.7z", &[], &header);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::ArithmeticOverflow
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn hostile_pack_region_end_overflow_is_refused() {
        // A plausible `pack_pos` plus a single `u64::MAX` stream: neither the
        // start offset nor the size sum overflows on its own, only their sum
        // (the region's end offset).
        let dir = tempdir().unwrap();
        let header = header_pack_region(100, &[u64::MAX]);
        let path = write_archive_bytes(dir.path(), "packend.7z", &[], &header);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::ArithmeticOverflow
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn pack_region_past_end_of_file_is_refused() {
        // The header declares 4 KiB of packed data; the file holds none of it.
        // Those bytes do not exist, so the archive is truncated.
        let dir = tempdir().unwrap();
        let header = header_pack_region(0, &[4096]);
        let path = write_archive_bytes(dir.path(), "packeof.7z", &[], &header);
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert_eq!(
            err,
            crate::dat::archive::sevenz_preflight::PreflightRefusal::Truncated
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn pack_region_overlapping_the_next_header_is_refused() {
        // Four packed bytes are stored (so the next header starts at 36) but
        // the header declares six, running the packed region into the
        // next-header region. Both readings of those bytes cannot be true.
        let dir = tempdir().unwrap();
        let header = header_pack_region(0, &[6]);
        let path = write_archive_bytes(dir.path(), "packoverlap.7z", &[0_u8; 4], &header);
        // The region stays inside the file: only the aliasing is wrong.
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(len > 32 + 6, "the fixture must not be truncated as well");
        let before = reader_new_calls();
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                    detail: "packed data overlaps the next header"
                }
            ),
            "unexpected refusal: {err:?}"
        );
        assert_eq!(reader_new_calls(), before);
    }

    #[test]
    fn pack_region_ending_exactly_at_the_next_header_is_accepted() {
        // The boundary case the overlap check must not over-refuse: the packed
        // region ends exactly where the next header begins.
        let dir = tempdir().unwrap();
        let header = header_pack_region(0, &[4]);
        let path = write_archive_bytes(dir.path(), "packflush.7z", &[0_u8; 4], &header);
        let info = preflight(&path, &ArchiveLimits::default()).unwrap();
        assert_eq!(info.member_count, 1);
    }

    #[test]
    fn bcj2_coder_with_wrong_input_count_is_refused() {
        // `BCJ2Reader` indexes a fixed four-element input array; a BCJ2 coder
        // declaring two inputs panics inside the decoder's read loop.
        let dir = tempdir().unwrap();
        let mut h = pack_info(&[0, 0]);
        h.push(K_UNPACK_INFO);
        h.push(K_FOLDER);
        uvarint(1, &mut h);
        h.push(0);
        uvarint(1, &mut h);
        h.push(0x14); // not simple, id_size = 4
        h.extend_from_slice(&[0x03, 0x03, 0x01, 0x1B]); // BCJ2
        uvarint(2, &mut h); // num_in (must be 4)
        uvarint(1, &mut h); // num_out
        uvarint(0, &mut h); // packed stream indices
        uvarint(1, &mut h);
        h.push(K_CODERS_UNPACK_SIZE);
        uvarint(0, &mut h);
        h.push(K_END);
        h.push(K_END);
        h.push(K_FILES_INFO);
        uvarint(1, &mut h);
        h.push(K_END);
        h.push(K_END);
        let path = write_archive_bytes(dir.path(), "bcj2.7z", &[], &h);
        let err = preflight(&path, &ArchiveLimits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                crate::dat::archive::sevenz_preflight::PreflightRefusal::Malformed {
                    detail: "BCJ2 coder must declare exactly four input streams"
                }
            ),
            "unexpected refusal: {err:?}"
        );
    }
}

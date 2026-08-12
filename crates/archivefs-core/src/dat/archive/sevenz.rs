//! Bounded, read-only 7z member reading (POST-ALPHA-1.1, experimental).
//!
//! Implements [`super::ArchiveMemberSource`] for `.7z` using `sevenz-rust`
//! (Apache-2.0, pure Rust). Everything is in-process and read-only:
//!
//! - The outer file is opened through `safe_read`/`TrustedRoots` (never a bare
//!   `File::open`).
//! - The 7z header is parsed first; encrypted headers, unsupported versions,
//!   multi-volume names, and corrupt headers are refused before any decode.
//! - Structure-level limits (member count, per-member size, total logical
//!   budget, **solid decode budget**, **decompression ratio**) are enforced
//!   from header metadata **before** any content is decoded. These are derived
//!   from `sevenz-rust`'s public `Archive`/`StreamMap`/`pack_sizes`/`files`
//!   because its `Folder`/`Coder` internals are `pub(crate)` and unnameable
//!   outside the crate.
//! - Member streams are hashed in bounded chunks with per-chunk cancellation.
//! - Nested-archive members are surfaced but never recursively opened.
//!
//! # Known limitation: dictionary pre-check
//!
//! sevenz-rust 0.6.1 does **not** expose coder properties publicly
//! (`Folder`/`Coder` are `pub(crate)`), so a sub-4 GiB dictionary cap cannot
//! be enforced from the header through its public API. What is enforceable:
//! the crate's own LZMA2 ceiling (`Error::MaxMemLimited`, `"Dictionary larger
//! than 4 GiB"`) is mapped to a [`ArchiveMemberSourceError::RefusedLimits`]
//! refusal at decode time, and the pure size-parsing helpers
//! ([`lzma_dictionary_size`], [`lzma2_dictionary_size`]) are provided and
//! tested as the intended mechanism for a later slice that either parses the
//! header itself or lands an upstream API change.
//!
//! # Zero production callers
//!
//! Nothing in shipped code calls this module. It is experimental scaffolding
//! exercised only by focused tests (`docs/research/SEVEN_Z_RAR_ARCHIVE_
//! VERIFICATION_RESEARCH.md` §12, slice 7Z-1).

use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use sevenz_rust::Error as SevenZError;
use sevenz_rust::{Archive, Password, SevenZReader};

use crate::safe_read::{TrustedRoots, open_bounded_read};

use super::hash::{MemberStreamError, hash_member_stream};
use super::limits::{ARCHIVE_HASH_CHUNK_BYTES, ArchiveLimits};
use super::{
    ArchiveMemberEvidence, ArchiveMemberSource, ArchiveMemberSourceError, ArchiveMemberStatus,
};

/// File extensions treated as "nested archive" member names. Members with one
/// of these extensions are surfaced with metadata but never opened.
const NESTED_ARCHIVE_EXTENSIONS: &[&str] = &["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst"];

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
    reader: SevenZReader<std::fs::File>,
    members: Vec<MemberMeta>,
    limits: ArchiveLimits,
}

impl std::fmt::Debug for SevenZArchiveSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SevenZArchiveSource")
            .field("members", &self.members)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl SevenZArchiveSource {
    /// Opens `path` under `trusted` and validates the archive up to decode.
    pub fn open(
        path: &Path,
        trusted: &TrustedRoots,
        limits: ArchiveLimits,
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
        let file = safe.into_file();
        let reader = SevenZReader::new(file, len, Password::empty())
            .map_err(|error| classify_error(&error))?;

        let members = collect_members(reader.archive());
        if members.len() > limits.max_members {
            return Err(ArchiveMemberSourceError::RefusedLimits {
                reason: "member count",
            });
        }
        validate_archive_structure(reader.archive(), &limits)?;

        Ok(Self {
            reader,
            members,
            limits,
        })
    }
}

impl ArchiveMemberSource for SevenZArchiveSource {
    fn archive_format(&self) -> &'static str {
        "7z"
    }

    fn member_count(&self) -> usize {
        self.members.len()
    }

    fn verify_all<F>(
        &mut self,
        cancel: &AtomicBool,
        mut visit: F,
    ) -> Result<(), ArchiveMemberSourceError>
    where
        F: FnMut(ArchiveMemberEvidence) -> Result<bool, ArchiveMemberSourceError>,
    {
        let members: Vec<MemberMeta> = self.members.clone();
        let limits = self.limits;
        let reader = &mut self.reader;
        let mut cursor: usize = 0;
        let mut total_hashed: u64 = 0;
        let mut internal_error: Option<ArchiveMemberSourceError> = None;

        let result = reader.for_each_entries(|entry, stream| {
            let Some(meta) = members.get(cursor).cloned() else {
                // Directory/empty entries (has_stream == false) are visited
                // after every stream member and never match a cursor member.
                return Ok(true);
            };
            if meta.name != entry.name() {
                internal_error = Some(ArchiveMemberSourceError::Corrupt {
                    detail: format!("member order mismatch at index {cursor}"),
                });
                return Ok(false);
            }
            let index = cursor;
            cursor += 1;

            if meta.logical_size == 0 {
                return match visit(evidence(&meta, index, ArchiveMemberStatus::EmptyFile, None)) {
                    Ok(true) => Ok(true),
                    Ok(false) => Ok(false),
                    Err(error) => {
                        internal_error = Some(error);
                        Ok(false)
                    }
                };
            }

            if meta.is_nested {
                if meta.logical_size > limits.max_member_logical_bytes {
                    let outcome = visit(evidence(
                        &meta,
                        index,
                        ArchiveMemberStatus::RefusedLimits {
                            reason: "member size",
                        },
                        None,
                    ));
                    match outcome {
                        Ok(_) => return Ok(false),
                        Err(error) => {
                            internal_error = Some(error);
                            return Ok(false);
                        }
                    }
                }
                // Drain (bounded) so a solid block stays aligned; never hash or
                // open the nested archive.
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
                return match visit(evidence(
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
            if total_hashed.saturating_add(meta.logical_size) > limits.max_archive_logical_bytes {
                let outcome = visit(evidence(
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

            match hash_member_stream(stream, limits.max_member_logical_bytes, cancel) {
                Ok(hashed) => {
                    total_hashed = total_hashed.saturating_add(hashed.bytes_read);
                    match visit(evidence(
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
            name: meta.name.clone(),
            index,
            logical_size: meta.logical_size,
            is_nested_archive: meta.is_nested,
            status,
            hashes: None,
        }
    }
}

/// Maps a `sevenz-rust` error onto our fail-closed refusal taxonomy.
fn classify_error(error: &SevenZError) -> ArchiveMemberSourceError {
    use sevenz_rust::Error as E;
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
        | E::BadTerminatedheader(_) => ArchiveMemberSourceError::Corrupt {
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
/// Uses only `sevenz-rust`'s public surface (`Archive`, `StreamMap`,
/// `pack_sizes`, `files`): folder `Folder`/`Coder` internals are not public,
/// so per-folder sizes and ratios are derived from the pack-stream ranges and
/// the files each folder owns.
fn validate_archive_structure(
    archive: &Archive,
    limits: &ArchiveLimits,
) -> Result<(), ArchiveMemberSourceError> {
    let folder_first_stream = &archive.stream_map.folder_first_pack_stream_index;
    let folder_count = folder_first_stream.len();
    for folder_index in 0..folder_count {
        let pack_start = folder_first_stream[folder_index];
        let pack_end = folder_first_stream
            .get(folder_index + 1)
            .copied()
            .unwrap_or(archive.pack_sizes.len());
        let pack: u64 = archive
            .pack_sizes
            .get(pack_start..pack_end)
            .map(|slice| slice.iter().sum())
            .unwrap_or(0);

        // Files owned by this folder: their declared sizes sum to the folder's
        // logical (unpacked) size.
        let (files_in_folder, unpack) = archive
            .files
            .iter()
            .enumerate()
            .filter(|(index, file)| {
                file.has_stream
                    && archive
                        .stream_map
                        .file_folder_index
                        .get(*index)
                        .copied()
                        .flatten()
                        == Some(folder_index)
            })
            .fold((0_usize, 0_u64), |(count, sum), (_, file)| {
                (count + 1, sum.saturating_add(file.size))
            });

        if pack > 0 && unpack / pack > limits.max_compression_ratio {
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

/// LZMA dictionary size from coder properties (`props[1..5]`, LE u32).
///
/// Kept pure and tested: the mechanism a later slice uses once coder
/// properties are readable (see the module doc's known limitation). Currently
/// unreachable from production because sevenz-rust 0.6.1 does not expose
/// `Coder` publicly.
#[allow(dead_code)]
fn lzma_dictionary_size(properties: &[u8]) -> Result<u32, &'static str> {
    let bytes = properties.get(1..5).ok_or("LZMA properties too short")?;
    let mut buffer = [0_u8; 4];
    buffer.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(buffer))
}

/// LZMA2 dictionary size from coder properties, mirroring sevenz-rust's own
/// formula (dict bits → bytes).
#[allow(dead_code)]
fn lzma2_dictionary_size(properties: &[u8]) -> Result<u32, &'static str> {
    let bits = 0xff & u32::from(*properties.first().ok_or("LZMA2 properties too short")?);
    if (bits & (!0x3f)) != 0 {
        return Err("Unsupported LZMA2 property bits");
    }
    if bits > 40 {
        return Err("LZMA2 dictionary larger than 4 GiB");
    }
    if bits == 40 {
        return Ok(0xFFFF_FFFF);
    }
    Ok((2 | (bits & 0x1)) << (bits / 2 + 11))
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
    meta: &MemberMeta,
    index: usize,
    status: ArchiveMemberStatus,
    hashes: Option<super::ArchiveMemberHashes>,
) -> ArchiveMemberEvidence {
    ArchiveMemberEvidence {
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
    //! Focused tests for the experimental 7z reader. Fixtures are generated
    //! in-test with the `sevenz-rust` writer (a dev-dependency feature) into a
    //! temp directory; no copyrighted data and no external tools are used.

    use super::*;
    use crate::safe_read::TrustedRoots;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use tempfile::tempdir;

    use sevenz_rust::{SevenZMethod, SevenZWriter};

    fn trusted_for(root: &std::path::Path) -> TrustedRoots {
        TrustedRoots::from_paths(std::iter::once(root))
    }

    fn write_text(path: &std::path::Path, contents: &str) {
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
    }

    /// Builds a stream-bearing fixture entry. Field assignment is the only
    /// way to construct this from outside `sevenz-rust` (its `content_methods`
    /// field is crate-private, which also rules out struct-literal syntax).
    #[allow(clippy::field_reassign_with_default)]
    fn fixture_entry(name: &str, size: u64) -> sevenz_rust::SevenZArchiveEntry {
        let mut entry = sevenz_rust::SevenZArchiveEntry::new();
        entry.name = name.to_string();
        entry.has_stream = true;
        entry.size = size;
        entry
    }

    /// Writes a 7z with the given entries, all in one non-solid archive.
    fn make_archive(dir: &std::path::Path, files: &[(&str, &str)]) -> PathBuf {
        let archive_path = dir.join("archive.7z");
        let mut writer = SevenZWriter::new(std::fs::File::create(&archive_path).unwrap()).unwrap();
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

    /// Writes a solid 7z (several members sharing one folder block) from a
    /// temp source directory.
    fn make_solid_archive(dir: &std::path::Path, source_dir: &std::path::Path) -> PathBuf {
        let archive_path = dir.join("solid.7z");
        let mut writer = SevenZWriter::new(std::fs::File::create(&archive_path).unwrap()).unwrap();
        writer.push_source_path(source_dir, |_| true).unwrap();
        writer.finish().unwrap();
        archive_path
    }

    /// Writes a password-encrypted 7z (encrypted header + encrypted content).
    fn make_encrypted_archive(dir: &std::path::Path, contents: &str) -> PathBuf {
        let archive_path = dir.join("encrypted.7z");
        let mut writer = SevenZWriter::new(std::fs::File::create(&archive_path).unwrap()).unwrap();
        writer.set_content_methods(vec![
            sevenz_rust::AesEncoderOptions::new(sevenz_rust::Password::from("secret")).into(),
            SevenZMethod::LZMA2.into(),
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
        source.verify_all(&cancel, |evidence| {
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
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap();
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
        let path = make_archive(
            dir.path(),
            &[("b.bin", "bbb"), ("a.rom", "aaa"), ("c.bin", "ccc")],
        );
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap();
        assert_eq!(source.member_count(), 3);
        let first = collect(&mut source).unwrap();
        let names: Vec<_> = first.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["b.bin", "a.rom", "c.bin"]);

        let mut source2 =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap();
        let second = collect(&mut source2).unwrap();
        assert_eq!(
            first, second,
            "member order and evidence must be deterministic"
        );
    }

    #[test]
    fn solid_archive_within_budget_hashes_every_member() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        write_text(&src.join("one.bin"), "first member payload");
        write_text(&src.join("two.bin"), "second member payload");
        let path = make_solid_archive(dir.path(), &src);
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap();
        assert_eq!(source.member_count(), 2);
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().all(|e| e.is_verified()));
    }

    #[test]
    fn oversized_member_is_refused_and_verification_stops() {
        let dir = tempdir().unwrap();
        let big = "x".repeat(4096);
        let path = make_archive(dir.path(), &[("small.bin", "ok"), ("big.bin", &big)]);
        let trusted = trusted_for(dir.path());
        let limits = ArchiveLimits {
            max_member_logical_bytes: 1024,
            ..ArchiveLimits::default()
        };
        let mut source = SevenZArchiveSource::open(&path, &trusted, limits).unwrap();
        let evidence = collect(&mut source).unwrap();
        // Small member verified, then the oversized member refused, then stop.
        assert_eq!(evidence.len(), 2);
        assert!(evidence[0].is_verified());
        assert_eq!(
            evidence[1].status,
            ArchiveMemberStatus::RefusedLimits {
                reason: "member size"
            }
        );
    }

    #[test]
    fn total_decode_budget_is_refused() {
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
        let mut source = SevenZArchiveSource::open(&path, &trusted, limits).unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 2);
        assert!(evidence[0].is_verified());
        assert_eq!(
            evidence[1].status,
            ArchiveMemberStatus::RefusedLimits {
                reason: "total logical budget"
            }
        );
    }

    #[test]
    fn dictionary_size_helpers_expose_huge_declarations() {
        // The pure helpers are the mechanism for a sub-4 GiB pre-decode cap
        // once coder properties are readable (see module doc). Assert both
        // helpers return a value above the configured ceiling for a crafted
        // large declaration.
        let limits = ArchiveLimits::default();
        // LZMA2 dictionary bits 40 → 0xFFFFFFFF (4 GiB - 1).
        let lzma2 = lzma2_dictionary_size(&[40]).unwrap();
        assert!(u64::from(lzma2) > limits.max_dictionary_bytes);
        // LZMA dictionary 2 GiB.
        let mut props = vec![0_u8; 5];
        props[1..5].copy_from_slice(&2_147_483_648_u32.to_le_bytes());
        let lzma = lzma_dictionary_size(&props).unwrap();
        assert!(u64::from(lzma) > limits.max_dictionary_bytes);
    }

    #[test]
    fn dictionary_memory_error_maps_to_refusal() {
        use sevenz_rust::Error as E;
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
        use sevenz_rust::Error as E;
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
        // A tiny pack with a huge unpack: craft a folder-shaped declaration via
        // the public Archive surface (pack 1 byte, unpack 2 GiB).
        let mut archive = Archive::default();
        archive.pack_sizes.push(1);
        archive.stream_map.folder_first_pack_stream_index.push(0);
        // One file of 2 GiB in folder 0.
        let file = fixture_entry("bomb.bin", 2 * 1024 * 1024 * 1024);
        archive.files.push(file);
        archive.stream_map.file_folder_index.push(Some(0));
        let limits = ArchiveLimits {
            max_compression_ratio: 1000,
            ..ArchiveLimits::default()
        };
        let err = validate_archive_structure(&archive, &limits).unwrap_err();
        assert_eq!(
            err,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "compression ratio"
            }
        );
    }

    #[test]
    fn solid_decode_budget_is_refused() {
        // Two files sharing one folder with a total above the solid budget.
        let mut archive = Archive::default();
        // Pack large enough that the ratio check passes (6 GiB / 64 MiB = 96)
        // so the solid-budget check is the one that fires.
        archive.pack_sizes.push(64 * 1024 * 1024);
        archive.stream_map.folder_first_pack_stream_index.push(0);
        let size = 3 * 1024 * 1024 * 1024;
        for name in ["a.bin", "b.bin"] {
            let file = fixture_entry(name, size);
            archive.files.push(file);
            archive.stream_map.file_folder_index.push(Some(0));
        }
        let limits = ArchiveLimits {
            max_solid_decode_bytes: 2 * 1024 * 1024 * 1024,
            ..ArchiveLimits::default()
        };
        let err = validate_archive_structure(&archive, &limits).unwrap_err();
        assert_eq!(
            err,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "solid decode budget"
            }
        );
    }

    #[test]
    fn encrypted_member_is_refused_and_stops() {
        // For tiny archives the 7z header is stored plain even with
        // `encrypt_header` on (compressing it would not pay), so the archive
        // opens; the encryption surfaces at decode time as a per-member
        // refusal. The genuinely-encrypted-header path is covered by the
        // `PasswordRequired → Encrypted` mapping test below.
        let dir = tempdir().unwrap();
        let path = make_encrypted_archive(dir.path(), "secret payload");
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap();
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
        let err = SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap_err();
        assert!(matches!(err, ArchiveMemberSourceError::Corrupt { .. }));
    }

    #[test]
    fn multi_volume_name_is_refused_before_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("game.7z.001");
        let trusted = trusted_for(dir.path());
        let err = SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap_err();
        assert!(matches!(err, ArchiveMemberSourceError::Unsupported { .. }));
    }

    #[test]
    fn nested_archive_member_is_surfaced_but_not_opened() {
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("inner.zip", "zip bytes")]);
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap();
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
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap();
        let cancel = AtomicBool::new(true);
        let mut seen = 0;
        let result = source.verify_all(&cancel, |_evidence| {
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
            SevenZArchiveSource::open(&path, &trusted, ArchiveLimits::default()).unwrap();
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
        let err = SevenZArchiveSource::open(&link, &trusted, ArchiveLimits::default()).unwrap_err();
        assert!(
            matches!(err, ArchiveMemberSourceError::Open { .. }),
            "a symlink resolving outside the trusted roots must be refused"
        );
    }

    #[test]
    fn member_count_limit_is_refused() {
        let dir = tempdir().unwrap();
        let path = make_archive(dir.path(), &[("a.bin", "aaa"), ("b.bin", "bbb")]);
        let trusted = trusted_for(dir.path());
        let limits = ArchiveLimits {
            max_members: 1,
            ..ArchiveLimits::default()
        };
        let err = SevenZArchiveSource::open(&path, &trusted, limits).unwrap_err();
        assert_eq!(
            err,
            ArchiveMemberSourceError::RefusedLimits {
                reason: "member count"
            }
        );
    }

    #[test]
    fn empty_stream_member_is_surfaced() {
        let dir = tempdir().unwrap();
        let archive_path = dir.path().join("empty.7z");
        let mut writer = SevenZWriter::new(std::fs::File::create(&archive_path).unwrap()).unwrap();
        let entry = fixture_entry("empty.bin", 0);
        writer
            .push_archive_entry(entry, Some(std::io::Cursor::new(Vec::<u8>::new())))
            .unwrap();
        writer.finish().unwrap();
        let trusted = trusted_for(dir.path());
        let mut source =
            SevenZArchiveSource::open(&archive_path, &trusted, ArchiveLimits::default()).unwrap();
        let evidence = collect(&mut source).unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].status, ArchiveMemberStatus::EmptyFile);
    }
}

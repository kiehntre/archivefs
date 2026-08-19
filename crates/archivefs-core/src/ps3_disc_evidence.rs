//! Pure, read-only PS3 *folder-install* layout evidence and a bounded
//! `.pkg` header observer - the previous milestone's PS3 support
//! ([`crate::ps3_boot_evidence`]) only accepted already-read bytes from a
//! logical media reader (a disc image), which left every real folder-based
//! PS3 sample this project has (`PS3_GAME/USRDIR/EBOOT.BIN` directory
//! trees, `.pkg` PSN packages) unreachable. This module closes that gap.
//!
//! # Path-based, like [`crate::gamecube_wii_boot_evidence`]
//!
//! [`observe_ps3_directory`] performs real filesystem I/O (`fs::metadata`,
//! bounded `fs::read`/`File::read`) rather than taking an in-memory byte
//! slice - the same tradeoff already accepted and documented in
//! [`crate::gamecube_wii_boot_evidence`] for the same reason: a
//! `PS3_GAME/` tree is not a single file a caller could reasonably read
//! wholesale into memory first. Every read is bounded (see each function's
//! own documentation); nothing is ever written, renamed, moved, or
//! deleted.
//!
//! # Evidence emission is reused, not duplicated
//!
//! [`observe_ps3_directory`] builds a [`crate::ps3_boot_evidence::Ps3LayoutObservation`]
//! and hands it to the already-reviewed
//! [`crate::ps3_boot_evidence::observe_ps3_evidence`] - this module adds no
//! second, competing evidence-emission function for the same facts. Only
//! [`crate::ps3_boot_evidence::Ps3LayoutObservation`] has no field for
//! `PS3_DISC.SFB` or `.pkg`, so those two facts are represented as
//! additional [`ContentEvidence`] this module emits itself, alongside (not
//! instead of) the reused PS3_GAME evidence.
//!
//! # `PS3_DISC.SFB`: signature only, scope deliberately narrow
//!
//! The PS3 Developer wiki's `PS3_DISC.SFB` page (search-summarized;
//! `psdevwiki.com` itself blocks automated fetches, exactly as already
//! noted in [`crate::param_sfo`]'s own module documentation) describes a
//! `.SFB`-magic key/value table holding `TITLE_ID`/`HYBRID_FLAG`, but no
//! independently-corroborated source this research pass could reach gives
//! exact field byte offsets for that table (unlike `PARAM.SFO`, which two
//! independent sources agree on exactly). Rather than guess an offset
//! layout, this module - matching the previous milestone's Sega CD
//! precedent of the same kind - only checks the `.SFB` magic itself
//! ([`looks_like_ps3_disc_sfb`]); `TITLE_ID`/`HYBRID_FLAG` extraction is
//! explicitly deferred, not attempted.
//!
//! # `.pkg`: bounded fixed header only, corroborated by two sources
//!
//! [`parse_pkg_header`] reads exactly the fixed 0x80-byte PKG header -
//! magic, revision, type, metadata offset/count, header size, item count,
//! total size, data offset/size, and the 48-byte Content ID - cross-checked
//! against two independent, mutually-agreeing sources: the PS3 Developer
//! wiki's `PKG_files` page (search-summarized) and
//! `HACKERCHANNEL/PS3Py`'s `pkg.py` header struct
//! (`https://github.com/HACKERCHANNEL/PS3Py/blob/master/pkg.py`), a real
//! PS3 homebrew tool. This module never reads the metadata table (which
//! starts at a header-declared offset that could be anywhere in a
//! multi-gigabyte file), never decrypts anything, never requires a RAP
//! file or any key material, and never extracts package contents - the
//! Content ID is evidence, not proof of a playable disc title.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::param_sfo::{MAX_SFO_BYTES, parse_param_sfo};
use crate::ps3_boot_evidence::{
    Ps3LayoutObservation, check_eboot_self_magic, observe_ps3_evidence,
};

/// Bounded read for `EBOOT.BIN`'s SELF-magic check - only the leading bytes
/// are ever read, never the whole (often multi-megabyte) executable.
pub const MAX_EBOOT_HEADER_READ_BYTES: usize = 4096;

pub const PS3_DISC_SFB_MAGIC: &[u8; 4] = b".SFB";

/// What was observed about a PS3 folder install - never a platform
/// decision.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ps3DirectoryObservation {
    pub layout: Ps3LayoutObservation,
    /// Whether a `PS3_DISC.SFB` file was found (at the disc root, a
    /// sibling of `PS3_GAME/` - see [`locate_ps3_game_dir`]) and began with
    /// the `.SFB` magic. `None` when no such file was even looked for
    /// (impossible given [`observe_ps3_directory`] always checks once a
    /// `PS3_GAME/` directory is located) or found.
    pub disc_sfb_present: bool,
}

/// Resolves `root` to a `PS3_GAME` directory, supporting both shapes
/// section 9 asks for: `root` *is* `PS3_GAME` itself, or `root` is the
/// outer game folder containing a `PS3_GAME/` subdirectory. Returns `None`
/// for anything else - this never falls back to scanning `root` for
/// unrelated content.
pub fn locate_ps3_game_dir(root: &Path) -> Option<PathBuf> {
    let is_named_ps3_game = root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("PS3_GAME"));
    if is_named_ps3_game && root.is_dir() {
        return Some(root.to_path_buf());
    }
    let candidate = root.join("PS3_GAME");
    candidate.is_dir().then_some(candidate)
}

/// The disc-root directory relative to a located `PS3_GAME` directory:
/// `PS3_DISC.SFB` lives beside `PS3_GAME/`, not inside it. When `root`
/// already *was* `PS3_GAME`, this is a best-effort `root`'s parent (which
/// may not be the true disc root if the caller pointed directly at
/// `PS3_GAME` without its usual siblings - `PS3_DISC.SFB` simply will not
/// be found in that case, which is an honest, non-crashing outcome, not an
/// error).
fn disc_root_for(root: &Path, game_dir: &Path) -> Option<PathBuf> {
    if root == game_dir {
        root.parent().map(Path::to_path_buf)
    } else {
        Some(root.to_path_buf())
    }
}

pub fn looks_like_ps3_disc_sfb(header: &[u8]) -> bool {
    header.len() >= PS3_DISC_SFB_MAGIC.len()
        && &header[..PS3_DISC_SFB_MAGIC.len()] == PS3_DISC_SFB_MAGIC.as_slice()
}

/// Observes a PS3 folder install rooted at `root` (see
/// [`locate_ps3_game_dir`] for the accepted shapes). Every read is bounded:
/// `PARAM.SFO` is only read if its file size is within
/// [`crate::param_sfo::MAX_SFO_BYTES`]; `EBOOT.BIN` and `PS3_DISC.SFB` are
/// only ever read up to a small fixed prefix. A `root` that does not
/// resolve to a `PS3_GAME` directory yields an all-`false`/`None`
/// observation, never an error - this mirrors every other boot-evidence
/// observer in this crate ("absence of evidence" is a legitimate result).
pub fn observe_ps3_directory(root: &Path) -> Ps3DirectoryObservation {
    let mut observation = Ps3DirectoryObservation::default();
    let Some(game_dir) = locate_ps3_game_dir(root) else {
        return observation;
    };
    observation.layout.ps3_game_dir_present = true;

    let usrdir = game_dir.join("USRDIR");
    observation.layout.usrdir_present = usrdir.is_dir();

    let sfo_path = game_dir.join("PARAM.SFO");
    if let Ok(metadata) = fs::metadata(&sfo_path)
        && metadata.is_file()
        && metadata.len() <= MAX_SFO_BYTES as u64
        && let Ok(bytes) = fs::read(&sfo_path)
    {
        observation.layout.param_sfo = parse_param_sfo(&bytes);
    }

    let eboot_path = usrdir.join("EBOOT.BIN");
    if eboot_path.is_file() {
        observation.layout.eboot_bin_present = true;
        if let Ok(mut file) = fs::File::open(&eboot_path) {
            let mut header = vec![0u8; MAX_EBOOT_HEADER_READ_BYTES];
            if let Ok(read) = file.read(&mut header) {
                header.truncate(read);
                check_eboot_self_magic(&mut observation.layout, &header);
            }
        }
    }

    if let Some(disc_root) = disc_root_for(root, &game_dir) {
        let sfb_path = disc_root.join("PS3_DISC.SFB");
        if let Ok(mut file) = fs::File::open(&sfb_path) {
            let mut header = [0u8; 4];
            observation.disc_sfb_present =
                file.read_exact(&mut header).is_ok() && looks_like_ps3_disc_sfb(&header);
        }
    }

    observation
}

/// Neutral evidence for a [`Ps3DirectoryObservation`]: every fact
/// [`crate::ps3_boot_evidence::observe_ps3_evidence`] already produces for
/// `observation.layout`, plus (only this module's own facts)
/// `PS3_DISC.SFB` (`Corroborated` `BootStructure` - a conventional disc
/// marker, not unique proof of platform, matching the same confidence as
/// `PS3_GAME` itself).
pub fn observe_ps3_directory_evidence(
    observation: &Ps3DirectoryObservation,
) -> Vec<ContentEvidence> {
    let mut evidence = observe_ps3_evidence(&observation.layout);
    if observation.disc_sfb_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "PS3_DISC.SFB",
            ContentEvidenceConfidence::Corroborated,
            "PS3_DISC.SFB magic present - a conventional disc-root descriptor, not unique proof of platform; TITLE_ID/HYBRID_FLAG extraction deliberately deferred (see module docs)",
        ));
    }
    evidence
}

// ---------------------------------------------------------------------
// .pkg fixed header
// ---------------------------------------------------------------------

pub const PKG_MAGIC: &[u8; 4] = &[0x7f, b'P', b'K', b'G'];
pub const PKG_HEADER_BYTES: usize = 0x80;
const PKG_REVISION_OFFSET: usize = 0x04;
const PKG_TYPE_OFFSET: usize = 0x06;
const PKG_METADATA_OFFSET_OFFSET: usize = 0x08;
const PKG_METADATA_COUNT_OFFSET: usize = 0x0C;
const PKG_HEADER_SIZE_OFFSET: usize = 0x10;
const PKG_ITEM_COUNT_OFFSET: usize = 0x14;
const PKG_TOTAL_SIZE_OFFSET: usize = 0x18;
const PKG_DATA_OFFSET_OFFSET: usize = 0x20;
const PKG_DATA_SIZE_OFFSET: usize = 0x28;
const PKG_CONTENT_ID_OFFSET: usize = 0x30;
const PKG_CONTENT_ID_BYTES: usize = 0x30;

pub fn looks_like_pkg(header: &[u8]) -> bool {
    header.len() >= PKG_MAGIC.len() && &header[..PKG_MAGIC.len()] == PKG_MAGIC.as_slice()
}

/// What a parsed `.pkg` fixed header directly states - see the module
/// documentation for the exact, two-source-corroborated field layout and
/// why nothing past this fixed header is ever read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgHeaderFact {
    pub revision: u16,
    pub package_type: u16,
    pub metadata_offset: u32,
    pub metadata_count: u32,
    pub header_size: u32,
    pub item_count: u32,
    pub total_size: u64,
    pub data_offset: u64,
    pub data_size: u64,
    /// NUL-trimmed, lossily-decoded Content ID - a candidate identifier,
    /// not verified against a canonical release list.
    pub content_id: String,
}

/// Parses the fixed [`PKG_HEADER_BYTES`]-byte `.pkg` header from `header`.
/// `None` when the magic does not match or fewer than
/// [`PKG_HEADER_BYTES`] bytes were supplied - fails closed, never panics.
pub fn parse_pkg_header(header: &[u8]) -> Option<PkgHeaderFact> {
    if !looks_like_pkg(header) || header.len() < PKG_HEADER_BYTES {
        return None;
    }
    let u16_at = |offset: usize| u16::from_be_bytes(header[offset..offset + 2].try_into().unwrap());
    let u32_at = |offset: usize| u32::from_be_bytes(header[offset..offset + 4].try_into().unwrap());
    let u64_at = |offset: usize| u64::from_be_bytes(header[offset..offset + 8].try_into().unwrap());

    let content_id_bytes =
        &header[PKG_CONTENT_ID_OFFSET..PKG_CONTENT_ID_OFFSET + PKG_CONTENT_ID_BYTES];
    let content_id_end = content_id_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(content_id_bytes.len());
    let content_id = String::from_utf8_lossy(&content_id_bytes[..content_id_end]).into_owned();

    Some(PkgHeaderFact {
        revision: u16_at(PKG_REVISION_OFFSET),
        package_type: u16_at(PKG_TYPE_OFFSET),
        metadata_offset: u32_at(PKG_METADATA_OFFSET_OFFSET),
        metadata_count: u32_at(PKG_METADATA_COUNT_OFFSET),
        header_size: u32_at(PKG_HEADER_SIZE_OFFSET),
        item_count: u32_at(PKG_ITEM_COUNT_OFFSET),
        total_size: u64_at(PKG_TOTAL_SIZE_OFFSET),
        data_offset: u64_at(PKG_DATA_OFFSET_OFFSET),
        data_size: u64_at(PKG_DATA_SIZE_OFFSET),
        content_id,
    })
}

/// Neutral evidence for a parsed `.pkg` header: the magic itself (`Strong`
/// `ContentSignature`) and, when non-empty, the Content ID (`Corroborated`
/// `ProductCode`). Never a claim that the package is a complete, valid, or
/// installable title - only that its fixed header parsed.
pub fn pkg_header_evidence(fact: &PkgHeaderFact) -> Vec<ContentEvidence> {
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::ContentSignature,
        "PKG",
        ContentEvidenceConfidence::Strong,
        "PS3/PSN .pkg header magic present and the fixed header parsed",
    )];
    if !fact.content_id.is_empty() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            fact.content_id.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate Content ID read from the .pkg header - not verified against a canonical release list, and not proof the package is complete or installable",
        ));
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "archivefs-ps3-disc-evidence-test-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(path).unwrap().write_all(bytes).unwrap();
    }

    fn synthetic_param_sfo(key: &str, value: &str) -> Vec<u8> {
        // Minimal valid PARAM.SFO: header + one text index entry + key
        // table + data table.
        let key_bytes = format!("{key}\0").into_bytes();
        let value_bytes = format!("{value}\0").into_bytes();
        let key_table_start = 20 + 16u32;
        let data_table_start = key_table_start + key_bytes.len() as u32;
        let mut file = Vec::new();
        file.extend_from_slice(&[0x00, b'P', b'S', b'F']);
        file.extend_from_slice(&1u32.to_le_bytes());
        file.extend_from_slice(&key_table_start.to_le_bytes());
        file.extend_from_slice(&data_table_start.to_le_bytes());
        file.extend_from_slice(&1u32.to_le_bytes());
        // index entry
        file.extend_from_slice(&0u16.to_le_bytes()); // key_offset
        file.extend_from_slice(&0x0204u16.to_le_bytes()); // data_fmt: UTF-8 NUL-terminated
        file.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes()); // data_len
        file.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes()); // data_max_len
        file.extend_from_slice(&0u32.to_le_bytes()); // data_offset
        file.extend_from_slice(&key_bytes);
        file.extend_from_slice(&value_bytes);
        file
    }

    #[test]
    fn locate_ps3_game_dir_finds_subdirectory() {
        let root = temp_dir("locate-subdir");
        fs::create_dir_all(root.join("PS3_GAME")).unwrap();
        assert_eq!(locate_ps3_game_dir(&root), Some(root.join("PS3_GAME")));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn locate_ps3_game_dir_accepts_the_directory_itself() {
        let root = temp_dir("locate-itself");
        let game_dir = root.join("PS3_GAME");
        fs::create_dir_all(&game_dir).unwrap();
        assert_eq!(locate_ps3_game_dir(&game_dir), Some(game_dir.clone()));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn locate_ps3_game_dir_accepts_lowercase_name_when_pointed_at_it_directly() {
        // Real filesystems (ext4, etc.) are case-sensitive, so a
        // subdirectory scan cannot be case-insensitive without a directory
        // listing this function deliberately avoids - but a caller that
        // already knows the exact on-disk name (however it is cased) can
        // still point straight at it.
        let root = temp_dir("locate-case");
        let game_dir = root.join("ps3_game");
        fs::create_dir_all(&game_dir).unwrap();
        assert_eq!(locate_ps3_game_dir(&game_dir), Some(game_dir.clone()));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn locate_ps3_game_dir_returns_none_for_unrelated_folder() {
        let root = temp_dir("locate-none");
        fs::create_dir_all(root.join("NOT_PS3")).unwrap();
        assert_eq!(locate_ps3_game_dir(&root), None);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn observe_ps3_directory_reports_absent_layout_for_unrelated_folder() {
        let root = temp_dir("observe-absent");
        fs::create_dir_all(&root).unwrap();
        let observation = observe_ps3_directory(&root);
        assert_eq!(observation, Ps3DirectoryObservation::default());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn observe_ps3_directory_finds_full_layout() {
        let root = temp_dir("observe-full");
        let game_dir = root.join("PS3_GAME");
        write_file(
            &game_dir.join("PARAM.SFO"),
            &synthetic_param_sfo("TITLE_ID", "BLUS30000"),
        );
        write_file(
            &game_dir.join("USRDIR/EBOOT.BIN"),
            &[0x53, 0x43, 0x45, 0x00, 1, 2, 3],
        );

        let observation = observe_ps3_directory(&root);
        assert!(observation.layout.ps3_game_dir_present);
        assert!(observation.layout.usrdir_present);
        assert!(observation.layout.eboot_bin_present);
        assert_eq!(observation.layout.title_id(), Some("BLUS30000"));
        assert_eq!(observation.layout.eboot_self_magic_present, Some(true));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn observe_ps3_directory_works_when_pointed_directly_at_ps3_game() {
        let root = temp_dir("observe-direct");
        let game_dir = root.join("PS3_GAME");
        write_file(
            &game_dir.join("PARAM.SFO"),
            &synthetic_param_sfo("TITLE_ID", "BLES00001"),
        );

        let observation = observe_ps3_directory(&game_dir);
        assert!(observation.layout.ps3_game_dir_present);
        assert_eq!(observation.layout.title_id(), Some("BLES00001"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn observe_ps3_directory_finds_disc_sfb_at_disc_root() {
        let root = temp_dir("observe-sfb");
        fs::create_dir_all(root.join("PS3_GAME")).unwrap();
        write_file(&root.join("PS3_DISC.SFB"), b".SFB\x00\x01\x00\x00padding");

        let observation = observe_ps3_directory(&root);
        assert!(observation.disc_sfb_present);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn observe_ps3_directory_missing_disc_sfb_is_false_not_error() {
        let root = temp_dir("observe-no-sfb");
        fs::create_dir_all(root.join("PS3_GAME")).unwrap();

        let observation = observe_ps3_directory(&root);
        assert!(!observation.disc_sfb_present);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn observe_ps3_directory_eboot_bin_with_wrong_magic_is_observed_as_false() {
        let root = temp_dir("observe-wrong-magic");
        let game_dir = root.join("PS3_GAME");
        write_file(&game_dir.join("USRDIR/EBOOT.BIN"), b"not a self file");

        let observation = observe_ps3_directory(&root);
        assert_eq!(observation.layout.eboot_self_magic_present, Some(false));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn observe_ps3_directory_oversized_param_sfo_is_skipped_not_read() {
        let root = temp_dir("observe-oversized-sfo");
        let game_dir = root.join("PS3_GAME");
        let oversized = vec![0u8; MAX_SFO_BYTES + 1];
        write_file(&game_dir.join("PARAM.SFO"), &oversized);

        let observation = observe_ps3_directory(&root);
        assert!(observation.layout.ps3_game_dir_present);
        assert_eq!(observation.layout.param_sfo, None);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn evidence_includes_reused_ps3_game_facts_and_disc_sfb() {
        let observation = Ps3DirectoryObservation {
            layout: Ps3LayoutObservation {
                ps3_game_dir_present: true,
                ..Default::default()
            },
            disc_sfb_present: true,
        };
        let evidence = observe_ps3_directory_evidence(&observation);
        assert!(evidence.iter().any(|item| item.value == "PS3_GAME"));
        assert!(evidence.iter().any(|item| item.value == "PS3_DISC.SFB"));
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let observation = Ps3DirectoryObservation {
            layout: Ps3LayoutObservation {
                ps3_game_dir_present: true,
                ..Default::default()
            },
            disc_sfb_present: true,
        };
        for item in observe_ps3_directory_evidence(&observation) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure
                    | ContentEvidenceKind::ProductCode
                    | ContentEvidenceKind::ContentSignature
            ));
        }
    }

    // ------------------------------------------------------------------
    // .pkg header
    // ------------------------------------------------------------------

    fn synthetic_pkg_header(content_id: &str) -> Vec<u8> {
        let mut header = vec![0u8; PKG_HEADER_BYTES];
        header[0..4].copy_from_slice(PKG_MAGIC.as_slice());
        header[PKG_REVISION_OFFSET..PKG_REVISION_OFFSET + 2]
            .copy_from_slice(&0x8000u16.to_be_bytes());
        header[PKG_TYPE_OFFSET..PKG_TYPE_OFFSET + 2].copy_from_slice(&0x0001u16.to_be_bytes());
        header[PKG_ITEM_COUNT_OFFSET..PKG_ITEM_COUNT_OFFSET + 4]
            .copy_from_slice(&5u32.to_be_bytes());
        header[PKG_TOTAL_SIZE_OFFSET..PKG_TOTAL_SIZE_OFFSET + 8]
            .copy_from_slice(&123456u64.to_be_bytes());
        let id_bytes = content_id.as_bytes();
        header[PKG_CONTENT_ID_OFFSET..PKG_CONTENT_ID_OFFSET + id_bytes.len()]
            .copy_from_slice(id_bytes);
        header
    }

    #[test]
    fn pkg_magic_is_detected() {
        let header = synthetic_pkg_header("UP0001-TEST00000_00-0000000000000000");
        assert!(looks_like_pkg(&header));
    }

    #[test]
    fn non_pkg_header_is_not_detected() {
        assert!(!looks_like_pkg(b"not a pkg"));
    }

    #[test]
    fn pkg_header_fields_are_parsed() {
        let header = synthetic_pkg_header("UP0001-TEST00000_00-0000000000000000");
        let fact = parse_pkg_header(&header).unwrap();
        assert_eq!(fact.revision, 0x8000);
        assert_eq!(fact.package_type, 0x0001);
        assert_eq!(fact.item_count, 5);
        assert_eq!(fact.total_size, 123456);
        assert_eq!(fact.content_id, "UP0001-TEST00000_00-0000000000000000");
    }

    #[test]
    fn pkg_header_too_short_fails_closed() {
        let mut header = synthetic_pkg_header("X");
        header.truncate(PKG_HEADER_BYTES - 1);
        assert_eq!(parse_pkg_header(&header), None);
    }

    #[test]
    fn pkg_header_wrong_magic_fails_closed() {
        assert_eq!(parse_pkg_header(&[0u8; PKG_HEADER_BYTES]), None);
    }

    #[test]
    fn pkg_header_evidence_includes_signature_and_product_code() {
        let header = synthetic_pkg_header("UP0001-TEST00000_00-0000000000000000");
        let fact = parse_pkg_header(&header).unwrap();
        let evidence = pkg_header_evidence(&fact);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature
                    && item.value == "PKG")
        );
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "UP0001-TEST00000_00-0000000000000000");
        assert_eq!(product.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn pkg_header_empty_content_id_yields_no_product_code() {
        let header = synthetic_pkg_header("");
        let fact = parse_pkg_header(&header).unwrap();
        let evidence = pkg_header_evidence(&fact);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode)
        );
    }

    #[test]
    fn pkg_evidence_never_assigns_a_platform() {
        let header = synthetic_pkg_header("SOME-ID");
        let fact = parse_pkg_header(&header).unwrap();
        for item in pkg_header_evidence(&fact) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::ContentSignature | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn ps3_disc_sfb_magic_is_detected() {
        assert!(looks_like_ps3_disc_sfb(b".SFB\x00\x00"));
        assert!(!looks_like_ps3_disc_sfb(b"not sfb"));
    }
}

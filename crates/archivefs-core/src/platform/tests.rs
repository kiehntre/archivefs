//! Table-driven tests for the platform registry and detector.
//!
//! Every test in here is offline and, apart from the explicitly named
//! temp-tree tests, touches no filesystem at all.

use super::detect::*;
use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const ROOT: &str = "/roms";

/// A throwaway directory. Real trees are needed to prove the bounded reads and
/// the read-only guarantee; nothing outside one is ever written.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-platform-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temp tree");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn file(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture");
        }
        fs::write(&path, contents).expect("fixture");
        path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Detection from path text alone - no content reads.
fn detect(path: &str) -> PlatformDetectionReport {
    detect_platform_report(&DetectionRequest::new(Path::new(path), Path::new(ROOT)))
}

/// The platform a folder name resolves to, via a file placed inside it.
fn platform_for_folder(folder: &str) -> Option<&'static str> {
    detect(&format!("{ROOT}/{folder}/game.rom")).platform
}

fn snapshot_tree(root: &Path) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&current) else {
            continue;
        };
        for entry in read_dir.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                stack.push(path.clone());
            }
            entries.insert(
                path.to_string_lossy().into_owned(),
                format!(
                    "{:?}|{}|{:?}",
                    metadata.file_type(),
                    metadata.len(),
                    metadata.modified().ok()
                ),
            );
        }
    }
    entries
}

// --- 1. Every requested platform and its common folder aliases ------------

/// Test 1
#[test]
fn every_platform_the_milestone_requested_is_in_the_registry() {
    // The exact list from the milestone, by canonical identifier.
    for id in [
        "ZX Spectrum",
        "BBC Micro",
        "Acorn Electron",
        "Amstrad CPC",
        "Amiga",
        "AmigaCD32",
        "Commodore 64",
        "Commodore 128",
        "VIC-20",
        "Atari 8-bit",
        "AtariST",
        "Atari Jaguar",
        "Atari Lynx",
        "MegaDrive",
        "Sega CD",
        "Sega 32X",
        "MasterSystem",
        "GameGear",
        "Philips CD-i",
        "NeoGeo",
        "Neo Geo CD",
        "MSX",
        "MSX2",
        "PC Engine",
        "TurboGrafx-16",
        "PC Engine CD",
        "ColecoVision",
        "Intellivision",
        "Vectrex",
        "DOS",
        "ScummVM",
        "Sharp X68000",
        "FM Towns",
        "PC-98",
        "Apple II",
        "Macintosh",
        "Acorn Archimedes",
        "3DO",
        "Commodore CDTV",
    ] {
        let platform = platform_by_id(id).unwrap_or_else(|| panic!("{id} is missing"));
        assert!(
            !platform.display_name.is_empty(),
            "{id} has no display name"
        );
        assert!(
            platform.explanation.len() > 40,
            "{id} needs a real explanation of what evidence exists for it"
        );
        assert!(
            !platform.folder_aliases.is_empty(),
            "{id} has no folder alias, so nothing could ever detect it"
        );
    }
}

/// Test 2
#[test]
fn the_folder_aliases_the_milestone_listed_all_resolve() {
    let cases: &[(&str, &str)] = &[
        ("spectrum", "ZX Spectrum"),
        ("zx-spectrum", "ZX Spectrum"),
        ("zx_spectrum", "ZX Spectrum"),
        ("zxspectrum", "ZX Spectrum"),
        ("speccy", "ZX Spectrum"),
        ("bbc", "BBC Micro"),
        ("bbcmicro", "BBC Micro"),
        ("bbc-micro", "BBC Micro"),
        ("bbc model b", "BBC Micro"),
        ("electron", "Acorn Electron"),
        ("acorn-electron", "Acorn Electron"),
        ("cd32", "AmigaCD32"),
        ("amiga-cd32", "AmigaCD32"),
        ("amigacd32", "AmigaCD32"),
        ("megacd", "Sega CD"),
        ("mega-cd", "Sega CD"),
        ("sega-cd", "Sega CD"),
        ("segacd", "Sega CD"),
        ("amstrad", "Amstrad CPC"),
        ("amstrad-cpc", "Amstrad CPC"),
        ("cpc", "Amstrad CPC"),
        ("cdi", "Philips CD-i"),
        ("cd-i", "Philips CD-i"),
        ("philips-cdi", "Philips CD-i"),
        ("philips cd-i", "Philips CD-i"),
    ];
    for (folder, expected) in cases {
        assert_eq!(
            platform_for_folder(folder),
            Some(*expected),
            "folder `{folder}` should resolve to {expected}"
        );
    }
}

/// Test 3
#[test]
fn case_punctuation_spaces_underscores_and_hyphens_all_fold_together() {
    for spelling in [
        "ZX Spectrum",
        "zx spectrum",
        "ZX-SPECTRUM",
        "Zx_Spectrum",
        "zx.spectrum",
        "  zx   spectrum  ",
        "ZXSpectrum",
    ] {
        assert_eq!(
            platform_for_folder(spelling),
            Some("ZX Spectrum"),
            "`{spelling}` should normalise to the same alias"
        );
    }
}

// --- 3. Exact component matching, never substring -------------------------

/// Test 4
#[test]
fn an_alias_never_matches_a_substring_of_a_longer_folder_name() {
    // Every one of these folders exists in a real library and contains the
    // text of a shorter alias. None may match it.
    for folder in [
        "zx-spectrum-next",
        "segacd32",
        "amiga-cd",
        "atari-jaguar-cd",
        "amstrad-gx4000",
        "amstrad-pcw",
        "neo-geo-x",
        "intellivision-amico",
        "apple-lisa",
        "apple-pippin",
        "colecoadam",
        "sharp-zaurus",
        "philips-vg-5000",
        "msx-turbo",
    ] {
        assert_eq!(
            platform_for_folder(folder),
            None,
            "`{folder}` is different hardware and must not match a shorter alias"
        );
    }
}

/// Test 5
#[test]
fn a_broad_alias_never_matches_an_unrelated_path_or_a_filename() {
    // The four aliases the milestone singled out as dangerous.
    for path in [
        "/roms/my bbc recordings/thing.rom",
        "/roms/cpcs-collection/thing.rom",
        "/roms/mac and cheese/thing.rom",
        "/roms/dos-not-a-platform-really/thing.rom",
    ] {
        let report =
            detect_platform_report(&DetectionRequest::new(Path::new(path), Path::new(ROOT)));
        assert!(
            !matches!(report.deciding_source, Some(DetectionSource::FolderAlias)),
            "`{path}` matched a folder alias by substring accident"
        );
    }
    // A file *named* after a platform is not in a platform folder.
    assert_eq!(detect("/roms/unsorted/bbc.rom").platform, None);
    assert_eq!(detect("/roms/unsorted/dos.zip").platform, None);
}

/// Test 6
#[test]
fn nothing_above_the_configured_source_root_participates() {
    let report = detect_platform_report(&DetectionRequest::new(
        Path::new("/roms/megadrive/unsorted/game.rom"),
        Path::new("/roms/megadrive/unsorted"),
    ));
    assert_eq!(
        report.platform, None,
        "a platform folder above the source root must not leak into detection"
    );
}

// --- 4. Existing systems unchanged ---------------------------------------

/// Test 7
#[test]
fn every_previously_supported_platform_and_alias_still_resolves() {
    // The complete alias set this build shipped before the registry existed.
    let cases: &[(&str, &str)] = &[
        ("msx", "MSX"),
        ("msx1", "MSX"),
        ("msx2", "MSX2"),
        ("neogeo", "NeoGeo"),
        ("neogeoaes", "NeoGeo"),
        ("neogeomvs", "NeoGeo"),
        ("neogeo64", "NeoGeo64"),
        ("ngage", "NGage"),
        ("intellivision", "Intellivision"),
        ("amiga", "Amiga"),
        ("commodoreamiga", "Amiga"),
        ("atarist", "AtariST"),
        ("atari2600", "Atari2600"),
        ("a2600", "Atari2600"),
        ("atarivcs", "Atari2600"),
        ("atari5200", "Atari5200"),
        ("atari7800", "Atari7800"),
        ("nes", "NES"),
        ("famicom", "NES"),
        ("snes", "SNES"),
        ("superfamicom", "SNES"),
        ("n64", "N64"),
        ("gamecube", "GameCube"),
        ("gcn", "GameCube"),
        ("gc", "GameCube"),
        ("ngc", "GameCube"),
        ("wii", "Wii"),
        ("wiiu", "WiiU"),
        ("switch", "Switch"),
        ("megadrive", "MegaDrive"),
        ("genesis", "MegaDrive"),
        ("smd", "MegaDrive"),
        ("mastersystem", "MasterSystem"),
        ("sms", "MasterSystem"),
        ("gamegear", "GameGear"),
        ("saturn", "Saturn"),
        ("dreamcast", "Dreamcast"),
        ("psx", "PSX"),
        ("ps1", "PSX"),
        ("playstation", "PSX"),
        ("ps2", "PS2"),
        ("ps3", "PS3"),
        ("psp", "PSP"),
        ("xbox", "Xbox"),
        ("xbox360", "Xbox360"),
        ("x360", "Xbox360"),
        ("arcade", "Arcade"),
        ("mame", "Arcade"),
        ("fbneo", "Arcade"),
        ("dos", "DOS"),
        ("msdos", "DOS"),
        ("scummvm", "ScummVM"),
        ("archimedes", "Acorn Archimedes"),
        ("riscos", "Acorn Archimedes"),
        ("pc", "PC"),
        ("windows", "PC"),
        ("gameboy", "Game Boy"),
        ("gb", "Game Boy"),
        ("gbc", "Game Boy Color"),
        ("gba", "Game Boy Advance"),
        ("nds", "Nintendo DS"),
        ("ds", "Nintendo DS"),
        ("c64", "Commodore 64"),
        ("zxs", "ZX Spectrum"),
        ("32x", "Sega 32X"),
        ("pcengine", "PC Engine"),
        ("turbografx16", "TurboGrafx-16"),
        ("tg16", "TurboGrafx-16"),
        ("lynx", "Atari Lynx"),
        ("jaguar", "Atari Jaguar"),
        ("ngp", "Neo Geo Pocket"),
        ("ngpc", "Neo Geo Pocket Color"),
        ("wonderswan", "WonderSwan"),
        ("wsc", "WonderSwan Color"),
        ("3do", "3DO"),
        ("vita", "PlayStation Vita"),
        ("colecovision", "ColecoVision"),
        ("coleco", "ColecoVision"),
        ("vectrex", "Vectrex"),
        ("virtualboy", "Virtual Boy"),
        ("vb", "Virtual Boy"),
        ("nintendo3ds", "Nintendo 3DS"),
        ("x68000", "Sharp X68000"),
        ("x68k", "Sharp X68000"),
        ("pc8801", "NEC PC-8801"),
        ("pc9801", "NEC PC-9801"),
        ("pcenginecd", "PC Engine CD"),
        ("tgcd", "PC Engine CD"),
        ("c128", "Commodore 128"),
        ("vic20", "VIC-20"),
        // libretro-database directory spellings.
        ("nintendonintendoentertainmentsystem", "NES"),
        ("segamegacdsegacd", "Sega CD"),
        ("snkneogeopocket", "Neo Geo Pocket"),
        ("bandaiwonderswancolor", "WonderSwan Color"),
        ("gcevectrex", "Vectrex"),
        ("microsoftmsx2", "MSX2"),
        ("atarilynxlynx", "Atari Lynx"),
        ("necpcengineturbografx16", "PC Engine"),
    ];
    for (alias, expected) in cases {
        assert_eq!(
            platform_for_folder(alias),
            Some(*expected),
            "previously supported alias `{alias}` regressed"
        );
    }
}

/// Test 8
#[test]
fn no_alias_is_claimed_by_two_platforms() {
    let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
    for platform in PLATFORMS {
        for alias in platform.folder_aliases {
            if let Some(existing) = owner.insert(alias, platform.id) {
                panic!(
                    "alias `{alias}` is claimed by both `{existing}` and `{}`",
                    platform.id
                );
            }
        }
    }
    assert!(
        owner.len() > 300,
        "the registry lost aliases: {}",
        owner.len()
    );
}

/// Test 9
#[test]
fn every_alias_is_already_normalised_so_lookup_can_be_an_exact_match() {
    for platform in PLATFORMS {
        for alias in platform.folder_aliases {
            assert_eq!(
                &normalize_alias(alias),
                alias,
                "alias `{alias}` on {} is not stored normalised",
                platform.id
            );
        }
    }
}

/// Test 10
#[test]
fn canonical_identifiers_are_unique_and_stable() {
    let ids = canonical_ids();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        ids, sorted,
        "canonical identifiers must be sorted and unique"
    );
    assert_eq!(
        ids.len(),
        PLATFORMS.len(),
        "two platforms share an identifier"
    );
}

// --- 5, 6. Inheritance and manual assignment ------------------------------

/// Test 11
#[test]
fn a_trusted_parent_platform_is_inherited_by_a_child_resource_file() {
    let path = Path::new("/roms/unsorted/laurabow2/RESOURCE.GEN");
    let report = detect_platform_report(
        &DetectionRequest::new(path, Path::new(ROOT)).with_trusted_platform(Some("ScummVM")),
    );
    assert_eq!(report.platform, Some("ScummVM"));
    assert_eq!(
        report.deciding_source,
        Some(DetectionSource::TrustedMetadata)
    );
}

/// Test 12
#[test]
fn a_weak_extension_never_replaces_a_trusted_parent_platform() {
    // `.bin` would otherwise pull in dozens of candidates.
    let report = detect_platform_report(
        &DetectionRequest::new(Path::new("/roms/unsorted/game/DATA.BIN"), Path::new(ROOT))
            .with_trusted_platform(Some("ScummVM")),
    );
    assert_eq!(report.platform, Some("ScummVM"));
    assert!(
        report
            .evidence
            .iter()
            .any(|item| item.source == DetectionSource::SharedExtension),
        "the weaker evidence should still be recorded for display"
    );
    assert_eq!(
        report.deciding_source,
        Some(DetectionSource::TrustedMetadata),
        "a shared extension must never outrank trusted parent metadata"
    );
}

/// Test 13
#[test]
fn a_manual_assignment_always_wins_and_is_labelled_as_manual() {
    // Every competing signal at once: a contradicting folder, a strong
    // extension for another platform, and trusted metadata.
    let report = detect_platform_report(
        &DetectionRequest::new(Path::new("/roms/megadrive/game.sfc"), Path::new(ROOT))
            .with_trusted_platform(Some("MegaDrive"))
            .with_manual_platform(Some("ZX Spectrum")),
    );
    assert_eq!(report.platform, Some("ZX Spectrum"));
    assert_eq!(report.confidence, DetectionConfidence::Confirmed);
    assert!(report.manually_assigned);
    assert!(
        !report.requires_confirmation,
        "a person already confirmed this one"
    );
    assert_eq!(
        report.deciding_source,
        Some(DetectionSource::ExplicitAssignment)
    );
}

/// Test 14
#[test]
fn a_manual_assignment_is_accepted_under_any_spelling_but_stored_canonically() {
    for spelling in ["zx-spectrum", "ZX Spectrum", "speccy", "zxspectrum"] {
        let report = detect_platform_report(
            &DetectionRequest::new(Path::new("/roms/unsorted/game.bin"), Path::new(ROOT))
                .with_manual_platform(Some(spelling)),
        );
        assert_eq!(
            report.platform,
            Some("ZX Spectrum"),
            "`{spelling}` should resolve to the canonical identifier"
        );
    }
}

/// Test 15
#[test]
fn an_unrecognised_manual_hint_is_refused_rather_than_invented() {
    let report = detect_platform_report(
        &DetectionRequest::new(Path::new("/roms/unsorted/game.bin"), Path::new(ROOT))
            .with_manual_platform(Some("Nintendo Playstation 5")),
    );
    assert!(
        !report.manually_assigned,
        "a hint the registry does not know must not become a platform"
    );
}

// --- 7, 8, 9, 10. Extension ranking, ambiguity and unknown ----------------

/// Test 16
#[test]
fn every_shared_extension_stays_ambiguous_on_its_own() {
    for extension in SHARED_EXTENSIONS {
        let report = detect(&format!("{ROOT}/unsorted/game.{extension}"));
        assert_eq!(
            report.platform, None,
            "`.{extension}` selected a platform on its own"
        );
        assert!(
            matches!(
                report.confidence,
                DetectionConfidence::Ambiguous | DetectionConfidence::Unknown
            ),
            "`.{extension}` should never be Probable or Confirmed alone"
        );
        assert!(report.requires_confirmation);
    }
}

/// Test 17
#[test]
fn a_bin_file_is_never_assumed_to_be_mega_drive() {
    let report = detect(&format!("{ROOT}/unsorted/game.bin"));
    assert_ne!(report.platform, Some("MegaDrive"));
    assert!(
        report
            .candidates
            .iter()
            .any(|candidate| candidate.platform == "MegaDrive"),
        "Mega Drive should be offered as one candidate among many"
    );
    assert!(
        report.candidates.len() > 5,
        "a `.bin` has many possible homes"
    );
}

/// Test 18
#[test]
fn an_iso_file_is_never_assumed_to_be_playstation() {
    let report = detect(&format!("{ROOT}/unsorted/game.iso"));
    assert_ne!(report.platform, Some("PSX"));
    let names: Vec<&str> = report
        .candidates
        .iter()
        .map(|candidate| candidate.platform)
        .collect();
    // Exactly the ambiguity the milestone's example names.
    for expected in ["Sega CD", "PSX", "PC Engine CD", "Philips CD-i"] {
        assert!(
            names.contains(&expected),
            "{expected} should be a candidate"
        );
    }
}

/// Test 19
#[test]
fn a_dsk_file_stays_ambiguous_across_every_system_that_uses_one() {
    let report = detect(&format!("{ROOT}/unsorted/game.dsk"));
    assert_eq!(report.platform, None);
    let names: Vec<&str> = report
        .candidates
        .iter()
        .map(|candidate| candidate.platform)
        .collect();
    for expected in [
        "Amstrad CPC",
        "BBC Micro",
        "Acorn Electron",
        "AtariST",
        "Apple II",
        "PC-98",
    ] {
        assert!(names.contains(&expected), "{expected} uses `.dsk` too");
    }
}

/// Test 20
#[test]
fn a_tap_file_stays_ambiguous_between_the_tape_systems() {
    let names: Vec<&str> = detect(&format!("{ROOT}/unsorted/game.tap"))
        .candidates
        .iter()
        .map(|candidate| candidate.platform)
        .collect();
    assert!(names.contains(&"ZX Spectrum"));
    assert!(names.contains(&"Commodore 64"));
}

/// Test 21
#[test]
fn a_zip_file_never_identifies_a_platform_by_itself() {
    let report = detect(&format!("{ROOT}/unsorted/game.zip"));
    assert_eq!(report.platform, None);
    assert!(
        report.candidates.len() > 10,
        "`.zip` is a container, not a platform"
    );
}

/// Test 22
#[test]
fn a_strong_extension_carries_a_detection_on_its_own() {
    for (extension, expected) in [
        ("sfc", "SNES"),
        ("z64", "N64"),
        ("gba", "Game Boy Advance"),
        ("col", "ColecoVision"),
        ("vec", "Vectrex"),
        ("cdt", "Amstrad CPC"),
        ("atr", "Atari 8-bit"),
        ("d64", "Commodore 64"),
        ("woz", "Apple II"),
        ("xdf", "Sharp X68000"),
        ("j64", "Atari Jaguar"),
    ] {
        let report = detect(&format!("{ROOT}/unsorted/game.{extension}"));
        assert_eq!(
            report.platform,
            Some(expected),
            "`.{extension}` should identify {expected}"
        );
        assert_eq!(
            report.confidence,
            DetectionConfidence::Probable,
            "an extension is good evidence but not proof"
        );
    }
}

/// Test 23
#[test]
fn a_folder_alias_defeats_a_contradicting_weak_extension() {
    let report = detect(&format!("{ROOT}/zx-spectrum/game.bin"));
    assert_eq!(report.platform, Some("ZX Spectrum"));
    assert_eq!(report.deciding_source, Some(DetectionSource::FolderAlias));
}

/// Test 24
#[test]
fn two_genuinely_different_platforms_sharing_evidence_return_ambiguous() {
    // `TMR SEGA` is the 8-bit Sega ROM header, shared by the Master System and
    // the Game Gear. They are different machines, so a header-only match must
    // not choose one.
    let tree = TempTree::new("ambiguous-signature");
    let mut rom = vec![0_u8; 0x8000];
    rom[0x7ff0..0x7ff8].copy_from_slice(b"TMR SEGA");
    let target = tree.file("unsorted/mystery.rom", &rom);
    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    assert_eq!(report.confidence, DetectionConfidence::Ambiguous);
    assert_eq!(report.platform, None);
    let reason = report
        .ambiguity_reason
        .expect("ambiguity must be explained");
    assert!(
        reason.contains("MasterSystem") && reason.contains("GameGear"),
        "both machines must be named: {reason}"
    );
}

/// Test 24b
#[test]
fn equivalent_identifiers_sharing_evidence_resolve_deterministically() {
    // `.pce` is strong for both `PC Engine` and `TurboGrafx-16`, which are one
    // console under two stored identifiers. That is a naming difference, not a
    // real ambiguity, so it must resolve rather than block - deterministically,
    // and with both identifiers still visible in the evidence.
    let report = detect(&format!("{ROOT}/unsorted/game.pce"));
    assert_eq!(report.platform, Some("PC Engine"));
    assert_eq!(report.confidence, DetectionConfidence::Probable);
    let pointed: Vec<&str> = report.evidence.iter().map(|item| item.platform).collect();
    assert!(pointed.contains(&"PC Engine") && pointed.contains(&"TurboGrafx-16"));
    for _ in 0..8 {
        assert_eq!(
            detect(&format!("{ROOT}/unsorted/game.pce")).platform,
            Some("PC Engine"),
            "the choice between equivalent identifiers must be stable"
        );
    }
}

/// Test 25
#[test]
fn unknown_remains_a_valid_result_with_a_stated_reason() {
    let report = detect(&format!("{ROOT}/unsorted/notes.txt"));
    assert_eq!(report.confidence, DetectionConfidence::Unknown);
    assert_eq!(report.platform, None);
    assert!(report.candidates.is_empty());
    assert!(
        report
            .ambiguity_reason
            .as_deref()
            .expect("unknown must say why")
            .contains("no folder, signature, layout or extension evidence"),
        "a person must be told why detection failed"
    );
    assert_eq!(report.summary(), "Platform unknown");
}

// --- 11, 12, 13, 14. ScummVM and Mega Drive ------------------------------

/// Test 26 - the exact reported misclassification.
#[test]
fn scummvm_resource_gen_is_never_classified_as_mega_drive() {
    let report = detect("/roms/scummvm/laurabow2/RESOURCE.GEN");
    assert_ne!(
        report.platform,
        Some("MegaDrive"),
        "this is the reported defect: a Sierra SCI resource file labelled as a Mega Drive ROM"
    );
    assert_eq!(report.platform, Some("ScummVM"));
    assert_eq!(report.deciding_source, Some(DetectionSource::FolderAlias));
}

/// Test 27
#[test]
fn a_gen_file_alone_is_not_a_mega_drive_rom() {
    // Without a folder saying otherwise, `.gen` is shared evidence at best.
    let report = detect("/roms/unsorted/RESOURCE.GEN");
    assert_ne!(report.platform, Some("MegaDrive"));
    assert!(
        crate::archive_kind(Path::new("/roms/unsorted/RESOURCE.GEN")).is_none(),
        "`.gen` must not be classified as a Mega Drive ROM from its extension"
    );
}

/// Test 28
#[test]
fn a_scummvm_child_resource_inherits_scummvm_rather_than_its_extension() {
    for name in [
        "RESOURCE.GEN",
        "RESOURCE.MAP",
        "RESOURCE.000",
        "RESOURCE.AUD",
        "DATA.BIN",
        "100.SCR",
    ] {
        let report = detect_platform_report(
            &DetectionRequest::new(
                Path::new(&format!("/roms/unsorted/laurabow2/{name}")),
                Path::new(ROOT),
            )
            .with_trusted_platform(Some("ScummVM")),
        );
        assert_eq!(
            report.platform,
            Some("ScummVM"),
            "`{name}` should keep its parent's ScummVM context"
        );
    }
}

/// Test 29
#[test]
fn a_scummvm_directory_layout_identifies_scummvm_from_its_files() {
    let tree = TempTree::new("scummvm-layout");
    // A real Sierra SCI game directory, in a folder whose name says nothing.
    let target = tree.file("unsorted/laurabow2/RESOURCE.GEN", b"not a rom");
    tree.file("unsorted/laurabow2/RESOURCE.MAP", b"map");
    tree.file("unsorted/laurabow2/RESOURCE.000", b"volume");

    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    assert_eq!(
        report.platform,
        Some("ScummVM"),
        "the directory layout alone should identify ScummVM: {:?}",
        report.evidence
    );
    assert_eq!(report.deciding_source, Some(DetectionSource::Layout));
}

/// Test 30
#[test]
fn a_real_mega_drive_rom_is_still_detected_from_its_cartridge_header() {
    let tree = TempTree::new("megadrive-header");
    // A Mega Drive header: `SEGA` at 0x100, in a folder that says nothing.
    let mut rom = vec![0_u8; 0x200];
    rom[0x100..0x104].copy_from_slice(b"SEGA");
    let target = tree.file("unsorted/mystery.bin", &rom);

    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    assert_eq!(report.platform, Some("MegaDrive"));
    assert_eq!(report.confidence, DetectionConfidence::Confirmed);
    assert_eq!(report.deciding_source, Some(DetectionSource::Signature));
}

/// Test 31
#[test]
fn a_mega_drive_rom_is_still_detected_from_its_folder_and_extension() {
    for path in [
        "/roms/genesis/Sonic (USA).md",
        "/roms/megadrive/Sonic (USA).bin",
        "/roms/sega-mega-drive/Sonic (USA).gen",
    ] {
        assert_eq!(
            detect(path).platform,
            Some("MegaDrive"),
            "{path} should still be recognised"
        );
    }
    // And `.smd` needs no corroboration at all.
    assert_eq!(
        crate::archive_kind(Path::new("/roms/unsorted/Sonic.smd")),
        Some(crate::ArchiveKind::MegaDriveRom)
    );
}

/// Test 32
#[test]
fn a_gen_file_inside_a_mega_drive_folder_is_still_a_mega_drive_rom() {
    assert_eq!(
        crate::archive_kind_in_root(Path::new("/roms/megadrive/Sonic.gen"), Path::new("/roms")),
        Some(crate::ArchiveKind::MegaDriveRom),
        "corroborated `.gen` must still work"
    );
    assert_eq!(
        crate::archive_kind_in_root(
            Path::new("/roms/scummvm/laurabow2/RESOURCE.GEN"),
            Path::new("/roms")
        ),
        None,
        "a ScummVM folder positively contradicts the Mega Drive reading"
    );
}

/// Test 33
#[test]
fn mega_cd_and_mega_drive_stay_separate_canonical_platforms() {
    assert_ne!(
        platform_by_id("Sega CD").map(|p| p.id),
        platform_by_id("MegaDrive").map(|p| p.id)
    );
    assert_eq!(platform_for_folder("megacd"), Some("Sega CD"));
    assert_eq!(platform_for_folder("megadrive"), Some("MegaDrive"));
    assert!(
        platform_by_id("Sega CD")
            .expect("registered")
            .conflicts_with
            .contains(&"MegaDrive"),
        "the two are confusable and should say so"
    );
}

/// Test 34
#[test]
fn a_mega_cd_image_is_identified_by_its_boot_signature_not_its_extension() {
    let tree = TempTree::new("megacd-header");
    let mut image = vec![0_u8; 0x40];
    image[0..14].copy_from_slice(b"SEGADISCSYSTEM");
    let target = tree.file("unsorted/disc.bin", &image);

    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    assert_eq!(report.platform, Some("Sega CD"));
    assert_eq!(report.confidence, DetectionConfidence::Confirmed);
}

// --- 15-19. Platforms that must stay distinct ----------------------------

/// Test 35
#[test]
fn platform_pairs_the_milestone_named_stay_separate() {
    for (left, right) in [
        ("Amiga", "AmigaCD32"),
        ("BBC Micro", "Acorn Electron"),
        ("PC Engine", "PC Engine CD"),
        ("NeoGeo", "Neo Geo CD"),
        ("MegaDrive", "Sega CD"),
        ("MegaDrive", "Sega 32X"),
        ("MSX", "MSX2"),
        ("Commodore 64", "Commodore 128"),
        ("Apple II", "Macintosh"),
        ("Amiga", "Commodore CDTV"),
    ] {
        assert!(platform_by_id(left).is_some(), "{left} missing");
        assert!(platform_by_id(right).is_some(), "{right} missing");
        assert_ne!(left, right);
        // Each must be reachable by its own folder alias, so a library can
        // actually separate them.
        let left_aliases = platform_by_id(left).expect("registered").folder_aliases;
        let right_aliases = platform_by_id(right).expect("registered").folder_aliases;
        assert!(
            left_aliases
                .iter()
                .all(|alias| !right_aliases.contains(alias)),
            "{left} and {right} share a folder alias"
        );
    }
}

/// Test 36
#[test]
fn philips_cd_i_is_identified_by_its_system_identifier_not_by_being_an_iso() {
    let tree = TempTree::new("cdi-header");
    // An ISO 9660 primary volume descriptor naming CD-RTOS as the system.
    let mut image = vec![0_u8; 0x8100];
    image[0x8001..0x8006].copy_from_slice(b"CD001");
    image[0x8008..0x8010].copy_from_slice(b"CD-RTOS ");
    let target = tree.file("unsorted/disc.iso", &image);

    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    assert_eq!(
        report.platform,
        Some("Philips CD-i"),
        "the system identifier is what separates CD-i from a generic ISO"
    );
    assert_eq!(report.confidence, DetectionConfidence::Confirmed);
}

/// Test 37
#[test]
fn a_playstation_iso_is_not_confused_with_a_cd_i_one() {
    let tree = TempTree::new("psx-header");
    let mut image = vec![0_u8; 0x8100];
    image[0x8001..0x8006].copy_from_slice(b"CD001");
    image[0x8008..0x8013].copy_from_slice(b"PLAYSTATION");
    let target = tree.file("unsorted/disc.iso", &image);

    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    // PSX and PS2 both name PLAYSTATION, so the family is confirmed but the
    // generation is honestly ambiguous.
    assert_eq!(report.confidence, DetectionConfidence::Ambiguous);
    let names: Vec<&str> = report
        .candidates
        .iter()
        .map(|candidate| candidate.platform)
        .collect();
    assert!(names.contains(&"PSX") && names.contains(&"PS2"));
    assert!(
        !report.evidence.iter().any(
            |item| item.source == DetectionSource::Signature && item.platform == "Philips CD-i"
        ),
        "a PlayStation disc must never match the CD-i signature"
    );
}

/// Test 38
#[test]
fn equivalent_identifiers_are_related_rather_than_renamed() {
    assert_eq!(equivalent_platform_ids("PC Engine"), vec!["TurboGrafx-16"]);
    assert_eq!(equivalent_platform_ids("TurboGrafx-16"), vec!["PC Engine"]);
    assert_eq!(equivalent_platform_ids("PC-98"), vec!["NEC PC-9801"]);
    // Both identifiers still exist, so nothing stored was rewritten.
    assert!(platform_by_id("PC Engine").is_some());
    assert!(platform_by_id("TurboGrafx-16").is_some());
    assert!(platform_by_id("PC-98").is_some());
    assert!(platform_by_id("NEC PC-9801").is_some());
    assert!(equivalent_platform_ids("MegaDrive").is_empty());
}

// --- 20, 21, 22. Failing safely ------------------------------------------

/// Test 39
#[test]
fn a_malformed_or_truncated_file_fails_safely() {
    let tree = TempTree::new("malformed");
    // Shorter than every signature offset, including a zero-byte file.
    for (name, contents) in [
        ("unsorted/empty.bin", &b""[..]),
        ("unsorted/tiny.iso", &b"SEG"[..]),
        ("unsorted/truncated.cue", &b"FILE \"missing"[..]),
        ("unsorted/garbage.bin", &[0xff_u8; 8][..]),
    ] {
        let target = tree.file(name, contents);
        let report = detect_platform_report(
            &DetectionRequest::new(&target, tree.path()).inspecting_content(),
        );
        assert!(
            report.platform.is_none() || report.confidence != DetectionConfidence::Confirmed,
            "`{name}` must not be confidently identified from nothing"
        );
    }
}

/// Test 40
#[test]
fn a_symlink_is_never_followed_to_read_a_signature() {
    #[cfg(unix)]
    {
        let tree = TempTree::new("symlink");
        let mut rom = vec![0_u8; 0x200];
        rom[0x100..0x104].copy_from_slice(b"SEGA");
        let real = tree.file("elsewhere/real.bin", &rom);
        let link = tree.path().join("unsorted/link.bin");
        fs::create_dir_all(link.parent().expect("parent")).expect("fixture");
        std::os::unix::fs::symlink(&real, &link).expect("fixture");

        let report =
            detect_platform_report(&DetectionRequest::new(&link, tree.path()).inspecting_content());
        assert!(
            !report
                .evidence
                .iter()
                .any(|item| item.source == DetectionSource::Signature),
            "a signature must never be read through a symlink"
        );
    }
}

/// Test 41
#[test]
fn an_inaccessible_or_missing_file_fails_safely() {
    let tree = TempTree::new("missing");
    let absent = tree.path().join("unsorted/absent.bin");
    let report =
        detect_platform_report(&DetectionRequest::new(&absent, tree.path()).inspecting_content());
    // The extension still contributes; nothing panics and nothing is confirmed.
    assert_ne!(report.confidence, DetectionConfidence::Confirmed);
    assert!(
        !report
            .evidence
            .iter()
            .any(|item| item.source == DetectionSource::Signature)
    );
}

/// Test 42
#[test]
fn a_directory_is_not_read_as_if_it_were_a_rom() {
    let tree = TempTree::new("directory");
    let directory = tree.path().join("unsorted/game.bin");
    fs::create_dir_all(&directory).expect("fixture");
    let report = detect_platform_report(
        &DetectionRequest::new(&directory, tree.path()).inspecting_content(),
    );
    assert!(
        !report
            .evidence
            .iter()
            .any(|item| item.source == DetectionSource::Signature),
        "a directory has no cartridge header"
    );
}

/// Test 43
#[test]
fn magic_reads_stay_within_the_documented_bound() {
    for platform in PLATFORMS {
        for rule in platform.magic {
            assert!(
                rule.bytes.len() <= MAX_MAGIC_READ_BYTES,
                "{} reads {} bytes, over the {MAX_MAGIC_READ_BYTES}-byte bound",
                platform.id,
                rule.bytes.len()
            );
            assert!(
                rule.offset <= MAX_MAGIC_OFFSET,
                "{} looks at offset {:#x}, past the documented {MAX_MAGIC_OFFSET:#x} limit",
                platform.id,
                rule.offset
            );
            assert!(
                !rule.bytes.is_empty(),
                "{} has an empty signature, which would match everything",
                platform.id
            );
            assert!(
                rule.description.len() > 20,
                "{} needs a real explanation of its signature",
                platform.id
            );
        }
    }
}

/// Test 44
#[test]
fn a_huge_file_is_not_read_beyond_its_signature_offsets() {
    let tree = TempTree::new("bounded-read");
    // 64 KiB is larger than every offset the registry uses, so this proves the
    // read is positional rather than a scan.
    let mut image = vec![0_u8; 0x10000];
    image[0x100..0x104].copy_from_slice(b"SEGA");
    let target = tree.file("unsorted/big.bin", &image);
    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    assert_eq!(report.platform, Some("MegaDrive"));
    // Nothing at any other offset was interpreted.
    assert_eq!(
        report
            .evidence
            .iter()
            .filter(|item| item.source == DetectionSource::Signature)
            .count(),
        1
    );
}

// --- 24, 25, 26. Purity, determinism, JSON -------------------------------

/// Test 45
#[test]
fn detection_never_writes_to_the_filesystem() {
    let tree = TempTree::new("read-only");
    let mut rom = vec![0_u8; 0x200];
    rom[0x100..0x104].copy_from_slice(b"SEGA");
    let target = tree.file("scummvm/laurabow2/RESOURCE.GEN", b"resource");
    tree.file("scummvm/laurabow2/RESOURCE.MAP", b"map");
    tree.file("unsorted/rom.bin", &rom);
    let before = snapshot_tree(tree.path());

    for path in [
        target.clone(),
        tree.path().join("unsorted/rom.bin"),
        tree.path().join("unsorted/absent.iso"),
    ] {
        let _ =
            detect_platform_report(&DetectionRequest::new(&path, tree.path()).inspecting_content());
    }

    assert_eq!(
        snapshot_tree(tree.path()),
        before,
        "detection must not create, remove, modify or re-timestamp anything"
    );
}

/// Test 46
#[test]
fn results_are_deterministic_regardless_of_directory_iteration_order() {
    let tree = TempTree::new("determinism");
    // Many sibling files, so the order `read_dir` happens to return them in
    // varies between runs and filesystems.
    for index in 0..64 {
        tree.file(&format!("unsorted/game/file{index:03}.dat"), b"x");
    }
    tree.file("unsorted/game/RESOURCE.MAP", b"map");
    tree.file("unsorted/game/RESOURCE.000", b"volume");
    let target = tree.file("unsorted/game/RESOURCE.GEN", b"resource");

    let first =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    for _ in 0..12 {
        let again = detect_platform_report(
            &DetectionRequest::new(&target, tree.path()).inspecting_content(),
        );
        assert_eq!(first, again, "the same inputs produced a different report");
    }
}

/// Test 47
#[test]
fn evidence_is_ordered_strongest_first_and_stably() {
    let report = detect_platform_report(
        &DetectionRequest::new(Path::new("/roms/megadrive/game.bin"), Path::new(ROOT))
            .with_trusted_platform(Some("Sega CD")),
    );
    let sources: Vec<DetectionSource> = report.evidence.iter().map(|item| item.source).collect();
    let mut expected = sources.clone();
    expected.sort_by(|left, right| right.cmp(left));
    assert_eq!(sources, expected, "evidence must be strongest-first");
}

/// Test 48
#[test]
fn the_json_evidence_shape_is_stable_and_typed() {
    let report = detect(&format!("{ROOT}/zx-spectrum/game.tap"));
    let json = serde_json::to_value(&report).expect("serialises");
    for key in [
        "platform",
        "display_name",
        "confidence",
        "deciding_source",
        "evidence",
        "candidates",
        "ambiguity_reason",
        "requires_confirmation",
        "manually_assigned",
    ] {
        assert!(json.get(key).is_some(), "`{key}` is missing from the JSON");
    }
    assert_eq!(json["confidence"], serde_json::json!("probable"));
    assert_eq!(json["platform"], serde_json::json!("ZX Spectrum"));
    assert!(json["requires_confirmation"].is_boolean());
    let first = &json["evidence"][0];
    assert_eq!(first["source"], serde_json::json!("folder_alias"));
    assert!(first["detail"].is_string());
    assert!(first["platform"].is_string());
}

/// Test 49
#[test]
fn confidence_and_source_names_are_snake_case_for_scripts() {
    for (confidence, expected) in [
        (DetectionConfidence::Confirmed, "confirmed"),
        (DetectionConfidence::Probable, "probable"),
        (DetectionConfidence::Ambiguous, "ambiguous"),
        (DetectionConfidence::Unknown, "unknown"),
    ] {
        assert_eq!(
            serde_json::to_value(confidence).expect("serialises"),
            serde_json::json!(expected)
        );
    }
    assert_eq!(
        serde_json::to_value(DetectionSource::SharedExtension).expect("serialises"),
        serde_json::json!("shared_extension")
    );
}

// --- Registry hygiene ----------------------------------------------------

/// Test 56b
#[test]
fn a_folder_and_a_signature_that_agree_confirm_the_platform() {
    let tree = TempTree::new("corroborated");
    let mut rom = vec![0_u8; 0x200];
    rom[0x100..0x104].copy_from_slice(b"SEGA");
    let target = tree.file("megadrive/Sonic.bin", &rom);

    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    assert_eq!(report.platform, Some("MegaDrive"));
    assert_eq!(
        report.confidence,
        DetectionConfidence::Confirmed,
        "a folder and a header that agree are corroboration, not mere probability"
    );
    assert!(
        !report.requires_confirmation,
        "nothing is left for a person to decide here"
    );
    // The folder still decided; the signature raised the confidence.
    assert_eq!(report.deciding_source, Some(DetectionSource::FolderAlias));
}

/// Test 56c
#[test]
fn a_folder_that_contradicts_a_signature_still_wins_but_only_as_probable() {
    let tree = TempTree::new("contradicted");
    // A genuine Mega Drive header inside a folder that says Master System.
    let mut rom = vec![0_u8; 0x200];
    rom[0x100..0x104].copy_from_slice(b"SEGA");
    let target = tree.file("mastersystem/mystery.bin", &rom);

    let report =
        detect_platform_report(&DetectionRequest::new(&target, tree.path()).inspecting_content());
    assert_eq!(
        report.platform,
        Some("MasterSystem"),
        "the folder outranks a contradicting signature, as the priority order requires"
    );
    assert_eq!(
        report.confidence,
        DetectionConfidence::Probable,
        "a contradicted result must not be presented as confirmed"
    );
    assert!(
        report
            .evidence
            .iter()
            .any(|item| item.source == DetectionSource::Signature && item.platform == "MegaDrive"),
        "the contradicting evidence must stay visible so a person can see the conflict"
    );
}

/// Test 50
#[test]
fn no_extension_is_both_strong_and_shared_for_one_platform() {
    for platform in PLATFORMS {
        for extension in platform.strong_extensions {
            assert!(
                !platform.weak_extensions.contains(extension),
                "{} lists `.{extension}` as both strong and shared",
                platform.id
            );
            assert!(
                !is_shared_extension(extension),
                "{} claims the shared extension `.{extension}` as strong evidence",
                platform.id
            );
        }
    }
}

/// Test 51
#[test]
fn every_conflicting_platform_actually_exists() {
    for platform in PLATFORMS {
        for conflict in platform.conflicts_with {
            assert!(
                platform_by_id(conflict).is_some(),
                "{} conflicts with `{conflict}`, which is not in the registry",
                platform.id
            );
            assert_ne!(
                *conflict, platform.id,
                "{} lists itself as a conflict",
                platform.id
            );
        }
    }
}

/// Test 52
#[test]
fn extensions_are_stored_lowercase_without_a_dot() {
    for platform in PLATFORMS {
        for extension in platform
            .strong_extensions
            .iter()
            .chain(platform.weak_extensions)
        {
            assert!(
                !extension.starts_with('.'),
                "{} stores `{extension}` with a leading dot",
                platform.id
            );
            assert_eq!(
                *extension,
                extension.to_ascii_lowercase(),
                "{} stores `{extension}` with uppercase characters",
                platform.id
            );
        }
    }
}

/// Test 53
#[test]
fn detection_is_case_insensitive_about_extensions() {
    for name in ["GAME.SFC", "game.sfc", "Game.Sfc"] {
        assert_eq!(
            detect(&format!("{ROOT}/unsorted/{name}")).platform,
            Some("SNES"),
            "`{name}` should detect the same way"
        );
    }
}

/// Test 54
#[test]
fn a_layout_rule_names_lowercase_files_so_matching_is_case_insensitive() {
    for platform in PLATFORMS {
        for rule in platform.layout {
            assert!(
                !rule.any_of_files.is_empty(),
                "{} has an empty layout rule",
                platform.id
            );
            for file in rule.any_of_files {
                assert_eq!(
                    *file,
                    file.to_ascii_lowercase(),
                    "{} stores layout file `{file}` with uppercase characters",
                    platform.id
                );
            }
        }
    }
}

/// Test 55
#[test]
fn the_registry_is_sorted_by_display_name_so_every_derived_list_is_stable() {
    let names: Vec<String> = PLATFORMS
        .iter()
        .map(|platform| platform.display_name.to_lowercase())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "the registry table is out of order");
}

/// Test 56
#[test]
fn no_network_or_process_call_exists_anywhere_in_the_platform_module() {
    for (name, source) in [
        ("mod.rs", include_str!("mod.rs")),
        ("detect.rs", include_str!("detect.rs")),
    ] {
        let code = source
            .split("#[cfg(test)]")
            .next()
            .expect("production half");
        for forbidden in [
            "ureq",
            "reqwest",
            "TcpStream",
            "Command::new",
            "std::process",
            "fs::write",
            "fs::create_dir",
            "fs::remove_",
            "File::create",
            "OpenOptions",
            "set_permissions",
        ] {
            assert!(
                !code.contains(forbidden),
                "`{forbidden}` must never appear in {name}: detection is read-only and offline"
            );
        }
    }
}

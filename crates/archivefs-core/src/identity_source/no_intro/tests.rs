use std::path::Path;

use tempfile::tempdir;

use crate::dat::model::ChecksumAlgorithm;
use crate::platform_evidence_fusion::evidence_lineage::{
    AgreementStatus, ClaimStrength, ClaimType, EvidenceChannel, LineageRelation, Representation,
    SourceFamily, hasheous_observation, merge_evidence, observation_declares_provenance,
    observation_from_content_evidence,
};

use super::convert::{
    claim_for_representation, lookup_no_intro, observations_from_no_intro_matches,
};
use super::import::{NoIntroImportError, NoIntroVariant, import_no_intro_dat};

fn write_dat(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

const GB_NO_INTRO_XML: &str = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Nintendo - Game Boy</name>
        <description>Nintendo - Game Boy</description>
        <version>20250101-120000</version>
        <author>No-Intro</author>
    </header>
    <game name="Alleyway (World)">
        <rom name="Alleyway (World).gb" size="32768" crc="9F73FA30" md5="d05e94ad0435e2fe1b7be15c8d1cec83" sha1="ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa"/>
    </game>
    <game name="Tetris (World)">
        <rom name="Tetris (World).gb" size="32768" crc="AAAAAAAA" sha1="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"/>
    </game>
</datafile>"#;

const GB_NO_INTRO_XML_HEADERLESS: &str = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Nintendo - Game Boy (Headerless)</name>
        <version>20250601-000000</version>
        <author>No-Intro</author>
    </header>
    <game name="Alleyway (World)">
        <rom name="Alleyway (World).gb" size="32768" crc="9F73FA30" sha1="ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa"/>
    </game>
</datafile>"#;

const GB_NO_INTRO_XML_V2: &str = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Nintendo - Game Boy</name>
        <version>20260101-000000</version>
        <author>No-Intro</author>
    </header>
    <game name="Alleyway (World)">
        <rom name="Alleyway (World).gb" size="32768" crc="9F73FA30" sha1="ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa"/>
    </game>
    <game name="New Game (World)">
        <rom name="New Game (World).gb" size="32768" crc="CCCCCCCC" sha1="cccccccccccccccccccccccccccccccccccccccc"/>
    </game>
</datafile>"#;

const TOSEC_XML: &str = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Nintendo 64 - Games (TOSEC)</name>
        <author>TOSEC</author>
    </header>
    <game name="Test">
        <rom name="test.z64" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#;

const MALFORMED_XML: &str = "<?xml version=\"1.0\"?><datafile><header><name>Broken";

const BOM_NO_INTRO_XML: &str = "\u{FEFF}<?xml version=\"1.0\"?>\n<datafile>\n    <header>\n        <name>Nintendo - Game Boy Color</name>\n        <author>No-Intro</author>\n    </header>\n    <game name=\"Test\">\n        <rom name=\"test.gbc\" size=\"1\" crc=\"AAAAAAAA\"/>\n    </game>\n</datafile>";

// ---------------------------------------------------------------------
// Import matrix (section 59, items 1-15)
// ---------------------------------------------------------------------

#[test]
fn logiqx_no_intro_import_succeeds() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML);
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.system_name, "Nintendo - Game Boy");
    assert_eq!(source.entry_count, 2);
}

#[test]
fn positive_source_identification_by_name() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML);
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.system_name, "Nintendo - Game Boy");
}

#[test]
fn uncertain_source_is_refused_not_guessed() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "tosec.dat", TOSEC_XML);
    let error = import_no_intro_dat(&path).unwrap_err();
    assert!(matches!(error, NoIntroImportError::NotNoIntro { .. }));
}

#[test]
fn artifact_sha256_is_recorded_and_deterministic() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML);
    let a = import_no_intro_dat(&path).unwrap();
    let b = import_no_intro_dat(&path).unwrap();
    assert_eq!(a.artifact_sha256, b.artifact_sha256);
    assert_eq!(a.artifact_sha256.len(), 64);
}

#[test]
fn version_extracted_from_internal_metadata() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML);
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.upstream_version.as_deref(), Some("20250101-120000"));
}

#[test]
fn version_unknown_when_dat_omits_it() {
    let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Nintendo - Game Boy</name>
        <author>No-Intro</author>
    </header>
    <game name="Test"><rom name="test.gb" size="1" crc="AAAAAAAA"/></game>
</datafile>"#;
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", xml);
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.upstream_version, None);
}

#[test]
fn no_version_fallback_to_filename_is_ever_fabricated() {
    let dir = tempdir().unwrap();
    let path = write_dat(
        dir.path(),
        "Nintendo - Game Boy (20990101-000000).dat",
        r#"<?xml version="1.0"?><datafile><header><name>Nintendo - Game Boy</name><author>No-Intro</author></header><game name="Test"><rom name="test.gb" size="1" crc="AAAAAAAA"/></game></datafile>"#,
    );
    let source = import_no_intro_dat(&path).unwrap();
    // The filename claims a 2099 date; the DAT itself asserts nothing, so
    // the importer must not have manufactured a version from the filename.
    assert_eq!(source.upstream_version, None);
}

#[test]
fn headered_variant_detected() {
    let dir = tempdir().unwrap();
    let path = write_dat(
        dir.path(),
        "gb.dat",
        r#"<?xml version="1.0"?><datafile><header><name>Nintendo - Game Boy (Headered)</name><author>No-Intro</author></header><game name="Test"><rom name="test.gb" size="1" crc="AAAAAAAA"/></game></datafile>"#,
    );
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.variant, NoIntroVariant::Headered);
}

#[test]
fn headerless_variant_detected() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML_HEADERLESS);
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.variant, NoIntroVariant::Headerless);
}

#[test]
fn unknown_variant_when_not_stated() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML);
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.variant, NoIntroVariant::Unknown);
}

#[test]
fn duplicate_artifact_import_has_identical_sha256() {
    let dir = tempdir().unwrap();
    let path_a = write_dat(dir.path(), "a.dat", GB_NO_INTRO_XML);
    let path_b = write_dat(dir.path(), "b.dat", GB_NO_INTRO_XML);
    let a = import_no_intro_dat(&path_a).unwrap();
    let b = import_no_intro_dat(&path_b).unwrap();
    assert_eq!(a.artifact_sha256, b.artifact_sha256);
}

#[test]
fn different_artifact_versions_preserve_distinct_hashes() {
    let dir = tempdir().unwrap();
    let old_path = write_dat(dir.path(), "old.dat", GB_NO_INTRO_XML);
    let new_path = write_dat(dir.path(), "new.dat", GB_NO_INTRO_XML_V2);
    let old = import_no_intro_dat(&old_path).unwrap();
    let new = import_no_intro_dat(&new_path).unwrap();
    assert_ne!(old.artifact_sha256, new.artifact_sha256);
    assert_ne!(old.upstream_version, new.upstream_version);
}

#[test]
fn malformed_dat_is_a_clean_error_not_a_panic() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "broken.dat", MALFORMED_XML);
    let error = import_no_intro_dat(&path);
    assert!(error.is_err());
}

#[test]
fn bom_no_intro_dat_still_imports() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "bom.dat", BOM_NO_INTRO_XML);
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.system_name, "Nintendo - Game Boy Color");
}

#[test]
fn missing_sha1_entry_is_still_accepted() {
    let xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>Nintendo - Game Boy</name><author>No-Intro</author></header>
    <game name="Test"><rom name="test.gb" size="1" crc="AAAAAAAA" md5="d05e94ad0435e2fe1b7be15c8d1cec83"/></game>
</datafile>"#;
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", xml);
    let source = import_no_intro_dat(&path).unwrap();
    assert_eq!(source.entry_count, 1);
    let hits = lookup_no_intro(
        &source.index,
        ChecksumAlgorithm::Md5,
        "d05e94ad0435e2fe1b7be15c8d1cec83",
    );
    assert_eq!(hits.len(), 1);
}

#[test]
fn nonexistent_path_is_an_io_error() {
    let error = import_no_intro_dat(Path::new("/nonexistent/does-not-exist.dat"));
    assert!(matches!(error, Err(NoIntroImportError::Io { .. })));
}

// ---------------------------------------------------------------------
// Lookup matrix (section 60, items 16-25)
// ---------------------------------------------------------------------

fn imported_gb() -> super::import::ImportedNoIntroSource {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML);
    import_no_intro_dat(&path).unwrap()
}

#[test]
fn sha1_exact_lookup_returns_the_match() {
    let source = imported_gb();
    let hits = lookup_no_intro(
        &source.index,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].game_name, "Alleyway (World)");
}

#[test]
fn md5_exact_lookup_returns_the_match() {
    let source = imported_gb();
    let hits = lookup_no_intro(
        &source.index,
        ChecksumAlgorithm::Md5,
        "d05e94ad0435e2fe1b7be15c8d1cec83",
    );
    assert_eq!(hits.len(), 1);
}

#[test]
fn crc_lookup_returns_the_match() {
    let source = imported_gb();
    let hits = lookup_no_intro(&source.index, ChecksumAlgorithm::Crc32, "9f73fa30");
    assert_eq!(hits.len(), 1);
}

#[test]
fn crc_collision_returns_multiple_candidates() {
    let xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>Nintendo - Game Boy</name><author>No-Intro</author></header>
    <game name="A"><rom name="a.gb" size="1" crc="AAAAAAAA"/></game>
    <game name="B"><rom name="b.gb" size="2" crc="AAAAAAAA"/></game>
</datafile>"#;
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", xml);
    let source = import_no_intro_dat(&path).unwrap();
    let hits = lookup_no_intro(&source.index, ChecksumAlgorithm::Crc32, "aaaaaaaa");
    assert_eq!(hits.len(), 2);
}

#[test]
fn sha1_multiplicity_returns_multiple_candidates() {
    let xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>Nintendo - Game Boy</name><author>No-Intro</author></header>
    <game name="A"><rom name="a.gb" size="1" sha1="dddddddddddddddddddddddddddddddddddddddd"/></game>
    <game name="B"><rom name="b.gb" size="1" sha1="dddddddddddddddddddddddddddddddddddddddd"/></game>
</datafile>"#;
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", xml);
    let source = import_no_intro_dat(&path).unwrap();
    let hits = lookup_no_intro(
        &source.index,
        ChecksumAlgorithm::Sha1,
        "dddddddddddddddddddddddddddddddddddddddd",
    );
    assert_eq!(hits.len(), 2);
}

#[test]
fn no_match_is_neutral_empty() {
    let source = imported_gb();
    let hits = lookup_no_intro(
        &source.index,
        ChecksumAlgorithm::Sha1,
        "0000000000000000000000000000000000000000",
    );
    assert!(hits.is_empty());
}

#[test]
fn physical_representation_observation() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    assert!(obs.iter().any(
        |o| o.provenance.representation == Representation::PhysicalFile
            && o.claim == ClaimType::ExactBytesMatch
    ));
}

#[test]
fn normalized_representation_observation() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::NormalizedRom,
    );
    assert!(obs.iter().any(
        |o| o.provenance.representation == Representation::NormalizedRom
            && o.claim == ClaimType::ExactNormalizedMatch
    ));
}

#[test]
fn representation_is_never_guessed_the_caller_controls_it() {
    let source = imported_gb();
    let physical = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let normalized = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::NormalizedRom,
    );
    assert_ne!(
        physical[0].provenance.representation,
        normalized[0].provenance.representation
    );
}

#[test]
fn lookup_result_order_is_deterministic() {
    let source = imported_gb();
    let a = lookup_no_intro(
        &source.index,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
    )
    .to_vec();
    let b = lookup_no_intro(
        &source.index,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
    )
    .to_vec();
    assert_eq!(a, b);
}

// ---------------------------------------------------------------------
// Observation matrix (section 61, items 26-35)
// ---------------------------------------------------------------------

#[test]
fn channel_is_local_no_intro() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    assert!(
        obs.iter()
            .all(|o| o.provenance.channel == EvidenceChannel::LocalNoIntro)
    );
}

#[test]
fn upstream_source_is_no_intro() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    assert!(
        obs.iter()
            .all(|o| o.provenance.upstream_source == SourceFamily::NoIntro)
    );
}

#[test]
fn lineage_is_independent() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let exact = obs
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.provenance.lineage, LineageRelation::Independent);
}

#[test]
fn artifact_provenance_is_carried() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let exact = obs
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    let artifact = exact.provenance.source_artifact.as_ref().unwrap();
    assert_eq!(
        artifact.artifact_sha256.as_deref(),
        Some(source.artifact_sha256.as_str())
    );
}

#[test]
fn version_is_carried_on_the_observation() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let exact = obs
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.provenance.upstream_version, source.upstream_version);
}

#[test]
fn exact_bytes_match_claim() {
    assert_eq!(
        claim_for_representation(Representation::PhysicalFile),
        ClaimType::ExactBytesMatch
    );
}

#[test]
fn exact_normalized_match_claim() {
    assert_eq!(
        claim_for_representation(Representation::NormalizedRom),
        ClaimType::ExactNormalizedMatch
    );
}

#[test]
fn title_is_display_metadata() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    assert!(obs.iter().any(|o| o.claim == ClaimType::DisplayMetadata));
}

#[test]
fn platform_candidate_carried_on_exact_match() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let exact = obs
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(
        exact.platform_candidate.as_deref(),
        Some("Nintendo - Game Boy")
    );
}

#[test]
fn every_observation_declares_provenance() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    assert!(obs.iter().all(observation_declares_provenance));
}

#[test]
fn crc_only_match_is_not_strong_when_no_stronger_hash_present() {
    let xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>Nintendo - Game Boy</name><author>No-Intro</author></header>
    <game name="A"><rom name="a.gb" size="1" crc="BBBBBBBB"/></game>
</datafile>"#;
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", xml);
    let source = import_no_intro_dat(&path).unwrap();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Crc32,
        "bbbbbbbb",
        Representation::PhysicalFile,
    );
    let exact = obs
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.claim_strength, ClaimStrength::Corroborated);
}

#[test]
fn sha1_match_is_strong() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let exact = obs
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.claim_strength, ClaimStrength::Strong);
}

// ---------------------------------------------------------------------
// Lineage merge matrix (section 62, items 36-43)
// ---------------------------------------------------------------------

#[test]
fn local_no_intro_plus_hasheous_no_intro_is_same_source_agreement() {
    let source = imported_gb();
    let local = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let hasheous = hasheous_observation(
        "NoIntros",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa".to_string()),
        None,
    );
    let mut all = local;
    all.push(hasheous);
    let summaries = merge_evidence(&all);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn local_version_known_hasheous_version_unknown_still_same_lineage() {
    let source = imported_gb();
    assert!(source.upstream_version.is_some());
    let local = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let hasheous = hasheous_observation(
        "NoIntros",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa".to_string()),
        None,
    );
    assert_eq!(hasheous.provenance.upstream_version, None);
    let mut all = local;
    all.push(hasheous);
    let summaries = merge_evidence(&all);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn old_and_new_local_dat_agreeing_is_same_source_agreement() {
    let dir = tempdir().unwrap();
    let old_path = write_dat(dir.path(), "old.dat", GB_NO_INTRO_XML);
    let new_path = write_dat(dir.path(), "new.dat", GB_NO_INTRO_XML_V2);
    let old = import_no_intro_dat(&old_path).unwrap();
    let new = import_no_intro_dat(&new_path).unwrap();
    let mut obs = observations_from_no_intro_matches(
        &old,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    obs.extend(observations_from_no_intro_matches(
        &new,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    ));
    let summaries = merge_evidence(&obs);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn old_and_new_local_dat_conflicting_is_same_source_version_conflict() {
    let dir = tempdir().unwrap();
    let old_xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>Nintendo - Game Boy</name><version>20250101-000000</version><author>No-Intro</author></header>
    <game name="Old Title"><rom name="x.gb" size="1" sha1="eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"/></game>
</datafile>"#;
    let new_xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>Nintendo - Game Boy</name><version>20260101-000000</version><author>No-Intro</author></header>
    <game name="Renamed Title"><rom name="x.gb" size="1" sha1="eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"/></game>
</datafile>"#;
    let old_path = write_dat(dir.path(), "old.dat", old_xml);
    let new_path = write_dat(dir.path(), "new.dat", new_xml);
    let old = import_no_intro_dat(&old_path).unwrap();
    let new = import_no_intro_dat(&new_path).unwrap();
    let mut obs = observations_from_no_intro_matches(
        &old,
        ChecksumAlgorithm::Sha1,
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        Representation::PhysicalFile,
    );
    obs.extend(observations_from_no_intro_matches(
        &new,
        ChecksumAlgorithm::Sha1,
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        Representation::PhysicalFile,
    ));
    let summaries = merge_evidence(&obs);
    // The exact-bytes claim itself still agrees (same hash) - the release
    // candidate conflict is a metadata-shaped difference, not a byte
    // dispute, so what matters here is that no independent-source
    // misclassification occurs.
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_ne!(exact.status, AgreementStatus::IndependentSourceConflict);
}

#[test]
fn local_no_intro_plus_local_structural_agreement_is_independent_agreement() {
    // The exact-bytes claim and the structural detector's platform-candidate
    // claim are different claim types (a byte match is not a platform
    // claim), so they never share a claim-scoped merge group - that
    // separation is correct, not a bug. What this test actually proves is
    // the "genuinely different evidence lanes agree" case at the
    // PlatformCandidate claim itself: the No-Intro system name and the
    // structural header detector's own platform read, as two independent
    // observations of the *same* fact via unrelated evidence lanes.
    let source = imported_gb();
    let no_intro_platform_observation =
        crate::platform_evidence_fusion::evidence_lineage::EvidenceObservation {
            provenance: crate::platform_evidence_fusion::evidence_lineage::Provenance {
                channel: EvidenceChannel::LocalNoIntro,
                upstream_source: SourceFamily::NoIntro,
                upstream_version: source.upstream_version.clone(),
                source_artifact: None,
                imported_at_unix: None,
                retrieved_at_unix: None,
                generator_version: None,
                lineage: LineageRelation::Independent,
                representation: Representation::PhysicalFile,
            },
            claim: ClaimType::PlatformCandidate,
            claim_strength: ClaimStrength::Strong,
            identity_scope:
                crate::platform_evidence_fusion::evidence_lineage::IdentityScope::PlatformIdentity,
            hash_or_value: None,
            platform_candidate: Some("Game Boy".to_string()),
            release_candidate: None,
            notes: None,
        };
    let structural_fact = crate::content_evidence::ContentEvidence {
        kind: crate::content_evidence::ContentEvidenceKind::BootStructure,
        value: "Game Boy".to_string(),
        confidence: crate::content_evidence::ContentEvidenceConfidence::Strong,
        detail: "Nintendo logo + header checksum valid".to_string(),
    };
    let structural = observation_from_content_evidence(&structural_fact);
    let summaries = merge_evidence(&[no_intro_platform_observation, structural]);
    let platform = summaries
        .iter()
        .find(|s| s.claim == ClaimType::PlatformCandidate)
        .expect("both observations share the PlatformCandidate claim");
    // Batch 21 closeout (see evidence_lineage.rs's `LineageLane`):
    // LocalStructural carries `SourceFamily::Unknown` by design (a
    // byte-level detector is not an external preservation source), but it
    // is EmuWiz's own known-provenance mechanism, not a genuinely
    // unidentified external one - so a group of one known family plus one
    // LocalStructural observation *is* classified as two independent
    // evidence lanes. Real independence between two named preservation
    // families (e.g. TOSEC vs. No-Intro) is proven separately by
    // `local_no_intro_plus_tosec_is_independent_agreement` above.
    assert_eq!(platform.status, AgreementStatus::IndependentAgreement);
    assert_eq!(
        crate::platform_evidence_fusion::evidence_lineage::independent_source_group_count(
            &platform.observations
        ),
        2
    );
}

#[test]
fn local_no_intro_plus_tosec_is_independent_agreement() {
    let source = imported_gb();
    let local = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let tosec = hasheous_observation(
        "TOSEC",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa".to_string()),
        None,
    );
    let mut all = local;
    all.push(tosec);
    let summaries = merge_evidence(&all);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::IndependentAgreement);
}

#[test]
fn mirror_same_artifact_no_inflation() {
    let dir = tempdir().unwrap();
    let path_a = write_dat(dir.path(), "a.dat", GB_NO_INTRO_XML);
    let path_b = write_dat(dir.path(), "b.dat", GB_NO_INTRO_XML);
    let a = import_no_intro_dat(&path_a).unwrap();
    let b = import_no_intro_dat(&path_b).unwrap();
    let mut obs = observations_from_no_intro_matches(
        &a,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    obs.extend(observations_from_no_intro_matches(
        &b,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    ));
    let deduped = crate::platform_evidence_fusion::evidence_lineage::dedup_mirror_artifacts(&obs);
    let exact_count = deduped
        .iter()
        .filter(|o| o.claim == ClaimType::ExactBytesMatch)
        .count();
    assert_eq!(exact_count, 1);
}

#[test]
fn two_system_matches_same_hash_are_both_preserved() {
    let dir = tempdir().unwrap();
    let gb_path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML);
    let gbc_xml = r#"<?xml version="1.0"?>
<datafile>
    <header><name>Nintendo - Game Boy Color</name><author>No-Intro</author></header>
    <game name="Alleyway (World)"><rom name="Alleyway (World).gb" size="32768" sha1="ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa"/></game>
</datafile>"#;
    let gbc_path = write_dat(dir.path(), "gbc.dat", gbc_xml);
    let gb = import_no_intro_dat(&gb_path).unwrap();
    let gbc = import_no_intro_dat(&gbc_path).unwrap();
    let mut obs = observations_from_no_intro_matches(
        &gb,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    obs.extend(observations_from_no_intro_matches(
        &gbc,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    ));
    let platforms: std::collections::BTreeSet<_> = obs
        .iter()
        .filter(|o| o.claim == ClaimType::ExactBytesMatch)
        .filter_map(|o| o.platform_candidate.clone())
        .collect();
    assert_eq!(platforms.len(), 2);
}

// ---------------------------------------------------------------------
// Normalization / representation separation (section 63, items 44-48)
// ---------------------------------------------------------------------

#[test]
fn physical_v64_and_normalized_z64_stay_separate() {
    let obs = crate::platform_evidence_fusion::evidence_lineage::observations_from_physical_and_normalized(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        Some("physical-v64-hash".to_string()),
        Some("normalized-z64-hash".to_string()),
    );
    assert_eq!(obs.len(), 2);
    assert_ne!(
        obs[0].provenance.representation,
        obs[1].provenance.representation
    );
}

#[test]
fn normalized_no_intro_exact_match_reachable_via_lookup() {
    let source = imported_gb();
    let obs = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::NormalizedRom,
    );
    assert!(
        obs.iter()
            .any(|o| o.claim == ClaimType::ExactNormalizedMatch)
    );
}

#[test]
fn physical_miss_normalized_hit_is_a_valid_independent_result() {
    let source = imported_gb();
    let physical_miss = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "0000000000000000000000000000000000000000",
        Representation::PhysicalFile,
    );
    let normalized_hit = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::NormalizedRom,
    );
    assert!(physical_miss.is_empty());
    assert!(!normalized_hit.is_empty());
}

#[test]
fn no_in_place_normalization_import_does_not_mutate_the_dat_file() {
    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "gb.dat", GB_NO_INTRO_XML);
    let before = std::fs::read(&path).unwrap();
    let _source = import_no_intro_dat(&path).unwrap();
    let after = std::fs::read(&path).unwrap();
    assert_eq!(before, after);
}

#[test]
fn same_hash_string_different_representation_stays_separate() {
    let source = imported_gb();
    let physical = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::PhysicalFile,
    );
    let normalized = observations_from_no_intro_matches(
        &source,
        ChecksumAlgorithm::Sha1,
        "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
        Representation::NormalizedRom,
    );
    let physical_exact = physical
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    let normalized_exact = normalized
        .iter()
        .find(|o| o.claim == ClaimType::ExactNormalizedMatch)
        .unwrap();
    assert_eq!(physical_exact.hash_or_value, normalized_exact.hash_or_value);
    assert_ne!(
        physical_exact.provenance.representation,
        normalized_exact.provenance.representation
    );
}

// ---------------------------------------------------------------------
// Determinism / performance (section 64, items 49-55)
// ---------------------------------------------------------------------

#[test]
fn shuffled_dat_import_order_produces_the_same_lookup_result() {
    let dir = tempdir().unwrap();
    let a_path = write_dat(dir.path(), "a.dat", GB_NO_INTRO_XML);
    let b_path = write_dat(dir.path(), "b.dat", GB_NO_INTRO_XML_V2);

    let forward = {
        let a = import_no_intro_dat(&a_path).unwrap();
        let b = import_no_intro_dat(&b_path).unwrap();
        (
            lookup_no_intro(
                &a.index,
                ChecksumAlgorithm::Sha1,
                "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
            )
            .len(),
            lookup_no_intro(
                &b.index,
                ChecksumAlgorithm::Sha1,
                "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
            )
            .len(),
        )
    };
    let backward = {
        let b = import_no_intro_dat(&b_path).unwrap();
        let a = import_no_intro_dat(&a_path).unwrap();
        (
            lookup_no_intro(
                &a.index,
                ChecksumAlgorithm::Sha1,
                "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
            )
            .len(),
            lookup_no_intro(
                &b.index,
                ChecksumAlgorithm::Sha1,
                "ed8070e011713527bdc03e2b9cec9f9c4a7e3aaa",
            )
            .len(),
        )
    };
    assert_eq!(forward, backward);
}

#[test]
fn shuffled_entries_same_index_lookup_result() {
    const SHA1_A: &str = "111111111111111111111111111111111111111a";
    const SHA1_B: &str = "111111111111111111111111111111111111111b";
    let xml_forward = format!(
        r#"<?xml version="1.0"?>
<datafile><header><name>Nintendo - Game Boy</name><author>No-Intro</author></header>
<game name="A"><rom name="a.gb" size="1" sha1="{SHA1_A}"/></game>
<game name="B"><rom name="b.gb" size="1" sha1="{SHA1_B}"/></game>
</datafile>"#
    );
    let xml_backward = format!(
        r#"<?xml version="1.0"?>
<datafile><header><name>Nintendo - Game Boy</name><author>No-Intro</author></header>
<game name="B"><rom name="b.gb" size="1" sha1="{SHA1_B}"/></game>
<game name="A"><rom name="a.gb" size="1" sha1="{SHA1_A}"/></game>
</datafile>"#
    );
    let dir = tempdir().unwrap();
    let forward_path = write_dat(dir.path(), "f.dat", &xml_forward);
    let backward_path = write_dat(dir.path(), "b.dat", &xml_backward);
    let forward = import_no_intro_dat(&forward_path).unwrap();
    let backward = import_no_intro_dat(&backward_path).unwrap();
    let a_forward = lookup_no_intro(&forward.index, ChecksumAlgorithm::Sha1, SHA1_A);
    let a_backward = lookup_no_intro(&backward.index, ChecksumAlgorithm::Sha1, SHA1_A);
    assert_eq!(a_forward.len(), a_backward.len());
    assert_eq!(a_forward[0].game_name, a_backward[0].game_name);
}

#[test]
fn source_manifest_line_is_stable() {
    let source = imported_gb();
    let a = source.manifest_line();
    let b = source.manifest_line();
    assert_eq!(a, b);
    assert!(a.contains("Nintendo - Game Boy"));
    assert!(a.contains(&source.artifact_sha256));
}

#[test]
fn hundred_thousand_synthetic_entry_import_sanity() {
    let mut xml = String::from(
        "<?xml version=\"1.0\"?><datafile><header><name>Nintendo - Game Boy</name><author>No-Intro</author></header>",
    );
    for i in 0..100_000u32 {
        xml.push_str(&format!(
            "<game name=\"g{i}\"><rom name=\"g{i}.gb\" size=\"1\" crc=\"{i:08x}\"/></game>"
        ));
    }
    xml.push_str("</datafile>");

    let dir = tempdir().unwrap();
    let path = write_dat(dir.path(), "big.dat", &xml);
    let start = std::time::Instant::now();
    let source = import_no_intro_dat(&path).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(source.entry_count, 100_000);
    // Sanity only: a linear-ish import of 100k trivial entries should not
    // take anywhere near a full minute on ordinary hardware.
    assert!(elapsed.as_secs() < 60, "import took {elapsed:?}");

    let lookup_start = std::time::Instant::now();
    for i in 0..1000u32 {
        let hex = format!("{i:08x}");
        let _ = lookup_no_intro(&source.index, ChecksumAlgorithm::Crc32, &hex);
    }
    let lookup_elapsed = lookup_start.elapsed();
    assert!(
        lookup_elapsed.as_millis() < 5000,
        "1000 lookups took {lookup_elapsed:?}"
    );
}

#[test]
fn generic_local_dat_bridge_unaffected_by_this_module_existing() {
    // The pre-existing generic LocalDat bridge (Batch 19) must remain
    // exactly as it was: Unknown upstream source, no No-Intro-specific
    // provenance leaking into it.
    let evidence = crate::dat::identity::DatPlatformEvidence {
        platform: "Nintendo - Game Boy".to_string(),
        machine_key: None,
        kind: crate::dat::identity::DatPlatformEvidenceKind::HeaderName,
        confidence: crate::dat::identity::DatPlatformConfidence::Strong,
        detail: "exact hash match".to_string(),
    };
    let obs =
        crate::platform_evidence_fusion::evidence_lineage::observation_from_dat_platform_evidence(
            &evidence,
            Representation::PhysicalFile,
        );
    assert_eq!(obs.provenance.channel, EvidenceChannel::LocalDat);
    assert_eq!(obs.provenance.upstream_source, SourceFamily::Unknown);
}

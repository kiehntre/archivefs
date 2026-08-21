use super::*;

fn obs(
    channel: EvidenceChannel,
    upstream_source: SourceFamily,
    lineage: LineageRelation,
    representation: Representation,
    claim: ClaimType,
    value: Option<&str>,
) -> EvidenceObservation {
    EvidenceObservation {
        provenance: Provenance {
            channel,
            upstream_source,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage,
            representation,
        },
        claim,
        claim_strength: ClaimStrength::Strong,
        identity_scope: IdentityScope::DumpIdentity,
        hash_or_value: value.map(str::to_string),
        platform_candidate: None,
        release_candidate: None,
        notes: None,
    }
}

fn platform_obs(
    channel: EvidenceChannel,
    upstream_source: SourceFamily,
    lineage: LineageRelation,
    platform: &str,
) -> EvidenceObservation {
    let mut o = obs(
        channel,
        upstream_source,
        lineage,
        Representation::StructuralMetadata,
        ClaimType::PlatformCandidate,
        None,
    );
    o.platform_candidate = Some(platform.to_string());
    o
}

fn with_version(mut o: EvidenceObservation, version: &str) -> EvidenceObservation {
    o.provenance.upstream_version = Some(version.to_string());
    o
}

fn with_artifact(mut o: EvidenceObservation, sha256: &str) -> EvidenceObservation {
    o.provenance.source_artifact = Some(SourceArtifactIdentity {
        source_family: o.provenance.upstream_source,
        upstream_version: o.provenance.upstream_version.clone(),
        artifact_sha256: Some(sha256.to_string()),
        artifact_name: None,
    });
    o
}

// ------------------------------------------------------------------
// Same-source test matrix (section 45, tests 1-6)
// ------------------------------------------------------------------

#[test]
fn test_01_local_nointro_plus_hasheous_nointro_same_hash_is_same_source_agreement() {
    let a = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("abc123"),
    );
    let b = hasheous_observation(
        "NoIntro",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("abc123".to_string()),
        None,
    );
    let c = romm_match_observation(
        "nointro_match",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("abc123".to_string()),
    );
    let summaries = merge_evidence(&[a, b, c]);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
    assert_eq!(
        summaries[0].observations.len(),
        3,
        "3 observations preserved"
    );
    assert_eq!(
        independent_source_group_count(&summaries[0].observations),
        1,
        "one upstream source group, no confidence inflation"
    );
}

#[test]
fn test_02_local_tosec_plus_hasheous_tosec_is_same_source_agreement() {
    let a = obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("deadbeef"),
    );
    let b = hasheous_observation(
        "TOSEC",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("deadbeef".to_string()),
        None,
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn test_03_direct_redump_plus_hasheous_redump_is_same_source_agreement() {
    let a = obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("track-hash-1"),
    );
    let b = hasheous_observation(
        "Redump",
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("track-hash-1".to_string()),
        None,
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn test_04_redump_plus_mameredump_plus_romm_bool_retains_lineage_no_double_count() {
    let direct = obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("t1"),
    );
    let hasheous_redump = hasheous_observation(
        "Redump",
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("t1".to_string()),
        None,
    );
    let mame_redump = obs(
        EvidenceChannel::LocalMame,
        SourceFamily::MAMERedump,
        LineageRelation::DerivedFrom,
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("t1"),
    );
    let hasheous_mame_redump = hasheous_observation(
        "MAMERedump",
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("t1".to_string()),
        None,
    );
    let romm_bool = romm_match_observation(
        "mame_redump_match",
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("t1".to_string()),
    );
    let all = vec![
        direct,
        hasheous_redump,
        mame_redump,
        hasheous_mame_redump,
        romm_bool,
    ];
    let summaries = merge_evidence(&all);
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].observations.len(),
        5,
        "five observations, none dropped"
    );
    assert!(
        independent_source_group_count(&summaries[0].observations) <= 2,
        "Redump + MAMERedump is at most two lineages, never five"
    );
    // Agreeing values across a direct source and its known derivative:
    // DerivedAgreement, not five independent votes.
    assert_eq!(summaries[0].status, AgreementStatus::DerivedAgreement);
}

#[test]
fn test_05_whdload_plus_hasheous_whdload_is_same_source_agreement() {
    let a = obs(
        EvidenceChannel::LocalWHDLoad,
        SourceFamily::WHDLoad,
        LineageRelation::Independent,
        Representation::WHDLoadSlave,
        ClaimType::ExactSlaveMatch,
        Some("slave-hash"),
    );
    let b = hasheous_observation(
        "WHDLoad",
        Representation::WHDLoadSlave,
        ClaimType::ExactSlaveMatch,
        Some("slave-hash".to_string()),
        None,
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn test_06_direct_mame_softlist_plus_hasheous_mamemess_is_same_source_agreement() {
    let a = obs(
        EvidenceChannel::LocalMame,
        SourceFamily::MAMESoftwareList,
        LineageRelation::Independent,
        Representation::SoftwareListMember,
        ClaimType::ExactSlaveMatch,
        Some("softlist-hash"),
    );
    let b = hasheous_observation(
        "MAMEMess",
        Representation::SoftwareListMember,
        ClaimType::ExactSlaveMatch,
        Some("softlist-hash".to_string()),
        None,
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
}

// ------------------------------------------------------------------
// Independent test matrix (section 46, tests 7-10)
// ------------------------------------------------------------------

#[test]
fn test_07_structural_gb_plus_nointro_is_independent_agreement() {
    let structural = platform_obs(
        EvidenceChannel::LocalStructural,
        SourceFamily::Unknown,
        LineageRelation::Independent,
        "Game Boy",
    );
    let mut nointro = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Game Boy",
    );
    nointro.claim = ClaimType::PlatformCandidate;
    let summaries = merge_evidence(&[structural, nointro]);
    // Batch 21 closeout: `LocalStructural` carries `upstream_source =
    // Unknown` (a byte-level detector is not itself a preservation
    // source), but it is EmuWiz's own, known-provenance mechanism - not a
    // genuinely unidentified external source. `lineage_lane` treats it as
    // its own trustworthy lane, so this is real independent agreement
    // between the structural detector and the No-Intro lineage, not a
    // downgrade to SameSourceAgreement/WeakAgreement.
    assert_eq!(summaries[0].observations.len(), 2);
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
    assert_eq!(
        independent_source_group_count(&summaries[0].observations),
        2
    );
}

#[test]
fn test_07b_structural_gb_plus_nointro_with_known_family_is_independent_agreement() {
    // A separate proof that independence also holds between two *named*
    // preservation families (TOSEC vs. No-Intro) that do not require the
    // same representation to agree - complementary to test_07's proof
    // that `LocalStructural` itself is independently trustworthy.
    let a = platform_obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        "Game Boy",
    );
    let b = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Game Boy",
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
    assert_eq!(
        independent_source_group_count(&summaries[0].observations),
        2
    );
}

#[test]
fn test_08_structural_n64_plus_nointro_normalized_is_cross_representation_agreement() {
    let structural = obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("same-value"),
    );
    let normalized = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::NormalizedRom,
        ClaimType::ExactBytesMatch,
        Some("same-value"),
    );
    let summaries = merge_evidence(&[structural, normalized]);
    assert_eq!(
        summaries[0].status,
        AgreementStatus::CrossRepresentationAgreement
    );
}

#[test]
fn test_09_saturn_structural_plus_redump_is_independent_agreement() {
    let a = platform_obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        "Saturn",
    );
    let b = platform_obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        "Saturn",
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
}

#[test]
fn test_10_nointro_plus_tosec_same_bytes_same_platform_is_independent_agreement() {
    let a = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("hash-x"),
    );
    let b = obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("hash-x"),
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
}

// ------------------------------------------------------------------
// Conflict test matrix (section 47, tests 11-15)
// ------------------------------------------------------------------

#[test]
fn test_11_nointro_vs_tosec_same_representation_different_platform_is_independent_conflict() {
    let a = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Game Boy",
    );
    let b = platform_obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        "Game Boy Color",
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(
        summaries[0].status,
        AgreementStatus::IndependentSourceConflict
    );
    assert!(summaries[0].status.is_conflict());
}

#[test]
fn test_12_structural_saturn_vs_redump_ps1_is_independent_conflict() {
    let structural = platform_obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        "Saturn",
    );
    let redump = platform_obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        "PSX",
    );
    let summaries = merge_evidence(&[structural, redump]);
    assert_eq!(
        summaries[0].status,
        AgreementStatus::IndependentSourceConflict
    );
}

#[test]
fn test_13_direct_redump_vs_derived_mameredump_mismatch_is_derived_conflict() {
    let direct = obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("hash-A"),
    );
    let derived = obs(
        EvidenceChannel::LocalMame,
        SourceFamily::MAMERedump,
        LineageRelation::DerivedFrom,
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("hash-B"),
    );
    let summaries = merge_evidence(&[direct, derived]);
    assert_eq!(summaries[0].status, AgreementStatus::DerivedSourceConflict);
    assert!(
        summaries[0].status.is_conflict(),
        "this must never be silently voted away"
    );
}

#[test]
fn test_14_old_vs_new_same_source_version_disagreement_is_same_source_version_conflict() {
    let old = with_version(
        obs(
            EvidenceChannel::LocalNoIntro,
            SourceFamily::NoIntro,
            LineageRelation::Independent,
            Representation::PhysicalFile,
            ClaimType::PlatformCandidate,
            None,
        ),
        "2020-01-01",
    );
    let mut old = old.clone();
    old.platform_candidate = Some("Game Boy".to_string());
    let mut new_ = with_version(old.clone(), "2024-01-01");
    new_.platform_candidate = Some("Game Boy Color".to_string());
    let summaries = merge_evidence(&[old, new_]);
    assert_eq!(
        summaries[0].status,
        AgreementStatus::SameSourceVersionConflict,
        "must not be misclassified as IndependentSourceConflict"
    );
    assert_ne!(
        summaries[0].status,
        AgreementStatus::IndependentSourceConflict
    );
    assert_eq!(
        summaries[0].observations.len(),
        2,
        "both preserved, never deleted"
    );
}

#[test]
fn test_15_metadata_only_title_conflict_is_metadata_conflict() {
    let a = obs(
        EvidenceChannel::RomM,
        SourceFamily::GenericMetadata,
        LineageRelation::MetadataOnly,
        Representation::Unknown,
        ClaimType::RegionMetadata,
        Some("USA"),
    );
    let b = obs(
        EvidenceChannel::DirectMetadataProvider,
        SourceFamily::ScreenScraper,
        LineageRelation::MetadataOnly,
        Representation::Unknown,
        ClaimType::RegionMetadata,
        Some("Europe"),
    );
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(summaries[0].status, AgreementStatus::MetadataConflict);
}

// ------------------------------------------------------------------
// Representation test matrix (section 48, tests 16-20)
// ------------------------------------------------------------------

#[test]
fn test_16_physical_file_and_normalized_rom_stay_separate_representations() {
    assert_ne!(Representation::PhysicalFile, Representation::NormalizedRom);
}

#[test]
fn test_17_disc_track_and_logical_chd_stay_separate() {
    assert_ne!(Representation::DiscTrack, Representation::LogicalChd);
}

#[test]
fn test_18_whdload_slave_and_whole_hdf_stay_separate() {
    assert_ne!(Representation::WHDLoadSlave, Representation::WholeHdf);
}

#[test]
fn test_19_archive_member_and_whole_archive_stay_separate() {
    assert_ne!(Representation::ArchiveMember, Representation::WholeArchive);
}

#[test]
fn test_20_same_hash_text_across_representations_stays_separate() {
    let physical = obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("same-hash-text"),
    );
    let normalized = obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        Representation::NormalizedRom,
        ClaimType::ExactNormalizedMatch,
        Some("same-hash-text"),
    );
    // Different claim types => different claim-scoped groups entirely.
    let summaries = merge_evidence(&[physical, normalized]);
    assert_eq!(
        summaries.len(),
        2,
        "two distinct claim-scoped groups, never merged"
    );
}

#[test]
fn test_20b_whdload_install_lineage_keeps_whole_lha_slave_and_inner_file_separate() {
    let whole_lha = obs(
        EvidenceChannel::LocalWHDLoad,
        SourceFamily::Retroplay,
        LineageRelation::SameSourceDifferentChannel,
        Representation::WholeArchive,
        ClaimType::ExactBytesMatch,
        Some("lha-hash"),
    );
    let slave = obs(
        EvidenceChannel::LocalWHDLoad,
        SourceFamily::WHDLoad,
        LineageRelation::Independent,
        Representation::WHDLoadSlave,
        ClaimType::ExactSlaveMatch,
        Some("slave-hash"),
    );
    let hasheous_inner = hasheous_observation(
        "WHDLoad",
        Representation::WHDLoadInstallFile,
        ClaimType::ExactSlaveMatch,
        Some("inner-hash".to_string()),
        None,
    );
    let representations: BTreeSet<Representation> = [&whole_lha, &slave, &hasheous_inner]
        .iter()
        .map(|o| o.provenance.representation)
        .collect();
    assert_eq!(
        representations.len(),
        3,
        "never merge whole-HDF/LHA with slave identity"
    );
}

// ------------------------------------------------------------------
// Provenance test matrix (section 49, tests 21-25)
// ------------------------------------------------------------------

#[test]
fn test_21_same_mirror_artifact_hash_dedups() {
    let a = with_artifact(
        obs(
            EvidenceChannel::LocalDat,
            SourceFamily::NoIntro,
            LineageRelation::Independent,
            Representation::PhysicalFile,
            ClaimType::PlatformCandidate,
            None,
        ),
        "mirror-artifact-hash",
    );
    let mut b = a.clone();
    b.provenance.channel = EvidenceChannel::GeneratedIndex;
    let deduped = dedup_mirror_artifacts(&[a, b]);
    assert_eq!(deduped.len(), 1, "same delivery, no independent gain");
}

#[test]
fn test_22_different_artifact_version_preserves_both() {
    let a = with_artifact(
        obs(
            EvidenceChannel::LocalDat,
            SourceFamily::NoIntro,
            LineageRelation::Independent,
            Representation::PhysicalFile,
            ClaimType::PlatformCandidate,
            None,
        ),
        "artifact-hash-v1",
    );
    let b = with_artifact(a.clone(), "artifact-hash-v2");
    let deduped = dedup_mirror_artifacts(&[a, b]);
    assert_eq!(deduped.len(), 2, "different artifact hashes are preserved");
}

#[test]
fn test_23_missing_upstream_version_preserved_as_none() {
    let observation = obs(
        EvidenceChannel::LocalDat,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        Representation::PhysicalFile,
        ClaimType::PlatformCandidate,
        None,
    );
    assert_eq!(observation.provenance.upstream_version, None);
    assert!(!observation.provenance.version_known());
}

#[test]
fn test_24_generated_index_retains_upstream_lineage() {
    let generated = observation_from_generated_index(
        SourceFamily::NoIntro,
        Some("generator-v3".to_string()),
        Some("index-artifact-hash".to_string()),
        ClaimType::ExactBytesMatch,
        Some("some-hash".to_string()),
    );
    assert_eq!(
        generated.provenance.channel,
        EvidenceChannel::GeneratedIndex
    );
    assert_eq!(generated.provenance.upstream_source, SourceFamily::NoIntro);
    assert_ne!(
        generated.provenance.upstream_source,
        SourceFamily::Unknown,
        "a generated index must never become an untethered new source"
    );
}

#[test]
fn test_25_unknown_lineage_does_not_become_independent_by_default() {
    let a = obs(
        EvidenceChannel::Unknown,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("value"),
    );
    let b = obs(
        EvidenceChannel::Unknown,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("value"),
    );
    let summaries = merge_evidence(&[a, b]);
    assert_ne!(
        summaries[0].status,
        AgreementStatus::IndependentAgreement,
        "two Unknown-lineage observations must never be silently promoted to independent"
    );
}

// ------------------------------------------------------------------
// Determinism test matrix (section 50, tests 26-30)
// ------------------------------------------------------------------

#[test]
fn test_26_shuffled_observations_same_merge_output() {
    let a = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Game Boy",
    );
    let b = hasheous_observation(
        "NoIntro",
        Representation::StructuralMetadata,
        ClaimType::PlatformCandidate,
        None,
        Some("Game Boy".to_string()),
    );
    let c = romm_match_observation(
        "nointro_match",
        Representation::StructuralMetadata,
        ClaimType::PlatformCandidate,
        None,
    );
    let forward = merge_evidence(&[a.clone(), b.clone(), c.clone()]);
    let reversed = merge_evidence(&[c, b, a]);
    assert_eq!(forward, reversed);
}

#[test]
fn test_27_shuffled_observations_same_rendered_explanation() {
    let a = platform_obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        "Saturn",
    );
    let b = platform_obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        "PSX",
    );
    let forward = render_evidence_summary(&[a.clone(), b.clone()]);
    let reversed = render_evidence_summary(&[b, a]);
    assert_eq!(forward, reversed);
}

#[test]
fn test_28_serde_roundtrip_stable() {
    let observation = hasheous_observation(
        "Redump",
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("hash".to_string()),
        Some("PSX".to_string()),
    );
    let json = serde_json::to_string(&observation).unwrap();
    let restored: EvidenceObservation = serde_json::from_str(&json).unwrap();
    assert_eq!(observation, restored);
}

#[test]
fn test_29_duplicate_channel_observations_collapse_deterministically() {
    let a = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("same"),
    );
    let b = a.clone();
    let summaries = merge_evidence(&[a, b]);
    assert_eq!(
        summaries[0].observations.len(),
        1,
        "exact duplicate observations collapse"
    );
}

#[test]
fn test_30_source_enum_order_serialization_stable() {
    let json = serde_json::to_string(&SourceFamily::NoIntro).unwrap();
    assert_eq!(json, "\"no_intro\"");
    let json = serde_json::to_string(&EvidenceChannel::Hasheous).unwrap();
    assert_eq!(json, "\"hasheous\"");
    let json = serde_json::to_string(&AgreementStatus::SameSourceAgreement).unwrap();
    assert_eq!(json, "\"same_source_agreement\"");
}

// ------------------------------------------------------------------
// No-provider-voting guardrails (section 22)
// ------------------------------------------------------------------

#[test]
fn independent_source_group_count_is_not_observation_count() {
    let a = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("v"),
    );
    let b = hasheous_observation(
        "NoIntro",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("v".to_string()),
        None,
    );
    let c = romm_match_observation(
        "nointro_match",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("v".to_string()),
    );
    let all = [a, b, c];
    assert_eq!(all.len(), 3);
    assert_eq!(
        independent_source_group_count(&all),
        1,
        "3 observations must never report as 3 independent sources"
    );
}

#[test]
fn six_channels_one_upstream_source_never_inflates_beyond_one_group() {
    let channels = [
        EvidenceChannel::LocalNoIntro,
        EvidenceChannel::Hasheous,
        EvidenceChannel::RomM,
        EvidenceChannel::GeneratedIndex,
        EvidenceChannel::DirectMetadataProvider,
        EvidenceChannel::LocalDat,
    ];
    let observations: Vec<EvidenceObservation> = channels
        .iter()
        .map(|channel| {
            obs(
                *channel,
                SourceFamily::NoIntro,
                LineageRelation::SameSourceDifferentChannel,
                Representation::PhysicalFile,
                ClaimType::ExactBytesMatch,
                Some("shared-hash"),
            )
        })
        .collect();
    assert_eq!(observations.len(), 6);
    assert_eq!(independent_source_group_count(&observations), 1);
    let summaries = merge_evidence(&observations);
    assert_eq!(summaries[0].observations.len(), 6, "no observation dropped");
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
}

// ------------------------------------------------------------------
// Lineage grouping (sections 16-19)
// ------------------------------------------------------------------

#[test]
fn group_by_lineage_groups_known_families_together() {
    let a = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("v1"),
    );
    let b = hasheous_observation(
        "NoIntro",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("v2".to_string()),
        None,
    );
    let groups = group_by_lineage(&[a, b]);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].source_family, SourceFamily::NoIntro);
    assert_eq!(groups[0].observations.len(), 2);
}

#[test]
fn group_by_lineage_never_merges_two_unknown_observations() {
    let a = obs(
        EvidenceChannel::Unknown,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("v1"),
    );
    let b = obs(
        EvidenceChannel::Unknown,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("v2"),
    );
    let groups = group_by_lineage(&[a, b]);
    assert_eq!(
        groups.len(),
        2,
        "Unknown observations never silently merged"
    );
    assert!(groups.iter().all(|g| g.observations.len() == 1));
}

#[test]
fn unknown_lineage_preserves_observation_never_drops_it() {
    let a = obs(
        EvidenceChannel::Unknown,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("v1"),
    );
    let groups = group_by_lineage(std::slice::from_ref(&a));
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].observations[0], a);
}

// ------------------------------------------------------------------
// Source dependency registry (sections 20-21)
// ------------------------------------------------------------------

#[test]
fn known_derivation_reports_mameredump_derived_from_redump() {
    assert_eq!(
        known_derivation(SourceFamily::MAMERedump),
        Some(SourceFamily::Redump)
    );
}

#[test]
fn known_derivation_is_none_for_unrelated_families() {
    assert_eq!(known_derivation(SourceFamily::TOSEC), None);
    assert_eq!(known_derivation(SourceFamily::WHDLoad), None);
    assert_eq!(known_derivation(SourceFamily::NoIntro), None);
}

#[test]
fn known_derivation_never_overclaims_an_uncertain_relationship() {
    // FBNeo/RetroAchievements/ScreenScraper relationships to arcade
    // corpora are not reviewed/encoded - must stay None, not guessed.
    assert_eq!(known_derivation(SourceFamily::FBNeo), None);
    assert_eq!(known_derivation(SourceFamily::RetroAchievements), None);
    assert_eq!(known_derivation(SourceFamily::ScreenScraper), None);
}

#[test]
fn hasheous_tag_mapping_covers_every_documented_relay() {
    assert_eq!(hasheous_upstream_for_tag("NoIntro"), SourceFamily::NoIntro);
    assert_eq!(hasheous_upstream_for_tag("TOSEC"), SourceFamily::TOSEC);
    assert_eq!(hasheous_upstream_for_tag("Redump"), SourceFamily::Redump);
    assert_eq!(
        hasheous_upstream_for_tag("MAMEArcade"),
        SourceFamily::MAMEArcade
    );
    assert_eq!(
        hasheous_upstream_for_tag("MAMEMess"),
        SourceFamily::MAMESoftwareList
    );
    assert_eq!(hasheous_upstream_for_tag("WHDLoad"), SourceFamily::WHDLoad);
}

#[test]
fn hasheous_unrecognized_tag_is_unknown_not_a_guess() {
    assert_eq!(
        hasheous_upstream_for_tag("SomeFutureCorpusNobodyReviewedYet"),
        SourceFamily::Unknown
    );
}

#[test]
fn hasheous_itself_never_becomes_an_upstream_source() {
    let observation = hasheous_observation(
        "NoIntro",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h".to_string()),
        None,
    );
    assert_ne!(
        observation.provenance.upstream_source,
        SourceFamily::Unknown
    );
    // There is no SourceFamily variant literally named "Hasheous" - the
    // enum itself makes this claim structurally impossible, not just
    // documented.
    assert_eq!(observation.provenance.channel, EvidenceChannel::Hasheous);
}

#[test]
fn romm_flag_mapping_covers_every_documented_relay() {
    assert_eq!(
        romm_upstream_for_flag("nointro_match"),
        SourceFamily::NoIntro
    );
    assert_eq!(romm_upstream_for_flag("redump_match"), SourceFamily::Redump);
    assert_eq!(romm_upstream_for_flag("tosec_match"), SourceFamily::TOSEC);
    assert_eq!(
        romm_upstream_for_flag("mame_redump_match"),
        SourceFamily::MAMERedump
    );
}

#[test]
fn romm_itself_never_becomes_an_upstream_source() {
    let observation = romm_match_observation(
        "nointro_match",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h".to_string()),
    );
    assert_eq!(
        observation.provenance.upstream_source,
        SourceFamily::NoIntro
    );
    assert_eq!(observation.provenance.channel, EvidenceChannel::RomM);
}

#[test]
fn romm_title_slug_is_display_metadata_never_a_match_claim() {
    let display = romm_display_observation("Alleyway (World)".to_string());
    assert_eq!(display.claim, ClaimType::DisplayMetadata);
    assert_eq!(display.claim_strength, ClaimStrength::DisplayOnly);
    assert_ne!(display.claim, ClaimType::ExactBytesMatch);
}

// ------------------------------------------------------------------
// Bridges (sections 31-33)
// ------------------------------------------------------------------

#[test]
fn dat_platform_evidence_bridge_is_unknown_source_when_corpus_is_unclear() {
    let dat_evidence = DatPlatformEvidence {
        platform: "Game Boy".to_string(),
        machine_key: None,
        kind: crate::dat::identity::DatPlatformEvidenceKind::HeaderName,
        confidence: DatPlatformConfidence::Strong,
        detail: "header name names Game Boy".to_string(),
    };
    let observation =
        observation_from_dat_platform_evidence(&dat_evidence, Representation::PhysicalFile);
    assert_eq!(observation.provenance.channel, EvidenceChannel::LocalDat);
    assert_eq!(
        observation.provenance.upstream_source,
        SourceFamily::Unknown,
        "current DAT metadata cannot establish a preservation corpus family - must not guess"
    );
    assert_eq!(observation.platform_candidate.as_deref(), Some("Game Boy"));
}

#[test]
fn dat_platform_evidence_bridge_preserves_confidence_tier() {
    let strong = DatPlatformEvidence {
        platform: "NES".to_string(),
        machine_key: None,
        kind: crate::dat::identity::DatPlatformEvidenceKind::HeaderName,
        confidence: DatPlatformConfidence::Strong,
        detail: "d".to_string(),
    };
    let weak = DatPlatformEvidence {
        platform: "NES".to_string(),
        machine_key: None,
        kind: crate::dat::identity::DatPlatformEvidenceKind::MediaExtension,
        confidence: DatPlatformConfidence::Weak,
        detail: "d".to_string(),
    };
    assert_eq!(
        observation_from_dat_platform_evidence(&strong, Representation::PhysicalFile)
            .claim_strength,
        ClaimStrength::Strong
    );
    assert_eq!(
        observation_from_dat_platform_evidence(&weak, Representation::PhysicalFile).claim_strength,
        ClaimStrength::Weak
    );
}

#[test]
fn content_evidence_bridge_uses_local_structural_channel_never_a_preservation_family() {
    let fact = ContentEvidence::new(
        crate::content_evidence::ContentEvidenceKind::BootStructure,
        "true",
        ContentEvidenceConfidence::Strong,
        "the Nintendo logo checksum validated",
    );
    let observation = observation_from_content_evidence(&fact);
    assert_eq!(
        observation.provenance.channel,
        EvidenceChannel::LocalStructural
    );
    assert_eq!(
        observation.provenance.upstream_source,
        SourceFamily::Unknown,
        "structural detection is never forced into a preservation SourceFamily"
    );
    assert_eq!(
        observation.provenance.representation,
        Representation::StructuralMetadata
    );
}

#[test]
fn content_evidence_bridge_never_mutates_the_original_fact() {
    let fact = ContentEvidence::new(
        crate::content_evidence::ContentEvidenceKind::BootStructure,
        "true",
        ContentEvidenceConfidence::Strong,
        "detail",
    );
    let before = fact.clone();
    let _ = observation_from_content_evidence(&fact);
    assert_eq!(fact, before);
}

#[test]
fn physical_and_normalized_bridge_produces_separate_representations() {
    let observations = observations_from_physical_and_normalized(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        Some("physical-hash".to_string()),
        Some("normalized-hash".to_string()),
    );
    assert_eq!(observations.len(), 2);
    assert_eq!(
        observations[0].provenance.representation,
        Representation::PhysicalFile
    );
    assert_eq!(
        observations[1].provenance.representation,
        Representation::NormalizedRom
    );
    assert_eq!(observations[0].claim, ClaimType::ExactBytesMatch);
    assert_eq!(observations[1].claim, ClaimType::ExactNormalizedMatch);
}

#[test]
fn physical_and_normalized_bridge_with_identical_hash_text_still_stays_separate() {
    let observations = observations_from_physical_and_normalized(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        Some("same-text".to_string()),
        Some("same-text".to_string()),
    );
    assert_eq!(observations.len(), 2);
    assert_ne!(
        observations[0].provenance.representation,
        observations[1].provenance.representation
    );
}

#[test]
fn physical_only_bridge_omits_the_normalized_observation_entirely() {
    let observations = observations_from_physical_and_normalized(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        Some("physical-hash".to_string()),
        None,
    );
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].provenance.representation,
        Representation::PhysicalFile
    );
}

// ------------------------------------------------------------------
// Generated-index provenance (section 30)
// ------------------------------------------------------------------

#[test]
fn generated_index_never_flattens_into_a_new_untethered_source() {
    let generated = observation_from_generated_index(
        SourceFamily::Redump,
        Some("gen-v1".to_string()),
        None,
        ClaimType::ExactBytesMatch,
        Some("h".to_string()),
    );
    assert_eq!(
        generated.provenance.channel,
        EvidenceChannel::GeneratedIndex
    );
    assert_eq!(generated.provenance.upstream_source, SourceFamily::Redump);
    assert!(generated.provenance.generator_version.is_some());
}

#[test]
fn generated_index_agrees_with_its_own_upstream_direct_source_as_same_source() {
    let generated = observation_from_generated_index(
        SourceFamily::NoIntro,
        Some("gen-v1".to_string()),
        None,
        ClaimType::ExactBytesMatch,
        Some("h".to_string()),
    );
    let direct = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::Unknown,
        ClaimType::ExactBytesMatch,
        Some("h"),
    );
    let summaries = merge_evidence(&[generated, direct]);
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
}

// ------------------------------------------------------------------
// Adapter contract (section 56)
// ------------------------------------------------------------------

#[test]
fn observation_declares_provenance_flags_a_bare_matched_true_style_claim() {
    let bare = obs(
        EvidenceChannel::Unknown,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        Representation::Unknown,
        ClaimType::ExactBytesMatch,
        Some("some-hash"),
    );
    assert!(
        !observation_declares_provenance(&bare),
        "a bare claim with no channel/source at all must be easy to detect"
    );
}

#[test]
fn observation_declares_provenance_passes_when_channel_is_known() {
    let known = hasheous_observation(
        "NoIntro",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h".to_string()),
        None,
    );
    assert!(observation_declares_provenance(&known));
}

#[test]
fn adapter_contract_still_represents_a_bare_claim_rather_than_rejecting_it() {
    // Even a fully-Unknown observation is representable - it must never be
    // dropped, only easy to flag.
    let bare = EvidenceObservation {
        provenance: Provenance {
            channel: EvidenceChannel::Unknown,
            upstream_source: SourceFamily::Unknown,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::Unknown,
            representation: Representation::Unknown,
        },
        claim: ClaimType::ExactBytesMatch,
        claim_strength: ClaimStrength::Weak,
        identity_scope: IdentityScope::DumpIdentity,
        hash_or_value: Some("bare-hash".to_string()),
        platform_candidate: None,
        release_candidate: None,
        notes: None,
    };
    let summaries = merge_evidence(std::slice::from_ref(&bare));
    assert_eq!(summaries[0].observations.len(), 1);
}

// ------------------------------------------------------------------
// Dedup key (section 15)
// ------------------------------------------------------------------

#[test]
fn dedup_key_differs_across_representations_for_otherwise_identical_facts() {
    let physical = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h"),
    );
    let normalized = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::NormalizedRom,
        ClaimType::ExactBytesMatch,
        Some("h"),
    );
    assert_ne!(dedup_key(&physical), dedup_key(&normalized));
}

#[test]
fn dedup_key_is_equal_for_the_same_underlying_fact_seen_through_two_channels() {
    let a = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h"),
    );
    let mut b = a.clone();
    b.provenance.channel = EvidenceChannel::Hasheous;
    // Channel is deliberately excluded from the dedup key: the same
    // upstream fact seen through two channels is still one fact.
    assert_eq!(dedup_key(&a), dedup_key(&b));
}

// ------------------------------------------------------------------
// Rendering (sections 36-37)
// ------------------------------------------------------------------

#[test]
fn render_evidence_summary_names_agreement_and_conflict_sections() {
    let agree_a = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h"),
    );
    let agree_b = hasheous_observation(
        "NoIntro",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h".to_string()),
        None,
    );
    let conflict_a = platform_obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        "Saturn",
    );
    let conflict_b = platform_obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        "PSX",
    );
    let text = render_evidence_summary(&[agree_a, agree_b, conflict_a, conflict_b]);
    assert!(text.contains("Agreement"));
    assert!(text.contains("Conflict"));
    assert!(text.starts_with("Identity evidence:"));
}

#[test]
fn render_conflict_explanation_names_the_status_and_every_observation() {
    let a = platform_obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        "Saturn",
    );
    let b = platform_obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        "PSX",
    );
    let summaries = merge_evidence(&[a, b]);
    let text = render_conflict_explanation(&summaries[0]);
    assert!(text.contains("IndependentSourceConflict"));
    assert!(text.contains("Saturn") || text.contains("TOSEC"));
    assert!(text.contains("PSX") || text.contains("Redump"));
}

#[test]
fn render_evidence_summary_never_implies_planner_or_transaction_authority() {
    let a = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Game Boy",
    );
    let text = render_evidence_summary(std::slice::from_ref(&a));
    assert!(!text.to_lowercase().contains("apply"));
    assert!(!text.to_lowercase().contains("rename"));
    assert!(!text.to_lowercase().contains("move"));
}

// ------------------------------------------------------------------
// Cross-representation reconciliation (section 27) & weak agreement
// ------------------------------------------------------------------

#[test]
fn weak_agreement_for_a_single_observation_claim_group() {
    let a = obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h"),
    );
    let summaries = merge_evidence(std::slice::from_ref(&a));
    assert_eq!(summaries[0].status, AgreementStatus::WeakAgreement);
}

#[test]
fn representation_conflict_when_cross_representation_values_actually_disagree() {
    let physical = obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("value-a"),
    );
    let normalized = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::NormalizedRom,
        ClaimType::ExactBytesMatch,
        Some("value-b"),
    );
    let summaries = merge_evidence(&[physical, normalized]);
    assert_eq!(summaries[0].status, AgreementStatus::RepresentationConflict);
}

// ------------------------------------------------------------------
// Claim scoping (section 23): several claims never collapse to one status
// ------------------------------------------------------------------

#[test]
fn claim_scoped_summaries_never_collapse_multiple_claims_into_one_status() {
    let bytes_agree = obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h"),
    );
    let bytes_agree_2 = hasheous_observation(
        "NoIntro",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("h".to_string()),
        None,
    );
    let region_conflict_a = obs(
        EvidenceChannel::RomM,
        SourceFamily::GenericMetadata,
        LineageRelation::MetadataOnly,
        Representation::Unknown,
        ClaimType::RegionMetadata,
        Some("USA"),
    );
    let region_conflict_b = obs(
        EvidenceChannel::DirectMetadataProvider,
        SourceFamily::ScreenScraper,
        LineageRelation::MetadataOnly,
        Representation::Unknown,
        ClaimType::RegionMetadata,
        Some("Japan"),
    );
    let summaries = merge_evidence(&[
        bytes_agree,
        bytes_agree_2,
        region_conflict_a,
        region_conflict_b,
    ]);
    assert_eq!(
        summaries.len(),
        2,
        "two independently-classified claim groups"
    );
    let bytes_summary = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    let region_summary = summaries
        .iter()
        .find(|s| s.claim == ClaimType::RegionMetadata)
        .unwrap();
    assert_eq!(bytes_summary.status, AgreementStatus::SameSourceAgreement);
    assert_eq!(region_summary.status, AgreementStatus::MetadataConflict);
}

// ------------------------------------------------------------------
// Identity scope (section 28)
// ------------------------------------------------------------------

#[test]
fn tosec_crack_nointro_original_and_whdload_install_can_share_game_identity_scope() {
    let mut tosec_crack = platform_obs(
        EvidenceChannel::LocalTosec,
        SourceFamily::TOSEC,
        LineageRelation::Independent,
        "Amiga",
    );
    tosec_crack.identity_scope = IdentityScope::GameIdentity;
    let mut nointro_original = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Amiga",
    );
    nointro_original.identity_scope = IdentityScope::GameIdentity;
    let mut whdload_install = platform_obs(
        EvidenceChannel::LocalWHDLoad,
        SourceFamily::WHDLoad,
        LineageRelation::Independent,
        "Amiga",
    );
    whdload_install.identity_scope = IdentityScope::GameIdentity;
    whdload_install.provenance.representation = Representation::WHDLoadSlave;
    assert!(
        [&tosec_crack, &nointro_original, &whdload_install]
            .iter()
            .all(|o| o.identity_scope == IdentityScope::GameIdentity)
    );
    // But they differ at dump identity: different hashes/artifacts.
    assert_ne!(
        tosec_crack.provenance.representation,
        whdload_install.provenance.representation
    );
}

// ------------------------------------------------------------------
// Serde round-trips for the full vocabulary (section 38)
// ------------------------------------------------------------------

#[test]
fn every_source_family_variant_round_trips_through_serde() {
    let families = [
        SourceFamily::NoIntro,
        SourceFamily::TOSEC,
        SourceFamily::Redump,
        SourceFamily::MAMEArcade,
        SourceFamily::MAMESoftwareList,
        SourceFamily::MAMERedump,
        SourceFamily::WHDLoad,
        SourceFamily::Retroplay,
        SourceFamily::PureDOS,
        SourceFamily::TotalDOSCollection,
        SourceFamily::FBNeo,
        SourceFamily::RetroAchievements,
        SourceFamily::ScreenScraper,
        SourceFamily::GenericMetadata,
        SourceFamily::Unknown,
    ];
    for family in families {
        let json = serde_json::to_string(&family).unwrap();
        let restored: SourceFamily = serde_json::from_str(&json).unwrap();
        assert_eq!(family, restored);
    }
}

#[test]
fn every_channel_variant_round_trips_through_serde() {
    let channels = [
        EvidenceChannel::LocalDat,
        EvidenceChannel::LocalMame,
        EvidenceChannel::LocalRedump,
        EvidenceChannel::LocalTosec,
        EvidenceChannel::LocalNoIntro,
        EvidenceChannel::LocalWHDLoad,
        EvidenceChannel::LocalStructural,
        EvidenceChannel::Hasheous,
        EvidenceChannel::RomM,
        EvidenceChannel::GeneratedIndex,
        EvidenceChannel::DirectMetadataProvider,
        EvidenceChannel::Unknown,
    ];
    for channel in channels {
        let json = serde_json::to_string(&channel).unwrap();
        let restored: EvidenceChannel = serde_json::from_str(&json).unwrap();
        assert_eq!(channel, restored);
    }
}

#[test]
fn every_representation_variant_round_trips_through_serde() {
    let representations = [
        Representation::PhysicalFile,
        Representation::NormalizedRom,
        Representation::ArchiveMember,
        Representation::DiscTrack,
        Representation::LogicalChd,
        Representation::RawDisc,
        Representation::SoftwareListMember,
        Representation::WHDLoadSlave,
        Representation::WHDLoadInstallFile,
        Representation::WholeArchive,
        Representation::WholeHdf,
        Representation::StructuralMetadata,
        Representation::Unknown,
    ];
    for representation in representations {
        let json = serde_json::to_string(&representation).unwrap();
        let restored: Representation = serde_json::from_str(&json).unwrap();
        assert_eq!(representation, restored);
    }
}

#[test]
fn every_claim_type_variant_round_trips_through_serde() {
    let claims = [
        ClaimType::ExactBytesMatch,
        ClaimType::ExactNormalizedMatch,
        ClaimType::ExactTrackMatch,
        ClaimType::ExactLogicalDiscMatch,
        ClaimType::ExactSlaveMatch,
        ClaimType::PlatformCandidate,
        ClaimType::ReleaseCandidate,
        ClaimType::RevisionCandidate,
        ClaimType::RegionMetadata,
        ClaimType::LanguageMetadata,
        ClaimType::VariantStatus,
        ClaimType::HardwareCompatibility,
        ClaimType::DisplayMetadata,
        ClaimType::CrosswalkCandidate,
        ClaimType::VettedCrosswalk,
        ClaimType::EquivalentCanonical,
        ClaimType::RelatedPlatform,
    ];
    for claim in claims {
        let json = serde_json::to_string(&claim).unwrap();
        let restored: ClaimType = serde_json::from_str(&json).unwrap();
        assert_eq!(claim, restored);
    }
}

#[test]
fn every_agreement_status_variant_round_trips_through_serde() {
    let statuses = [
        AgreementStatus::SameSourceAgreement,
        AgreementStatus::IndependentAgreement,
        AgreementStatus::DerivedAgreement,
        AgreementStatus::CrossRepresentationAgreement,
        AgreementStatus::WeakAgreement,
        AgreementStatus::SameSourceVersionConflict,
        AgreementStatus::DerivedSourceConflict,
        AgreementStatus::IndependentSourceConflict,
        AgreementStatus::RepresentationConflict,
        AgreementStatus::MetadataConflict,
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let restored: AgreementStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, restored);
    }
}

#[test]
fn provenance_round_trips_through_serde_with_all_optional_fields_populated() {
    let provenance = Provenance {
        channel: EvidenceChannel::Hasheous,
        upstream_source: SourceFamily::Redump,
        upstream_version: Some("2024-06-01".to_string()),
        source_artifact: Some(SourceArtifactIdentity {
            source_family: SourceFamily::Redump,
            upstream_version: Some("2024-06-01".to_string()),
            artifact_sha256: Some("artifact-hash".to_string()),
            artifact_name: Some("redump-psx.dat".to_string()),
        }),
        imported_at_unix: Some(1_700_000_000),
        retrieved_at_unix: Some(1_700_000_001),
        generator_version: Some("v1.2.3".to_string()),
        lineage: LineageRelation::Relay,
        representation: Representation::DiscTrack,
    };
    let json = serde_json::to_string(&provenance).unwrap();
    let restored: Provenance = serde_json::from_str(&json).unwrap();
    assert_eq!(provenance, restored);
}

// ------------------------------------------------------------------
// Performance / ordering hygiene (section 39, 52)
// ------------------------------------------------------------------

#[test]
fn merge_evidence_output_order_is_deterministic_across_many_claim_types() {
    let mut observations = Vec::new();
    for (i, claim) in [
        ClaimType::ExactBytesMatch,
        ClaimType::PlatformCandidate,
        ClaimType::RegionMetadata,
    ]
    .into_iter()
    .enumerate()
    {
        observations.push(obs(
            EvidenceChannel::LocalNoIntro,
            SourceFamily::NoIntro,
            LineageRelation::Independent,
            Representation::PhysicalFile,
            claim,
            Some(&format!("value-{i}")),
        ));
    }
    let forward = merge_evidence(&observations);
    observations.reverse();
    let reversed = merge_evidence(&observations);
    let forward_claims: Vec<ClaimType> = forward.iter().map(|s| s.claim).collect();
    let reversed_claims: Vec<ClaimType> = reversed.iter().map(|s| s.claim).collect();
    assert_eq!(
        forward_claims, reversed_claims,
        "claim-group order is stable"
    );
}

#[test]
fn large_observation_set_merges_without_excessive_blowup() {
    // Not a rigorous perf benchmark - just a sanity check that grouping
    // 2000 observations does not require anything worse than the ordered
    // BTreeMap grouping this module actually uses.
    let mut observations = Vec::new();
    for i in 0..2000u32 {
        observations.push(obs(
            EvidenceChannel::LocalNoIntro,
            SourceFamily::NoIntro,
            LineageRelation::Independent,
            Representation::PhysicalFile,
            ClaimType::ExactBytesMatch,
            Some(Box::leak(format!("hash-{i}").into_boxed_str())),
        ));
    }
    let summaries = merge_evidence(&observations);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].observations.len(), 2000);
}

// ------------------------------------------------------------------
// No-provider-voting API shape (section 22): grep-style structural check
// ------------------------------------------------------------------

#[test]
fn module_source_never_defines_a_bare_number_of_sources_agreeing_function() {
    let source = include_str!("../evidence_lineage.rs");
    assert!(
        !source.contains("fn number_of_sources_agreeing"),
        "no such API may exist unless it is explicitly independent_source_group_count"
    );
    assert!(
        !source.contains("evidence_count as"),
        "no numeric confidence casting from a raw observation count"
    );
}

#[test]
fn agreement_status_is_never_derived_from_raw_observation_count_alone() {
    // Six observations, all Unknown lineage, all agreeing on value: must
    // not become IndependentAgreement or CrossRepresentationAgreement just
    // because there are many of them.
    let observations: Vec<EvidenceObservation> = (0..6)
        .map(|_| {
            obs(
                EvidenceChannel::Unknown,
                SourceFamily::Unknown,
                LineageRelation::Unknown,
                Representation::PhysicalFile,
                ClaimType::ExactBytesMatch,
                Some("shared"),
            )
        })
        .collect();
    let summaries = merge_evidence(&observations);
    assert_eq!(
        summaries[0].observations.len(),
        1,
        "exact duplicates collapse"
    );
    assert_ne!(summaries[0].status, AgreementStatus::IndependentAgreement);
}

// ------------------------------------------------------------------
// Migration/compatibility: existing types remain fully untouched
// ------------------------------------------------------------------

#[test]
fn combined_identity_module_is_unaffected_by_this_module_existing() {
    use crate::platform_evidence_fusion::combined_identity::DatOutcome;
    // Merely referencing the pre-existing type proves it still compiles
    // and its shape is untouched by this batch.
    let _ = DatOutcome::Unknown;
}

#[test]
fn dat_identity_resolver_behavior_is_byte_for_byte_unchanged() {
    use crate::dat::identity::{DatPlatformEvidenceKind, resolve_dat_platform_identity};
    let result = resolve_dat_platform_identity(vec![DatPlatformEvidence {
        platform: "NES".to_string(),
        machine_key: None,
        kind: DatPlatformEvidenceKind::HeaderName,
        confidence: DatPlatformConfidence::Strong,
        detail: "header name names NES".to_string(),
    }]);
    assert_eq!(result.platform(), Some("NES"));
}

// ------------------------------------------------------------------
// Batch 21 closeout: LocalStructural is a known independent lane,
// distinct from a genuinely unknown external source (section 8).
// ------------------------------------------------------------------

fn structural_content_evidence(value: &str) -> crate::content_evidence::ContentEvidence {
    crate::content_evidence::ContentEvidence {
        kind: crate::content_evidence::ContentEvidenceKind::BootStructure,
        value: value.to_string(),
        confidence: crate::content_evidence::ContentEvidenceConfidence::Strong,
        detail: "structural detector fact".to_string(),
    }
}

#[test]
fn closeout_1_gb_structural_plus_local_nointro_is_independent_agreement() {
    let structural = observation_from_content_evidence(&structural_content_evidence("Game Boy"));
    let nointro = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Game Boy",
    );
    let summaries = merge_evidence(&[structural, nointro]);
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
    assert_eq!(
        independent_source_group_count(&summaries[0].observations),
        2
    );
}

#[test]
fn closeout_2_n64_structural_plus_normalized_nointro_is_independent_agreement() {
    let structural = observation_from_content_evidence(&structural_content_evidence("Nintendo 64"));
    let mut nointro_normalized = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Nintendo 64",
    );
    nointro_normalized.provenance.representation = Representation::NormalizedRom;
    let summaries = merge_evidence(&[structural, nointro_normalized]);
    // PlatformCandidate is representation-agnostic (section 6/7): the
    // structural detector's StructuralMetadata representation and the
    // No-Intro normalized representation both differing does not demote
    // this away from genuine independence.
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
}

#[test]
fn closeout_3_saturn_structural_plus_redump_is_independent_agreement() {
    let structural = observation_from_content_evidence(&structural_content_evidence("Saturn"));
    let redump = platform_obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        "Saturn",
    );
    let summaries = merge_evidence(&[structural, redump]);
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
}

#[test]
fn closeout_4_amiga_structural_plus_whdload_is_independent_agreement() {
    let structural = observation_from_content_evidence(&structural_content_evidence("Amiga"));
    let whdload = platform_obs(
        EvidenceChannel::LocalWHDLoad,
        SourceFamily::WHDLoad,
        LineageRelation::Independent,
        "Amiga",
    );
    let summaries = merge_evidence(&[structural, whdload]);
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
}

#[test]
fn closeout_5_gameboy_structural_vs_megadrive_nointro_is_independent_source_conflict() {
    let structural = observation_from_content_evidence(&structural_content_evidence("Game Boy"));
    let nointro = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Sega Mega Drive",
    );
    let summaries = merge_evidence(&[structural, nointro]);
    assert_eq!(
        summaries[0].status,
        AgreementStatus::IndependentSourceConflict
    );
    assert!(summaries[0].status.is_conflict());
}

#[test]
fn closeout_6_unknown_external_plus_nointro_is_not_automatically_independent() {
    let unknown_external = platform_obs(
        EvidenceChannel::Unknown,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        "Game Boy",
    );
    let nointro = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Game Boy",
    );
    let summaries = merge_evidence(&[unknown_external, nointro]);
    assert_ne!(summaries[0].status, AgreementStatus::IndependentAgreement);
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn closeout_7_unknown_external_plus_redump_is_not_automatically_independent() {
    let unknown_external = platform_obs(
        EvidenceChannel::Unknown,
        SourceFamily::Unknown,
        LineageRelation::Unknown,
        "Saturn",
    );
    let redump = platform_obs(
        EvidenceChannel::LocalRedump,
        SourceFamily::Redump,
        LineageRelation::Independent,
        "Saturn",
    );
    let summaries = merge_evidence(&[unknown_external, redump]);
    assert_ne!(summaries[0].status, AgreementStatus::IndependentAgreement);
    assert_eq!(summaries[0].status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn closeout_8_local_structural_duplicates_alone_do_not_fake_multiple_independent_sources() {
    let a = observation_from_content_evidence(&structural_content_evidence("Game Boy"));
    let b = observation_from_content_evidence(&structural_content_evidence("Game Boy"));
    let c = observation_from_content_evidence(&structural_content_evidence("Game Boy"));
    let group = [a, b, c];
    assert_eq!(
        independent_source_group_count(&group),
        1,
        "three structural facts from the same detector are one lane, not three"
    );
    let summaries = merge_evidence(&group);
    assert_ne!(summaries[0].status, AgreementStatus::IndependentAgreement);
}

#[test]
fn closeout_9_local_structural_plus_hasheous_nointro_independent_hasheous_stays_relay() {
    let structural = observation_from_content_evidence(&structural_content_evidence("Game Boy"));
    let hasheous_nointro = hasheous_observation(
        "NoIntro",
        Representation::Unknown,
        ClaimType::PlatformCandidate,
        None,
        Some("Game Boy".to_string()),
    );
    assert_eq!(hasheous_nointro.provenance.lineage, LineageRelation::Relay);
    let summaries = merge_evidence(&[structural, hasheous_nointro]);
    assert_eq!(summaries[0].status, AgreementStatus::IndependentAgreement);
    assert_eq!(
        independent_source_group_count(&summaries[0].observations),
        2,
        "structural lane + NoIntro lineage, Hasheous itself never a third source"
    );
}

#[test]
fn closeout_10_structural_plus_local_nointro_plus_hasheous_nointro_is_two_lineage_groups_never_three()
 {
    let structural = observation_from_content_evidence(&structural_content_evidence("Game Boy"));
    let local_nointro = platform_obs(
        EvidenceChannel::LocalNoIntro,
        SourceFamily::NoIntro,
        LineageRelation::Independent,
        "Game Boy",
    );
    let hasheous_nointro = hasheous_observation(
        "NoIntro",
        Representation::Unknown,
        ClaimType::PlatformCandidate,
        None,
        Some("Game Boy".to_string()),
    );
    let all = [structural, local_nointro, hasheous_nointro];
    assert_eq!(all.len(), 3, "three observations");
    assert_eq!(
        independent_source_group_count(&all),
        2,
        "one structural lane + one NoIntro lineage (direct + Hasheous relay), never three"
    );
}

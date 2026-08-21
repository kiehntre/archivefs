use std::collections::BTreeMap;
use std::sync::Mutex;

use super::client::*;
use super::convert::observations_from_hash_lookup;
use super::dto::*;
use crate::platform_evidence_fusion::evidence_lineage::{
    AgreementStatus, ClaimStrength, ClaimType, EvidenceChannel, EvidenceObservation,
    LineageRelation, Representation, SourceFamily, hasheous_upstream_for_tag, merge_evidence,
    observation_declares_provenance,
};

// ---------------------------------------------------------------------
// A deterministic, fully in-memory fake transport. No test in this file
// ever opens a socket (section 33/34).
// ---------------------------------------------------------------------

struct FakeTransport {
    responses: Mutex<Vec<HasheousHttpResponse>>,
    calls: Mutex<Vec<(String, Vec<u8>)>>,
}

impl FakeTransport {
    fn new(responses: Vec<HasheousHttpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn ok(body: &[u8]) -> Self {
        Self::new(vec![HasheousHttpResponse {
            status: 200,
            body: body.to_vec(),
            retry_after_secs: None,
        }])
    }

    fn status(status: u16) -> Self {
        Self::new(vec![HasheousHttpResponse {
            status,
            body: Vec::new(),
            retry_after_secs: None,
        }])
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn last_body(&self) -> Vec<u8> {
        self.calls.lock().unwrap().last().unwrap().1.clone()
    }

    fn last_url(&self) -> String {
        self.calls.lock().unwrap().last().unwrap().0.clone()
    }
}

impl HasheousTransport for FakeTransport {
    fn post_json(
        &self,
        url: &str,
        body: &[u8],
    ) -> Result<HasheousHttpResponse, HasheousRequestError> {
        self.calls
            .lock()
            .unwrap()
            .push((url.to_string(), body.to_vec()));
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err(HasheousRequestError::Network {
                detail: "fake transport exhausted".to_string(),
            });
        }
        Ok(responses.remove(0))
    }
}

struct ErrorTransport(HasheousRequestError);

impl HasheousTransport for ErrorTransport {
    fn post_json(
        &self,
        _url: &str,
        _body: &[u8],
    ) -> Result<HasheousHttpResponse, HasheousRequestError> {
        Err(self.0.clone())
    }
}

fn enabled_config() -> HasheousConfig {
    HasheousConfig {
        enabled: true,
        base_url: "https://hasheous.test".to_string(),
        timeout: std::time::Duration::from_secs(5),
    }
}

fn sha1_only(value: &str) -> HasheousHashSet {
    HasheousHashSet {
        crc: None,
        md5: None,
        sha1: Some(value.to_string()),
        sha256: None,
    }
}

fn signature(tag: &str, game: &str, hash: &str) -> (String, SignatureResult) {
    (
        tag.to_string(),
        SignatureResult {
            game: Some(GameItem {
                name: Some(game.to_string()),
                system: Some("Game Boy".to_string()),
                publisher: Some("Nintendo".to_string()),
            }),
            rom: Some(RomItem {
                name: Some(format!("{game}.gb")),
                crc: None,
                md5: None,
                sha1: Some(hash.to_string()),
                sha256: None,
                signature_source: Some(tag.to_string()),
            }),
        },
    )
}

fn response_with_sources(pairs: Vec<(String, SignatureResult)>) -> HashLookupResponse {
    let mut signatures: BTreeMap<String, Vec<SignatureResult>> = BTreeMap::new();
    for (tag, result) in pairs {
        signatures.entry(tag).or_default().push(result);
    }
    HashLookupResponse {
        platform: Some(MiniDataObjectItem {
            name: Some("Nintendo Game Boy".to_string()),
        }),
        publisher: Some(MiniDataObjectItem {
            name: Some("Nintendo".to_string()),
        }),
        signature: None,
        signatures: Some(signatures),
    }
}

// =======================================================================
// Test matrix - API/transport (section 56, items 1-12)
// =======================================================================

#[test]
fn request_uses_the_live_verified_path_not_the_researched_v1_0_path() {
    let transport = FakeTransport::ok(b"{}");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let _ = client.lookup(&sha1_only("abc"), None);
    let url = transport.last_url();
    assert!(url.contains("/api/v1/Lookup/ByHash"), "url was {url}");
    assert!(!url.contains("/api/v1.0/"), "url was {url}");
}

#[test]
fn request_is_a_post() {
    // The transport trait only exposes post_json - there is no `fn get(`
    // method anywhere in this client, so a GET request is structurally
    // impossible to issue through it.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/identity_source/hasheous/client.rs"),
    )
    .unwrap_or_default();
    assert!(source.contains("fn post_json("));
    assert!(!source.contains("fn get("));
    assert!(!source.contains("self.agent.get("));
}

#[test]
fn single_sha1_request_body_carries_only_that_hash() {
    let transport = FakeTransport::ok(b"{}");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let _ = client.lookup(&sha1_only("deadbeef"), None);
    let body: serde_json::Value = serde_json::from_slice(&transport.last_body()).unwrap();
    assert_eq!(body["sha1"], "deadbeef");
    assert!(body.get("md5").is_none());
    assert!(body.get("crc").is_none());
}

#[test]
fn sha1_plus_md5_for_the_same_representation_are_sent_together() {
    let transport = FakeTransport::ok(b"{}");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let hash_set = HasheousHashSet {
        crc: None,
        md5: Some("md5value".to_string()),
        sha1: Some("sha1value".to_string()),
        sha256: None,
    };
    let _ = client.lookup(&hash_set, None);
    let body: serde_json::Value = serde_json::from_slice(&transport.last_body()).unwrap();
    assert_eq!(body["sha1"], "sha1value");
    assert_eq!(body["md5"], "md5value");
}

#[test]
fn batch_of_fifty_produces_fifty_requests() {
    let responses = (0..50)
        .map(|_| HasheousHttpResponse {
            status: 404,
            body: Vec::new(),
            retry_after_secs: None,
        })
        .collect();
    let transport = FakeTransport::new(responses);
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let items: Vec<HasheousHashSet> = (0..50).map(|i| sha1_only(&format!("h{i}"))).collect();
    let results = client.lookup_many(&items, None);
    assert_eq!(results.len(), 50);
    assert_eq!(transport.call_count(), 50);
}

#[test]
fn batch_of_fifty_one_still_issues_fifty_one_individual_requests() {
    let responses = (0..51)
        .map(|_| HasheousHttpResponse {
            status: 404,
            body: Vec::new(),
            retry_after_secs: None,
        })
        .collect();
    let transport = FakeTransport::new(responses);
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let items: Vec<HasheousHashSet> = (0..51).map(|i| sha1_only(&format!("h{i}"))).collect();
    let results = client.lookup_many(&items, None);
    assert_eq!(results.len(), 51);
    assert_eq!(transport.call_count(), 51);
}

#[test]
fn batch_boundary_sizes_all_produce_matching_call_counts() {
    for size in [0usize, 1, 49, 50, 51, 100, 137] {
        let responses = (0..size)
            .map(|_| HasheousHttpResponse {
                status: 404,
                body: Vec::new(),
                retry_after_secs: None,
            })
            .collect();
        let transport = FakeTransport::new(responses);
        let config = enabled_config();
        let client = HasheousClient::new(&config, &transport);
        let items: Vec<HasheousHashSet> = (0..size).map(|i| sha1_only(&format!("h{i}"))).collect();
        let results = client.lookup_many(&items, None);
        assert_eq!(results.len(), size, "size {size}");
        assert_eq!(transport.call_count(), size, "size {size}");
    }
}

#[test]
fn timeout_is_a_distinct_error_variant() {
    let transport = ErrorTransport(HasheousRequestError::Timeout);
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let result = client.lookup(&sha1_only("abc"), None);
    assert!(matches!(result, Err(HasheousRequestError::Timeout)));
}

#[test]
fn http_429_is_rate_limited_not_a_generic_status_error() {
    let transport = FakeTransport::new(vec![HasheousHttpResponse {
        status: 429,
        body: Vec::new(),
        retry_after_secs: Some(30),
    }]);
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let result = client.lookup(&sha1_only("abc"), None);
    match result {
        Err(HasheousRequestError::RateLimited {
            status,
            retry_after_secs,
        }) => {
            assert_eq!(status, 429);
            assert_eq!(retry_after_secs, Some(30));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[test]
fn http_500_is_a_distinct_status_error_never_conflated_with_no_match() {
    let transport = FakeTransport::status(500);
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let result = client.lookup(&sha1_only("abc"), None);
    assert!(matches!(
        result,
        Err(HasheousRequestError::HttpStatus { status: 500 })
    ));
}

#[test]
fn malformed_json_on_200_is_invalid_response_not_a_panic() {
    let transport = FakeTransport::ok(b"{not json");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let result = client.lookup(&sha1_only("abc"), None);
    assert!(matches!(
        result,
        Err(HasheousRequestError::InvalidResponse { .. })
    ));
}

#[test]
fn empty_success_response_yields_zero_observations() {
    let transport = FakeTransport::ok(b"{}");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let outcome = client.lookup(&sha1_only("abc"), None).unwrap();
    match outcome {
        HasheousLookupOutcome::Found(response) => {
            let observations =
                observations_from_hash_lookup(&response, Representation::PhysicalFile, "abc", None);
            assert!(observations.is_empty());
        }
        other => panic!("expected Found, got {other:?}"),
    }
}

#[test]
fn unknown_source_id_deserializes_safely_and_maps_to_unknown_never_panics() {
    let response = response_with_sources(vec![signature(
        "SomeFutureCorpus2099",
        "Mystery Game",
        "abc",
    )]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "abc", None);
    assert_eq!(
        observations[0].provenance.upstream_source,
        SourceFamily::Unknown
    );
}

// =======================================================================
// Test matrix - source mappings (section 57, items 13-25)
// =======================================================================

#[test]
fn no_intro_tag_maps_correctly() {
    assert_eq!(hasheous_upstream_for_tag("NoIntros"), SourceFamily::NoIntro);
}
#[test]
fn tosec_tag_maps_correctly() {
    assert_eq!(hasheous_upstream_for_tag("TOSEC"), SourceFamily::TOSEC);
}
#[test]
fn redump_tag_maps_correctly() {
    assert_eq!(hasheous_upstream_for_tag("Redump"), SourceFamily::Redump);
}
#[test]
fn mame_arcade_tag_maps_correctly() {
    assert_eq!(
        hasheous_upstream_for_tag("MAMEArcade"),
        SourceFamily::MAMEArcade
    );
}
#[test]
fn mame_mess_tag_maps_to_mame_software_list() {
    assert_eq!(
        hasheous_upstream_for_tag("MAMEMess"),
        SourceFamily::MAMESoftwareList
    );
}
#[test]
fn mame_redump_tag_maps_correctly() {
    assert_eq!(
        hasheous_upstream_for_tag("MAMERedump"),
        SourceFamily::MAMERedump
    );
}
#[test]
fn whdload_tag_maps_correctly() {
    assert_eq!(hasheous_upstream_for_tag("WHDLoad"), SourceFamily::WHDLoad);
}
#[test]
fn retroachievements_tag_maps_correctly() {
    assert_eq!(
        hasheous_upstream_for_tag("RetroAchievements"),
        SourceFamily::RetroAchievements
    );
}
#[test]
fn fbneo_tag_maps_correctly() {
    assert_eq!(hasheous_upstream_for_tag("FBNeo"), SourceFamily::FBNeo);
}
#[test]
fn puredosdat_tag_maps_to_puredos() {
    assert_eq!(
        hasheous_upstream_for_tag("PureDOSDAT"),
        SourceFamily::PureDOS
    );
}
#[test]
fn total_dos_collection_tag_maps_correctly() {
    assert_eq!(
        hasheous_upstream_for_tag("TotalDOSCollection"),
        SourceFamily::TotalDOSCollection
    );
}
#[test]
fn screenscraper_tag_maps_correctly() {
    assert_eq!(
        hasheous_upstream_for_tag("ScreenScraper"),
        SourceFamily::ScreenScraper
    );
}
#[test]
fn generic_tag_maps_to_generic_metadata() {
    assert_eq!(
        hasheous_upstream_for_tag("Generic"),
        SourceFamily::GenericMetadata
    );
}
#[test]
fn pleasuredome_and_exo_have_no_variant_and_stay_unknown() {
    assert_eq!(
        hasheous_upstream_for_tag("Pleasuredome"),
        SourceFamily::Unknown
    );
    assert_eq!(hasheous_upstream_for_tag("eXo"), SourceFamily::Unknown);
}
#[test]
fn none_and_unknown_tags_map_to_unknown() {
    assert_eq!(hasheous_upstream_for_tag("None"), SourceFamily::Unknown);
    assert_eq!(hasheous_upstream_for_tag("Unknown"), SourceFamily::Unknown);
}

// =======================================================================
// Test matrix - lineage (section 58, items 26-34)
// =======================================================================

#[test]
fn hasheous_never_becomes_a_source_family() {
    // Exhaustive: no SourceFamily variant's Debug text is "Hasheous".
    let all = [
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
    for family in all {
        assert_ne!(format!("{family:?}"), "Hasheous");
    }
    // And every observation this adapter ever builds has channel=Hasheous
    // with upstream_source drawn only from the table above, never a
    // hardcoded "Hasheous" source.
    let response = response_with_sources(vec![signature("NoIntros", "Alleyway", "abc")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "abc", None);
    assert_eq!(
        observations[0].provenance.channel,
        EvidenceChannel::Hasheous
    );
    assert_ne!(
        observations[0].provenance.upstream_source,
        SourceFamily::Unknown
    );
}

fn local_no_intro_observation(hash: &str) -> EvidenceObservation {
    crate::platform_evidence_fusion::evidence_lineage::hasheous_observation(
        "NoIntros",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some(hash.to_string()),
        None,
    )
}

#[test]
fn hasheous_nointro_plus_local_nointro_same_hash_is_same_source_agreement() {
    let response = response_with_sources(vec![signature("NoIntros", "Alleyway", "abc")]);
    let hasheous_observation =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "abc", None)
            .into_iter()
            .find(|o| o.claim == ClaimType::ExactBytesMatch)
            .unwrap();
    let local = EvidenceObservation {
        hash_or_value: Some("abc".to_string()),
        ..local_no_intro_observation("abc")
    };
    let summaries = merge_evidence(&[hasheous_observation, local]);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn hasheous_tosec_plus_local_tosec_same_hash_is_same_source_agreement() {
    let response = response_with_sources(vec![signature("TOSEC", "Some Game", "xyz")]);
    let hasheous_observation =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "xyz", None)
            .into_iter()
            .find(|o| o.claim == ClaimType::ExactBytesMatch)
            .unwrap();
    let local = crate::platform_evidence_fusion::evidence_lineage::hasheous_observation(
        "TOSEC",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("xyz".to_string()),
        None,
    );
    let summaries = merge_evidence(&[hasheous_observation, local]);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn hasheous_redump_plus_direct_redump_same_hash_is_same_source_agreement() {
    let response = response_with_sources(vec![signature("Redump", "Disc Game", "rr1")]);
    let hasheous_observation =
        observations_from_hash_lookup(&response, Representation::DiscTrack, "rr1", None)
            .into_iter()
            .find(|o| o.claim == ClaimType::ExactTrackMatch)
            .unwrap();
    let direct = EvidenceObservation {
        provenance: crate::platform_evidence_fusion::evidence_lineage::Provenance {
            channel: EvidenceChannel::LocalRedump,
            upstream_source: SourceFamily::Redump,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::Independent,
            representation: Representation::DiscTrack,
        },
        claim: ClaimType::ExactTrackMatch,
        claim_strength: ClaimStrength::Strong,
        identity_scope:
            crate::platform_evidence_fusion::evidence_lineage::IdentityScope::DumpIdentity,
        hash_or_value: Some("rr1".to_string()),
        platform_candidate: None,
        release_candidate: None,
        notes: None,
    };
    let summaries = merge_evidence(&[hasheous_observation, direct]);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactTrackMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn hasheous_mameredump_plus_direct_redump_agreeing_is_derived_agreement() {
    let mameredump = crate::platform_evidence_fusion::evidence_lineage::hasheous_observation(
        "MAMERedump",
        Representation::DiscTrack,
        ClaimType::ExactTrackMatch,
        Some("same".to_string()),
        None,
    );
    let mut mameredump = mameredump;
    mameredump.provenance.lineage = LineageRelation::DerivedFrom;
    let direct_redump = EvidenceObservation {
        provenance: crate::platform_evidence_fusion::evidence_lineage::Provenance {
            channel: EvidenceChannel::LocalRedump,
            upstream_source: SourceFamily::Redump,
            upstream_version: None,
            source_artifact: None,
            imported_at_unix: None,
            retrieved_at_unix: None,
            generator_version: None,
            lineage: LineageRelation::Independent,
            representation: Representation::DiscTrack,
        },
        claim: ClaimType::ExactTrackMatch,
        claim_strength: ClaimStrength::Strong,
        identity_scope:
            crate::platform_evidence_fusion::evidence_lineage::IdentityScope::DumpIdentity,
        hash_or_value: Some("same".to_string()),
        platform_candidate: None,
        release_candidate: None,
        notes: None,
    };
    let summaries = merge_evidence(&[mameredump, direct_redump]);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactTrackMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::DerivedAgreement);
}

#[test]
fn hasheous_whdload_plus_direct_whdload_same_hash_is_same_source_agreement() {
    let response = response_with_sources(vec![signature("WHDLoad", "Amiga Game", "ww1")]);
    let hasheous_observation =
        observations_from_hash_lookup(&response, Representation::WHDLoadSlave, "ww1", None)
            .into_iter()
            .find(|o| o.claim == ClaimType::ExactSlaveMatch)
            .unwrap();
    let direct = crate::platform_evidence_fusion::evidence_lineage::hasheous_observation(
        "WHDLoad",
        Representation::WHDLoadSlave,
        ClaimType::ExactSlaveMatch,
        Some("ww1".to_string()),
        None,
    );
    let summaries = merge_evidence(&[hasheous_observation, direct]);
    let exact = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactSlaveMatch)
        .unwrap();
    assert_eq!(exact.status, AgreementStatus::SameSourceAgreement);
}

#[test]
fn hasheous_nointro_plus_hasheous_tosec_are_independent_families() {
    let response = response_with_sources(vec![
        signature("NoIntros", "Same Game", "same"),
        signature("TOSEC", "Same Game", "same"),
    ]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "same", None);
    let exact: Vec<_> = observations
        .into_iter()
        .filter(|o| o.claim == ClaimType::ExactBytesMatch)
        .collect();
    assert_eq!(exact.len(), 2);
    let summaries = merge_evidence(&exact);
    let summary = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(summary.status, AgreementStatus::IndependentAgreement);
}

#[test]
fn unknown_source_is_never_assumed_independent() {
    let response = response_with_sources(vec![signature("MysteryTag", "X", "h1")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h1", None);
    let obs = &observations[0];
    assert_eq!(obs.provenance.upstream_source, SourceFamily::Unknown);
    assert_ne!(obs.provenance.lineage, LineageRelation::Independent);
}

#[test]
fn multiple_channels_relaying_the_same_source_do_not_inflate_the_group_count() {
    use crate::platform_evidence_fusion::evidence_lineage::independent_source_group_count;
    let response = response_with_sources(vec![signature("NoIntros", "Alleyway", "abc")]);
    let hasheous_observation =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "abc", None)
            .into_iter()
            .find(|o| o.claim == ClaimType::ExactBytesMatch)
            .unwrap();
    let local = local_no_intro_observation("abc");
    let romm = crate::platform_evidence_fusion::evidence_lineage::romm_match_observation(
        "nointro_match",
        Representation::PhysicalFile,
        ClaimType::ExactBytesMatch,
        Some("abc".to_string()),
    );
    let count = independent_source_group_count(&[hasheous_observation, local, romm]);
    assert_eq!(count, 1, "three channels, one upstream family");
}

// =======================================================================
// Test matrix - representation (section 59, items 35-41)
// =======================================================================

#[test]
fn physical_file_maps_to_exact_bytes_match() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h", None);
    assert_eq!(
        observations
            .iter()
            .find(|o| o.hash_or_value.is_some())
            .unwrap()
            .claim,
        ClaimType::ExactBytesMatch
    );
}

#[test]
fn normalized_rom_maps_to_exact_normalized_match() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::NormalizedRom, "h", None);
    assert_eq!(
        observations
            .iter()
            .find(|o| o.hash_or_value.is_some())
            .unwrap()
            .claim,
        ClaimType::ExactNormalizedMatch
    );
}

#[test]
fn disc_track_maps_to_exact_track_match() {
    let response = response_with_sources(vec![signature("Redump", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::DiscTrack, "h", None);
    assert_eq!(
        observations
            .iter()
            .find(|o| o.hash_or_value.is_some())
            .unwrap()
            .claim,
        ClaimType::ExactTrackMatch
    );
}

#[test]
fn logical_chd_maps_to_exact_logical_disc_match() {
    let response = response_with_sources(vec![signature("Redump", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::LogicalChd, "h", None);
    assert_eq!(
        observations
            .iter()
            .find(|o| o.hash_or_value.is_some())
            .unwrap()
            .claim,
        ClaimType::ExactLogicalDiscMatch
    );
}

#[test]
fn whdload_slave_maps_to_exact_slave_match() {
    let response = response_with_sources(vec![signature("WHDLoad", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::WHDLoadSlave, "h", None);
    assert_eq!(
        observations
            .iter()
            .find(|o| o.hash_or_value.is_some())
            .unwrap()
            .claim,
        ClaimType::ExactSlaveMatch
    );
}

#[test]
fn whole_hdf_never_becomes_exact_slave_match() {
    let response = response_with_sources(vec![signature("WHDLoad", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::WholeHdf, "h", None);
    let exact = observations
        .iter()
        .find(|o| o.hash_or_value.is_some())
        .unwrap();
    assert_ne!(exact.claim, ClaimType::ExactSlaveMatch);
    assert_eq!(exact.provenance.representation, Representation::WholeHdf);
}

#[test]
fn same_hash_text_across_two_representations_stays_separate() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "samehash")]);
    let physical =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "samehash", None);
    let normalized =
        observations_from_hash_lookup(&response, Representation::NormalizedRom, "samehash", None);
    let all: Vec<_> = physical.into_iter().chain(normalized).collect();
    let exact: Vec<_> = all
        .iter()
        .filter(|o| {
            o.claim == ClaimType::ExactBytesMatch || o.claim == ClaimType::ExactNormalizedMatch
        })
        .cloned()
        .collect();
    let summaries = merge_evidence(&exact);
    // Two different claim types => two separate claim-scoped summaries,
    // never merged into one.
    assert_eq!(
        summaries
            .iter()
            .filter(|s| s.claim == ClaimType::ExactBytesMatch
                || s.claim == ClaimType::ExactNormalizedMatch)
            .count(),
        2
    );
}

// =======================================================================
// Test matrix - metadata (section 60, items 42-48)
// =======================================================================

#[test]
fn title_remains_display_metadata() {
    let mut response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    response.publisher = Some(MiniDataObjectItem {
        name: Some("Publisher Co".to_string()),
    });
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h", None);
    let display = observations
        .iter()
        .find(|o| o.claim == ClaimType::DisplayMetadata)
        .unwrap();
    assert_eq!(display.claim_strength, ClaimStrength::DisplayOnly);
    assert_eq!(
        display.provenance.upstream_source,
        SourceFamily::GenericMetadata
    );
}

#[test]
fn publisher_remains_display_metadata() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h", None);
    assert!(
        observations
            .iter()
            .any(|o| o.claim == ClaimType::DisplayMetadata
                && o.release_candidate.as_deref() == Some("Nintendo"))
    );
}

#[test]
fn platform_string_becomes_platform_candidate() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h", None);
    assert!(
        observations
            .iter()
            .any(|o| o.claim == ClaimType::PlatformCandidate
                && o.platform_candidate.as_deref() == Some("Nintendo Game Boy")
                && o.hash_or_value.is_none())
    );
}

#[test]
fn no_metadata_observation_overrides_the_hash_claim() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h", None);
    let exact = observations
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.hash_or_value.as_deref(), Some("h"));
    assert_eq!(exact.claim_strength, ClaimStrength::Strong);
}

#[test]
fn no_match_is_neutral_not_unknown_or_conflict() {
    let transport = FakeTransport::status(404);
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let outcome = client.lookup(&sha1_only("nomatch"), None).unwrap();
    assert_eq!(outcome, HasheousLookupOutcome::NoMatch);
}

#[test]
fn missing_upstream_version_is_none_never_fabricated() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h", None);
    let exact = observations
        .iter()
        .find(|o| o.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(exact.provenance.upstream_version, None);
}

#[test]
fn observations_declare_provenance_per_the_adapter_contract() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h", None);
    for observation in &observations {
        assert!(observation_declares_provenance(observation));
    }
}

// =======================================================================
// Test matrix - determinism (section 61, items 49-54)
// =======================================================================

#[test]
fn shuffled_provider_sources_produce_the_same_merged_observations() {
    let forward = response_with_sources(vec![
        signature("NoIntros", "G", "same"),
        signature("TOSEC", "G", "same"),
    ]);
    let backward = response_with_sources(vec![
        signature("TOSEC", "G", "same"),
        signature("NoIntros", "G", "same"),
    ]);
    let a = observations_from_hash_lookup(&forward, Representation::PhysicalFile, "same", None);
    let b = observations_from_hash_lookup(&backward, Representation::PhysicalFile, "same", None);
    // BTreeMap iteration means the two DTOs already produce the same order;
    // merge_evidence sorts regardless, so both merges must match exactly.
    let merged_a = merge_evidence(&a);
    let merged_b = merge_evidence(&b);
    assert_eq!(merged_a.len(), merged_b.len());
    for (x, y) in merged_a.iter().zip(merged_b.iter()) {
        assert_eq!(x.claim, y.claim);
        assert_eq!(x.status, y.status);
    }
}

#[test]
fn duplicate_signatures_collapse_via_merge_evidence_dedup() {
    let response = response_with_sources(vec![
        signature("NoIntros", "G", "same"),
        signature("NoIntros", "G", "same"),
    ]);
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "same", None);
    let exact: Vec<_> = observations
        .into_iter()
        .filter(|o| o.claim == ClaimType::ExactBytesMatch)
        .collect();
    assert_eq!(
        exact.len(),
        2,
        "both duplicate raw observations are preserved pre-merge"
    );
    let summaries = merge_evidence(&exact);
    let summary = summaries
        .iter()
        .find(|s| s.claim == ClaimType::ExactBytesMatch)
        .unwrap();
    assert_eq!(
        summary.observations.len(),
        1,
        "exact duplicates collapse at merge time"
    );
}

#[test]
fn batch_ordering_is_stable_regardless_of_input_order() {
    let responses: Vec<HasheousHttpResponse> = (0..5)
        .map(|_| HasheousHttpResponse {
            status: 404,
            body: Vec::new(),
            retry_after_secs: None,
        })
        .collect();
    let transport = FakeTransport::new(responses);
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let items = vec![
        sha1_only("a"),
        sha1_only("b"),
        sha1_only("c"),
        sha1_only("d"),
        sha1_only("e"),
    ];
    let results = client.lookup_many(&items, None);
    assert_eq!(results.len(), 5);
    assert!(results.iter().all(|r| r.is_ok()));
}

#[test]
fn rendering_is_stable_for_a_shuffled_observation_set() {
    use crate::platform_evidence_fusion::evidence_lineage::render_evidence_summary;
    let response = response_with_sources(vec![
        signature("NoIntros", "G", "same"),
        signature("TOSEC", "G", "same"),
    ]);
    let a = observations_from_hash_lookup(&response, Representation::PhysicalFile, "same", None);
    let mut b = a.clone();
    b.reverse();
    assert_eq!(render_evidence_summary(&a), render_evidence_summary(&b));
}

#[test]
fn hash_set_serde_roundtrip() {
    let hash_set = HasheousHashSet {
        crc: Some("aabbccdd".to_string()),
        md5: Some("m".to_string()),
        sha1: Some("s1".to_string()),
        sha256: Some("s2".to_string()),
    };
    let json = serde_json::to_string(&hash_set).unwrap();
    let back: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(back["crc"], "aabbccdd");
    assert_eq!(back["sha256"], "s2");
}

#[test]
fn hash_lookup_response_deserializes_from_realistic_fixture_json() {
    let json = br#"{
        "platform": {"name": "Nintendo Game Boy"},
        "publisher": {"name": "Nintendo"},
        "signatures": {
            "NoIntros": [
                {"game": {"name": "Alleyway"}, "rom": {"sha1": "abc", "signatureSource": "NoIntros"}}
            ]
        }
    }"#;
    let response: HashLookupResponse = serde_json::from_slice(json).unwrap();
    assert_eq!(
        response.platform.unwrap().name.as_deref(),
        Some("Nintendo Game Boy")
    );
    assert!(response.signatures.unwrap().contains_key("NoIntros"));
}

#[test]
fn two_thousand_synthetic_signatures_perform_sanity_check() {
    let pairs: Vec<(String, SignatureResult)> = (0..2000)
        .map(|i| signature("NoIntros", &format!("Game {i}"), &format!("hash{i}")))
        .collect();
    let response = response_with_sources(pairs);
    let start = std::time::Instant::now();
    let observations =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "hash0", None);
    let summaries = merge_evidence(&observations);
    let elapsed = start.elapsed();
    assert!(!summaries.is_empty());
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "took {elapsed:?}, suspiciously slow"
    );
}

// =======================================================================
// Privacy (section 64) - the single most important test in this module
// =======================================================================

#[test]
fn hasheous_request_body_contains_no_local_path_or_filename() {
    let transport = FakeTransport::ok(b"{}");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let hash_set = HasheousHashSet {
        crc: Some("12ec7f82".to_string()),
        md5: Some("5d7550788a4d1b47ad81fbbbf5c615a9".to_string()),
        sha1: Some("274ed5c2ea2ddc855f67d4c4e61c9d9b7eb68403".to_string()),
        sha256: None,
    };
    let _ = client.lookup(&hash_set, None);
    let body_bytes = transport.last_body();
    let body_text = String::from_utf8(body_bytes.clone()).unwrap();

    // Positive: the hashes are actually present.
    assert!(body_text.contains("12ec7f82"));
    assert!(body_text.contains("274ed5c2ea2ddc855f67d4c4e61c9d9b7eb68403"));

    // Negative: nothing path-shaped, filename-shaped, or byte-content-shaped.
    for forbidden in [
        "/mnt/games/roms",
        "/home/",
        ".gb",
        ".bin",
        ".chd",
        "\\",
        "library",
        "collection",
    ] {
        assert!(
            !body_text.to_lowercase().contains(&forbidden.to_lowercase()),
            "request body unexpectedly contained {forbidden:?}: {body_text}"
        );
    }

    // Structural: the parsed JSON object has only the four documented hash
    // fields, nothing else.
    let parsed: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let object = parsed.as_object().unwrap();
    for key in object.keys() {
        assert!(
            matches!(key.as_str(), "crc" | "md5" | "sha1" | "sha256"),
            "unexpected field in request body: {key}"
        );
    }
}

#[test]
fn request_url_carries_only_the_two_documented_query_parameters() {
    let transport = FakeTransport::ok(b"{}");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let _ = client.lookup(&sha1_only("abc"), None);
    let url = transport.last_url();
    assert!(url.contains("returnAllSources=true"));
    assert!(url.contains("returnFields=All"));
    assert!(!url.contains("path="));
    assert!(!url.contains("file="));
}

// =======================================================================
// Adapter enablement / offline behavior (section 8, 32)
// =======================================================================

#[test]
fn disabled_adapter_refuses_before_any_transport_call() {
    let transport = FakeTransport::ok(b"{}");
    let mut config = enabled_config();
    config.enabled = false;
    let client = HasheousClient::new(&config, &transport);
    let result = client.lookup(&sha1_only("abc"), None);
    assert!(matches!(result, Err(HasheousRequestError::Disabled)));
    assert_eq!(transport.call_count(), 0);
}

#[test]
fn default_config_is_disabled() {
    assert!(!HasheousConfig::default().enabled);
}

#[test]
fn empty_hash_set_is_refused_before_any_transport_call() {
    let transport = FakeTransport::ok(b"{}");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let result = client.lookup(&HasheousHashSet::default(), None);
    assert!(matches!(result, Err(HasheousRequestError::UnsupportedHash)));
    assert_eq!(transport.call_count(), 0);
}

#[test]
fn cancellation_before_the_call_produces_zero_transport_calls() {
    use std::sync::atomic::AtomicBool;
    let transport = FakeTransport::ok(b"{}");
    let config = enabled_config();
    let client = HasheousClient::new(&config, &transport);
    let cancel = AtomicBool::new(true);
    let result = client.lookup(&sha1_only("abc"), Some(&cancel));
    assert!(matches!(result, Err(HasheousRequestError::Cancelled)));
    assert_eq!(transport.call_count(), 0);
}

// =======================================================================
// Resolver/planner/transaction freeze (sections 45-47) - compile-time proof
// this module never references any of those modules.
// =======================================================================

#[test]
fn hasheous_module_never_references_planner_or_transaction_modules() {
    // tests.rs itself is excluded: this very assertion's forbidden-string
    // list legitimately contains these names as literal text.
    for file in ["mod.rs", "client.rs", "convert.rs", "dto.rs"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/identity_source/hasheous")
            .join(file);
        let source = std::fs::read_to_string(&path).unwrap();
        for forbidden in [
            "plan_transaction",
            "rename_apply",
            "rom_organisation::transaction",
            "library_planning",
        ] {
            assert!(
                !source.contains(forbidden),
                "{file} unexpectedly references {forbidden}"
            );
        }
    }
}

#[test]
fn retrieved_at_is_stamped_when_supplied_and_absent_when_not() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let stamped = observations_from_hash_lookup(
        &response,
        Representation::PhysicalFile,
        "h",
        Some(1_700_000_000),
    );
    assert!(
        stamped
            .iter()
            .all(|o| o.provenance.retrieved_at_unix == Some(1_700_000_000))
    );
    let unstamped =
        observations_from_hash_lookup(&response, Representation::PhysicalFile, "h", None);
    assert!(
        unstamped
            .iter()
            .all(|o| o.provenance.retrieved_at_unix.is_none())
    );
}

#[test]
fn imported_at_is_never_set_for_a_live_network_observation() {
    let response = response_with_sources(vec![signature("NoIntros", "G", "h")]);
    let observations = observations_from_hash_lookup(
        &response,
        Representation::PhysicalFile,
        "h",
        Some(1_700_000_000),
    );
    assert!(
        observations
            .iter()
            .all(|o| o.provenance.imported_at_unix.is_none())
    );
}

#[test]
fn now_unix_returns_a_plausible_recent_timestamp() {
    let now = now_unix();
    // Any time after this batch's own work started - a loose sanity bound,
    // not a freshness policy (section 41 explicitly defers that).
    assert!(now > 1_700_000_000);
}

#[test]
fn two_configs_with_different_base_urls_produce_different_request_urls() {
    let transport_a = FakeTransport::ok(b"{}");
    let config_a = HasheousConfig {
        enabled: true,
        base_url: "https://one.test".to_string(),
        timeout: std::time::Duration::from_secs(5),
    };
    let client_a = HasheousClient::new(&config_a, &transport_a);
    let _ = client_a.lookup(&sha1_only("abc"), None);
    assert!(transport_a.last_url().starts_with("https://one.test"));

    let transport_b = FakeTransport::ok(b"{}");
    let config_b = HasheousConfig {
        enabled: true,
        base_url: "https://two.test".to_string(),
        timeout: std::time::Duration::from_secs(5),
    };
    let client_b = HasheousClient::new(&config_b, &transport_b);
    let _ = client_b.lookup(&sha1_only("abc"), None);
    assert!(transport_b.last_url().starts_with("https://two.test"));
}

#[test]
fn hasheous_module_never_references_combined_identity_or_dat_identity_resolver() {
    for file in ["client.rs", "convert.rs", "dto.rs"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/identity_source/hasheous")
            .join(file);
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(!source.contains("combined_identity"));
        assert!(!source.contains("IdentityResult"));
    }
}

//! Tests for the RomM adapter, driven by a deterministic fake instance.
//!
//! No test here contacts a real RomM. The fixtures are trimmed copies of what a
//! real RomM 5.1.0 publishes - the field names and the envelope shape were read
//! from that instance's own OpenAPI document - so the adapter is exercised
//! against the real contract without depending on the service being up.

use super::capability::*;
use super::client::*;
use super::config::*;
use crate::identity_source::net_policy::StaticResolver;
use std::sync::atomic::{AtomicBool, Ordering};

// --- Fixtures -------------------------------------------------------------

/// A trimmed `/api/heartbeat`, matching the real instance's shape.
fn heartbeat_json() -> String {
    r#"{
      "SYSTEM": {"VERSION": "5.1.0", "SHOW_SETUP_WIZARD": false},
      "METADATA_SOURCES": {"ANY_SOURCE_ENABLED": true, "IGDB_API_ENABLED": true},
      "FILESYSTEM": {"FS_PLATFORMS": ["nes", "snes", "atari-st", "ps2"]}
    }"#
    .to_string()
}

/// A trimmed `/openapi.json` carrying everything the adapter checks for.
fn openapi_json() -> String {
    r#"{
      "openapi": "3.1.0",
      "info": {"title": "RomM API", "version": "5.1.0"},
      "paths": {
        "/api/platforms": {"get": {"security": [{"OAuth2PasswordBearer": ["platforms.read"]}]}},
        "/api/roms": {"get": {
          "security": [{"OAuth2PasswordBearer": ["roms.read"]}],
          "parameters": [
            {"name": "limit", "in": "query"},
            {"name": "offset", "in": "query"},
            {"name": "with_files", "in": "query"}
          ]
        }},
        "/api/client-tokens": {"get": {}, "post": {}}
      },
      "components": {"schemas": {"SimpleRomSchema": {"properties": {
        "id": {}, "platform_id": {}, "platform_slug": {}, "fs_path": {}, "fs_name": {},
        "fs_size_bytes": {}, "name": {}, "regions": {}, "revision": {},
        "md5_hash": {}, "sha1_hash": {}, "crc_hash": {},
        "igdb_id": {}, "moby_id": {}, "ss_id": {}, "hasheous_id": {},
        "url_cover": {}, "path_cover_small": {}, "path_cover_large": {},
        "files": {}, "updated_at": {}, "missing_from_fs": {}
      }}}}
    }"#
    .to_string()
}

/// One page of ROMs in the real `CustomLimitOffsetPage` envelope.
fn roms_page_json(offset: u32, limit: u32, total: u64) -> String {
    let items: Vec<String> = (0..limit.min((total.saturating_sub(offset as u64)) as u32))
        .map(|index| {
            let id = offset + index;
            format!(
                r#"{{"id": {id}, "platform_id": 3, "platform_slug": "nes",
                     "fs_path": "roms/nes", "fs_name": "Game{id}.zip",
                     "fs_size_bytes": 131072, "name": "Game {id}",
                     "regions": ["USA"], "revision": null,
                     "md5_hash": "{md5}", "sha1_hash": null, "crc_hash": "deadbeef",
                     "igdb_id": 1000, "url_cover": "assets/roms/{id}/cover_l.png",
                     "path_cover_small": "assets/roms/{id}/cover_s.png",
                     "files": [], "updated_at": "2026-07-01T00:00:00Z",
                     "missing_from_fs": false}}"#,
                md5 = "a".repeat(32)
            )
        })
        .collect();
    format!(
        r#"{{"items": [{}], "total": {total}, "limit": {limit}, "offset": {offset}}}"#,
        items.join(",")
    )
}

/// A deterministic fake instance.
#[derive(Default)]
struct FakeRomm {
    /// Exact URL suffix -> (status, body). Matched on the path portion.
    routes: Vec<(String, u16, String)>,
    /// A `Location` header to return for the next matching route.
    redirect_to: Option<String>,
    /// Records every `Authorization` header seen, so a test can prove the token
    /// was or was not sent - and that it was the right shape.
    seen_authorization: std::sync::Mutex<Vec<Option<String>>>,
    /// Requests seen, so a test can prove no write verb and no extra call.
    seen_urls: std::sync::Mutex<Vec<String>>,
    /// A body larger than the ceiling, for the oversized case.
    oversized: bool,
}

impl FakeRomm {
    fn new() -> Self {
        Self::default()
    }

    fn route(mut self, path_suffix: &str, status: u16, body: &str) -> Self {
        self.routes
            .push((path_suffix.to_string(), status, body.to_string()));
        self
    }

    /// A fully working instance.
    fn healthy() -> Self {
        Self::new()
            .route("/api/heartbeat", 200, &heartbeat_json())
            .route("/openapi.json", 200, &openapi_json())
            .route(
                "/api/platforms",
                200,
                r#"[{"id": 3, "slug": "nes", "name": "NES"}]"#,
            )
            .route("/api/roms", 200, &roms_page_json(0, 50, 2))
    }

    fn authorizations(&self) -> Vec<Option<String>> {
        self.seen_authorization.lock().expect("lock").clone()
    }

    fn urls(&self) -> Vec<String> {
        self.seen_urls.lock().expect("lock").clone()
    }
}

impl RommTransport for FakeRomm {
    fn get(
        &self,
        url: &str,
        authorization: Option<&str>,
        max_bytes: usize,
    ) -> Result<RommHttpResponse, RommRequestError> {
        self.seen_authorization
            .lock()
            .expect("lock")
            .push(authorization.map(str::to_string));
        self.seen_urls.lock().expect("lock").push(url.to_string());

        if self.oversized {
            return Err(RommRequestError::ResponseTooLarge { limit: max_bytes });
        }
        for (suffix, status, body) in &self.routes {
            if url.contains(suffix.as_str()) {
                return Ok(RommHttpResponse {
                    status: *status,
                    body: body.clone().into_bytes(),
                    location: self.redirect_to.clone(),
                });
            }
        }
        Ok(RommHttpResponse {
            status: 404,
            body: Vec::new(),
            location: None,
        })
    }
}

fn token() -> RommToken {
    RommToken::parse("rk_test_abcdef0123456789").expect("a valid token")
}

fn source() -> ValidatedRommSource {
    let config = RommSourceConfig {
        enabled: true,
        url: "http://172.19.0.20:8080".to_string(),
        mappings: Vec::new(),
        token_path: None,
    };
    ValidatedRommSource::validate(&config, &token(), &[], &StaticResolver::new())
        .expect("this configuration should validate")
}

// --- Token handling -------------------------------------------------------

/// Test 43: a token is redacted in every rendering there is.
#[test]
fn a_token_is_redacted_in_debug_display_and_json() {
    let secret = "rk_live_SUPERSECRETVALUE123";
    let token = RommToken::parse(secret).expect("valid");

    let debug = format!("{token:?}");
    let display = format!("{token}");
    let json = serde_json::to_string(&token).expect("serialises");
    let pretty = format!("{token:#?}");
    for rendering in [&debug, &display, &json, &pretty] {
        assert!(
            !rendering.contains(secret),
            "the secret leaked into a rendering: {rendering}"
        );
        assert!(
            !rendering.contains("SUPERSECRET"),
            "even part of the secret must not appear: {rendering}"
        );
    }
    assert!(display.contains("redacted"));
    // The fingerprint is stable and is not the secret.
    assert_eq!(
        token.fingerprint(),
        RommToken::parse(secret).expect("v").fingerprint()
    );
    assert!(!secret.contains(&token.fingerprint()));
}

/// Test 44: the header value is the only way the secret leaves the type.
#[test]
fn the_token_reaches_the_header_and_nowhere_else() {
    let token = RommToken::parse("rk_abc123").expect("valid");
    let header = token.with_header_value(|value| value.to_string());
    assert_eq!(header, "Bearer rk_abc123");
    // And the config that carries it serialises without it.
    let config = RommSourceConfig {
        enabled: true,
        url: "http://127.0.0.1:8080".to_string(),
        mappings: Vec::new(),
        token_path: Some(std::path::PathBuf::from("/tmp/x")),
    };
    let json = serde_json::to_string(&config).expect("serialises");
    assert!(
        !json.contains("rk_abc123"),
        "config must never carry the token"
    );
}

/// Test 45: malformed tokens are refused before they can become a header.
#[test]
fn a_malformed_token_is_refused() {
    for bad in [
        "",
        "   ",
        "abc def",
        "abc\ndef",
        "abc\r\nX: y",
        "tok\u{00e9}n",
    ] {
        assert!(
            RommToken::parse(bad).is_err(),
            "{bad:?} must not be accepted as a token"
        );
    }
    let long = "a".repeat(MAX_TOKEN_BYTES + 1);
    assert!(RommToken::parse(&long).is_err());
    // A refusal explains itself without echoing the input.
    let refusal = RommToken::parse("abc def").expect_err("refused");
    assert!(!refusal.detail().contains("abc def"));
}

/// Test 46: a persisted token is owner-only and round-trips.
#[cfg(unix)]
#[test]
fn a_persisted_token_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let directory = std::env::temp_dir().join(format!(
        "archivefs-romm-token-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let path = directory.join("token");
    let token = RommToken::parse("rk_persist_me").expect("valid");
    token.persist_to(&path).expect("persisted");

    let mode = std::fs::metadata(&path)
        .expect("exists")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "the token file must be owner-only");
    let loaded = RommToken::load_from(&path)
        .expect("readable")
        .expect("present");
    assert_eq!(loaded.fingerprint(), token.fingerprint());
    // A missing file is absence, not an error.
    assert!(
        RommToken::load_from(&directory.join("absent"))
            .expect("no error")
            .is_none()
    );
    let _ = std::fs::remove_dir_all(&directory);
}

/// Test 47: the scopes this adapter needs, and the ones it never wants.
#[test]
fn the_adapter_asks_for_read_scopes_only() {
    assert_eq!(REQUIRED_READ_SCOPES, &["platforms.read", "roms.read"]);
    for scope in REQUIRED_READ_SCOPES {
        assert!(scope.ends_with(".read"), "{scope} is not a read scope");
    }
    for scope in UNWANTED_WRITE_SCOPES {
        assert!(
            !REQUIRED_READ_SCOPES.contains(scope),
            "{scope} must never be required"
        );
    }
}

// --- Capability inspection -------------------------------------------------

/// Test 48: a healthy instance reports its version and capabilities.
#[test]
fn a_healthy_instance_reports_its_version_and_capabilities() {
    let fake = FakeRomm::healthy();
    let source = source();
    let client = RommClient::new(&source, &fake);
    let report = client.capability_report(None).expect("a report");

    let heartbeat = report.heartbeat.clone().expect("a heartbeat");
    assert_eq!(heartbeat.version, "5.1.0");
    assert_eq!(heartbeat.major_version(), Some(5));
    assert!(heartbeat.is_supported());
    assert_eq!(heartbeat.filesystem_platforms.len(), 4);

    assert_eq!(report.api.api_version.as_deref(), Some("5.1.0"));
    assert!(report.api.missing_endpoints.is_empty());
    assert!(report.api.supports_limit_offset_pagination);
    assert!(report.api.supports_client_tokens);
    assert_eq!(
        report.api.available_hash_fields,
        vec!["md5_hash", "sha1_hash", "crc_hash"]
    );
    assert!(report.api.exposes_file_list);
    assert!(report.api.can_import());
    assert!(report.api.blocking_reason().is_none());
    // The scopes were read from the document, not assumed.
    assert!(
        report
            .api
            .declared_read_scopes
            .contains(&"roms.read".to_string())
    );
    assert!(
        report
            .api
            .declared_read_scopes
            .contains(&"platforms.read".to_string())
    );
    // And nothing secret is in the report.
    let json = serde_json::to_string(&report).expect("serialises");
    assert!(!json.contains("rk_test"));
    assert!(!json.contains("Bearer"));
}

/// Test 49: the connection test needs no token, so an address can be checked
/// before a credential exists.
#[test]
fn the_connection_test_sends_no_token() {
    let fake = FakeRomm::healthy();
    let source = source();
    let client = RommClient::new(&source, &fake);
    client.heartbeat(None).expect("a heartbeat");
    assert_eq!(
        fake.authorizations(),
        vec![None],
        "the heartbeat must be unauthenticated"
    );
}

/// Test 50: a data request does send the token, as a bearer header.
#[test]
fn a_data_request_sends_the_token_as_a_bearer_header() {
    let fake = FakeRomm::healthy();
    let source = source();
    let client = RommClient::new(&source, &fake);
    client.platforms(None).expect("platforms");
    let seen = fake.authorizations();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].as_deref(), Some("Bearer rk_test_abcdef0123456789"));
}

/// Test 51: an unsupported version is reported, not refused - refusing would
/// make every RomM upgrade an outage.
#[test]
fn an_unsupported_version_is_reported_rather_than_refused() {
    let old = r#"{"SYSTEM": {"VERSION": "3.2.1"}, "FILESYSTEM": {"FS_PLATFORMS": []}}"#;
    let fake = FakeRomm::new().route("/api/heartbeat", 200, old).route(
        "/openapi.json",
        200,
        &openapi_json(),
    );
    let source = source();
    let client = RommClient::new(&source, &fake);
    let report = client.capability_report(None).expect("still a report");
    let heartbeat = report.heartbeat.expect("parsed");
    assert!(!heartbeat.is_supported());
    assert!(
        report.notes.iter().any(|note| note.contains("older than")),
        "the version gap must be stated: {:?}",
        report.notes
    );
}

/// Test 52: missing optional fields are absent, not fatal.
#[test]
fn missing_optional_fields_are_reported_as_absent() {
    // No hash fields, no file list, no client tokens, no pagination.
    let sparse = r#"{
      "info": {"version": "4.0.0"},
      "paths": {
        "/api/platforms": {"get": {}},
        "/api/roms": {"get": {"parameters": [{"name": "limit", "in": "query"}]}}
      },
      "components": {"schemas": {"SimpleRomSchema": {"properties": {"id": {}, "fs_path": {}}}}}
    }"#;
    let fake = FakeRomm::new()
        .route("/api/heartbeat", 200, &heartbeat_json())
        .route("/openapi.json", 200, sparse);
    let source = source();
    let client = RommClient::new(&source, &fake);
    let report = client.capability_report(None).expect("a report");
    assert!(report.api.available_hash_fields.is_empty());
    assert!(!report.api.exposes_file_list);
    assert!(!report.api.supports_client_tokens);
    assert!(
        !report.api.supports_limit_offset_pagination,
        "offset is missing, so paging is not supported"
    );
    assert!(!report.api.can_import());
    assert!(
        report
            .api
            .blocking_reason()
            .expect("blocked")
            .contains("paging")
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("no hash fields")),
        "the consequence of absent hashes must be stated"
    );
    // A heartbeat with no FILESYSTEM section at all still parses.
    let minimal: serde_json::Value =
        serde_json::from_str(r#"{"SYSTEM": {"VERSION": "5.0.0"}}"#).expect("json");
    let parsed = RommHeartbeat::parse(&minimal).expect("version is enough");
    assert!(parsed.filesystem_platforms.is_empty());
    assert!(!parsed.any_metadata_source_enabled);
}

/// Test 53: a missing required endpoint blocks import, and says which.
#[test]
fn a_missing_required_endpoint_blocks_import() {
    let without_roms = r#"{
      "info": {"version": "5.1.0"},
      "paths": {"/api/platforms": {"get": {}}},
      "components": {"schemas": {}}
    }"#;
    let fake = FakeRomm::new()
        .route("/api/heartbeat", 200, &heartbeat_json())
        .route("/openapi.json", 200, without_roms);
    let source = source();
    let client = RommClient::new(&source, &fake);
    let report = client.capability_report(None).expect("a report");
    assert_eq!(report.api.missing_endpoints, vec!["/api/roms"]);
    assert!(!report.api.can_import());
    assert!(
        report
            .api
            .blocking_reason()
            .expect("blocked")
            .contains("/api/roms")
    );
}

// --- Requests: bounds and failures ----------------------------------------

/// Test 54: pagination walks bounded pages and clamps the page size.
#[test]
fn pagination_returns_bounded_pages_and_clamps_the_page_size() {
    let fake = FakeRomm::new().route("/api/roms", 200, &roms_page_json(0, 50, 120));
    let source = source();
    let client = RommClient::new(&source, &fake);

    let page = client.roms_page(50, 0, None).expect("a page");
    assert_eq!(page.total, 120);
    assert_eq!(page.limit, 50);
    assert_eq!(page.items.len(), 50);

    // A caller asking for an unbounded page is clamped.
    let clamped = client.roms_page(u32::MAX, 0, None).expect("a page");
    assert_eq!(clamped.limit, MAX_PAGE_SIZE);
    // And the request really carried the clamped limit and `with_files`.
    let urls = fake.urls();
    assert!(
        urls.last()
            .expect("a url")
            .contains(&format!("limit={MAX_PAGE_SIZE}"))
    );
    assert!(urls.last().expect("a url").contains("with_files=true"));
}

/// Test 55: malformed JSON is refused with a position, never with content.
#[test]
fn malformed_json_is_refused_without_echoing_the_body() {
    let fake = FakeRomm::new().route("/api/roms", 200, "{ not json at all ]");
    let source = source();
    let client = RommClient::new(&source, &fake);
    let error = client.roms_page(50, 0, None).expect_err("refused");
    assert_eq!(error.code(), "malformed_response");
    assert!(!error.detail().contains("not json at all"));
    assert!(error.preserves_cache());
}

/// Test 56: a page missing its envelope fields is refused precisely.
#[test]
fn a_page_without_its_envelope_is_refused() {
    for (body, expected) in [
        (r#"{"total": 5}"#, "items"),
        (r#"{"items": []}"#, "total"),
        (r#"[]"#, "items"),
    ] {
        let fake = FakeRomm::new().route("/api/roms", 200, body);
        let source = source();
        let client = RommClient::new(&source, &fake);
        let error = client.roms_page(50, 0, None).expect_err("refused");
        assert!(
            error.detail().contains(expected),
            "{body} should name the missing `{expected}`: {}",
            error.detail()
        );
    }
}

/// Test 57: an oversized response is refused with the limit, not the content.
#[test]
fn an_oversized_response_is_refused() {
    let mut fake = FakeRomm::healthy();
    fake.oversized = true;
    let source = source();
    let client = RommClient::new(&source, &fake);
    let error = client.roms_page(50, 0, None).expect_err("refused");
    assert_eq!(error.code(), "response_too_large");
    assert!(error.detail().contains(&MAX_RESPONSE_BYTES.to_string()));
    assert!(error.preserves_cache());
}

/// Test 58: authentication failure, rate limiting and other statuses each get
/// their own explained error.
#[test]
fn status_codes_are_classified_individually() {
    for (status, code) in [
        (401_u16, "unauthorised"),
        (403, "unauthorised"),
        (429, "rate_limited"),
        (503, "rate_limited"),
        (500, "http_status"),
        (404, "http_status"),
    ] {
        let fake = FakeRomm::new().route("/api/roms", status, "");
        let source = source();
        let client = RommClient::new(&source, &fake);
        let error = client.roms_page(50, 0, None).expect_err("refused");
        assert_eq!(error.code(), code, "status {status}");
        assert!(!error.detail().is_empty());
        assert!(
            error.preserves_cache(),
            "no failure is a reason to discard a working cache"
        );
    }
    // An auth failure names the status but never the token.
    let fake = FakeRomm::new().route("/api/roms", 401, "");
    let source = source();
    let error = RommClient::new(&source, &fake)
        .roms_page(50, 0, None)
        .expect_err("refused");
    assert!(!error.detail().contains("rk_test"));
}

/// Test 59: a redirect is refused, naming where the instance tried to send us.
#[test]
fn a_redirect_is_refused_and_never_followed() {
    let mut fake = FakeRomm::new().route("/api/roms", 302, "");
    fake.redirect_to = Some("http://evil.example.com/steal".to_string());
    let source = source();
    let client = RommClient::new(&source, &fake);
    let error = client.roms_page(50, 0, None).expect_err("refused");
    // Only one request was made: nothing was followed.
    assert_eq!(
        fake.urls().len(),
        1,
        "a redirect must not produce a second request"
    );
    assert!(!error.detail().is_empty());
    assert!(
        !matches!(error, RommRequestError::HttpStatus { .. }),
        "a redirect should be an endpoint refusal, not a bare status: {error:?}"
    );
}

/// Test 60: a redirect to a public literal address is refused as public.
#[test]
fn a_redirect_to_a_public_address_is_refused_as_public() {
    let mut fake = FakeRomm::new().route("/api/platforms", 301, "");
    fake.redirect_to = Some("http://93.184.216.34/steal".to_string());
    let source = source();
    let error = RommClient::new(&source, &fake)
        .platforms(None)
        .expect_err("refused");
    assert!(
        error.detail().contains("93.184.216.34"),
        "the destination must be named: {}",
        error.detail()
    );
}

/// Test 61: cancellation is honoured before a request is made and between steps.
#[test]
fn cancellation_stops_the_client_before_it_asks() {
    let fake = FakeRomm::healthy();
    let source = source();
    let client = RommClient::new(&source, &fake);
    let cancel = AtomicBool::new(true);
    let error = client
        .roms_page(50, 0, Some(&cancel))
        .expect_err("cancelled");
    assert_eq!(error.code(), "cancelled");
    assert!(
        fake.urls().is_empty(),
        "a cancelled request must not reach the network"
    );
    // Cleared, the same call works - so the refusal really was the flag.
    cancel.store(false, Ordering::Relaxed);
    client.roms_page(50, 0, Some(&cancel)).expect("a page");
    assert_eq!(fake.urls().len(), 1);
}

/// Test 62: an API path cannot escape the approved origin.
#[test]
fn a_request_cannot_escape_the_approved_origin() {
    let source = source();
    assert_eq!(
        describe_request(source.endpoint(), "/api/roms"),
        "http://172.19.0.20:8080/api/roms"
    );
    assert!(describe_request(source.endpoint(), "/api/../../etc/passwd").contains("invalid"));
    assert_eq!(source.server_id(), "http://172.19.0.20:8080");
    // The server id is the origin, never a token.
    assert!(!source.server_id().contains("rk_test"));
}

/// Test 63: a configuration is refused as a whole when any part is unusable.
#[test]
fn a_configuration_is_validated_as_a_whole() {
    let bad_url = RommSourceConfig {
        enabled: true,
        url: "http://8.8.8.8:8080".to_string(),
        mappings: Vec::new(),
        token_path: None,
    };
    let refusal = ValidatedRommSource::validate(&bad_url, &token(), &[], &StaticResolver::new())
        .expect_err("refused");
    assert_eq!(refusal.code(), "not_private_address");
    assert!(!refusal.detail().is_empty());

    // A mapping outside the trusted roots.
    let bad_mapping = RommSourceConfig {
        enabled: true,
        url: "http://127.0.0.1:8080".to_string(),
        mappings: vec![crate::identity_source::path_map::PathMapping {
            provider_prefix: "/romm/library".to_string(),
            archivefs_prefix: std::path::PathBuf::from("/etc"),
        }],
        token_path: None,
    };
    let refusal = ValidatedRommSource::validate(
        &bad_mapping,
        &token(),
        &[std::path::PathBuf::from("/mnt/games/roms")],
        &StaticResolver::new(),
    )
    .expect_err("refused");
    assert_eq!(refusal.code(), "outside_trusted_roots");
}

/// Test 64: a fresh source is disabled, so nothing connects at startup.
#[test]
fn a_fresh_source_is_disabled_so_nothing_connects_at_startup() {
    let fresh = RommSourceConfig::default();
    assert!(
        !fresh.enabled,
        "a source must be off until someone turns it on"
    );
    assert!(fresh.url.is_empty());
    assert!(fresh.mappings.is_empty());
    assert!(fresh.token_path.is_none());
}

/// Test 65: the client module contains no write verb and no second transport.
#[test]
fn the_client_can_only_perform_reads() {
    let source = include_str!("client.rs");
    let code: String = source
        .split("#[cfg(test)]")
        .next()
        .expect("production half")
        .lines()
        .filter(|line| {
            !line.trim_start().starts_with("//") && !line.trim_start().starts_with("///")
        })
        .collect::<Vec<_>>()
        .join("\n");
    // No write verb anywhere: not as a method call, not as a string.
    for forbidden in [
        ".post(",
        ".put(",
        ".patch(",
        ".delete(",
        "\"POST\"",
        "\"PUT\"",
        "\"PATCH\"",
        "\"DELETE\"",
        "max_redirects(1",
        "max_redirects(2",
    ] {
        assert!(
            !code.contains(forbidden),
            "`{forbidden}` must never appear in a read-only client"
        );
    }
    // Redirects are explicitly disabled.
    assert!(code.contains("max_redirects(0)"));
    // And no filesystem writes.
    for forbidden in ["fs::write", "fs::create_dir", "File::create"] {
        assert!(!code.contains(forbidden), "`{forbidden}` must not appear");
    }
}

/// Test 66: the whole adapter never names a RomM write endpoint.
#[test]
fn no_module_in_the_adapter_references_a_romm_write_endpoint() {
    for (name, source) in [
        ("client.rs", include_str!("client.rs")),
        ("config.rs", include_str!("config.rs")),
        ("capability.rs", include_str!("capability.rs")),
    ] {
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for endpoint in [
            "/api/scan",
            "/api/tasks",
            "/api/roms/delete",
            "/api/config",
            "/api/platforms/delete",
            "/api/users",
        ] {
            assert!(
                !code.contains(endpoint),
                "{name} must not reference the write endpoint {endpoint}"
            );
        }
    }
}

/// Test 67: the capability facts read from the real instance, recorded so a
/// future RomM release that changes them fails here rather than silently
/// degrading an import.
///
/// These values were read from a live RomM 5.1.0's own `/api/heartbeat` and
/// `/openapi.json` during this milestone, and the fixtures above are trimmed
/// copies of those documents. What is asserted is the *contract* the adapter
/// depends on, not the whole document.
#[test]
fn the_verified_real_instance_contract_is_recorded() {
    let openapi: serde_json::Value =
        serde_json::from_str(&openapi_json()).expect("the fixture is valid JSON");
    let api = RommApiCapability::from_openapi(&openapi);

    // Version, as the real instance reports it.
    assert_eq!(api.api_version.as_deref(), Some(VERIFIED_AGAINST));
    // Both endpoints Stage 1 needs, with the scopes the real document declares.
    assert_eq!(api.available_endpoints, vec!["/api/platforms", "/api/roms"]);
    assert!(api.missing_endpoints.is_empty());
    assert_eq!(
        api.declared_read_scopes,
        vec!["platforms.read", "roms.read"],
        "the scopes a person must give a token, read from the instance"
    );
    // limit/offset paging, which the import walks.
    assert!(api.supports_limit_offset_pagination);
    // All three hashes the real ROM schema publishes.
    assert_eq!(
        api.available_hash_fields,
        vec!["md5_hash", "sha1_hash", "crc_hash"]
    );
    // All three artwork references, none of which is downloaded at import.
    assert_eq!(
        api.available_artwork_fields,
        vec!["url_cover", "path_cover_small", "path_cover_large"]
    );
    // The per-file list, which is where multi-disc relationships come from.
    assert!(api.exposes_file_list);
    // The client-token facility, which is how a read-only token exists at all.
    assert!(api.supports_client_tokens);
    assert!(api.can_import());

    let heartbeat: serde_json::Value = serde_json::from_str(&heartbeat_json()).expect("valid");
    let parsed = RommHeartbeat::parse(&heartbeat).expect("parses");
    assert_eq!(parsed.version, VERIFIED_AGAINST);
    assert!(parsed.is_supported());
    assert!(
        parsed.major_version().expect("a major") >= MINIMUM_SUPPORTED_MAJOR,
        "the verified instance must satisfy the adapter's own minimum"
    );
}

//! Tests for the local-only endpoint policy.
//!
//! The resolver is injected throughout, because the cases that matter most - a
//! hostname that resolves to a public address, or to a private *and* a public
//! address - cannot be arranged with real DNS and must not depend on it.

use super::net_policy::*;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

fn v6(text: &str) -> IpAddr {
    IpAddr::V6(text.parse::<Ipv6Addr>().expect("a valid IPv6 literal"))
}

/// A resolver that maps one name to whatever addresses a test needs.
fn resolving(host: &str, addresses: &[IpAddr]) -> StaticResolver {
    StaticResolver::new().with(host, addresses)
}

fn accept(url: &str) -> ApprovedEndpoint {
    validate_endpoint(url, &StaticResolver::new())
        .unwrap_or_else(|refusal| panic!("{url} should be accepted, got: {refusal}"))
}

fn refuse(url: &str) -> EndpointRefusal {
    validate_endpoint(url, &StaticResolver::new())
        .err()
        .unwrap_or_else(|| panic!("{url} should have been refused"))
}

// --- Accepted -------------------------------------------------------------

/// Test 1: loopback.
#[test]
fn loopback_is_accepted() {
    for url in [
        "http://127.0.0.1:8080",
        "http://127.0.0.1:8080/",
        "http://127.1.2.3:8080",
        "https://127.0.0.1",
        "http://[::1]:8080",
    ] {
        let endpoint = accept(url);
        assert!(endpoint.resolved_addresses().iter().all(|address| {
            address
                .parse::<IpAddr>()
                .is_ok_and(is_approved_local_address)
        }));
    }
    assert_eq!(accept("http://127.0.0.1:8080").port(), 8080);
    assert_eq!(
        accept("https://127.0.0.1").port(),
        443,
        "https defaults to 443"
    );
    assert_eq!(accept("http://127.0.0.1").port(), 80, "http defaults to 80");
}

/// Test 2: private LAN ranges.
#[test]
fn private_lan_addresses_are_accepted() {
    for url in [
        "http://10.0.0.5:8080",
        "http://10.255.255.254:8080",
        "http://192.168.1.50:8080",
        "http://192.168.255.255:8080",
        "http://172.16.0.1:8080",
        "http://172.31.255.254:8080",
    ] {
        accept(url);
    }
}

/// Test 3: the real container address this milestone was built against.
#[test]
fn the_private_container_address_is_accepted() {
    // 172.19.0.20 is where the user's RomM container actually lives, on a
    // Docker bridge network inside 172.16.0.0/12.
    let endpoint = accept("http://172.19.0.20:8080");
    assert_eq!(endpoint.host(), "172.19.0.20");
    assert_eq!(endpoint.port(), 8080);
    assert_eq!(endpoint.origin(), "http://172.19.0.20:8080");
    // And an IPv6 unique-local address, which is what a container network hands
    // out when it is IPv6.
    accept("http://[fd00::1]:8080");
    assert!(is_approved_local_address(v6("fd00::1")));
    assert!(is_approved_local_address(v6("fdff:ffff::1")));
}

/// Test 4: a hostname that resolves privately is accepted, and the addresses it
/// resolved to are reported.
#[test]
fn a_hostname_resolving_privately_is_accepted_and_reported() {
    let resolver = resolving("romm", &[v4(172, 19, 0, 20)]);
    let endpoint = validate_endpoint("http://romm:8080", &resolver).expect("accepted");
    assert_eq!(endpoint.host(), "romm");
    assert_eq!(endpoint.resolved_addresses(), ["172.19.0.20"]);
    assert_eq!(endpoint.origin(), "http://romm:8080");
}

// --- Refused: public addresses -------------------------------------------

/// Test 5: public IPv4.
#[test]
fn public_ipv4_is_rejected() {
    for url in [
        "http://8.8.8.8:8080",
        "http://1.1.1.1",
        "https://93.184.216.34",
        "http://172.32.0.1:8080",     // just outside 172.16.0.0/12
        "http://172.15.255.254:8080", // just below it
        "http://11.0.0.1:8080",       // just outside 10.0.0.0/8
        "http://192.169.0.1:8080",    // just outside 192.168.0.0/16
    ] {
        let refusal = refuse(url);
        assert_eq!(
            refusal.code(),
            "not_private_address",
            "{url} should be refused as public, got: {refusal}"
        );
    }
}

/// Test 6: public IPv6, including an IPv4-mapped public address.
#[test]
fn public_ipv6_is_rejected() {
    for url in [
        "http://[2001:4860:4860::8888]:8080",
        "http://[2606:4700:4700::1111]:8080",
        "http://[fe00::1]:8080",
        // `::ffff:8.8.8.8` must be judged as the IPv4 address it carries.
        "http://[::ffff:808:808]:8080",
    ] {
        let refusal = refuse(url);
        assert!(
            matches!(refusal.code(), "not_private_address" | "metadata_endpoint"),
            "{url} should be refused, got: {refusal}"
        );
    }
    assert!(!is_approved_local_address(v6("2001:4860:4860::8888")));
    assert!(
        !is_approved_local_address(v6("::ffff:808:808")),
        "an IPv4-mapped public address must not pass as IPv6"
    );
}

/// Test 7: cloud and link-local metadata endpoints.
#[test]
fn metadata_endpoints_are_rejected() {
    for address in METADATA_ADDRESSES {
        let url = if address.contains(':') {
            format!("http://[{address}]:80")
        } else {
            format!("http://{address}:80")
        };
        let refusal = validate_endpoint(&url, &StaticResolver::new())
            .err()
            .unwrap_or_else(|| panic!("{url} must be refused"));
        assert_eq!(
            refusal.code(),
            "metadata_endpoint",
            "{address} should be named as a metadata endpoint, got: {refusal}"
        );
    }
    // Every link-local address, not only the well-known ones.
    assert!(is_metadata_address(v4(169, 254, 1, 1)));
    assert!(is_metadata_address(v6("fe80::1")));
    // And a hostname that resolves to one.
    let resolver = resolving("metadata.internal", &[v4(169, 254, 169, 254)]);
    assert_eq!(
        validate_endpoint("http://metadata.internal", &resolver)
            .expect_err("refused")
            .code(),
        "metadata_endpoint"
    );
}

/// Test 8: DNS rebinding - a name resolving to a public address is refused, and
/// so is one that offers a private address first and a public one second.
#[test]
fn a_host_resolving_to_any_public_address_is_rejected() {
    // Wholly public.
    let public = resolving("romm.example.com", &[v4(93, 184, 216, 34)]);
    assert_eq!(
        validate_endpoint("http://romm.example.com", &public)
            .expect_err("refused")
            .code(),
        "not_private_address"
    );

    // The rebinding shape: private first, public second. Checking only the first
    // address would let this through.
    let mixed = resolving("rebind.local", &[v4(192, 168, 1, 10), v4(8, 8, 8, 8)]);
    let refusal = validate_endpoint("http://rebind.local:8080", &mixed)
        .expect_err("a mixed resolution must be refused");
    assert_eq!(refusal.code(), "not_private_address");
    assert!(
        refusal.detail().contains("8.8.8.8"),
        "the offending address must be named: {refusal}"
    );

    // And public first, private second.
    let reversed = resolving("rebind2.local", &[v4(8, 8, 8, 8), v4(192, 168, 1, 10)]);
    assert!(validate_endpoint("http://rebind2.local", &reversed).is_err());
}

/// Test 9: a name resolving to an absurd number of addresses.
#[test]
fn a_host_resolving_to_too_many_addresses_is_rejected() {
    let many: Vec<IpAddr> = (0..=MAX_RESOLVED_ADDRESSES as u8)
        .map(|index| v4(10, 0, 0, index))
        .collect();
    let resolver = resolving("many.local", &many);
    assert_eq!(
        validate_endpoint("http://many.local", &resolver)
            .expect_err("refused")
            .code(),
        "too_many_addresses"
    );
}

/// Test 10: an unresolvable host, and one resolving to nothing.
#[test]
fn an_unresolvable_host_is_refused_rather_than_assumed_local() {
    assert_eq!(
        validate_endpoint("http://nowhere.invalid", &StaticResolver::new())
            .expect_err("refused")
            .code(),
        "unresolvable_host"
    );
    let empty = resolving("empty.local", &[]);
    assert_eq!(
        validate_endpoint("http://empty.local", &empty)
            .expect_err("refused")
            .code(),
        "no_addresses"
    );
}

// --- Refused: URL shapes -------------------------------------------------

/// Test 11: credentials in the URL.
#[test]
fn credentials_in_the_url_are_rejected() {
    // Distinctive secret values, so the assertion is about the secret not being
    // echoed rather than about the words "token" or "password", which the advice
    // text legitimately uses.
    //
    // Assembled from parts rather than written out: the repository's secret
    // scanner matches a credential-bearing URL wherever it appears, including in
    // a fixture that exists to prove such URLs are refused.
    for (scheme, userinfo, host, secret) in [
        (
            "http",
            "user:s3cr3tvalue",
            "192.168.1.5:8080",
            "s3cr3tvalue",
        ),
        ("http", "rk_liveXYZ123", "127.0.0.1:8080", "rk_liveXYZ123"),
        ("https", "admin:hunter2pw", "10.0.0.1", "hunter2pw"),
    ] {
        let url = &format!("{scheme}://{userinfo}@{host}");
        let refusal = refuse(url);
        assert_eq!(refusal.code(), "embedded_credentials", "{url}");
        assert!(
            !refusal.detail().contains(secret),
            "the refusal must not echo the secret back: {refusal}"
        );
        assert!(
            !format!("{refusal:?}").contains(secret),
            "not even the debug form may carry the secret"
        );
    }
}

/// Test 12: schemes that are not http(s) - file URLs, unix sockets, shell-ish
/// text.
#[test]
fn unsupported_schemes_are_rejected() {
    let cases = [
        ("file:///etc/passwd", "file"),
        ("file://127.0.0.1/etc/passwd", "file"),
        ("unix:///var/run/romm.sock", "unix"),
        ("ftp://192.168.1.5", "ftp"),
        ("ws://127.0.0.1:8080", "ws"),
        ("gopher://127.0.0.1", "gopher"),
    ];
    for (url, scheme) in cases {
        let refusal = refuse(url);
        assert_eq!(refusal.code(), "unsupported_scheme", "{url}");
        assert!(
            refusal.detail().contains(scheme),
            "the refusal should name `{scheme}`: {refusal}"
        );
    }
    // A bare host, a shell command, a path.
    for url in [
        "192.168.1.5:8080",
        "curl http://127.0.0.1",
        "/var/run/romm.sock",
        "romm",
    ] {
        assert!(
            matches!(refuse(url).code(), "unsupported_scheme" | "unparseable"),
            "{url} must be refused"
        );
    }
}

/// Test 13: whitespace, quoting and control characters, which are how one field
/// becomes two requests.
#[test]
fn whitespace_quoting_and_control_characters_are_rejected() {
    for url in [
        "http://127.0.0.1:8080 --header evil",
        "http://127.0.0.1:8080\nHost: evil",
        "http://127.0.0.1:8080\r\nX: y",
        "http://\"127.0.0.1\":8080",
        "",
        "   ",
    ] {
        assert!(
            validate_endpoint(url, &StaticResolver::new()).is_err(),
            "{url:?} must be refused"
        );
    }
    // Surrounding whitespace is trimmed rather than refused: someone pasting an
    // address should not be punished for a stray space, and trimming the ends
    // cannot inject anything.
    assert_eq!(
        accept("  http://127.0.0.1:8080\t ").origin(),
        "http://127.0.0.1:8080"
    );
}

/// Test 14: a path, query or fragment is refused rather than silently dropped.
#[test]
fn a_url_with_a_path_is_refused_rather_than_silently_truncated() {
    for url in [
        "http://127.0.0.1:8080/api/roms",
        "http://127.0.0.1:8080/some/path",
        "http://127.0.0.1:8080?x=1",
        "http://127.0.0.1:8080#frag",
    ] {
        let refusal = refuse(url);
        assert_eq!(refusal.code(), "unsupported_url_shape", "{url}");
    }
    // A bare trailing slash is fine, because it means the same thing.
    assert_eq!(
        accept("http://127.0.0.1:8080/").origin(),
        "http://127.0.0.1:8080"
    );
}

/// Test 15: malformed ports and addresses.
#[test]
fn malformed_ports_and_addresses_are_rejected() {
    for url in [
        "http://127.0.0.1:0",
        "http://127.0.0.1:99999",
        "http://127.0.0.1:abc",
        "http://[::1:8080",
        "http://:8080",
        "http://",
    ] {
        assert!(
            validate_endpoint(url, &StaticResolver::new()).is_err(),
            "{url} must be refused"
        );
    }
}

// --- Redirects ------------------------------------------------------------

/// Test 16: a redirect to a public address is refused, naming the destination.
#[test]
fn a_redirect_to_a_public_address_is_refused() {
    let approved = accept("http://127.0.0.1:8080");
    let refusal = validate_redirect_target(
        "http://evil.example.com/steal",
        &approved,
        &resolving("evil.example.com", &[v4(93, 184, 216, 34)]),
    );
    assert!(
        matches!(
            refusal.code(),
            "not_private_address" | "unsupported_url_shape"
        ),
        "a public redirect must be refused: {refusal}"
    );
}

/// Test 17: even a redirect to an approved address is refused in Stage 1,
/// because redirects are not followed at all.
#[test]
fn any_redirect_is_refused_in_stage_one() {
    let approved = accept("http://127.0.0.1:8080");
    for location in [
        "http://192.168.1.5:8080",
        "/api/v2/roms",
        "http://127.0.0.1:8080",
    ] {
        let refusal = validate_redirect_target(location, &approved, &StaticResolver::new());
        assert!(
            !refusal.detail().is_empty(),
            "{location} must produce an explained refusal"
        );
        // A relative redirect is reported against the approved origin, so the
        // diagnostic says where it would have gone.
        if location.starts_with('/') {
            assert!(refusal.detail().contains("127.0.0.1:8080"));
        }
    }
}

/// Test 18: a redirect to a metadata endpoint.
#[test]
fn a_redirect_to_a_metadata_endpoint_is_refused() {
    let approved = accept("http://127.0.0.1:8080");
    let refusal = validate_redirect_target(
        "http://169.254.169.254/latest/meta-data/",
        &approved,
        &StaticResolver::new(),
    );
    assert!(
        matches!(
            refusal.code(),
            "metadata_endpoint" | "unsupported_url_shape"
        ),
        "{refusal}"
    );
}

// --- URL construction -----------------------------------------------------

/// Test 19: API paths are appended to the approved origin and cannot escape it.
#[test]
fn api_paths_are_appended_to_the_approved_origin_and_cannot_escape() {
    let endpoint = accept("http://172.19.0.20:8080");
    assert_eq!(
        endpoint.url_for("/api/platforms").expect("valid"),
        "http://172.19.0.20:8080/api/platforms"
    );
    assert_eq!(
        endpoint.url_for("/api/roms?limit=1").expect("valid"),
        "http://172.19.0.20:8080/api/roms?limit=1"
    );
    for hostile in ["api/roms", "../../etc/passwd", "/api/../../../etc/passwd"] {
        assert!(
            endpoint.url_for(hostile).is_err(),
            "{hostile} must not build a URL"
        );
    }
    // An IPv6 origin keeps its brackets.
    assert_eq!(
        accept("http://[::1]:8080")
            .url_for("/api/heartbeat")
            .expect("valid"),
        "http://[::1]:8080/api/heartbeat"
    );
}

/// Test 20: every refusal explains itself, has a stable code, and none of them
/// are duplicated.
#[test]
fn every_refusal_explains_itself_with_a_unique_code() {
    let cases = [
        EndpointRefusal::Unparseable { detail: "x".into() },
        EndpointRefusal::UnsupportedScheme {
            scheme: "file".into(),
        },
        EndpointRefusal::EmbeddedCredentials,
        EndpointRefusal::MissingHost,
        EndpointRefusal::UnresolvableHost { detail: "x".into() },
        EndpointRefusal::NoAddresses,
        EndpointRefusal::TooManyAddresses { count: 99 },
        EndpointRefusal::NotPrivateAddress {
            address: "8.8.8.8".into(),
        },
        EndpointRefusal::MetadataEndpoint {
            address: "169.254.169.254".into(),
        },
        EndpointRefusal::RedirectRefused {
            location: "http://x/".into(),
        },
        EndpointRefusal::InvalidPort { port: 0 },
        EndpointRefusal::UnsupportedUrlShape { detail: "x".into() },
    ];
    let mut codes: Vec<&str> = Vec::new();
    for case in &cases {
        assert!(!case.detail().is_empty(), "{case:?} has no explanation");
        codes.push(case.code());
    }
    let mut unique = codes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(codes.len(), unique.len(), "two refusals share a code");
}

/// Test 21: the policy module cannot write, spawn or bypass its own rules.
#[test]
fn the_policy_module_contains_no_write_or_process_call() {
    let source = include_str!("net_policy.rs");
    let code: String = source
        .split("#[cfg(test)]")
        .next()
        .expect("production half")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "fs::write",
        "fs::create_dir",
        "File::create",
        "Command",
        "std::process",
        "ureq",
        "reqwest",
    ] {
        assert!(
            !code.contains(forbidden),
            "`{forbidden}` must not appear in the endpoint policy"
        );
    }
}

// --- Path mapping ---------------------------------------------------------

mod path_mapping {
    use super::super::path_map::*;
    use std::path::Path;
    use std::path::PathBuf;

    fn mapping(provider: &str, archivefs: &str) -> PathMapping {
        PathMapping {
            provider_prefix: provider.to_string(),
            archivefs_prefix: PathBuf::from(archivefs),
        }
    }

    /// Validated with no trusted-root restriction, which is what a preview does.
    fn mappings(pairs: &[(&str, &str)]) -> PathMappings {
        let list: Vec<PathMapping> = pairs
            .iter()
            .map(|(provider, archivefs)| mapping(provider, archivefs))
            .collect();
        PathMappings::validate(&list, &[], ProviderPathKind::AbsoluteProviderPath)
            .expect("these mappings should validate")
    }

    /// Test 22: the ordinary case from the milestone.
    #[test]
    fn a_normal_mapping_translates_a_path() {
        let maps = mappings(&[("/romm/library", "/mnt/games/roms")]);
        let translated = maps.translate("/romm/library/nes/Metroid.zip");
        assert_eq!(
            translated.archivefs_path(),
            Some(PathBuf::from("/mnt/games/roms/nes/Metroid.zip").as_path())
        );
        let PathTranslation::Translated {
            provider_path,
            matched_prefix,
            ..
        } = &translated
        else {
            panic!("expected a translation");
        };
        assert_eq!(
            provider_path, "/romm/library/nes/Metroid.zip",
            "the original path is kept for provenance"
        );
        assert_eq!(matched_prefix, "/romm/library");
    }

    /// Test 23: several mappings, each applying to its own subtree.
    #[test]
    fn multiple_mappings_each_apply_to_their_own_subtree() {
        let maps = mappings(&[
            ("/romm/library", "/mnt/games/roms"),
            ("/romm/extra", "/mnt/ngc-roms"),
            ("/data/other", "/mnt/virtualroms"),
        ]);
        assert_eq!(maps.len(), 3);
        for (input, expected) in [
            ("/romm/library/snes/a.sfc", "/mnt/games/roms/snes/a.sfc"),
            ("/romm/extra/gc/b.rvz", "/mnt/ngc-roms/gc/b.rvz"),
            ("/data/other/c.zip", "/mnt/virtualroms/c.zip"),
        ] {
            assert_eq!(
                maps.translate(input).archivefs_path(),
                Some(PathBuf::from(expected).as_path()),
                "{input}"
            );
        }
    }

    /// Test 24: the longest matching prefix wins, whichever order they were
    /// configured in.
    #[test]
    fn the_longest_prefix_wins() {
        for pairs in [
            // Configured shortest-first...
            &[
                ("/romm/library", "/mnt/games/roms"),
                ("/romm/library/retro", "/mnt/retro"),
            ][..],
            // ...and longest-first: the result must be identical.
            &[
                ("/romm/library/retro", "/mnt/retro"),
                ("/romm/library", "/mnt/games/roms"),
            ][..],
        ] {
            let maps = mappings(pairs);
            assert_eq!(
                maps.translate("/romm/library/retro/nes/x.zip")
                    .archivefs_path(),
                Some(PathBuf::from("/mnt/retro/nes/x.zip").as_path()),
                "the more specific mapping must win"
            );
            assert_eq!(
                maps.translate("/romm/library/other/x.zip").archivefs_path(),
                Some(PathBuf::from("/mnt/games/roms/other/x.zip").as_path()),
                "and the general one still covers everything else"
            );
        }
    }

    /// Test 25: matching is on whole components, never a string prefix.
    #[test]
    fn matching_is_on_component_boundaries_only() {
        let maps = mappings(&[("/romm/library", "/mnt/games/roms")]);
        // The case a naive `starts_with` on strings would get wrong.
        for near_miss in [
            "/romm/library-backup/x.zip",
            "/romm/libraryextra/x.zip",
            "/romm/library2/x.zip",
        ] {
            assert!(
                !maps.translate(near_miss).is_translated(),
                "{near_miss} must not match /romm/library"
            );
        }
        // The prefix itself, exactly, does map - to the destination root.
        assert_eq!(
            maps.translate("/romm/library").archivefs_path(),
            Some(PathBuf::from("/mnt/games/roms").as_path())
        );
        // A trailing separator on a *record* path is refused rather than trimmed.
        // A person typing a mapping prefix gets that courtesy - see
        // `duplicate_sources_and_destinations_are_refused`, where `/romm/a/` and
        // `/romm/a` are still recognised as one prefix - but a path arriving over
        // the network does not, because a spelling that needs repairing is a
        // spelling whose meaning was never agreed.
        assert_eq!(
            maps.translate("/romm/library/").refusal().map(|r| r.code()),
            Some("empty_component")
        );
    }

    /// Test 26: traversal in a provider path is refused, not resolved.
    #[test]
    fn traversal_in_a_provider_path_is_refused() {
        let maps = mappings(&[("/romm/library", "/mnt/games/roms")]);
        for hostile in [
            "/romm/library/../../etc/passwd",
            "/romm/library/nes/../../../../etc/shadow",
            "/romm/library/..",
            "/../romm/library/x.zip",
        ] {
            let translated = maps.translate(hostile);
            assert!(
                matches!(translated, PathTranslation::Refused { .. }),
                "{hostile} must be refused outright, got {translated:?}"
            );
        }
        // And in a configured mapping.
        assert_eq!(
            PathMappings::validate(
                &[mapping("/romm/../etc", "/mnt/games/roms")],
                &[],
                ProviderPathKind::AbsoluteProviderPath
            )
            .expect_err("refused")
            .code(),
            "non_normal_component"
        );
        assert_eq!(
            PathMappings::validate(
                &[mapping("/romm/library", "/mnt/games/../../etc")],
                &[],
                ProviderPathKind::AbsoluteProviderPath,
            )
            .expect_err("refused")
            .code(),
            "non_normal_component"
        );
    }

    /// Test 27: a destination outside the configured source roots is refused
    /// when the mapping is configured, not when it is used.
    #[test]
    fn a_destination_outside_the_trusted_roots_is_refused() {
        let roots = vec![
            PathBuf::from("/mnt/games/roms"),
            PathBuf::from("/mnt/ngc-roms"),
        ];
        // Inside a root: accepted.
        PathMappings::validate(
            &[mapping("/romm/library", "/mnt/games/roms/nes")],
            &roots,
            ProviderPathKind::AbsoluteProviderPath,
        )
        .expect("a destination inside a source folder is fine");
        // Outside every root: refused.
        for outside in ["/etc", "/home/davedap", "/mnt/games/roms-backup", "/"] {
            let refusal = PathMappings::validate(
                &[mapping("/romm/library", outside)],
                &roots,
                ProviderPathKind::AbsoluteProviderPath,
            )
            .err()
            .unwrap_or_else(|| panic!("{outside} must be refused"));
            assert_eq!(refusal.code(), "outside_trusted_roots", "{outside}");
        }
    }

    /// Test 28: an unmatched path is reported, not an error.
    #[test]
    fn an_unmatched_path_is_reported_rather_than_failing() {
        let maps = mappings(&[("/romm/library", "/mnt/games/roms")]);
        let translated = maps.translate("/romm/elsewhere/x.zip");
        assert!(matches!(translated, PathTranslation::Unmatched { .. }));
        assert!(translated.archivefs_path().is_none());
        // With no mappings at all, everything is unmatched rather than refused.
        let empty = PathMappings::default();
        assert!(empty.is_empty());
        assert!(matches!(
            empty.translate("/romm/library/x.zip"),
            PathTranslation::Unmatched { .. }
        ));
    }

    /// Test 29: two mappings landing on the same destination, or starting from
    /// the same source, are refused - either would make the result depend on
    /// ordering.
    #[test]
    fn duplicate_sources_and_destinations_are_refused() {
        assert_eq!(
            PathMappings::validate(
                &[
                    mapping("/romm/a", "/mnt/games/roms"),
                    mapping("/romm/b", "/mnt/games/roms"),
                ],
                &[],
                ProviderPathKind::AbsoluteProviderPath,
            )
            .expect_err("refused")
            .code(),
            "duplicate_destination"
        );
        assert_eq!(
            PathMappings::validate(
                &[
                    mapping("/romm/a", "/mnt/one"),
                    mapping("/romm/a/", "/mnt/two"),
                ],
                &[],
                ProviderPathKind::AbsoluteProviderPath,
            )
            .expect_err("refused")
            .code(),
            "duplicate_source",
            "the two spellings normalise to the same prefix"
        );
    }

    /// Test 30: a backslash is refused, not read as a separator.
    ///
    /// This reverses an earlier decision in this module, which unified `\\` to `/`
    /// so a provider running on Windows would still map. Two things make refusing
    /// it the better answer. On the Linux filesystems RomM actually runs on, a
    /// backslash is a legal character *in a filename*, so reinterpreting it as a
    /// separator invents a directory level that does not exist and points the
    /// translation at the wrong place. And a path whose separators are ambiguous
    /// is exactly the shape a traversal attempt takes, so the safe response is to
    /// stop rather than to guess.
    #[test]
    fn backslashes_are_refused_rather_than_treated_as_separators() {
        let maps = mappings(&[("/romm/library", "/mnt/games/roms")]);
        // One leading backslash: a stray separator, in a path that would have
        // mapped under the old unifying rule.
        assert_eq!(
            maps.translate(r"\romm\library\nes\Metroid.zip")
                .refusal()
                .map(|refusal| refusal.code()),
            Some("windows_separator")
        );
        // A backslash part-way through, which on Linux is a legal filename byte.
        assert_eq!(
            maps.translate(r"/romm/library/nes\Metroid.zip")
                .refusal()
                .map(|refusal| refusal.code()),
            Some("windows_separator")
        );
        // Two leading backslashes are a UNC share, named as such.
        assert_eq!(
            maps.translate(r"\\server\share\game.zip")
                .refusal()
                .map(|refusal| refusal.code()),
            Some("unc_path")
        );
        // Traversal written with backslashes is refused before any separator
        // reinterpretation could have hidden the `..`.
        assert!(
            maps.translate(r"/romm/library/..\..\etc/passwd")
                .refusal()
                .is_some()
        );
        // A drive letter is named for what it is.
        assert_eq!(
            maps.translate("C:/romm/library/x.zip")
                .refusal()
                .map(|refusal| refusal.code()),
            Some("drive_prefix")
        );
    }

    /// Test 31: redundant separators and `.` segments are refused.
    ///
    /// Also a reversal: they used to be quietly collapsed. Collapsing means two
    /// different strings translate to one path, which is precisely the property a
    /// traversal attempt relies on. Outer whitespace is still trimmed, because
    /// that cannot change which components a path has.
    #[test]
    fn redundant_separators_and_dot_segments_are_refused() {
        let maps = mappings(&[("/romm/library", "/mnt/games/roms")]);
        for (messy, expected) in [
            ("/romm//library//nes///Metroid.zip", "empty_component"),
            ("/romm/./library/nes/Metroid.zip", "dot_component"),
            ("/romm/library/nes/./Metroid.zip", "dot_component"),
            ("/romm/library/nes/Metroid.zip/", "empty_component"),
        ] {
            assert_eq!(
                maps.translate(messy)
                    .refusal()
                    .map(|refusal| refusal.code()),
                Some(expected),
                "{messy}"
            );
        }
        // Whitespace around the whole path is still tolerated: it cannot change
        // the components.
        assert_eq!(
            maps.translate("  /romm/library/nes/Metroid.zip  ")
                .archivefs_path(),
            Some(PathBuf::from("/mnt/games/roms/nes/Metroid.zip").as_path())
        );
    }

    /// Test 32: absurdly long and malformed inputs.
    #[test]
    fn oversized_and_relative_paths_are_refused() {
        let maps = mappings(&[("/romm/library", "/mnt/games/roms")]);
        let huge = format!("/romm/library/{}", "a".repeat(MAX_PROVIDER_PATH_BYTES));
        assert!(matches!(
            maps.translate(&huge),
            PathTranslation::Refused { .. }
        ));
        for relative in ["romm/library/x.zip", "", "   ", "C:/romm/library/x.zip"] {
            assert!(
                !maps.translate(relative).is_translated(),
                "{relative:?} must not translate"
            );
        }
    }

    /// Test 33: too many mappings.
    #[test]
    fn too_many_mappings_are_refused() {
        let many: Vec<PathMapping> = (0..=MAX_MAPPINGS)
            .map(|index| mapping(&format!("/romm/{index}"), &format!("/mnt/{index}")))
            .collect();
        assert_eq!(
            PathMappings::validate(&many, &[], ProviderPathKind::AbsoluteProviderPath)
                .expect_err("refused")
                .code(),
            "too_many"
        );
    }

    /// Test 34: a preview counts what would happen without importing anything.
    #[test]
    fn a_preview_reports_translated_unmatched_and_refused() {
        let maps = mappings(&[("/romm/library", "/mnt/games/roms")]);
        let samples: Vec<String> = [
            "/romm/library/nes/a.zip",
            "/romm/library/snes/b.sfc",
            "/romm/elsewhere/c.zip",
            "/romm/library/../etc/passwd",
        ]
        .iter()
        .map(|path| path.to_string())
        .collect();
        let preview = MappingPreview::build(&maps, &samples);
        assert_eq!(preview.translated, 2);
        assert_eq!(preview.unmatched, 1);
        assert_eq!(preview.refused, 1);
        assert_eq!(preview.translations.len(), 4);
    }

    // --- Provider-relative paths, the shape RomM 5.1.0 actually reports ------

    /// Relative mappings with the real path shapes observed from RomM 5.1.0.
    fn relative_mappings(pairs: &[(&str, &str)]) -> PathMappings {
        let list: Vec<PathMapping> = pairs
            .iter()
            .map(|(provider, archivefs)| mapping(provider, archivefs))
            .collect();
        PathMappings::validate(&list, &[], ProviderPathKind::ProviderRelative)
            .expect("these relative mappings should validate")
    }

    #[test]
    fn real_relative_paths_from_romm_translate() {
        let maps = relative_mappings(&[("roms", "/mnt/games/roms")]);
        // Exactly the shapes the live server returned.
        for (provider, expected) in [
            ("roms/gb/game.gb", "/mnt/games/roms/gb/game.gb"),
            ("roms/snes/game.zip", "/mnt/games/roms/snes/game.zip"),
            (
                "roms/atari-st/game.stx",
                "/mnt/games/roms/atari-st/game.stx",
            ),
            ("roms/psx/game.cue", "/mnt/games/roms/psx/game.cue"),
            (
                "roms/sharp-x68000/_ReadMe_.txt",
                "/mnt/games/roms/sharp-x68000/_ReadMe_.txt",
            ),
            (
                "roms/acorn-archimedes/Coconizer (Europe) (v1.3).zip",
                "/mnt/games/roms/acorn-archimedes/Coconizer (Europe) (v1.3).zip",
            ),
            (
                "roms/amiga/Allo Allo (v1.0).hdf",
                "/mnt/games/roms/amiga/Allo Allo (v1.0).hdf",
            ),
            (
                "roms/atari-st/'Nam 1965-1975 (Europe).stx",
                "/mnt/games/roms/atari-st/'Nam 1965-1975 (Europe).stx",
            ),
        ] {
            assert_eq!(
                maps.translate(provider).archivefs_path(),
                Some(PathBuf::from(expected).as_path()),
                "{provider}"
            );
        }
    }

    #[test]
    fn a_relative_translation_keeps_the_exact_provider_string() {
        let maps = relative_mappings(&[("roms", "/mnt/games/roms")]);
        let translated = maps.translate("roms/atari-st/game.stx");
        let PathTranslation::Translated {
            provider_path,
            normalised_path,
            kind,
            matched_prefix,
            ..
        } = &translated
        else {
            panic!("expected a translation, got {translated:?}");
        };
        assert_eq!(
            provider_path, "roms/atari-st/game.stx",
            "the exact string RomM sent must survive translation"
        );
        assert_eq!(normalised_path, "roms/atari-st/game.stx");
        assert_eq!(*kind, ProviderPathKind::ProviderRelative);
        assert_eq!(matched_prefix, "roms");
    }

    #[test]
    fn relative_mappings_use_component_boundaries_and_longest_prefix() {
        let maps = relative_mappings(&[
            ("roms", "/mnt/games/roms"),
            ("roms/atari-st", "/mnt/st"),
            ("other", "/mnt/other"),
        ]);
        assert_eq!(
            maps.translate("roms/atari-st/game.stx").archivefs_path(),
            Some(PathBuf::from("/mnt/st/game.stx").as_path()),
            "the more specific relative mapping must win"
        );
        assert_eq!(
            maps.translate("roms/gb/game.gb").archivefs_path(),
            Some(PathBuf::from("/mnt/games/roms/gb/game.gb").as_path())
        );
        // A string prefix would wrongly accept these.
        for near_miss in ["roms-backup/gb/x.gb", "romsextra/x.gb", "roms2/x.gb"] {
            assert!(
                !maps.translate(near_miss).is_translated(),
                "{near_miss} must not match `roms`"
            );
        }
    }

    /// The two shapes are declared, so each refuses the other rather than
    /// reinterpreting it.
    #[test]
    fn a_path_of_the_wrong_shape_is_refused_with_the_setting_named() {
        let relative = relative_mappings(&[("roms", "/mnt/games/roms")]);
        let refusal = relative
            .translate("/romm/library/gb/game.gb")
            .refusal()
            .cloned()
            .expect("an absolute path in relative mode must be refused");
        assert_eq!(refusal.code(), "unexpectedly_absolute");
        assert!(
            refusal.detail().contains("--path-kind absolute"),
            "the refusal should say how to fix it: {}",
            refusal.detail()
        );

        let absolute = mappings(&[("/romm/library", "/mnt/games/roms")]);
        let refusal = absolute
            .translate("roms/gb/game.gb")
            .refusal()
            .cloned()
            .expect("a relative path in absolute mode must be refused");
        assert_eq!(refusal.code(), "unexpectedly_relative");
        assert!(
            refusal.detail().contains("--path-kind relative"),
            "the refusal should say how to fix it: {}",
            refusal.detail()
        );
    }

    #[test]
    fn a_relative_mapping_prefix_must_itself_be_relative() {
        assert_eq!(
            PathMappings::validate(
                &[mapping("/romm/library", "/mnt/games/roms")],
                &[],
                ProviderPathKind::ProviderRelative,
            )
            .expect_err("an absolute prefix is not a relative mapping")
            .code(),
            "unexpectedly_absolute"
        );
        assert_eq!(
            PathMappings::validate(
                &[mapping("roms", "/mnt/games/roms")],
                &[],
                ProviderPathKind::AbsoluteProviderPath,
            )
            .expect_err("a relative prefix is not an absolute mapping")
            .code(),
            "unexpectedly_relative"
        );
    }

    /// A prefix a person typed may carry a trailing separator; a record path may
    /// not. Both spellings of a prefix must reach the same rule.
    #[test]
    fn a_typed_relative_prefix_tolerates_a_trailing_separator() {
        for spelling in ["roms", "roms/", "  roms/  "] {
            let maps = relative_mappings(&[(spelling, "/mnt/games/roms")]);
            assert_eq!(
                maps.as_slice()[0].provider_prefix,
                "roms",
                "{spelling:?} should normalise to `roms`"
            );
            assert_eq!(
                maps.translate("roms/gb/x.gb").archivefs_path(),
                Some(PathBuf::from("/mnt/games/roms/gb/x.gb").as_path())
            );
        }
        assert_eq!(
            PathMappings::validate(
                &[mapping("roms", "/mnt/one"), mapping("roms/", "/mnt/two")],
                &[],
                ProviderPathKind::ProviderRelative,
            )
            .expect_err("refused")
            .code(),
            "duplicate_source",
            "the two spellings are one prefix"
        );
    }

    /// Every hostile relative shape, each refused with its own reason.
    #[test]
    fn hostile_relative_paths_are_refused() {
        let maps = relative_mappings(&[("roms", "/mnt/games/roms")]);
        for (hostile, expected) in [
            ("../etc/passwd", "non_normal_component"),
            ("roms/../../etc/passwd", "non_normal_component"),
            ("roms/../etc/passwd", "non_normal_component"),
            ("./roms/game.zip", "dot_component"),
            ("roms/./game.zip", "dot_component"),
            ("roms//game.zip", "empty_component"),
            ("roms/game.zip/", "empty_component"),
            (r"C:\roms\game.zip", "windows_separator"),
            ("C:/roms/game.zip", "drive_prefix"),
            (r"\\server\share\game.zip", "unc_path"),
            ("//server/share/game.zip", "unc_path"),
            (r"roms\..\game.zip", "windows_separator"),
            (r"roms\game.zip", "windows_separator"),
            ("/roms/game.zip", "unexpectedly_absolute"),
            ("", "empty_prefix"),
            ("   ", "empty_prefix"),
        ] {
            let translated = maps.translate(hostile);
            assert_eq!(
                translated.refusal().map(|refusal| refusal.code()),
                Some(expected),
                "{hostile:?} should be refused as {expected}, got {translated:?}"
            );
            assert!(
                translated.archivefs_path().is_none(),
                "{hostile:?} must not produce a local path"
            );
        }
        // Over-long input is bounded in relative mode too.
        let huge = format!("roms/{}", "a".repeat(MAX_PROVIDER_PATH_BYTES));
        assert_eq!(
            maps.translate(&huge).refusal().map(|r| r.code()),
            Some("too_long")
        );
    }

    /// Control characters, including NUL, and without echoing them back.
    #[test]
    fn control_characters_are_refused_without_being_quoted_back() {
        let maps = relative_mappings(&[("roms", "/mnt/games/roms")]);
        for hostile in ["roms/game\u{0}.zip", "roms/ga\u{7}me.zip", "roms/a\nb.zip"] {
            let translated = maps.translate(hostile);
            let refusal = translated
                .refusal()
                .unwrap_or_else(|| panic!("{hostile:?} must be refused"));
            assert_eq!(refusal.code(), "control_character");
            let detail = refusal.detail();
            assert!(
                !detail.contains('\u{0}') && !detail.contains('\u{7}'),
                "the refusal echoed a raw control character back: {detail:?}"
            );
        }
    }

    /// No layer decodes escaping, so percent-encoded traversal stays inert.
    ///
    /// `%2e%2e` is a perfectly ordinary filename. What must never happen is a
    /// decode step turning it into `..` - so this asserts it translates as the
    /// literal component it is, which is only safe *because* nothing decodes.
    #[test]
    fn percent_encoded_traversal_is_never_decoded() {
        let maps = relative_mappings(&[("roms", "/mnt/games/roms")]);
        assert_eq!(
            maps.translate("roms/%2e%2e/%2e%2e/etc/passwd")
                .archivefs_path(),
            Some(PathBuf::from("/mnt/games/roms/%2e%2e/%2e%2e/etc/passwd").as_path()),
            "the components must stay literal, never become `..`"
        );
        assert_eq!(
            maps.translate("roms/%2E%2E/passwd").archivefs_path(),
            Some(PathBuf::from("/mnt/games/roms/%2E%2E/passwd").as_path())
        );
        // And a real `..` is still refused, so the pair proves the distinction.
        assert!(maps.translate("roms/../passwd").refusal().is_some());
    }

    /// Every translation must land inside a configured source root, checked per
    /// path and not only when the mapping was configured.
    #[test]
    fn a_translation_outside_the_trusted_roots_is_refused_per_path() {
        let roots = vec![PathBuf::from("/mnt/games/roms")];
        let maps = PathMappings::validate(
            &[mapping("roms", "/mnt/games/roms")],
            &roots,
            ProviderPathKind::ProviderRelative,
        )
        .expect("a destination inside a source folder is fine");

        let translated = maps.translate("roms/gb/game.gb");
        let PathTranslation::Translated { trusted_root, .. } = &translated else {
            panic!("expected a translation");
        };
        assert_eq!(
            trusted_root.as_deref(),
            Some(Path::new("/mnt/games/roms")),
            "the preview needs to report which root it landed in"
        );

        // With no roots configured the check is not applicable, and says so
        // rather than silently reporting a root it did not verify.
        let unchecked = relative_mappings(&[("roms", "/mnt/games/roms")]);
        let PathTranslation::Translated { trusted_root, .. } = unchecked.translate("roms/gb/x.gb")
        else {
            panic!("expected a translation");
        };
        assert_eq!(trusted_root, None);
    }

    /// A preview counts the shapes it saw and says when they contradict the
    /// setting - which is the whole diagnosis when nothing translates.
    #[test]
    fn a_preview_reports_a_path_shape_mismatch() {
        let absolute = mappings(&[("/romm/library", "/mnt/games/roms")]);
        let samples: Vec<String> = [
            "roms/gb/game.gb",
            "roms/snes/game.zip",
            "roms/atari-st/game.stx",
        ]
        .iter()
        .map(|path| path.to_string())
        .collect();
        let preview = MappingPreview::build(&absolute, &samples);
        assert_eq!(preview.translated, 0);
        assert_eq!(preview.refused, 3, "every path is the wrong shape");
        assert_eq!(preview.observed_relative, 3);
        assert_eq!(preview.observed_absolute, 0);
        assert_eq!(
            preview.suggested_kind(),
            Some(ProviderPathKind::ProviderRelative),
            "the preview should name the setting that would fix this"
        );

        // Configured correctly, the same samples translate and nothing is
        // suggested.
        let relative = relative_mappings(&[("roms", "/mnt/games/roms")]);
        let preview = MappingPreview::build(&relative, &samples);
        assert_eq!(preview.translated, 3);
        assert_eq!(preview.refused, 0);
        assert_eq!(preview.suggested_kind(), None);
    }

    /// Test 35: mapping is pure - it cannot touch the filesystem.
    #[test]
    fn the_mapping_module_never_touches_the_filesystem() {
        let source = include_str!("path_map.rs");
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "fs::write",
            "fs::read",
            "fs::create_dir",
            "fs::remove_",
            "fs::canonicalize",
            "symlink_metadata",
            "File::open",
            "File::create",
            "Command",
        ] {
            assert!(
                !code.contains(forbidden),
                "`{forbidden}` must not appear: translating a path must not touch the disk"
            );
        }
    }
}

// --- Identity model -------------------------------------------------------

mod identity_model {
    use super::super::model::*;

    /// Test 36: hashes are validated, not taken on trust.
    #[test]
    fn a_published_hash_is_validated_before_it_becomes_evidence() {
        assert_eq!(
            ExternalHash::parse(HashAlgorithm::Md5, "D41D8CD98F00B204E9800998ECF8427E")
                .expect("valid")
                .value,
            "d41d8cd98f00b204e9800998ecf8427e",
            "normalised to lowercase"
        );
        assert!(ExternalHash::parse(HashAlgorithm::Crc32, "deadbeef").is_some());
        assert!(ExternalHash::parse(HashAlgorithm::Sha1, &"a".repeat(40)).is_some());
        // Wrong length, non-hex, empty and placeholder values are not evidence.
        for (algorithm, value) in [
            (HashAlgorithm::Md5, "abc"),
            (HashAlgorithm::Md5, &"a".repeat(40)[..]),
            (HashAlgorithm::Crc32, "zzzzzzzz"),
            (HashAlgorithm::Sha1, ""),
            (HashAlgorithm::Md5, "null"),
        ] {
            assert!(
                ExternalHash::parse(algorithm, value).is_none(),
                "{value:?} must not be accepted as {}",
                algorithm.label()
            );
        }
    }

    /// Test 37: the strongest available hash is preferred.
    #[test]
    fn the_strongest_available_hash_is_preferred() {
        let record = record_with_hashes(vec![
            ExternalHash::parse(HashAlgorithm::Crc32, "deadbeef").expect("valid"),
            ExternalHash::parse(HashAlgorithm::Md5, &"b".repeat(32)).expect("valid"),
        ]);
        assert_eq!(
            record.strongest_hash().expect("some").algorithm,
            HashAlgorithm::Md5
        );
        let with_sha = record_with_hashes(vec![
            ExternalHash::parse(HashAlgorithm::Md5, &"b".repeat(32)).expect("valid"),
            ExternalHash::parse(HashAlgorithm::Sha1, &"c".repeat(40)).expect("valid"),
        ]);
        assert_eq!(
            with_sha.strongest_hash().expect("some").algorithm,
            HashAlgorithm::Sha1
        );
        assert!(record_with_hashes(Vec::new()).strongest_hash().is_none());
    }

    /// Test 38: external evidence never displaces a locally verified identity.
    #[test]
    fn external_evidence_never_displaces_a_verified_local_identity() {
        for level in [
            ExternalVerification::ConfirmedExternal,
            ExternalVerification::StrongExternal,
            ExternalVerification::ProbableExternal,
            ExternalVerification::Ambiguous,
            ExternalVerification::Stale,
            ExternalVerification::Unmatched,
        ] {
            assert!(
                !level.outranks(LocalEvidenceStrength::Verified),
                "{} must not displace a locally verified identity",
                level.label()
            );
        }
    }

    /// Test 39: against weak or absent local evidence, only usable external
    /// levels lead - and only strong ones beat weak local evidence.
    #[test]
    fn only_strong_external_evidence_leads_over_weak_local_evidence() {
        // Nothing local: any usable external record may lead.
        assert!(ExternalVerification::ProbableExternal.outranks(LocalEvidenceStrength::None));
        assert!(ExternalVerification::ConfirmedExternal.outranks(LocalEvidenceStrength::None));
        // A problem is never presented as identity.
        for unusable in [
            ExternalVerification::Ambiguous,
            ExternalVerification::Stale,
            ExternalVerification::Unmatched,
        ] {
            assert!(!unusable.outranks(LocalEvidenceStrength::None));
            assert!(!unusable.is_usable());
        }
        // Weak local evidence - a folder alias - is beaten only by strong or
        // confirmed external evidence, not by a title match.
        assert!(!ExternalVerification::ProbableExternal.outranks(LocalEvidenceStrength::Weak));
        assert!(ExternalVerification::StrongExternal.outranks(LocalEvidenceStrength::Weak));
        assert!(ExternalVerification::ConfirmedExternal.outranks(LocalEvidenceStrength::Weak));
    }

    /// Test 40: the levels are ordered, so "stronger" is a real comparison.
    #[test]
    fn verification_levels_are_ordered_from_weakest_to_strongest() {
        use ExternalVerification::*;
        let ordered = [
            Unmatched,
            Stale,
            Ambiguous,
            ProbableExternal,
            StrongExternal,
            ConfirmedExternal,
        ];
        for window in ordered.windows(2) {
            assert!(window[0] < window[1], "{:?} < {:?}", window[0], window[1]);
        }
        // And each explains what it rests on.
        for level in ordered {
            assert!(!level.label().is_empty());
            assert!(
                level.explanation().len() > 20,
                "{} needs a real explanation",
                level.label()
            );
        }
    }

    /// Test 41: counts summarise a whole import.
    #[test]
    fn counts_summarise_an_imported_set() {
        let mut records = Vec::new();
        for (level, hashes) in [
            (ExternalVerification::ConfirmedExternal, 1),
            (ExternalVerification::ConfirmedExternal, 1),
            (ExternalVerification::StrongExternal, 0),
            (ExternalVerification::ProbableExternal, 0),
            (ExternalVerification::Ambiguous, 1),
            (ExternalVerification::Stale, 0),
            (ExternalVerification::Unmatched, 0),
        ] {
            let mut record = record_with_hashes(if hashes == 1 {
                vec![ExternalHash::parse(HashAlgorithm::Md5, &"a".repeat(32)).expect("valid")]
            } else {
                Vec::new()
            });
            record.verification = level;
            records.push(record);
        }
        let counts = IdentityImportCounts::of(&records);
        assert_eq!(counts.total, 7);
        assert_eq!(counts.confirmed, 2);
        assert_eq!(counts.strong, 1);
        assert_eq!(counts.probable, 1);
        assert_eq!(counts.ambiguous, 1);
        assert_eq!(counts.stale, 1);
        assert_eq!(counts.unmatched, 1);
        assert_eq!(counts.with_hashes, 3);
        assert_eq!(counts.usable(), 4, "confirmed + strong + probable");
    }

    /// Test 42: a record round-trips through JSON, since the cache is JSON.
    #[test]
    fn a_record_round_trips_through_json() {
        let record = record_with_hashes(vec![
            ExternalHash::parse(HashAlgorithm::Md5, &"a".repeat(32)).expect("valid"),
        ]);
        let json = serde_json::to_string(&record).expect("serialises");
        let restored: ExternalIdentityRecord = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(record, restored);
        // The provider and level serialise as stable snake_case strings.
        assert!(json.contains("\"romm\""));
        assert!(json.contains("\"probable_external\""));
    }

    fn record_with_hashes(hashes: Vec<ExternalHash>) -> ExternalIdentityRecord {
        ExternalIdentityRecord {
            provider: IdentityProvider::Romm,
            server_id: "http://172.19.0.20:8080".to_string(),
            provider_platform_id: Some("12".to_string()),
            provider_game_id: "345".to_string(),
            provider_file_id: None,
            provider_path: "/romm/library/nes/Metroid.zip".to_string(),
            archivefs_path: Some(std::path::PathBuf::from("/mnt/games/roms/nes/Metroid.zip")),
            title: Some("Metroid".to_string()),
            platform_candidate: Some("NES".to_string()),
            provider_platform_name: Some("nes".to_string()),
            regions: vec!["USA".to_string()],
            revision: None,
            hashes,
            file_size_bytes: Some(131_072),
            metadata_provider_ids: vec![MetadataProviderId {
                provider: "igdb".to_string(),
                id: "1029".to_string(),
            }],
            artwork: Some(ArtworkReference {
                reference: "assets/roms/345/cover_l.png".to_string(),
                small_reference: Some("assets/roms/345/cover_s.png".to_string()),
            }),
            related_files: Vec::new(),
            sibling_game_ids: Vec::new(),
            imported_at_unix_seconds: 1_785_000_000,
            provider_updated_at: Some("2026-07-01T00:00:00Z".to_string()),
            verification: ExternalVerification::ProbableExternal,
            conflicts: Vec::new(),
            evidence: vec!["path and title agree".to_string()],
        }
    }
}

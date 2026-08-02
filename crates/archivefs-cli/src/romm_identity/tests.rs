//! Stage 1C tests: the RomM identity CLI.
//!
//! Every test runs against a temporary tree and, where a server is needed, a
//! loopback stub built in-process. Nothing here contacts a real RomM instance,
//! reads the machine's own configuration, or touches a real library.

use super::*;
use std::io::{BufRead, BufReader, Read};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

// --- Fixtures -------------------------------------------------------------

/// A temporary tree: a library to stand in for a source folder, an identity root
/// for ArchiveFS's own files, and a place for token files.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-romm-1c-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for directory in ["library", "identity", "elsewhere"] {
            std::fs::create_dir_all(root.join(directory)).expect("fixture");
        }
        Self { root }
    }

    fn library(&self) -> PathBuf {
        self.root.join("library")
    }

    fn identity(&self) -> PathBuf {
        self.root.join("identity")
    }

    fn elsewhere(&self) -> PathBuf {
        self.root.join("elsewhere")
    }

    fn file(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.library().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture");
        }
        std::fs::write(&path, contents).expect("fixture");
        path
    }

    /// A token file with restrictive permissions, as the policy requires.
    fn token_file(&self, contents: &str) -> PathBuf {
        let path = self.root.join("token");
        std::fs::write(&path, contents).expect("fixture");
        set_mode(&path, 0o600);
        path
    }

    fn config_path(&self) -> PathBuf {
        self.identity().join("romm").join("config.json")
    }

    fn cache_path(&self) -> PathBuf {
        self.identity().join("romm").join("identity-cache.json")
    }

    /// Everything ArchiveFS wrote under the identity root, concatenated, so a
    /// test can assert a secret appears in none of it.
    fn all_written_text(&self) -> String {
        let mut text = String::new();
        collect_text(&self.identity(), &mut text);
        text
    }

    fn run(&self, args: &[&str]) -> CapturedRun {
        let library = self.library();
        self.run_with_roots(args, &[library.as_path()])
    }

    /// A run with no configured source folders at all.
    fn run_without_roots(&self, args: &[&str]) -> CapturedRun {
        self.run_with_roots(args, &[])
    }

    fn run_with_roots(&self, args: &[&str], roots: &[&Path]) -> CapturedRun {
        let mut full: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        full.push("--identity-root".to_string());
        full.push(self.identity().display().to_string());
        let borrowed: Vec<&str> = full.iter().map(String::as_str).collect();
        run_captured(&borrowed, roots)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("fixture");
}

fn collect_text(directory: &Path, into: &mut String) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_text(&path, into);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            into.push_str(&text);
            into.push('\n');
        }
    }
}

// --- The loopback stub ----------------------------------------------------

/// The token the stub accepts. A distinctive value, so a test can prove it
/// appears nowhere it should not.
const STUB_TOKEN: &str = "af-stage1c-secret-token-do-not-print";

/// A RomM stand-in on 127.0.0.1, serving only GETs.
///
/// It has no write endpoints at all, so "no command writes to RomM" is not merely
/// unasserted here - there is nothing a write could reach.
struct StubServer {
    port: u16,
    /// Requests for more than this many records are answered as too large, the
    /// way a real oversized body is refused before being read.
    max_safe_limit: std::sync::Arc<AtomicUsize>,
    requests: std::sync::Arc<AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubServer {
    fn start(roms: serde_json::Value, total: u64) -> Self {
        Self::start_with_limit(roms, total, usize::MAX)
    }

    /// A stub that refuses any page larger than `max_safe_limit` records.
    fn start_with_limit(roms: serde_json::Value, total: u64, max_safe_limit: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        listener
            .set_nonblocking(true)
            .expect("the accept loop needs to be interruptible");
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let ceiling = std::sync::Arc::new(AtomicUsize::new(max_safe_limit));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let counter = requests.clone();
        let served_ceiling = ceiling.clone();
        let stopper = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stopper.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.set_nonblocking(false);
                        serve(stream, &roms, total, served_ceiling.load(Ordering::SeqCst));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            max_safe_limit: ceiling,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    /// Raises the ceiling part-way through a test.
    #[allow(dead_code)]
    fn allow_pages_up_to(&self, limit: usize) {
        self.max_safe_limit.store(limit, Ordering::SeqCst);
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Stops accepting, so a later command sees a refused connection.
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(mut stream: TcpStream, roms: &serde_json::Value, total: u64, max_safe_limit: usize) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut authorization = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("authorization:") {
            authorization = value.trim().to_string();
        }
    }
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let expected = format!("bearer {}", STUB_TOKEN.to_ascii_lowercase());
    let (status, body) = match path {
        "/api/heartbeat" => (
            200,
            serde_json::json!({
                "SYSTEM": {"VERSION": "5.1.0"},
                "FILESYSTEM": {"FS_PLATFORMS": ["gb"]},
                "METADATA_SOURCES": {"ANY_SOURCE_ENABLED": true}
            }),
        ),
        "/openapi.json" => (200, openapi()),
        _ if authorization != expected => (401, serde_json::json!({"detail": "Not authenticated"})),
        "/api/platforms" => (
            200,
            serde_json::json!([{
                "id": 7, "slug": "gb", "fs_slug": "gb", "name": "Game Boy", "rom_count": total
            }]),
        ),
        "/api/roms" => {
            let number = |key: &str, fallback: usize| {
                query
                    .split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(name, _)| *name == key)
                    .and_then(|(_, value)| value.parse::<usize>().ok())
                    .unwrap_or(fallback)
            };
            let limit = number("limit", 50);
            let offset = number("offset", 0);
            if limit > max_safe_limit {
                // Padded past the client's ceiling, so the client refuses it for
                // its size exactly as it would a real oversized catalogue page.
                // Declaring a huge Content-Length would do it too, but sending the
                // bytes proves the refusal happens on the body.
                let filler = "x".repeat(9 * 1024 * 1024);
                (200, serde_json::json!({"items": [], "padding": filler}))
            } else {
                let items = roms.as_array().cloned().unwrap_or_default();
                let page: Vec<serde_json::Value> =
                    items.into_iter().skip(offset).take(limit).collect();
                (
                    200,
                    serde_json::json!({
                        "items": page, "total": total, "limit": limit, "offset": offset
                    }),
                )
            }
        }
        _ => (404, serde_json::json!({"detail": "Not Found"})),
    };

    let encoded = serde_json::to_vec(&body).expect("serialise");
    let header = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        encoded.len()
    );
    use std::io::Write as _;
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&encoded);
    let _ = stream.flush();
    // Drain whatever is left, so the client sees a clean close rather than a reset.
    let mut sink = Vec::new();
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));
    let _ = stream.take(1024).read_to_end(&mut sink);
}

fn openapi() -> serde_json::Value {
    serde_json::json!({
        "info": {"version": "5.1.0"},
        "paths": {
            "/api/platforms": {"get": {"security": [{"OAuth2PasswordBearer": ["platforms.read"]}]}},
            "/api/roms": {"get": {
                "security": [{"OAuth2PasswordBearer": ["roms.read"]}],
                "parameters": [{"name": "limit"}, {"name": "offset"}]
            }},
            "/api/client-tokens": {"post": {}}
        },
        "components": {"schemas": {"SimpleRomSchema": {"properties": {
            "id": {}, "md5_hash": {}, "sha1_hash": {}, "crc_hash": {},
            "url_cover": {}, "path_cover_small": {}, "path_cover_large": {}, "files": {}
        }}}}
    })
}

/// One ROM record as RomM would report it.
fn rom(id: u64, name: &str, file_name: &str, size: u64, hashes: [&str; 3]) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "platform_id": 7,
        "platform_slug": "gb",
        "platform_display_name": "Game Boy",
        "name": name,
        "fs_path": "/romm/library/gb",
        "fs_name": file_name,
        "full_path": format!("/romm/library/gb/{file_name}"),
        "fs_size_bytes": size,
        "crc_hash": hashes[0],
        "md5_hash": hashes[1],
        "sha1_hash": hashes[2],
        "regions": ["USA"],
        "igdb_id": 4242,
        "updated_at": "2026-07-30T12:00:00Z",
        "missing_from_fs": false,
        "has_multiple_files": false
    })
}

/// One ROM record in the shape RomM 5.1.0 actually returns: paths relative to
/// the instance's own library base, with no leading separator.
fn relative_rom(
    id: u64,
    name: &str,
    relative_path: &str,
    size: u64,
    hashes: [&str; 3],
) -> serde_json::Value {
    let (directory, file_name) = relative_path
        .rsplit_once('/')
        .unwrap_or(("roms", relative_path));
    serde_json::json!({
        "id": id,
        "platform_id": 7,
        "platform_slug": "gb",
        "platform_display_name": "Game Boy",
        "name": name,
        "fs_path": directory,
        "fs_name": file_name,
        "full_path": relative_path,
        "fs_size_bytes": size,
        "crc_hash": hashes[0],
        "md5_hash": hashes[1],
        "sha1_hash": hashes[2],
        "regions": ["USA"],
        "updated_at": "2026-07-30T12:00:00Z",
        "missing_from_fs": false,
        "has_multiple_files": false
    })
}

/// A source configured for provider-relative paths, mapping `roms` at the whole
/// fixture library, which is how the live server's shape has to be handled.
fn ready_relative(tree: &Tree, stub: &StubServer) {
    let token = tree.token_file(STUB_TOKEN);
    let configured = tree.run(&[
        "configure",
        "--url",
        &stub.url(),
        "--token-file",
        &token.display().to_string(),
        "--path-kind",
        "relative",
        "--enable",
    ]);
    assert!(configured.succeeded(), "{:?}", configured.error);
    let mapped = tree.run(&[
        "mappings",
        "add",
        "--romm-root",
        "roms",
        "--archivefs-root",
        &tree.library().display().to_string(),
    ]);
    assert!(mapped.succeeded(), "{:?}", mapped.error);
}

/// Placeholder hashes, well-formed but belonging to nothing.
fn dud_hashes() -> [String; 3] {
    ["00000000".to_string(), "0".repeat(32), "0".repeat(40)]
}

/// A bank of records with dud hashes, for tests about paging rather than matching.
fn many_roms(count: u64) -> serde_json::Value {
    let dud = dud_hashes();
    let roms: Vec<serde_json::Value> = (0..count)
        .map(|index| {
            rom(
                index,
                &format!("Game {index}"),
                &format!("{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    serde_json::json!(roms)
}

/// The real hashes of `contents`, so a test asserts genuine agreement rather than
/// agreement with a value it also invented.
fn true_hashes(contents: &[u8]) -> [String; 3] {
    let directory = std::env::temp_dir().join(format!(
        "archivefs-romm-1c-hash-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&directory).expect("fixture");
    let file = directory.join("payload.gb");
    std::fs::write(&file, contents).expect("fixture");
    let trusted = TrustedRoots::from_paths(std::slice::from_ref(&directory));
    let hashed = hash_file(&file, &trusted, None).expect("the fixture file must be hashable");
    let _ = std::fs::remove_dir_all(&directory);
    [hashed.crc32, hashed.md5, hashed.sha1]
}

/// A fully configured, enabled source with one mapping, ready to import.
fn ready(tree: &Tree, stub: &StubServer) {
    let token = tree.token_file(STUB_TOKEN);
    let configured = tree.run(&[
        "configure",
        "--url",
        &stub.url(),
        "--token-file",
        &token.display().to_string(),
        "--enable",
    ]);
    assert!(configured.succeeded(), "{:?}", configured.error);
    let destination = tree.library().join("gb");
    std::fs::create_dir_all(&destination).expect("fixture");
    let mapped = tree.run(&[
        "mappings",
        "add",
        "--romm-root",
        "/romm/library/gb",
        "--archivefs-root",
        &destination.display().to_string(),
    ]);
    assert!(mapped.succeeded(), "{:?}", mapped.error);
}

// --- Argument handling ----------------------------------------------------

#[test]
fn a_missing_command_lists_what_is_available() {
    let tree = Tree::new("no-command");
    let run = tree.run(&[]);
    let message = run.error_text().to_string();
    assert!(message.contains("requires a command"), "{message}");
    for command in COMMANDS {
        assert!(
            message.contains(command),
            "the refusal should name {command}: {message}"
        );
    }
}

#[test]
fn an_unknown_command_is_named_back_and_the_valid_ones_listed() {
    let tree = Tree::new("bad-command");
    let message = tree.run(&["frobnicate"]).error_text().to_string();
    assert!(message.contains("frobnicate"), "{message}");
    assert!(message.contains("status"), "{message}");
}

#[test]
fn every_advertised_command_is_reachable() {
    // Guards against the command list and the dispatch drifting apart: each name
    // must produce something other than "unknown command".
    let tree = Tree::new("reachable");
    for command in COMMANDS {
        let run = tree.run(&[command]);
        if let Some(error) = &run.error {
            assert!(
                !error.contains("unknown identity source romm command"),
                "{command} is advertised but not dispatched: {error}"
            );
        }
    }
}

#[test]
fn a_flag_without_a_value_is_refused() {
    let tree = Tree::new("flag-no-value");
    let message = tree.run(&["configure", "--url"]).error_text().to_string();
    assert!(message.contains("--url needs a value"), "{message}");
}

#[test]
fn a_flag_followed_by_another_flag_is_refused() {
    let tree = Tree::new("flag-flag");
    let message = tree
        .run(&["configure", "--url", "--token-file"])
        .error_text()
        .to_string();
    assert!(message.contains("not another flag"), "{message}");
}

#[test]
fn a_repeated_flag_is_refused_rather_than_silently_taking_one() {
    let tree = Tree::new("flag-twice");
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:1",
            "--url",
            "http://127.0.0.1:2",
        ])
        .error_text()
        .to_string();
    assert!(message.contains("more than once"), "{message}");
}

#[test]
fn unexpected_arguments_are_refused_rather_than_ignored() {
    let tree = Tree::new("extra-args");
    let message = tree.run(&["status", "--wat"]).error_text().to_string();
    assert!(message.contains("does not accept"), "{message}");
    assert!(message.contains("--wat"), "{message}");
}

#[test]
fn a_non_numeric_limit_is_refused() {
    let tree = Tree::new("bad-limit");
    let message = tree
        .run(&["records", "--limit", "many"])
        .error_text()
        .to_string();
    assert!(message.contains("whole number"), "{message}");
    assert!(message.contains("many"), "{message}");
}

#[test]
fn an_unknown_status_filter_lists_the_real_ones() {
    let tree = Tree::new("bad-status");
    let message = tree
        .run(&["records", "--status", "brilliant"])
        .error_text()
        .to_string();
    assert!(message.contains("brilliant"), "{message}");
    for slug in [
        "confirmed",
        "strong",
        "probable",
        "ambiguous",
        "stale",
        "unmatched",
    ] {
        assert!(message.contains(slug), "{message}");
    }
}

#[test]
fn every_printed_status_slug_can_be_used_as_a_filter() {
    // The slugs the listing prints and the slugs the filter accepts are one
    // vocabulary, so a value can be copied straight back in.
    for verification in [
        ExternalVerification::ConfirmedExternal,
        ExternalVerification::StrongExternal,
        ExternalVerification::ProbableExternal,
        ExternalVerification::Ambiguous,
        ExternalVerification::Stale,
        ExternalVerification::Unmatched,
    ] {
        let slug = verification_slug(verification);
        assert_eq!(
            parse_verification(slug),
            Ok(verification),
            "{slug} is printed but not accepted back"
        );
    }
}

// --- Token handling -------------------------------------------------------

#[test]
fn a_token_on_the_command_line_is_not_a_thing() {
    let tree = Tree::new("no-token-flag");
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token",
            "hunter2",
        ])
        .error_text()
        .to_string();
    assert!(message.contains("does not accept"), "{message}");
    assert!(message.contains("--token"), "{message}");
}

#[test]
fn a_symlinked_token_file_is_refused() {
    let tree = Tree::new("token-symlink");
    let real = tree.token_file(STUB_TOKEN);
    let link = tree.root.join("token-link");
    std::os::unix::fs::symlink(&real, &link).expect("fixture");
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &link.display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(message.contains("symlink"), "{message}");
}

#[test]
fn a_token_file_others_can_read_is_refused_with_the_fix() {
    let tree = Tree::new("token-open");
    let token = tree.token_file(STUB_TOKEN);
    set_mode(&token, 0o644);
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &token.display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(message.contains("readable by others"), "{message}");
    assert!(
        message.contains("chmod 600"),
        "the fix should be stated: {message}"
    );
    assert!(!message.contains(STUB_TOKEN), "the token leaked: {message}");
}

#[test]
fn a_directory_given_as_a_token_file_is_refused() {
    let tree = Tree::new("token-dir");
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &tree.elsewhere().display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(message.contains("regular file"), "{message}");
}

#[test]
fn an_empty_token_file_is_refused() {
    let tree = Tree::new("token-empty");
    let token = tree.token_file("");
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &token.display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(message.contains("usable token"), "{message}");
}

#[test]
fn a_whitespace_only_token_file_is_refused() {
    let tree = Tree::new("token-blank");
    let token = tree.token_file("   \n");
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &token.display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(message.contains("usable token"), "{message}");
}

#[test]
fn a_missing_token_file_is_refused_by_name() {
    let tree = Tree::new("token-missing");
    let absent = tree.root.join("no-such-token");
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &absent.display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(message.contains("does not exist"), "{message}");
}

#[test]
fn a_trailing_newline_is_trimmed_and_the_token_still_works() {
    // The CLI layer strips exactly one trailing newline, because that is what an
    // editor or `echo` adds. It cannot change a token's value by stripping too
    // much: a valid token contains no whitespace at all, so `parse` refuses
    // anything an over-eager trim could have salvaged.
    let tree = Tree::new("token-newline");
    let path = tree.root.join("token");
    std::fs::write(&path, format!("{STUB_TOKEN}\n")).expect("fixture");
    set_mode(&path, 0o600);

    let with_newline = load_token_file(Some(&path)).expect("one trailing newline is trimmed");
    std::fs::write(&path, STUB_TOKEN).expect("fixture");
    let without = load_token_file(Some(&path)).expect("no trailing newline is fine either");
    assert_eq!(
        with_newline.fingerprint(),
        without.fingerprint(),
        "a trailing newline changed the token"
    );
}

#[test]
fn a_token_containing_whitespace_is_refused_rather_than_repaired() {
    let tree = Tree::new("token-inner-space");
    let path = tree.root.join("token");
    std::fs::write(&path, "two words\n").expect("fixture");
    set_mode(&path, 0o600);
    let refusal = load_token_file(Some(&path));
    assert!(
        refusal.is_err(),
        "a token with embedded whitespace cannot be a header value"
    );
}

#[test]
fn the_token_is_written_nowhere_and_printed_nowhere() {
    let tree = Tree::new("token-secrecy");
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([rom(1, "A Game", "a.gb", 4, [&dud[0], &dud[1], &dud[2]])]),
        1,
    );
    ready(&tree, &stub);
    tree.file("gb/a.gb", b"data");

    let import = tree.run(&["import"]);
    assert!(import.succeeded(), "{:?}", import.error);
    let status = tree.run(&["status"]);
    let test = tree.run(&["test"]);
    let records = tree.run(&["records", "--json"]);

    for (label, run) in [
        ("import", &import),
        ("status", &status),
        ("test", &test),
        ("records", &records),
    ] {
        assert!(
            !run.stdout.contains(STUB_TOKEN),
            "{label} printed the token on stdout"
        );
        assert!(
            !run.stderr.contains(STUB_TOKEN),
            "{label} printed the token on stderr"
        );
    }
    let written = tree.all_written_text();
    assert!(
        !written.contains(STUB_TOKEN),
        "the token was written into ArchiveFS's own files"
    );
    assert!(
        !written.contains("Bearer"),
        "an authorization header was written to disk"
    );
    // The path is recorded; the value is not.
    assert!(written.contains("token_path"), "the path should be stored");
    stub.stop();
}

#[test]
fn the_config_file_is_private_and_holds_no_secret() {
    let tree = Tree::new("config-perms");
    let token = tree.token_file(STUB_TOKEN);
    let run = tree.run(&[
        "configure",
        "--url",
        "http://127.0.0.1:8080",
        "--token-file",
        &token.display().to_string(),
    ]);
    assert!(run.succeeded(), "{:?}", run.error);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(tree.config_path())
        .expect("config written")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "config.json should be readable only by its owner"
    );
    let text = std::fs::read_to_string(tree.config_path()).expect("config readable");
    assert!(!text.contains(STUB_TOKEN), "the token is in config.json");
}

#[test]
fn a_token_error_is_reported_without_quoting_the_token() {
    let tree = Tree::new("token-redacted");
    let path = tree.root.join("token");
    // Contents whose own text would be tempting to echo back.
    std::fs::write(&path, "\u{0}not-a-usable-token-value").expect("fixture");
    set_mode(&path, 0o600);
    let run = tree.run(&[
        "configure",
        "--url",
        "http://127.0.0.1:8080",
        "--token-file",
        &path.display().to_string(),
    ]);
    let message = run.error_text();
    assert!(
        !message.contains("not-a-usable-token-value"),
        "the file's contents were echoed: {message}"
    );
}

// --- Network policy at the CLI edge ---------------------------------------

#[test]
fn a_public_url_is_refused_at_configuration_time() {
    let tree = Tree::new("url-public");
    let token = tree.token_file(STUB_TOKEN);
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://93.184.216.34:8080",
            "--token-file",
            &token.display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(
        message.contains("not on a local or private network"),
        "{message}"
    );
    assert!(
        !tree.config_path().exists(),
        "a refused URL must not be stored"
    );
}

#[test]
fn a_url_carrying_credentials_is_refused() {
    let tree = Tree::new("url-creds");
    let message = tree
        .run(&["configure", "--url", "http://user:pw@127.0.0.1:8080"])
        .error_text()
        .to_string();
    assert!(message.contains("username or password"), "{message}");
}

#[test]
fn a_file_url_is_refused() {
    let tree = Tree::new("url-file");
    let run = tree.run(&["configure", "--url", "file:///etc/passwd"]);
    assert!(!run.succeeded(), "a file URL is not an identity source");
    assert!(!tree.config_path().exists());
}

#[test]
fn configuring_without_a_url_the_first_time_is_refused() {
    let tree = Tree::new("no-url");
    let token = tree.token_file(STUB_TOKEN);
    let message = tree
        .run(&["configure", "--token-file", &token.display().to_string()])
        .error_text()
        .to_string();
    assert!(message.contains("--url is required"), "{message}");
}

#[test]
fn ordinary_commands_contact_nothing() {
    let tree = Tree::new("no-network");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    ready(&tree, &stub);
    let after_setup = stub.request_count();

    for args in [
        vec!["status"],
        vec!["mappings", "list"],
        vec!["disable"],
        vec!["enable"],
        vec!["records"],
        vec!["conflicts"],
    ] {
        let _ = tree.run(&args);
    }
    assert_eq!(
        stub.request_count(),
        after_setup,
        "a read-only local command reached the server"
    );
    stub.stop();
}

// --- Mappings -------------------------------------------------------------

#[test]
fn a_mapping_destination_outside_the_source_folders_is_refused() {
    let tree = Tree::new("map-outside");
    let message = tree
        .run(&[
            "mappings",
            "add",
            "--romm-root",
            "/romm/library/gb",
            "--archivefs-root",
            &tree.elsewhere().display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(
        message.contains("not inside any configured source folder"),
        "{message}"
    );
}

#[test]
fn a_second_mapping_for_the_same_romm_root_needs_replace() {
    let tree = Tree::new("map-dup");
    let first = tree.library().join("one");
    let second = tree.library().join("two");
    std::fs::create_dir_all(&first).expect("fixture");
    std::fs::create_dir_all(&second).expect("fixture");
    assert!(
        tree.run(&[
            "mappings",
            "add",
            "--romm-root",
            "/romm/gb",
            "--archivefs-root",
            &first.display().to_string(),
        ])
        .succeeded()
    );
    let message = tree
        .run(&[
            "mappings",
            "add",
            "--romm-root",
            "/romm/gb",
            "--archivefs-root",
            &second.display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(message.contains("already exists"), "{message}");
    assert!(
        message.contains("--replace"),
        "the way forward should be stated: {message}"
    );

    let replaced = tree.run(&[
        "mappings",
        "add",
        "--romm-root",
        "/romm/gb",
        "--archivefs-root",
        &second.display().to_string(),
        "--replace",
    ]);
    assert!(replaced.succeeded(), "{:?}", replaced.error);
    assert!(replaced.stdout.contains("/two"), "{}", replaced.stdout);
    assert!(!replaced.stdout.contains("/one"), "{}", replaced.stdout);
}

#[test]
fn two_mappings_pointing_at_the_same_place_are_refused() {
    let tree = Tree::new("map-same-dest");
    let destination = tree.library().join("shared");
    std::fs::create_dir_all(&destination).expect("fixture");
    assert!(
        tree.run(&[
            "mappings",
            "add",
            "--romm-root",
            "/romm/gb",
            "--archivefs-root",
            &destination.display().to_string(),
        ])
        .succeeded()
    );
    let run = tree.run(&[
        "mappings",
        "add",
        "--romm-root",
        "/romm/gbc",
        "--archivefs-root",
        &destination.display().to_string(),
    ]);
    assert!(
        !run.succeeded(),
        "two RomM roots resolving to one directory would make identity ambiguous"
    );
}

#[test]
fn mappings_are_listed_most_specific_first() {
    let tree = Tree::new("map-order");
    for (provider, local) in [("/romm", "broad"), ("/romm/library/gb", "narrow")] {
        let destination = tree.library().join(local);
        std::fs::create_dir_all(&destination).expect("fixture");
        let run = tree.run(&[
            "mappings",
            "add",
            "--romm-root",
            provider,
            "--archivefs-root",
            &destination.display().to_string(),
        ]);
        assert!(run.succeeded(), "{:?}", run.error);
    }
    let run = tree.run(&["mappings", "list", "--json"]);
    let roots: Vec<String> = run.json()["mappings"]
        .as_array()
        .expect("mappings array")
        .iter()
        .map(|entry| entry["romm_root"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(roots, vec!["/romm/library/gb", "/romm"]);
}

#[test]
fn removing_a_mapping_that_is_not_there_is_refused() {
    let tree = Tree::new("map-remove-missing");
    let message = tree
        .run(&["mappings", "remove", "--romm-root", "/romm/nope"])
        .error_text()
        .to_string();
    assert!(message.contains("no mapping starts from"), "{message}");
}

#[test]
fn a_mapping_can_be_removed() {
    let tree = Tree::new("map-remove");
    let destination = tree.library().join("gb");
    std::fs::create_dir_all(&destination).expect("fixture");
    assert!(
        tree.run(&[
            "mappings",
            "add",
            "--romm-root",
            "/romm/gb",
            "--archivefs-root",
            &destination.display().to_string(),
        ])
        .succeeded()
    );
    assert!(
        tree.run(&["mappings", "remove", "--romm-root", "/romm/gb"])
            .succeeded()
    );
    let run = tree.run(&["mappings", "list", "--json"]);
    assert!(run.json()["mappings"].as_array().expect("array").is_empty());
}

#[test]
fn an_unknown_mappings_action_is_refused() {
    let tree = Tree::new("map-action");
    let message = tree.run(&["mappings", "twiddle"]).error_text().to_string();
    assert!(message.contains("twiddle"), "{message}");
}

#[test]
fn preview_reads_the_cache_and_says_which_paths_exist() {
    let tree = Tree::new("preview");
    let contents = b"a real file".to_vec();
    let hashes = true_hashes(&contents);
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([
            rom(
                1,
                "Present",
                "present.gb",
                contents.len() as u64,
                [&hashes[0], &hashes[1], &hashes[2]]
            ),
            rom(2, "Absent", "absent.gb", 10, [&dud[0], &dud[1], &dud[2]]),
        ]),
        2,
    );
    ready(&tree, &stub);
    tree.file("gb/present.gb", &contents);
    assert!(tree.run(&["import"]).succeeded());

    let run = tree.run(&["mappings", "preview", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let preview = run.json();
    assert_eq!(preview["sample_source"], "cached identity");
    assert_eq!(preview["translated"], 2);
    let examples = preview["examples"].as_array().expect("examples");
    let present = examples
        .iter()
        .find(|example| {
            example["romm_path"]
                .as_str()
                .is_some_and(|path| path.ends_with("present.gb"))
        })
        .expect("the present file should be previewed");
    let absent = examples
        .iter()
        .find(|example| {
            example["romm_path"]
                .as_str()
                .is_some_and(|path| path.ends_with("absent.gb"))
        })
        .expect("the absent file should be previewed");
    assert_eq!(present["file_exists"], true);
    assert_eq!(absent["file_exists"], false);
    assert_eq!(present["canonical_platform"], "Game Boy");
    stub.stop();
}

#[test]
fn preview_is_bounded_however_large_the_limit_asked_for() {
    let tree = Tree::new("preview-bound");
    let mut stub = StubServer::start(many_roms(40), 40);
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let run = tree.run(&["mappings", "preview", "--limit", "100000", "--json"]);
    let count = run.json()["examples"].as_array().expect("examples").len();
    assert!(
        count <= MAX_PREVIEW_LIMIT,
        "preview returned {count}, above the {MAX_PREVIEW_LIMIT} bound"
    );
    stub.stop();
}

// --- Connection test ------------------------------------------------------

#[test]
fn the_connection_test_reports_capability_and_changes_nothing() {
    let tree = Tree::new("test-ok");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    ready(&tree, &stub);

    let run = tree.run(&["test", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let report = run.json();
    assert_eq!(report["reachable"], true);
    assert_eq!(report["romm_version"], "5.1.0");
    assert_eq!(report["version_supported"], true);
    assert_eq!(report["supports_pagination"], true);
    assert_eq!(report["supports_client_tokens"], true);
    assert_eq!(report["can_import"], true);
    assert_eq!(report["cache_modified"], false);
    assert_eq!(report["romm_modified"], false);
    let scopes: Vec<&str> = report["declared_read_scopes"]
        .as_array()
        .expect("scopes")
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(scopes.contains(&"platforms.read"), "{scopes:?}");
    assert!(scopes.contains(&"roms.read"), "{scopes:?}");
    // Both endpoints were actually read with the token, not merely declared.
    let reads = report["authenticated_reads"].as_array().expect("reads");
    assert_eq!(reads.len(), 2);
    assert!(reads.iter().all(|read| read["ok"] == true), "{reads:?}");
    assert!(
        !tree.cache_path().exists(),
        "a connection test must not publish a cache"
    );
    stub.stop();
}

#[test]
fn a_token_the_server_rejects_is_reported_as_a_failed_read() {
    let tree = Tree::new("test-401");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    let token = tree.token_file("a-token-the-stub-does-not-accept");
    assert!(
        tree.run(&[
            "configure",
            "--url",
            &stub.url(),
            "--token-file",
            &token.display().to_string(),
            "--enable",
        ])
        .succeeded()
    );
    let run = tree.run(&["test"]);
    let message = run.error_text();
    assert!(
        message.contains("platforms.read") || message.contains("roms.read"),
        "the refusal should name the scopes needed: {message}"
    );
    stub.stop();
}

#[test]
fn a_test_against_an_unreachable_server_says_so() {
    let tree = Tree::new("test-down");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    ready(&tree, &stub);
    stub.stop();
    let message = tree.run(&["test"]).error_text().to_string();
    assert!(message.contains("could not reach RomM"), "{message}");
}

// --- Import and refresh ---------------------------------------------------

#[test]
fn an_import_without_configuration_says_what_to_run() {
    let tree = Tree::new("import-unconfigured");
    let message = tree.run(&["import"]).error_text().to_string();
    assert!(message.contains("no RomM URL is configured"), "{message}");
    assert!(message.contains("configure"), "{message}");
}

#[test]
fn an_import_while_disabled_is_refused() {
    let tree = Tree::new("import-disabled");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    ready(&tree, &stub);
    assert!(tree.run(&["disable"]).succeeded());
    let message = tree.run(&["import"]).error_text().to_string();
    assert!(message.contains("disabled"), "{message}");
    stub.stop();
}

#[test]
fn refresh_does_not_take_a_sample_size() {
    let tree = Tree::new("refresh-sample");
    let message = tree
        .run(&["refresh", "--sample", "5"])
        .error_text()
        .to_string();
    assert!(message.contains("does not take --sample"), "{message}");
}

#[test]
fn a_sample_import_publishes_nothing() {
    let tree = Tree::new("sample");
    let mut stub = StubServer::start(many_roms(10), 10);
    ready(&tree, &stub);

    let run = tree.run(&["import", "--sample", "3", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["mode"], "sample");
    assert_eq!(result["sample_limit"], 3);
    assert_eq!(result["published"], false);
    assert_eq!(
        result["records"], 3,
        "a sample must stop at what was asked for"
    );
    assert!(
        !tree.cache_path().exists(),
        "a sample import must not create the active cache"
    );
    stub.stop();
}

#[test]
fn a_sample_import_leaves_an_existing_cache_alone() {
    let tree = Tree::new("sample-keeps-cache");
    let mut stub = StubServer::start(many_roms(6), 6);
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());
    let before = std::fs::read(tree.cache_path()).expect("cache published");

    assert!(tree.run(&["import", "--sample", "2"]).succeeded());
    let after = std::fs::read(tree.cache_path()).expect("cache still there");
    assert_eq!(before, after, "the sample replaced the active cache");
    stub.stop();
}

#[test]
fn a_full_import_publishes_and_can_then_be_browsed_offline() {
    let tree = Tree::new("import-full");
    let contents = b"game data".to_vec();
    let hashes = true_hashes(&contents);
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([
            rom(
                1,
                "Matched",
                "matched.gb",
                contents.len() as u64,
                [&hashes[0], &hashes[1], &hashes[2]]
            ),
            rom(2, "Gone", "gone.gb", 4, [&dud[0], &dud[1], &dud[2]]),
        ]),
        2,
    );
    ready(&tree, &stub);
    tree.file("gb/matched.gb", &contents);

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["mode"], "import");
    assert_eq!(result["published"], true);
    assert_eq!(result["records"], 2);
    assert_eq!(result["platforms"], 1);
    assert!(tree.cache_path().exists());

    // With the server gone, the cache still serves.
    stub.stop();
    let reported = tree.run(&["status", "--json"]).json();
    assert_eq!(reported["records_imported"], 2);
    assert!(
        reported["state"]["state"]
            .as_str()
            .expect("state")
            .starts_with("ready"),
        "{reported:#}"
    );
    let records = tree.run(&["records", "--json"]);
    assert_eq!(records.json()["matching_filters"], 2);
}

#[test]
fn a_failed_refresh_preserves_the_previous_cache() {
    let tree = Tree::new("refresh-fails");
    let mut stub = StubServer::start(many_roms(1), 1);
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());
    let before = std::fs::read(tree.cache_path()).expect("cache published");

    stub.stop();
    let run = tree.run(&["refresh", "--json"]);
    assert!(!run.succeeded(), "a refresh against nothing should fail");
    let result = run.json();
    assert_eq!(result["published"], false);
    assert_eq!(result["previous_cache_usable"], true);
    assert!(result["error_code"].is_string(), "{result:#}");

    let after = std::fs::read(tree.cache_path()).expect("cache preserved");
    assert_eq!(before, after, "a failed refresh replaced the cache");
    // And it is still usable, not merely present.
    assert_eq!(
        tree.run(&["records", "--json"]).json()["matching_filters"],
        1
    );
}

#[test]
fn a_failed_import_leaves_no_temporary_files_behind() {
    let tree = Tree::new("no-temp-files");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    ready(&tree, &stub);
    stub.stop();
    let _ = tree.run(&["refresh"]);

    let leftovers: Vec<String> = std::fs::read_dir(tree.identity().join("romm"))
        .expect("provider directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name != "config.json" && name != "identity-cache.json")
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}

#[test]
fn in_json_mode_stdout_is_one_document_and_progress_goes_nowhere_near_it() {
    let tree = Tree::new("json-purity");
    let mut stub = StubServer::start(many_roms(5), 5);
    ready(&tree, &stub);

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    // Parses as exactly one document, with nothing trailing.
    let mut stream =
        serde_json::Deserializer::from_str(&run.stdout).into_iter::<serde_json::Value>();
    assert!(stream.next().expect("one document").is_ok());
    assert!(
        stream.next().is_none(),
        "stdout held more than one document"
    );
    assert!(
        run.stderr.is_empty(),
        "JSON mode emitted progress chatter: {}",
        run.stderr
    );
    stub.stop();
}

#[test]
fn without_json_progress_goes_to_stderr_and_the_result_to_stdout() {
    let tree = Tree::new("progress-stream");
    let mut stub = StubServer::start(many_roms(5), 5);
    ready(&tree, &stub);

    let run = tree.run(&["import"]);
    assert!(run.succeeded(), "{:?}", run.error);
    assert!(run.stderr.contains("Importing"), "{}", run.stderr);
    assert!(run.stdout.contains("Import complete"), "{}", run.stdout);
    assert!(
        !run.stdout.contains("Importing the full"),
        "progress leaked into stdout: {}",
        run.stdout
    );
    stub.stop();
}

#[test]
fn the_human_and_json_renderings_state_the_same_counts() {
    let tree = Tree::new("same-facts");
    let contents = b"payload".to_vec();
    let hashes = true_hashes(&contents);
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([
            rom(
                1,
                "Matched",
                "matched.gb",
                contents.len() as u64,
                [&hashes[0], &hashes[1], &hashes[2]]
            ),
            rom(2, "Missing", "missing.gb", 9, [&dud[0], &dud[1], &dud[2]]),
        ]),
        2,
    );
    ready(&tree, &stub);
    tree.file("gb/matched.gb", &contents);
    assert!(tree.run(&["import"]).succeeded());

    let json = tree.run(&["status", "--json"]).json();
    let human = tree.run(&["status"]).stdout;
    assert!(
        human.contains(&format!("Records:          {}", json["records_imported"])),
        "human output disagrees with JSON:\n{human}\n{json:#}"
    );
    assert!(
        human.contains(&format!("Stale:            {}", json["counts"]["stale"])),
        "{human}"
    );
    stub.stop();
}

#[test]
fn invalid_provider_hashes_stay_visible_as_rejected() {
    let tree = Tree::new("bad-hashes");
    let mut stub = StubServer::start(
        serde_json::json!([rom(1, "Bad", "bad.gb", 4, ["zzzz", "not-hex", "short"])]),
        1,
    );
    ready(&tree, &stub);

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(
        run.json()["invalid_hashes"],
        3,
        "a rejected hash must be reported, not quietly dropped"
    );
    // And the record carries no hash it could be wrongly matched on.
    let record = tree.run(&["record", "1", "--json"]).json();
    assert!(record["hashes"].as_array().expect("hashes").is_empty());
    stub.stop();
}

#[test]
fn an_unrecognised_platform_is_counted_rather_than_guessed() {
    let tree = Tree::new("unknown-platform");
    let dud = dud_hashes();
    let mut record = rom(1, "Odd", "odd.gb", 4, [&dud[0], &dud[1], &dud[2]]);
    record["platform_slug"] = serde_json::json!("nonexistent-console");
    let mut stub = StubServer::start(serde_json::json!([record]), 1);
    ready(&tree, &stub);

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(run.json()["unknown_platforms"], 1);
    let stored = tree.run(&["record", "1", "--json"]).json();
    assert!(
        stored["canonical_platform"].is_null(),
        "an unknown slug must not be assigned a platform: {stored:#}"
    );
    stub.stop();
}

// --- Records and conflicts -----------------------------------------------

#[test]
fn records_can_be_filtered_and_paged() {
    let tree = Tree::new("records-page");
    let mut stub = StubServer::start(many_roms(12), 12);
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let first = tree.run(&["records", "--limit", "5", "--json"]).json();
    assert_eq!(first["matching_filters"], 12);
    assert_eq!(first["records"].as_array().expect("records").len(), 5);
    assert_eq!(first["offset"], 0);

    let second = tree
        .run(&["records", "--limit", "5", "--offset", "10", "--json"])
        .json();
    assert_eq!(
        second["records"].as_array().expect("records").len(),
        2,
        "the last page should be short, not wrap"
    );

    let ids = |page: &serde_json::Value| -> Vec<String> {
        page["records"]
            .as_array()
            .expect("records")
            .iter()
            .map(|record| {
                record["romm_game_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    };
    let later = ids(&second);
    let overlap = ids(&first)
        .into_iter()
        .filter(|id| later.contains(id))
        .count();
    assert_eq!(overlap, 0, "pages overlapped");

    let beyond = tree.run(&["records", "--offset", "9999", "--json"]).json();
    assert!(
        beyond["records"].as_array().expect("records").is_empty(),
        "an offset past the end should be empty, not an error"
    );
    stub.stop();
}

#[test]
fn a_listing_limit_is_clamped_rather_than_honoured_without_bound() {
    let tree = Tree::new("records-clamp");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());
    let page = tree
        .run(&["records", "--limit", "10000000", "--json"])
        .json();
    assert_eq!(page["limit"], MAX_LIST_LIMIT);
    stub.stop();
}

#[test]
fn records_can_be_filtered_by_platform() {
    let tree = Tree::new("records-platform");
    let dud = dud_hashes();
    let mut other = rom(2, "Other", "other.gb", 4, [&dud[0], &dud[1], &dud[2]]);
    other["platform_slug"] = serde_json::json!("snes");
    let mut stub = StubServer::start(
        serde_json::json!([
            rom(1, "Boy", "boy.gb", 4, [&dud[0], &dud[1], &dud[2]]),
            other,
        ]),
        2,
    );
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let page = tree
        .run(&["records", "--platform", "Game Boy", "--json"])
        .json();
    assert_eq!(page["matching_filters"], 1);
    assert_eq!(page["total_in_cache"], 2);
    stub.stop();
}

#[test]
fn a_listing_without_a_cache_says_how_to_build_one() {
    let tree = Tree::new("records-no-cache");
    let message = tree.run(&["records"]).error_text().to_string();
    assert!(message.contains("import"), "{message}");
}

#[test]
fn a_record_can_be_fetched_by_id_and_an_unknown_id_is_refused() {
    let tree = Tree::new("record-by-id");
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([rom(
            77,
            "Findable",
            "findable.gb",
            4,
            [&dud[0], &dud[1], &dud[2]]
        )]),
        1,
    );
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let found = tree.run(&["record", "77", "--json"]).json();
    assert_eq!(found["romm_game_id"], "77");
    assert_eq!(found["title"], "Findable");
    assert_eq!(found["romm_path"], "/romm/library/gb/findable.gb");
    assert!(found["metadata_provider_ids"].is_array());

    let message = tree.run(&["record", "12345"]).error_text().to_string();
    assert!(message.contains("12345"), "{message}");
    stub.stop();
}

#[test]
fn conflicts_are_listed_with_both_sides_kept() {
    let tree = Tree::new("conflicts");
    let contents = b"contested".to_vec();
    let hashes = true_hashes(&contents);
    // Two RomM records translating to one local file: which one describes it
    // cannot be decided from the mapping alone.
    let mut stub = StubServer::start(
        serde_json::json!([
            rom(
                1,
                "Original",
                "same.gb",
                contents.len() as u64,
                [&hashes[0], &hashes[1], &hashes[2]]
            ),
            rom(
                2,
                "Duplicate",
                "same.gb",
                contents.len() as u64,
                [&hashes[0], &hashes[1], &hashes[2]]
            ),
        ]),
        2,
    );
    ready(&tree, &stub);
    tree.file("gb/same.gb", &contents);
    assert!(tree.run(&["import"]).succeeded());

    let run = tree.run(&["conflicts", "--json"]);
    assert!(run.succeeded(), "listing conflicts is not itself a failure");
    let page = run.json();
    assert_eq!(page["matching_filters"], 2, "{page:#}");
    let first = &page["records"].as_array().expect("records")[0];
    let conflicts = first["conflicts"].as_array().expect("conflicts");
    assert!(!conflicts.is_empty(), "{first:#}");
    // Both sides are retained, so nothing was quietly overwritten.
    assert!(conflicts[0]["romm"].is_string());
    assert!(conflicts[0]["local"].is_string());
    stub.stop();
}

#[test]
fn a_clean_library_reports_no_conflicts_and_exits_successfully() {
    let tree = Tree::new("no-conflicts");
    let contents = b"clean".to_vec();
    let hashes = true_hashes(&contents);
    let mut stub = StubServer::start(
        serde_json::json!([rom(
            1,
            "Clean",
            "clean.gb",
            contents.len() as u64,
            [&hashes[0], &hashes[1], &hashes[2]]
        )]),
        1,
    );
    ready(&tree, &stub);
    tree.file("gb/clean.gb", &contents);
    assert!(tree.run(&["import"]).succeeded());

    let run = tree.run(&["conflicts"]);
    assert!(run.succeeded());
    assert!(run.stdout.contains("No conflicts"), "{}", run.stdout);
    stub.stop();
}

#[test]
fn a_record_whose_file_is_gone_is_stale_not_deleted() {
    let tree = Tree::new("stale");
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([rom(
            1,
            "Vanished",
            "vanished.gb",
            100,
            [&dud[0], &dud[1], &dud[2]]
        )]),
        1,
    );
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let page = tree.run(&["records", "--status", "stale", "--json"]).json();
    assert_eq!(page["matching_filters"], 1, "{page:#}");
    assert_eq!(page["records"][0]["stale"], true);
    stub.stop();
}

// --- verify-hash ----------------------------------------------------------

#[test]
fn verify_hash_refuses_a_path_outside_the_source_folders() {
    let tree = Tree::new("hash-outside");
    let outside = tree.elsewhere().join("secret");
    std::fs::write(&outside, b"not yours to read").expect("fixture");
    let message = tree
        .run(&["verify-hash", "--path", &outside.display().to_string()])
        .error_text()
        .to_string();
    assert!(
        message.contains("not inside a configured source folder"),
        "{message}"
    );
}

#[test]
fn verify_hash_refuses_a_relative_path() {
    let tree = Tree::new("hash-relative");
    let message = tree
        .run(&["verify-hash", "--path", "some/file.gb"])
        .error_text()
        .to_string();
    assert!(message.contains("absolute"), "{message}");
}

#[test]
fn verify_hash_refuses_a_directory() {
    let tree = Tree::new("hash-dir");
    let directory = tree.library().join("gb");
    std::fs::create_dir_all(&directory).expect("fixture");
    let message = tree
        .run(&["verify-hash", "--path", &directory.display().to_string()])
        .error_text()
        .to_string();
    assert!(message.contains("regular file"), "{message}");
}

#[test]
fn verify_hash_refuses_a_symlink_that_leaves_the_library() {
    let tree = Tree::new("hash-escape");
    let outside = tree.elsewhere().join("target");
    std::fs::write(&outside, b"outside").expect("fixture");
    let link = tree.library().join("escape.gb");
    std::os::unix::fs::symlink(&outside, &link).expect("fixture");
    let message = tree
        .run(&["verify-hash", "--path", &link.display().to_string()])
        .error_text()
        .to_string();
    assert!(message.contains("leads out of"), "{message}");
}

#[test]
fn verify_hash_refuses_a_broken_symlink() {
    let tree = Tree::new("hash-broken");
    let link = tree.library().join("broken.gb");
    std::os::unix::fs::symlink(tree.library().join("gone.gb"), &link).expect("fixture");
    let message = tree
        .run(&["verify-hash", "--path", &link.display().to_string()])
        .error_text()
        .to_string();
    assert!(message.contains("cannot be resolved"), "{message}");
}

#[test]
fn verify_hash_refuses_a_missing_file() {
    let tree = Tree::new("hash-missing");
    let absent = tree.library().join("absent.gb");
    let message = tree
        .run(&["verify-hash", "--path", &absent.display().to_string()])
        .error_text()
        .to_string();
    assert!(message.contains("cannot be examined"), "{message}");
}

#[test]
fn verify_hash_refuses_when_no_source_folder_is_configured() {
    let tree = Tree::new("hash-no-roots");
    let file = tree.file("gb/a.gb", b"data");
    let run = tree.run_without_roots(&["verify-hash", "--path", &file.display().to_string()]);
    let message = run.error_text();
    assert!(message.contains("no source folders"), "{message}");
}

#[test]
fn verify_hash_reports_agreement_and_does_not_change_the_file() {
    let tree = Tree::new("hash-agrees");
    let contents = b"the real bytes".to_vec();
    let hashes = true_hashes(&contents);
    let mut stub = StubServer::start(
        serde_json::json!([rom(
            1,
            "Real",
            "real.gb",
            contents.len() as u64,
            [&hashes[0], &hashes[1], &hashes[2]]
        )]),
        1,
    );
    ready(&tree, &stub);
    let file = tree.file("gb/real.gb", &contents);
    assert!(tree.run(&["import"]).succeeded());

    let before = std::fs::metadata(&file).expect("fixture");
    let run = tree.run(&[
        "verify-hash",
        "--path",
        &file.display().to_string(),
        "--json",
    ]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["romm_game_id"], "1");
    assert_eq!(result["all_agree"], true);
    assert_eq!(result["any_disagree"], false);
    assert_eq!(result["bytes_hashed"], contents.len());
    assert_eq!(result["file_modified"], false);
    assert_eq!(result["verification_after"], "confirmed_external");
    assert_eq!(
        result["comparisons"].as_array().expect("comparisons").len(),
        3
    );

    let after = std::fs::metadata(&file).expect("still there");
    assert_eq!(before.len(), after.len());
    assert_eq!(
        before.modified().ok(),
        after.modified().ok(),
        "hashing altered the file"
    );
    assert_eq!(
        std::fs::read(&file).expect("readable"),
        contents,
        "hashing changed the contents"
    );
    stub.stop();
}

#[test]
fn verify_hash_reports_a_disagreement_without_touching_the_cache() {
    let tree = Tree::new("hash-disagrees");
    let contents = b"actual bytes".to_vec();
    let mut stub = StubServer::start(
        serde_json::json!([rom(
            1,
            "Wrong",
            "wrong.gb",
            contents.len() as u64,
            ["deadbeef", &"a".repeat(32), &"b".repeat(40)]
        )]),
        1,
    );
    ready(&tree, &stub);
    let file = tree.file("gb/wrong.gb", &contents);
    assert!(tree.run(&["import"]).succeeded());
    let cache_before = std::fs::read(tree.cache_path()).expect("cache");

    let run = tree.run(&[
        "verify-hash",
        "--path",
        &file.display().to_string(),
        "--json",
    ]);
    assert!(run.succeeded(), "reporting a disagreement is not a failure");
    let result = run.json();
    assert_eq!(result["all_agree"], false);
    assert_eq!(result["any_disagree"], true);
    assert!(
        result["comparisons"]
            .as_array()
            .expect("comparisons")
            .iter()
            .all(|comparison| comparison["agrees"] == false),
        "{result:#}"
    );
    // A verification reports; it does not rewrite what was imported.
    assert_eq!(
        std::fs::read(tree.cache_path()).expect("cache"),
        cache_before,
        "verify-hash modified the cache"
    );
    stub.stop();
}

#[test]
fn verify_hash_still_reports_hashes_when_no_record_claims_the_file() {
    let tree = Tree::new("hash-unclaimed");
    let file = tree.file("gb/unclaimed.gb", b"lonely");
    let run = tree.run(&[
        "verify-hash",
        "--path",
        &file.display().to_string(),
        "--json",
    ]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert!(result["romm_game_id"].is_null());
    assert!(
        result["all_agree"].is_null(),
        "no hashes to compare is not agreement"
    );
    assert_eq!(result["sha1"].as_str().expect("sha1").len(), 40);
}

// --- Lifecycle ------------------------------------------------------------

#[test]
fn enabling_before_a_url_is_configured_is_refused() {
    let tree = Tree::new("enable-early");
    let message = tree.run(&["enable"]).error_text().to_string();
    assert!(message.contains("configure a URL"), "{message}");
}

#[test]
fn enable_and_disable_round_trip_without_contacting_anything() {
    let tree = Tree::new("enable-disable");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    let token = tree.token_file(STUB_TOKEN);
    assert!(
        tree.run(&[
            "configure",
            "--url",
            &stub.url(),
            "--token-file",
            &token.display().to_string(),
        ])
        .succeeded()
    );
    let baseline = stub.request_count();

    let enabled = tree.run(&["enable", "--json"]);
    assert!(enabled.succeeded());
    assert_eq!(enabled.json()["enabled"], true);
    assert_eq!(enabled.json()["connected"], false);

    let disabled = tree.run(&["disable", "--json"]);
    assert!(disabled.succeeded());
    assert_eq!(disabled.json()["enabled"], false);

    assert_eq!(
        stub.request_count(),
        baseline,
        "enabling or disabling reached the server"
    );
    stub.stop();
}

#[test]
fn disabling_keeps_the_configuration_and_the_cache() {
    let tree = Tree::new("disable-keeps");
    let mut stub = StubServer::start(many_roms(1), 1);
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());
    assert!(tree.run(&["disable"]).succeeded());
    assert!(tree.config_path().exists());
    assert!(tree.cache_path().exists());
    // Still browsable while disabled: disabled means "do not refresh", not
    // "forget what you know".
    assert_eq!(
        tree.run(&["records", "--json"]).json()["matching_filters"],
        1
    );
    stub.stop();
}

#[test]
fn removal_requires_confirmation() {
    let tree = Tree::new("remove-unconfirmed");
    let mut stub = StubServer::start(many_roms(1), 1);
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let message = tree.run(&["remove"]).error_text().to_string();
    assert!(message.contains("--confirm"), "{message}");
    assert!(message.contains("nothing was removed"), "{message}");
    assert!(tree.cache_path().exists(), "the cache was removed anyway");
    stub.stop();
}

#[test]
fn removal_takes_only_archivefs_own_files_and_leaves_the_token() {
    let tree = Tree::new("remove-confirmed");
    let contents = b"a rom".to_vec();
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([rom(
            1,
            "Kept",
            "kept.gb",
            contents.len() as u64,
            [&dud[0], &dud[1], &dud[2]]
        )]),
        1,
    );
    ready(&tree, &stub);
    let game = tree.file("gb/kept.gb", &contents);
    assert!(tree.run(&["import"]).succeeded());
    let token = tree.root.join("token");
    let requests_before = stub.request_count();

    let run = tree.run(&["remove", "--confirm", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["cache_removed"], true);
    assert_eq!(result["config_removed"], true);
    assert_eq!(result["romm_modified"], false);
    assert_eq!(result["roms_modified"], false);

    assert!(!tree.cache_path().exists());
    assert!(!tree.config_path().exists());
    assert!(token.exists(), "the token file was deleted");
    assert_eq!(
        std::fs::read(&game).expect("the ROM is still there"),
        contents,
        "removal touched a ROM"
    );
    assert_eq!(
        stub.request_count(),
        requests_before,
        "removal contacted RomM"
    );
    stub.stop();
}

#[test]
fn removal_can_keep_the_configuration() {
    let tree = Tree::new("remove-keep-config");
    let mut stub = StubServer::start(many_roms(1), 1);
    ready(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let run = tree.run(&["remove", "--confirm", "--keep-config", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(run.json()["config_removed"], false);
    assert!(!tree.cache_path().exists());
    assert!(tree.config_path().exists());
    stub.stop();
}

#[test]
fn removing_when_there_is_nothing_to_remove_is_not_an_error() {
    let tree = Tree::new("remove-nothing");
    let run = tree.run(&["remove", "--confirm", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(run.json()["cache_removed"], false);
}

// --- Configuration handling ----------------------------------------------

#[test]
fn status_before_anything_is_configured_explains_what_to_do() {
    let tree = Tree::new("status-fresh");
    let run = tree.run(&["status"]);
    assert!(run.succeeded(), "status must work before configuration");
    assert!(run.stdout.contains("Not configured"), "{}", run.stdout);
    assert!(
        run.stdout.contains(SUGGESTED_TOKEN_PATH),
        "the suggested token path should be offered: {}",
        run.stdout
    );
    // Suggesting a path must not create anything.
    assert!(!tree.config_path().exists());
}

#[test]
fn a_reconfigured_page_size_is_kept_within_bounds() {
    let tree = Tree::new("page-size");
    let token = tree.token_file(STUB_TOKEN);
    let run = tree.run(&[
        "configure",
        "--url",
        "http://127.0.0.1:8080",
        "--token-file",
        &token.display().to_string(),
        "--page-size",
        "100000",
    ]);
    assert!(run.succeeded(), "{:?}", run.error);
    let reported = tree.run(&["status", "--json"]).json();
    let size = reported["page_size"].as_u64().expect("page size");
    assert!(
        size <= u64::from(archivefs_core::identity_source::settings::MAX_CONFIGURED_PAGE_SIZE),
        "page size {size} was not clamped"
    );
}

#[test]
fn configuration_survives_being_reloaded() {
    let tree = Tree::new("round-trip");
    let token = tree.token_file(STUB_TOKEN);
    let destination = tree.library().join("gb");
    std::fs::create_dir_all(&destination).expect("fixture");
    assert!(
        tree.run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &token.display().to_string(),
            "--enable",
        ])
        .succeeded()
    );
    assert!(
        tree.run(&[
            "mappings",
            "add",
            "--romm-root",
            "/romm/library/gb",
            "--archivefs-root",
            &destination.display().to_string(),
        ])
        .succeeded()
    );

    let reported = tree.run(&["status", "--json"]).json();
    assert_eq!(reported["url"], "http://127.0.0.1:8080");
    assert_eq!(reported["enabled"], true);
    assert_eq!(reported["token_available"], true);
    assert!(reported["token_problem"].is_null());
    let listed = tree.run(&["mappings", "list", "--json"]).json();
    assert_eq!(listed["mappings"].as_array().expect("mappings").len(), 1);
}

#[test]
fn a_corrupt_configuration_is_reported_rather_than_silently_reset() {
    let tree = Tree::new("corrupt-config");
    std::fs::create_dir_all(tree.identity().join("romm")).expect("fixture");
    std::fs::write(tree.config_path(), b"{ this is not json").expect("fixture");
    let message = tree.run(&["status"]).error_text().to_string();
    assert!(
        message.contains("config.json"),
        "the file at fault should be named: {message}"
    );
}

#[test]
fn a_cache_from_a_different_server_is_never_presented_as_current() {
    let tree = Tree::new("server-change");
    let mut first = StubServer::start(many_roms(1), 1);
    ready(&tree, &first);
    assert!(tree.run(&["import"]).succeeded());
    first.stop();

    // A different port is a different origin, so a different server identity.
    let mut second = StubServer::start(serde_json::json!([]), 0);
    let token = tree.root.join("token");
    assert!(
        tree.run(&[
            "configure",
            "--url",
            &second.url(),
            "--token-file",
            &token.display().to_string(),
        ])
        .succeeded()
    );
    let reported = tree.run(&["status", "--json"]).json();
    let state = reported["state"]["state"].as_str().expect("state");
    let server = reported["server_id"].as_str();
    assert!(
        state != "ready" || server == Some(second.url().as_str()),
        "a cache from another server was presented as current: {reported:#}"
    );
    second.stop();
}

// --- Provider-relative paths ---------------------------------------------

#[test]
fn a_relative_shape_import_matches_records_end_to_end() {
    let tree = Tree::new("relative-import");
    let contents = b"relative game data".to_vec();
    let hashes = true_hashes(&contents);
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([
            relative_rom(
                1,
                "Matched",
                "roms/gb/matched.gb",
                contents.len() as u64,
                [&hashes[0], &hashes[1], &hashes[2]]
            ),
            relative_rom(
                2,
                "Missing",
                "roms/gb/missing.gb",
                4,
                [&dud[0], &dud[1], &dud[2]]
            ),
        ]),
        2,
    );
    ready_relative(&tree, &stub);
    tree.file("gb/matched.gb", &contents);

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["published"], true);
    assert_eq!(result["records"], 2);
    assert_eq!(
        result["unmatched"], 0,
        "a relative mapping must translate every relative path: {result:#}"
    );
    assert_eq!(result["strong"], 1, "{result:#}");
    assert_eq!(
        result["stale"], 1,
        "the absent file is stale, not unmatched"
    );

    // The record kept the exact relative string, and points at the local file.
    let record = tree.run(&["record", "1", "--json"]).json();
    assert_eq!(record["romm_path"], "roms/gb/matched.gb");
    assert_eq!(
        record["archivefs_path"],
        tree.library().join("gb/matched.gb").display().to_string()
    );
    stub.stop();
}

/// Item 12 directly: a *sample* import with a valid relative mapping must yield
/// matched records, and item 13: it must still publish nothing.
#[test]
fn a_relative_sample_import_matches_and_publishes_nothing() {
    let tree = Tree::new("relative-sample");
    let contents = b"sampled bytes".to_vec();
    let hashes = true_hashes(&contents);
    let mut stub = StubServer::start(
        serde_json::json!([relative_rom(
            1,
            "Sampled",
            "roms/gb/sampled.gb",
            contents.len() as u64,
            [&hashes[0], &hashes[1], &hashes[2]]
        )]),
        1,
    );
    ready_relative(&tree, &stub);
    tree.file("gb/sampled.gb", &contents);

    let run = tree.run(&["import", "--sample", "5", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["mode"], "sample");
    assert_eq!(result["published"], false);
    assert_eq!(result["records"], 1);
    assert_eq!(result["unmatched"], 0, "{result:#}");
    assert_eq!(result["strong"], 1, "{result:#}");
    assert!(
        !tree.cache_path().exists(),
        "a sample must not publish, whatever the path shape"
    );
    stub.stop();
}

#[test]
fn the_real_observed_romm_path_shapes_translate() {
    let tree = Tree::new("relative-real-shapes");
    let dud = dud_hashes();
    // Exactly the paths the live RomM 5.1.0 instance reported.
    let observed = [
        "roms/sharp-x68000/_ReadMe_.txt",
        "roms/acorn-archimedes/Coconizer (Europe) (v1.3).zip",
        "roms/amiga/Allo Allo (v1.0).hdf",
        "roms/atari-st/'Nam 1965-1975 (Europe).stx",
        "roms/gb/game.gb",
        "roms/snes/game.zip",
        "roms/psx/game.cue",
    ];
    let roms: Vec<serde_json::Value> = observed
        .iter()
        .enumerate()
        .map(|(index, path)| {
            relative_rom(
                index as u64 + 1,
                &format!("Game {index}"),
                path,
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    let mut stub = StubServer::start(serde_json::json!(roms), observed.len() as u64);
    ready_relative(&tree, &stub);

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    assert_eq!(
        run.json()["unmatched"],
        0,
        "every real observed path shape should translate: {:#}",
        run.json()
    );

    let preview = tree
        .run(&["mappings", "preview", "--limit", "20", "--json"])
        .json();
    assert_eq!(preview["translated"], observed.len());
    assert_eq!(preview["refused"], 0);
    assert_eq!(preview["observed_relative"], observed.len());
    assert_eq!(preview["observed_absolute"], 0);
    assert!(preview["suggested_path_kind"].is_null());
    stub.stop();
}

#[test]
fn the_preview_reports_every_stage_of_one_translation() {
    let tree = Tree::new("preview-detail");
    let contents = b"present".to_vec();
    let hashes = true_hashes(&contents);
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([
            relative_rom(
                1,
                "Present",
                "roms/gb/present.gb",
                contents.len() as u64,
                [&hashes[0], &hashes[1], &hashes[2]]
            ),
            relative_rom(
                2,
                "Absent",
                "roms/gb/absent.gb",
                4,
                [&dud[0], &dud[1], &dud[2]]
            ),
            // A path no mapping covers.
            relative_rom(
                3,
                "Elsewhere",
                "backups/gb/other.gb",
                4,
                [&dud[0], &dud[1], &dud[2]]
            ),
        ]),
        3,
    );
    ready_relative(&tree, &stub);
    tree.file("gb/present.gb", &contents);
    assert!(tree.run(&["import"]).succeeded());

    let run = tree.run(&["mappings", "preview", "--limit", "10", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let report = run.json();
    assert_eq!(report["configured_path_kind"], "provider_relative");
    assert_eq!(report["existing_files"], 1);
    assert_eq!(report["unmatched"], 1);

    let examples = report["examples"].as_array().expect("examples");
    let present = examples
        .iter()
        .find(|example| example["romm_path"] == "roms/gb/present.gb")
        .expect("the present record should be previewed");
    // Item 8's list, each field asserted.
    assert_eq!(present["romm_path"], "roms/gb/present.gb");
    assert_eq!(present["path_kind"], "provider_relative");
    assert_eq!(present["matched_prefix"], "roms");
    assert_eq!(
        present["archivefs_path"],
        tree.library().join("gb/present.gb").display().to_string()
    );
    assert_eq!(present["file_exists"], true);
    assert_eq!(
        present["trusted_root"],
        tree.library().display().to_string(),
        "the source folder the result landed in should be named"
    );
    assert_eq!(present["inside_trusted_root"], true);
    assert_eq!(present["outcome"], "translated");
    assert!(present["refusal"].is_null());

    let unmatched = examples
        .iter()
        .find(|example| example["romm_path"] == "backups/gb/other.gb")
        .expect("the uncovered record should be previewed");
    assert_eq!(unmatched["outcome"], "unmatched");
    assert!(unmatched["archivefs_path"].is_null());
    stub.stop();
}

#[test]
fn a_shape_mismatch_is_diagnosed_by_preview_and_by_test() {
    let tree = Tree::new("shape-mismatch");
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([relative_rom(
            1,
            "Relative",
            "roms/gb/game.gb",
            4,
            [&dud[0], &dud[1], &dud[2]]
        )]),
        1,
    );
    // Configured absolute - the wrong shape for this server - which is the exact
    // state the live smoke run was in.
    ready(&tree, &stub);

    let test = tree.run(&["test", "--json"]);
    assert!(test.succeeded(), "{:?}", test.error);
    let report = test.json();
    assert_eq!(report["configured_path_kind"], "absolute_provider_path");
    assert_eq!(report["observed_path_kind"], "provider_relative");
    assert_eq!(report["path_kind_mismatch"], true);
    assert_eq!(report["sample_provider_path"], "roms/gb/game.gb");

    let human = tree.run(&["test"]).stdout;
    assert!(
        human.contains("--path-kind relative"),
        "the test should name the fix: {human}"
    );

    // And the preview says the same thing rather than just refusing silently.
    assert!(tree.run(&["import"]).succeeded());
    let preview = tree.run(&["mappings", "preview", "--json"]).json();
    assert_eq!(preview["refused"], 1);
    assert_eq!(preview["translated"], 0);
    assert_eq!(preview["suggested_path_kind"], "provider_relative");
    // The path is relative where absolute was declared, so that is the refusal.
    assert_eq!(
        preview["examples"][0]["refusal_code"], "unexpectedly_relative",
        "{preview:#}"
    );
    stub.stop();
}

#[test]
fn an_unknown_path_kind_is_refused_with_both_spellings_offered() {
    let tree = Tree::new("bad-path-kind");
    let token = tree.token_file(STUB_TOKEN);
    let message = tree
        .run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &token.display().to_string(),
            "--path-kind",
            "sideways",
        ])
        .error_text()
        .to_string();
    assert!(message.contains("sideways"), "{message}");
    assert!(message.contains("relative"), "{message}");
    assert!(message.contains("absolute"), "{message}");
}

#[test]
fn changing_the_path_shape_is_refused_while_it_would_strand_a_mapping() {
    let tree = Tree::new("strand-guard");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    ready(&tree, &stub); // absolute, with /romm/library/gb mapped

    let message = tree
        .run(&["configure", "--path-kind", "relative"])
        .error_text()
        .to_string();
    assert!(message.contains("cannot be used as relative"), "{message}");
    assert!(
        message.contains("mappings remove"),
        "the way forward should be stated: {message}"
    );
    // The setting was not changed behind the refusal.
    assert_eq!(
        tree.run(&["status", "--json"]).json()["path_kind"],
        "absolute_provider_path"
    );

    // Remove the stale mapping, and the switch is then allowed.
    assert!(
        tree.run(&["mappings", "remove", "--romm-root", "/romm/library/gb"])
            .succeeded()
    );
    let switched = tree.run(&["configure", "--path-kind", "relative"]);
    assert!(switched.succeeded(), "{:?}", switched.error);
    assert_eq!(
        tree.run(&["status", "--json"]).json()["path_kind"],
        "provider_relative"
    );
    stub.stop();
}

/// A configuration hand-edited into a mismatched state must stay inspectable and
/// repairable: listing cannot fail, and the stranded mapping can be removed.
#[test]
fn a_stranded_mapping_is_listed_and_can_still_be_removed() {
    let tree = Tree::new("stranded");
    let token = tree.token_file(STUB_TOKEN);
    assert!(
        tree.run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &token.display().to_string(),
        ])
        .succeeded()
    );
    let destination = tree.library().join("gb");
    std::fs::create_dir_all(&destination).expect("fixture");
    assert!(
        tree.run(&[
            "mappings",
            "add",
            "--romm-root",
            "/romm/library/gb",
            "--archivefs-root",
            &destination.display().to_string(),
        ])
        .succeeded()
    );
    // Flip the declared shape directly in the stored configuration, as a hand
    // edit would.
    let text = std::fs::read_to_string(tree.config_path()).expect("config");
    let flipped = text.replace(
        "\"provider_path_kind\": \"absolute_provider_path\"",
        "\"provider_path_kind\": \"provider_relative\"",
    );
    assert_ne!(text, flipped, "the field should be present to flip");
    std::fs::write(tree.config_path(), flipped).expect("fixture");

    let listed = tree.run(&["mappings", "list", "--json"]);
    assert!(
        listed.succeeded(),
        "a listing must never fail, or a bad state has no way out: {:?}",
        listed.error
    );
    let document = listed.json();
    let entries = document["mappings"].as_array().expect("mappings");
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0]["problem"]
            .as_str()
            .is_some_and(|problem| problem.contains("absolute")),
        "the problem should be explained: {entries:#?}"
    );
    let human = tree.run(&["mappings", "list"]).stdout;
    assert!(human.contains("cannot be used as configured"), "{human}");

    assert!(
        tree.run(&["mappings", "remove", "--romm-root", "/romm/library/gb"])
            .succeeded(),
        "a stranded mapping must remain removable"
    );
    assert!(
        tree.run(&["mappings", "list", "--json"]).json()["mappings"]
            .as_array()
            .expect("mappings")
            .is_empty()
    );
}

#[test]
fn a_relative_mapping_destination_must_still_be_inside_a_source_folder() {
    let tree = Tree::new("relative-outside");
    let token = tree.token_file(STUB_TOKEN);
    assert!(
        tree.run(&[
            "configure",
            "--url",
            "http://127.0.0.1:8080",
            "--token-file",
            &token.display().to_string(),
            "--path-kind",
            "relative",
        ])
        .succeeded()
    );
    let message = tree
        .run(&[
            "mappings",
            "add",
            "--romm-root",
            "roms",
            "--archivefs-root",
            &tree.elsewhere().display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(
        message.contains("not inside any configured source folder"),
        "{message}"
    );
}

#[test]
fn a_relative_prefix_cannot_be_added_to_an_absolute_source() {
    let tree = Tree::new("relative-prefix-absolute-source");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    let token = tree.token_file(STUB_TOKEN);
    assert!(
        tree.run(&[
            "configure",
            "--url",
            &stub.url(),
            "--token-file",
            &token.display().to_string(),
        ])
        .succeeded()
    );
    let message = tree
        .run(&[
            "mappings",
            "add",
            "--romm-root",
            "roms",
            "--archivefs-root",
            &tree.library().display().to_string(),
        ])
        .error_text()
        .to_string();
    assert!(message.contains("--path-kind relative"), "{message}");
    stub.stop();
}

#[test]
fn hostile_relative_paths_from_a_server_are_refused_during_import() {
    let tree = Tree::new("hostile-import");
    let dud = dud_hashes();
    let hostile = [
        "../etc/passwd",
        "roms/../../etc/passwd",
        "./roms/game.zip",
        "roms//game.zip",
        r"C:\roms\game.zip",
        r"\\server\share\game.zip",
        r"roms\..\game.zip",
        "roms/%2e%2e/%2e%2e/etc/passwd",
    ];
    let roms: Vec<serde_json::Value> = hostile
        .iter()
        .enumerate()
        .map(|(index, path)| {
            relative_rom(
                index as u64 + 1,
                &format!("Hostile {index}"),
                path,
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    let mut stub = StubServer::start(serde_json::json!(roms), hostile.len() as u64);
    ready_relative(&tree, &stub);

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    // Every record is still imported and visible - a refused path is reported,
    // not hidden - but none of them acquired a local path.
    assert_eq!(run.json()["records"], hostile.len());

    let records = tree.run(&["records", "--limit", "50", "--json"]).json();
    for record in records["records"].as_array().expect("records") {
        let romm_path = record["romm_path"].as_str().unwrap_or_default();
        // The one exception is the percent-encoded path, which is a legitimate
        // set of literal components and must translate as such - inside the
        // library, never above it.
        if romm_path.contains("%2e") {
            let local = record["archivefs_path"]
                .as_str()
                .expect("a literal path should translate");
            assert!(
                local.starts_with(&tree.library().display().to_string()),
                "{local} escaped the library"
            );
            assert!(!local.contains(".."), "{local} contains traversal");
            continue;
        }
        assert!(
            record["archivefs_path"].is_null(),
            "{romm_path} must not have produced a local path: {record:#}"
        );
    }
    stub.stop();
}

#[test]
fn a_configured_source_with_no_import_yet_is_not_reported_as_an_error() {
    let tree = Tree::new("never-imported");
    let mut stub = StubServer::start(serde_json::json!([]), 0);
    ready_relative(&tree, &stub);

    let run = tree.run(&["status", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let reported = run.json();
    assert_eq!(
        reported["state"]["state"], "never_imported",
        "being set up and not yet imported is not a failure: {reported:#}"
    );
    assert!(
        reported["state"].get("detail").is_none(),
        "there is no error detail to give: {reported:#}"
    );
    let human = tree.run(&["status"]).stdout;
    assert!(
        human.contains("nothing imported yet"),
        "the state should read as a stage, not a fault: {human}"
    );
    assert!(!human.contains("State:            Error"), "{human}");
    stub.stop();
}

// --- Adaptive page sizing, through the CLI --------------------------------

/// Test A16: the JSON carries every adaptive-pagination field.
#[test]
fn the_import_json_reports_what_adaptive_paging_did() {
    let tree = Tree::new("adaptive-json");
    // 120 records, and any page above 50 comes back too large.
    let dud = dud_hashes();
    let roms: Vec<serde_json::Value> = (0..120)
        .map(|index| {
            relative_rom(
                index,
                &format!("Game {index}"),
                &format!("roms/gb/{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    let mut stub = StubServer::start_with_limit(serde_json::json!(roms), 120, 50);
    ready_relative(&tree, &stub);

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["published"], true);
    assert_eq!(result["records"], 120, "every record should still arrive");
    assert_eq!(result["configured_page_size"], 100);
    assert_eq!(result["effective_page_size"], 50);
    assert_eq!(result["smallest_page_size"], 50);
    assert_eq!(result["page_size_reductions"], 1);
    assert_eq!(result["oversized_page_retries"], 1);
    assert_eq!(result["previous_cache_usable"], true);
    // Pages: one refused attempt is not a page; 120 records at 50 is 3 pages.
    assert_eq!(result["pages_fetched"], 3);
    assert_eq!(result["records_fetched"], 120);
    stub.stop();
}

/// The progress line a person sees, on stderr and only when JSON is off.
#[test]
fn a_reduction_is_announced_on_stderr_with_its_offset() {
    let tree = Tree::new("adaptive-progress");
    let dud = dud_hashes();
    let roms: Vec<serde_json::Value> = (0..60)
        .map(|index| {
            relative_rom(
                index,
                &format!("Game {index}"),
                &format!("roms/gb/{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    let mut stub = StubServer::start_with_limit(serde_json::json!(roms), 60, 25);
    ready_relative(&tree, &stub);

    let run = tree.run(&["import"]);
    assert!(run.succeeded(), "{:?}", run.error);
    assert!(
        run.stderr
            .contains("page response exceeded 8 MiB at offset 0"),
        "the reduction should be announced with its offset: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("retrying with page size 50"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("retrying with page size 25"),
        "the second step should be announced too: {}",
        run.stderr
    );
    // The result itself says what happened, on stdout.
    assert!(
        run.stdout
            .contains("2 reduction(s) down to 25, finished at 25"),
        "{}",
        run.stdout
    );
    // Exactly one page-size line, so the two branches cannot both fire.
    assert_eq!(
        run.stdout
            .lines()
            .filter(|line| line.contains("Page size:"))
            .count(),
        1,
        "{}",
        run.stdout
    );
    // And in JSON mode none of that reaches stdout.
    let json_run = tree.run(&["import", "--json"]);
    assert!(json_run.succeeded());
    assert!(json_run.stderr.is_empty(), "{}", json_run.stderr);
    assert!(
        !json_run.stdout.contains("page response exceeded"),
        "{}",
        json_run.stdout
    );
    stub.stop();
}

/// Test A13: an adaptive import that fails leaves the old cache byte-identical.
#[test]
fn a_failed_adaptive_import_preserves_the_previous_cache_byte_for_byte() {
    let tree = Tree::new("adaptive-preserves-cache");
    let dud = dud_hashes();
    let roms: Vec<serde_json::Value> = (0..40)
        .map(|index| {
            relative_rom(
                index,
                &format!("Game {index}"),
                &format!("roms/gb/{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    // First a clean import at the full page size, to establish a good cache.
    let mut stub = StubServer::start_with_limit(serde_json::json!(roms.clone()), 40, usize::MAX);
    ready_relative(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());
    let before = std::fs::read(tree.cache_path()).expect("cache published");
    stub.stop();

    // Now a server where nothing is small enough: the ladder is exhausted and the
    // import fails at an oversized record.
    let mut hostile = StubServer::start_with_limit(serde_json::json!(roms), 40, 0);
    let token = tree.token_file(STUB_TOKEN);
    assert!(
        tree.run(&[
            "configure",
            "--url",
            &hostile.url(),
            "--token-file",
            &token.display().to_string(),
        ])
        .succeeded()
    );
    let run = tree.run(&["refresh", "--json"]);
    assert!(!run.succeeded(), "an unreadable catalogue should fail");
    let result = run.json();
    assert_eq!(result["error_code"], "oversized_record");
    assert_eq!(result["published"], false);
    // The reductions it managed before giving up are still reported.
    assert_eq!(
        result["page_size_reductions"], 5,
        "100 -> 50 -> 25 -> 10 -> 5 -> 1"
    );
    assert_eq!(result["smallest_page_size"], 1);

    let after = std::fs::read(tree.cache_path()).expect("cache still there");
    assert_eq!(
        before, after,
        "the previous cache was not preserved exactly"
    );
    hostile.stop();
}

/// Test A14: a first-ever import that fails this way publishes nothing at all.
#[test]
fn a_first_ever_adaptive_failure_publishes_no_cache() {
    let tree = Tree::new("adaptive-first-failure");
    let dud = dud_hashes();
    let roms: Vec<serde_json::Value> = (0..10)
        .map(|index| {
            relative_rom(
                index,
                &format!("Game {index}"),
                &format!("roms/gb/{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    // Nothing is ever small enough.
    let mut stub = StubServer::start_with_limit(serde_json::json!(roms), 10, 0);
    ready_relative(&tree, &stub);

    let run = tree.run(&["import", "--json"]);
    assert!(!run.succeeded());
    let result = run.json();
    assert_eq!(result["error_code"], "oversized_record");
    assert_eq!(result["published"], false);
    assert_eq!(result["previous_cache_usable"], false);
    assert!(
        !tree.cache_path().exists(),
        "no cache should have been created"
    );
    // Nor a stray temporary file.
    let leftovers: Vec<String> = std::fs::read_dir(tree.identity().join("romm"))
        .expect("provider directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name != "config.json")
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");

    // And the state is honest about it: nothing imported, not a fake readiness.
    let status = tree.run(&["status", "--json"]).json();
    assert_eq!(status["state"]["state"], "never_imported");
    stub.stop();
}

/// Test A15 at the CLI level: a sample adapts and publishes nothing.
#[test]
fn a_sample_import_adapts_and_still_publishes_nothing_through_the_cli() {
    let tree = Tree::new("adaptive-cli-sample");
    let dud = dud_hashes();
    let roms: Vec<serde_json::Value> = (0..200)
        .map(|index| {
            relative_rom(
                index,
                &format!("Game {index}"),
                &format!("roms/gb/{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    let mut stub = StubServer::start_with_limit(serde_json::json!(roms), 200, 25);
    ready_relative(&tree, &stub);

    let run = tree.run(&["import", "--sample", "30", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["mode"], "sample");
    assert_eq!(result["published"], false);
    assert_eq!(result["records"], 30);
    assert_eq!(result["page_size_reductions"], 2, "100 -> 50 -> 25");
    assert_eq!(result["effective_page_size"], 25);
    assert!(
        !tree.cache_path().exists(),
        "a sample must not publish, adaptive or not"
    );
    stub.stop();
}

/// The configured page size is actually used, not silently replaced by the
/// module default. It was stored and displayed but never passed to the importer.
#[test]
fn the_configured_page_size_reaches_the_importer() {
    let tree = Tree::new("configured-page-size");
    let dud = dud_hashes();
    let roms: Vec<serde_json::Value> = (0..30)
        .map(|index| {
            relative_rom(
                index,
                &format!("Game {index}"),
                &format!("roms/gb/{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    let mut stub = StubServer::start(serde_json::json!(roms), 30);
    ready_relative(&tree, &stub);
    assert!(
        tree.run(&["configure", "--page-size", "10"]).succeeded(),
        "the page size should be configurable"
    );

    let run = tree.run(&["import", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let result = run.json();
    assert_eq!(result["configured_page_size"], 10);
    assert_eq!(result["effective_page_size"], 10);
    assert_eq!(result["page_size_reductions"], 0);
    assert_eq!(
        result["pages_fetched"], 4,
        "30 records at 10 per page is three full pages plus the short one"
    );
    assert_eq!(result["records"], 30);
    stub.stop();
}

// --- stale-summary --------------------------------------------------------

#[test]
fn the_stale_summary_explains_the_population_and_stays_bounded() {
    let tree = Tree::new("stale-summary");
    let contents = b"present bytes".to_vec();
    let hashes = true_hashes(&contents);
    let dud = dud_hashes();
    let mut roms = vec![
        // One that matches, and so must be excluded from the summary.
        relative_rom(
            1,
            "Present",
            "roms/gb/present.gb",
            contents.len() as u64,
            [&hashes[0], &hashes[1], &hashes[2]],
        ),
        // One simply absent.
        relative_rom(2, "Gone", "roms/gb/gone.gb", 4, [&dud[0], &dud[1], &dud[2]]),
        // One that is a folder-based game, present as a directory.
        relative_rom(
            3,
            "Folder",
            "roms/dc/Shenmue",
            0,
            [&dud[0], &dud[1], &dud[2]],
        ),
        // One whose link no longer resolves.
        relative_rom(
            4,
            "Orphan",
            "roms/gb/orphan.gb",
            4,
            [&dud[0], &dud[1], &dud[2]],
        ),
        // One whose whole collection is missing.
        relative_rom(
            5,
            "NoFolder",
            "roms/nowhere/game.gb",
            4,
            [&dud[0], &dud[1], &dud[2]],
        ),
    ];
    // RomM's own view: it knows number 2 is gone.
    roms[1]["missing_from_fs"] = serde_json::json!(true);
    let mut stub = StubServer::start(serde_json::json!(roms), 5);
    ready_relative(&tree, &stub);

    tree.file("gb/present.gb", &contents);
    std::fs::create_dir_all(tree.library().join("dc/Shenmue")).expect("fixture");
    std::fs::write(tree.library().join("dc/Shenmue/Disc1.cdi"), b"disc").expect("fixture");
    let orphan = tree.library().join("gb/orphan.gb");
    std::os::unix::fs::symlink(tree.library().join("gb/nothing.gb"), &orphan).expect("fixture");

    assert!(tree.run(&["import"]).succeeded());

    let run = tree.run(&["stale-summary", "--json"]);
    assert!(run.succeeded(), "{:?}", run.error);
    let summary = run.json();
    assert_eq!(summary["total_in_cache"], 5);
    assert_eq!(summary["stale"], 4, "the matched record is not stale");

    // The reasons partition the stale population exactly.
    let reasons = summary["by_reason"].as_array().expect("reasons");
    let counted: u64 = reasons
        .iter()
        .map(|reason| reason["count"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(counted, 4, "{summary:#}");
    let by_code: std::collections::HashMap<&str, u64> = reasons
        .iter()
        .map(|reason| {
            (
                reason["code"].as_str().unwrap_or_default(),
                reason["count"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(by_code.get("absent"), Some(&1));
    assert_eq!(by_code.get("directory"), Some(&1));
    assert_eq!(by_code.get("dangling_symlink"), Some(&1));
    assert_eq!(by_code.get("parent_absent"), Some(&1));

    assert_eq!(summary["present_as_directory"], 1);
    assert_eq!(summary["dangling_symlinks"], 1);
    assert_eq!(summary["romm_reports_missing"], 1);
    assert_eq!(summary["unmapped"], 0);

    // Every group names the mapping that produced it.
    let mappings = summary["by_mapping"].as_array().expect("mappings");
    assert_eq!(mappings.len(), 1);
    assert!(
        mappings[0]["key"]
            .as_str()
            .is_some_and(|key| key.starts_with("roms ->")),
        "{mappings:#?}"
    );
    stub.stop();
}

#[test]
fn the_stale_summary_never_calls_a_present_directory_missing() {
    let tree = Tree::new("stale-directory");
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([relative_rom(
            1,
            "Folder",
            "roms/dc/Shenmue",
            0,
            [&dud[0], &dud[1], &dud[2]]
        )]),
        1,
    );
    ready_relative(&tree, &stub);
    std::fs::create_dir_all(tree.library().join("dc/Shenmue")).expect("fixture");
    assert!(tree.run(&["import"]).succeeded());

    // The record's own evidence is accurate.
    let record = tree.run(&["record", "1", "--json"]).json();
    let evidence = record["evidence"]
        .as_array()
        .expect("evidence")
        .iter()
        .filter_map(|line| line.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        !evidence.contains("does not exist"),
        "the directory is right there: {evidence}"
    );
    assert!(evidence.contains("is a directory"), "{evidence}");

    // And the summary counts it as a directory rather than as a missing file.
    let summary = tree.run(&["stale-summary", "--json"]).json();
    assert_eq!(summary["present_as_directory"], 1);
    let human = tree.run(&["stale-summary"]).stdout;
    assert!(
        human.contains("folder-based games, not missing files"),
        "{human}"
    );
    stub.stop();
}

#[test]
fn the_stale_summary_reports_its_verdict_on_the_population() {
    let tree = Tree::new("stale-verdict");
    let dud = dud_hashes();
    // Everything absent and flagged by RomM: ordinary library drift.
    let roms: Vec<serde_json::Value> = (0..10)
        .map(|index| {
            let mut rom = relative_rom(
                index,
                &format!("Gone {index}"),
                &format!("roms/gb/gone-{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            );
            rom["missing_from_fs"] = serde_json::json!(true);
            rom
        })
        .collect();
    let mut stub = StubServer::start(serde_json::json!(roms), 10);
    ready_relative(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let summary = tree.run(&["stale-summary", "--json"]).json();
    assert_eq!(summary["stale"], 10);
    assert_eq!(summary["romm_reports_missing"], 10);
    assert_eq!(
        summary["looks_like_library_drift"], true,
        "RomM saying the files are gone is not a mapping fault: {summary:#}"
    );
    let human = tree.run(&["stale-summary"]).stdout;
    assert!(human.contains("ordinary library drift"), "{human}");
    stub.stop();
}

#[test]
fn the_stale_summary_needs_a_cache_and_contacts_nothing() {
    let tree = Tree::new("stale-no-cache");
    let message = tree.run(&["stale-summary"]).error_text().to_string();
    assert!(message.contains("import"), "{message}");

    // With a cache, it makes no request at all.
    let dud = dud_hashes();
    let mut stub = StubServer::start(
        serde_json::json!([relative_rom(
            1,
            "Gone",
            "roms/gb/gone.gb",
            4,
            [&dud[0], &dud[1], &dud[2]]
        )]),
        1,
    );
    ready_relative(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());
    let before = stub.request_count();
    assert!(tree.run(&["stale-summary"]).succeeded());
    assert_eq!(
        stub.request_count(),
        before,
        "a summary of the cache must not contact RomM"
    );
    stub.stop();
}

#[test]
fn the_stale_summary_example_count_is_bounded() {
    let tree = Tree::new("stale-examples");
    let dud = dud_hashes();
    let roms: Vec<serde_json::Value> = (0..40)
        .map(|index| {
            relative_rom(
                index,
                &format!("Gone {index}"),
                &format!("roms/gb/gone-{index}.gb"),
                4,
                [&dud[0], &dud[1], &dud[2]],
            )
        })
        .collect();
    let mut stub = StubServer::start(serde_json::json!(roms), 40);
    ready_relative(&tree, &stub);
    assert!(tree.run(&["import"]).succeeded());

    let summary = tree
        .run(&["stale-summary", "--examples", "100000", "--json"])
        .json();
    for reason in summary["by_reason"].as_array().expect("reasons") {
        let examples = reason["examples"].as_array().expect("examples").len();
        assert!(
            examples <= archivefs_core::identity_source::stale::MAX_EXAMPLES,
            "{examples} examples is over the bound"
        );
    }
    stub.stop();
}

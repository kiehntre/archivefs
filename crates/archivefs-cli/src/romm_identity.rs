//! `archivefs-cli identity source romm <command>`.
//!
//! Follows the conventions the rest of this binary already uses: a flat
//! sub-command match, the shared `--json` flag, the `take_*` argument helpers,
//! and an error returned as `Box<dyn Error>` so `main` renders it and exits
//! non-zero. No new argument framework.
//!
//! # What no command here can do
//!
//! Write to RomM, trigger a RomM scan, modify a ROM, modify an emulator
//! configuration, or reach a public address. Those are properties of the core
//! this module calls - see `archivefs_core::identity_source` - and this module
//! adds no request path of its own.
//!
//! # Secrets
//!
//! A token is never accepted on the command line, never printed, and never
//! placed in JSON. It is referenced by file path, loaded through
//! `load_token_file`, and held in a type whose every rendering is redacted.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use archivefs_core::identity_source::cache::{CacheRefusal, IdentityCache};
use archivefs_core::identity_source::hashing::{LocalHashCache, LocalHashes, hash_file};
use archivefs_core::identity_source::matching::{LocalFileFacts, PathClaims};
use archivefs_core::identity_source::model::{
    ExternalIdentityRecord, ExternalVerification, IdentityProvider, LocalEvidenceStrength,
};
use archivefs_core::identity_source::net_policy::SystemResolver;
use archivefs_core::identity_source::path_map::{
    MappingPreview, PathMapping, PathMappings, PathTranslation, ProviderPathKind, normalise_prefix,
};
use archivefs_core::identity_source::romm::client::UreqTransport;
use archivefs_core::identity_source::romm::config::ValidatedRommSource;
use archivefs_core::identity_source::romm::import::{ImportProgress, ImportScope};
use archivefs_core::identity_source::settings::{
    ProviderSettings, SUGGESTED_TOKEN_PATH, SettingsLocation, default_identity_root,
    load_token_file,
};
use archivefs_core::identity_source::stale::{DEFAULT_EXAMPLES, StaleSummary};
use archivefs_core::identity_source::status::{IdentitySourceApi, ProviderStatus, RefreshRequest};
use archivefs_core::safe_read::TrustedRoots;
use serde::Serialize;

/// Every command this module accepts, for the help text and the tests.
pub const COMMANDS: &[&str] = &[
    "status",
    "configure",
    "test",
    "mappings",
    "import",
    "refresh",
    "records",
    "record",
    "conflicts",
    "stale-summary",
    "verify-hash",
    "enable",
    "disable",
    "remove",
];

/// The largest page a listing command will return, whatever is asked for.
pub const MAX_LIST_LIMIT: usize = 500;
pub const DEFAULT_LIST_LIMIT: usize = 25;
/// The largest number of preview examples.
pub const MAX_PREVIEW_LIMIT: usize = 100;
pub const DEFAULT_PREVIEW_LIMIT: usize = 10;

/// Entry point, called from `main` for `identity source romm ...`.
pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    dispatch(args, &Output::Standard, None)
}

/// `source_roots` is `None` in normal use, meaning "read them from the
/// configuration". The tests pass their own so that what a mapping or a
/// `verify-hash` path is measured against is the fixture's library rather than
/// whatever library this machine happens to have.
fn dispatch(
    args: Vec<String>,
    output: &Output,
    source_roots: Option<Vec<PathBuf>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args;
    let json = take_flag(&mut args, "--json");
    // An explicit root exists so a test can drive the whole surface without
    // touching the real data directory.
    let identity_root = match take_path(&mut args, "--identity-root")? {
        Some(root) => root,
        None => default_identity_root()?,
    };
    let context = Context {
        json,
        output,
        source_roots: source_roots.unwrap_or_else(configured_source_roots),
        settings: SettingsLocation::new(&identity_root, IdentityProvider::Romm),
        api: IdentitySourceApi::new(&identity_root, IdentityProvider::Romm),
    };

    let Some(command) = args.first().cloned() else {
        return Err(format!(
            "identity source romm requires a command; one of: {}",
            COMMANDS.join(", ")
        )
        .into());
    };
    args.remove(0);

    match command.as_str() {
        "status" => {
            reject_extra(&args, "status")?;
            status(&context)
        }
        "configure" => configure(&context, args),
        "test" => {
            reject_extra(&args, "test")?;
            test_connection(&context)
        }
        "mappings" => mappings(&context, args),
        "import" => import(&context, args, false),
        "refresh" => import(&context, args, true),
        "records" => records(&context, args),
        "record" => record(&context, args),
        "conflicts" => conflicts(&context, args),
        "stale-summary" => stale_summary(&context, args),
        "verify-hash" => verify_hash(&context, args),
        "enable" => {
            reject_extra(&args, "enable")?;
            set_enabled(&context, true)
        }
        "disable" => {
            reject_extra(&args, "disable")?;
            set_enabled(&context, false)
        }
        "remove" => remove(&context, args),
        other => Err(format!(
            "unknown identity source romm command {other:?}; one of: {}",
            COMMANDS.join(", ")
        )
        .into()),
    }
}

/// Where a command's output goes.
///
/// Exists so the tests can read back exactly what a person would see, including
/// which stream each line went to. Without it, "no progress chatter on stdout in
/// JSON mode" would be a claim no test could check.
enum Output {
    Standard,
    #[cfg(test)]
    Captured {
        out: std::cell::RefCell<String>,
        err: std::cell::RefCell<String>,
    },
}

impl Output {
    fn line(&self, text: &str) {
        match self {
            Self::Standard => println!("{text}"),
            #[cfg(test)]
            Self::Captured { out, .. } => {
                out.borrow_mut().push_str(text);
                out.borrow_mut().push('\n');
            }
        }
    }

    fn error_line(&self, text: &str) {
        match self {
            Self::Standard => {
                let _ = writeln!(std::io::stderr(), "{text}");
            }
            #[cfg(test)]
            Self::Captured { err, .. } => {
                err.borrow_mut().push_str(text);
                err.borrow_mut().push('\n');
            }
        }
    }
}

/// What every command needs.
struct Context<'a> {
    json: bool,
    output: &'a Output,
    /// The configured source folders. Mapping destinations must be inside one,
    /// and so must any path `verify-hash` is asked to read.
    source_roots: Vec<PathBuf>,
    settings: SettingsLocation,
    api: IdentitySourceApi,
}

impl Context<'_> {
    fn load_settings(&self) -> Result<ProviderSettings, Box<dyn std::error::Error>> {
        self.settings.load().map_err(|error| error.detail().into())
    }

    /// Validates the stored configuration into something usable.
    ///
    /// Resolves the host for real, so the local-only policy applies to what the
    /// name actually points at now rather than what it pointed at when it was
    /// configured.
    fn validated(
        &self,
        settings: &ProviderSettings,
    ) -> Result<ValidatedRommSource, Box<dyn std::error::Error>> {
        if settings.source.url.trim().is_empty() {
            return Err("no RomM URL is configured; run `identity source romm configure --url <local-url> --token-file <path>` first".into());
        }
        let token = load_token_file(settings.source.token_path.as_deref())
            .map_err(|refusal| refusal.detail())?;
        ValidatedRommSource::validate(
            &settings.source,
            &token,
            &self.source_roots,
            &SystemResolver,
        )
        .map_err(|refusal| refusal.detail().into())
    }

    /// Prints either JSON or the human rendering, never both.
    fn emit<T: Serialize>(
        &self,
        value: &T,
        human: impl FnOnce() -> Vec<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.json {
            self.output.line(&serde_json::to_string_pretty(value)?);
        } else {
            for line in human() {
                self.output.line(&line);
            }
        }
        Ok(())
    }

    /// Progress goes to stderr, and only when JSON is off, so a JSON consumer
    /// receives one document on stdout and nothing else.
    fn progress(&self, message: &str) {
        if !self.json {
            self.output.error_line(message);
        }
    }
}

/// The configured source folders, which mapping destinations must sit inside.
///
/// A missing or unreadable configuration yields an empty list rather than an
/// error: the mapping engine treats that as "no trusted-root restriction", which
/// is right for someone configuring RomM before adding source folders.
fn configured_source_roots() -> Vec<PathBuf> {
    archivefs_core::Config::load_default()
        .map(|config| config.source_folders)
        .unwrap_or_default()
}

// --- status ---------------------------------------------------------------

/// Status, plus the facts that live outside the core's own status type.
#[derive(Debug, Serialize)]
struct StatusReport {
    /// The URL as configured. Safe by construction: the policy refuses a URL
    /// containing credentials, so there is nothing here to redact.
    url: Option<String>,
    enabled: bool,
    token_file: Option<PathBuf>,
    /// Whether a usable token is present, without saying anything about it.
    token_available: bool,
    token_problem: Option<String>,
    page_size: u32,
    path_kind: ProviderPathKind,
    cache_format_version: Option<u32>,
    #[serde(flatten)]
    provider: ProviderStatus,
}

fn status(context: &Context) -> Result<(), Box<dyn std::error::Error>> {
    let settings = context.load_settings()?;
    let hashes = LocalHashCache::new();
    // `reachable: false` - status never contacts anything, which is what makes it
    // safe to run offline and at any time.
    let provider = context.api.status(&settings.source, &hashes, false);
    let token = load_token_file(settings.source.token_path.as_deref());
    let cache_format_version = context
        .api
        .open_cache(None)
        .ok()
        .map(|cache| cache.format_version);

    let report = StatusReport {
        url: (!settings.source.url.trim().is_empty()).then(|| settings.source.url.clone()),
        enabled: settings.source.enabled,
        token_file: settings.source.token_path.clone(),
        token_available: token.is_ok(),
        token_problem: token.err().map(|refusal| refusal.detail()),
        page_size: settings.effective_page_size(),
        path_kind: settings.source.provider_path_kind,
        cache_format_version,
        provider,
    };
    context.emit(&report, || {
        let provider = &report.provider;
        let mut lines = vec![
            "RomM identity source".to_string(),
            format!("  State:            {}", provider.state.label()),
            format!(
                "  URL:              {}",
                report.url.as_deref().unwrap_or("(not configured)")
            ),
            format!("  Enabled:          {}", yes_no(report.enabled)),
            format!(
                "  Token file:       {}",
                report
                    .token_file
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| format!(
                        "(not configured; suggested {SUGGESTED_TOKEN_PATH})"
                    ))
            ),
            format!(
                "  Token usable:     {}{}",
                yes_no(report.token_available),
                report
                    .token_problem
                    .as_deref()
                    .map(|problem| format!(" - {problem}"))
                    .unwrap_or_default()
            ),
            format!("  Page size:        {}", report.page_size),
            format!("  Path shape:       {}", report.path_kind.label()),
        ];
        if let Some(server) = &provider.server_id {
            lines.push(format!("  Server identity:  {server}"));
        }
        if let Some(version) = &provider.server_version {
            lines.push(format!("  RomM version:     {version}"));
        }
        lines.push(String::new());
        lines.push("Imported identity".to_string());
        lines.push(format!(
            "  Platforms:        {}",
            provider.platforms_imported
        ));
        lines.push(format!("  Records:          {}", provider.records_imported));
        let counts = &provider.counts;
        lines.push(format!("  Matched (usable): {}", counts.usable()));
        lines.push(format!("    Confirmed:      {}", counts.confirmed));
        lines.push(format!("    Strong:         {}", counts.strong));
        lines.push(format!("    Probable:       {}", counts.probable));
        lines.push(format!("  Ambiguous:        {}", counts.ambiguous));
        lines.push(format!("  Stale:            {}", counts.stale));
        lines.push(format!("  Unmatched:        {}", counts.unmatched));
        lines.push(format!("  Invalid hashes:   {}", provider.invalid_hashes));
        lines.push(format!(
            "  Unknown platforms:{}",
            provider.unknown_platforms
        ));
        lines.push(format!(
            "  Duplicate targets:{}",
            provider.duplicate_mappings
        ));
        lines.push(format!(
            "  Multi-file groups:{}",
            provider.multi_file_groups
        ));
        lines.push(format!("  Locally verified: {}", provider.locally_verified));
        lines.push(String::new());
        lines.push("Cache".to_string());
        lines.push(format!(
            "  Location:         {}",
            provider
                .cache_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "(none)".to_string())
        ));
        lines.push(format!(
            "  Size:             {}",
            provider
                .cache_size_bytes
                .map(|bytes| format!("{bytes} bytes"))
                .unwrap_or_else(|| "(none)".to_string())
        ));
        lines.push(format!(
            "  Format version:   {}",
            report
                .cache_format_version
                .map(|version| version.to_string())
                .unwrap_or_else(|| "(none)".to_string())
        ));
        lines.push(format!(
            "  Last refresh:     {}",
            provider
                .last_successful_refresh_unix_seconds
                .map(|seconds| format!("unix {seconds}"))
                .unwrap_or_else(|| "never".to_string())
        ));
        lines.push(format!(
            "  Offline browsing: {}",
            yes_no(provider.state.can_browse())
        ));
        if let Some(error) = &provider.last_error {
            lines.push(format!("  Last error:       {error}"));
        }
        lines
    })
}

// --- configure ------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ConfigureResult {
    config_path: PathBuf,
    url: String,
    token_file: Option<PathBuf>,
    token_available: bool,
    token_problem: Option<String>,
    enabled: bool,
    page_size: u32,
    path_kind: ProviderPathKind,
    /// The addresses the URL resolved to at configuration time, all approved.
    resolved_addresses: Vec<String>,
}

fn configure(context: &Context, mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let url = take_string(&mut args, "--url")?;
    let token_file = take_path(&mut args, "--token-file")?;
    let page_size = take_number(&mut args, "--page-size")?;
    let path_kind = match take_string(&mut args, "--path-kind")? {
        Some(text) => Some(ProviderPathKind::parse(&text).ok_or_else(|| {
            format!(
                "unknown --path-kind {text:?}; use `relative` for a RomM that reports                  `roms/gb/game.gb`, or `absolute` for one that reports                  `/romm/library/gb/game.gb`"
            )
        })?),
        None => None,
    };
    let enable = take_flag(&mut args, "--enable");
    reject_extra(&args, "configure")?;

    let mut settings = context.load_settings()?;
    if let Some(url) = url {
        // Validated before it is stored, so an unusable address is refused at the
        // moment it is offered rather than at first use.
        let approved =
            archivefs_core::identity_source::net_policy::validate_endpoint(&url, &SystemResolver)
                .map_err(|refusal| refusal.detail())?;
        settings.source.url = approved.origin().to_string();
    }
    if let Some(path) = token_file {
        // Checked now, so a bad path is reported immediately - but the token is
        // never copied anywhere, only referenced.
        load_token_file(Some(&path)).map_err(|refusal| refusal.detail())?;
        settings.source.token_path = Some(path);
    }
    if let Some(size) = page_size {
        settings.page_size = Some(u32::try_from(size).map_err(|_| "--page-size is too large")?);
    }
    if let Some(kind) = path_kind {
        // Changing the shape orphans every mapping written in the other one, so
        // the switch is refused while one is still stored rather than leaving a
        // configuration whose mappings can never match anything.
        let stranded: Vec<String> = settings
            .source
            .mappings
            .iter()
            .filter(|mapping| normalise_prefix(&mapping.provider_prefix, kind).is_err())
            .map(|mapping| mapping.provider_prefix.clone())
            .collect();
        if !stranded.is_empty() {
            return Err(format!(
                "{} configured mapping(s) are written for {} paths and cannot be used as {}: {}.                  Remove them first with `mappings remove --romm-root <path>`, then set the shape                  and add the replacements.",
                stranded.len(),
                settings.source.provider_path_kind.slug(),
                kind.slug(),
                stranded.join(", ")
            )
            .into());
        }
        settings.source.provider_path_kind = kind;
    }
    if enable {
        settings.source.enabled = true;
    }
    if settings.source.url.trim().is_empty() {
        return Err("--url is required the first time this source is configured".into());
    }

    let config_path = context
        .settings
        .save(&settings)
        .map_err(|error| error.detail())?;
    let token = load_token_file(settings.source.token_path.as_deref());
    let resolved = archivefs_core::identity_source::net_policy::validate_endpoint(
        &settings.source.url,
        &SystemResolver,
    )
    .map(|approved| approved.resolved_addresses().to_vec())
    .unwrap_or_default();

    let result = ConfigureResult {
        config_path,
        url: settings.source.url.clone(),
        token_file: settings.source.token_path.clone(),
        token_available: token.is_ok(),
        token_problem: token.err().map(|refusal| refusal.detail()),
        enabled: settings.source.enabled,
        page_size: settings.effective_page_size(),
        path_kind: settings.source.provider_path_kind,
        resolved_addresses: resolved,
    };
    context.emit(&result, || {
        let mut lines = vec![
            format!(
                "Saved RomM configuration to {}",
                result.config_path.display()
            ),
            format!("  URL:         {}", result.url),
            format!("  Resolved to: {}", result.resolved_addresses.join(", ")),
            format!("  Enabled:     {}", yes_no(result.enabled)),
            format!("  Page size:   {}", result.page_size),
            format!("  Path shape:  {}", result.path_kind.label()),
        ];
        match (&result.token_file, &result.token_problem) {
            (Some(path), None) => lines.push(format!("  Token file:  {} (usable)", path.display())),
            (Some(path), Some(problem)) => {
                lines.push(format!("  Token file:  {} - {problem}", path.display()));
            }
            (None, _) => lines.push(format!(
                "  Token file:  not configured; create a read-only client token and pass \
                 --token-file (suggested {SUGGESTED_TOKEN_PATH})"
            )),
        }
        if !result.enabled {
            lines.push(
                "The source is disabled. Run `identity source romm enable` when you are ready."
                    .to_string(),
            );
        }
        lines
    })
}

// --- test -----------------------------------------------------------------

#[derive(Debug, Serialize)]
struct TestResult {
    server_id: String,
    reachable: bool,
    romm_version: Option<String>,
    version_supported: bool,
    api_version: Option<String>,
    available_endpoints: Vec<String>,
    missing_endpoints: Vec<String>,
    declared_read_scopes: Vec<String>,
    /// Whether an authenticated read actually worked, per endpoint.
    authenticated_reads: Vec<AuthenticatedRead>,
    supports_pagination: bool,
    available_hash_fields: Vec<String>,
    available_artwork_fields: Vec<String>,
    exposes_file_list: bool,
    supports_client_tokens: bool,
    can_import: bool,
    blocking_reason: Option<String>,
    notes: Vec<String>,
    /// What this source is configured to expect.
    configured_path_kind: ProviderPathKind,
    /// The shape the instance actually reports, from one sampled record.
    observed_path_kind: Option<ProviderPathKind>,
    /// That record's path, so the shape can be seen rather than taken on trust.
    sample_provider_path: Option<String>,
    /// Set when the two disagree: the setting needs changing before an import can
    /// match anything.
    path_kind_mismatch: bool,
    /// Stated explicitly, because a connection test that changed something would
    /// be a bad connection test.
    cache_modified: bool,
    romm_modified: bool,
}

#[derive(Debug, Serialize)]
struct AuthenticatedRead {
    endpoint: String,
    ok: bool,
    detail: Option<String>,
}

fn test_connection(context: &Context) -> Result<(), Box<dyn std::error::Error>> {
    let settings = context.load_settings()?;
    let source = context.validated(&settings)?;
    let transport = UreqTransport::new();
    context.progress("Contacting RomM...");

    let report = context
        .api
        .test_connection(&source, &transport, None)
        .map_err(|error| error.detail())?;

    // An authenticated probe of each endpoint the import needs: capability says
    // the endpoint exists, this says the token may actually read it.
    let client =
        archivefs_core::identity_source::romm::client::RommClient::new(&source, &transport);
    let probe = |endpoint: &str, outcome: Result<(), String>| AuthenticatedRead {
        endpoint: endpoint.to_string(),
        ok: outcome.is_ok(),
        detail: outcome.err(),
    };
    // One record only: enough to prove read access, small enough to be free, and
    // enough to see which shape of path this instance reports.
    let first_page = client.roms_page(1, 0, None);
    let sample_provider_path = first_page
        .as_ref()
        .ok()
        .and_then(|page| page.items.first())
        .map(archivefs_core::identity_source::romm::normalise::provider_path_of)
        .filter(|path| !path.is_empty());
    let observed_path_kind = sample_provider_path
        .as_deref()
        .map(ProviderPathKind::observed_in);
    let authenticated_reads = vec![
        probe(
            "/api/platforms",
            client
                .platforms(None)
                .map(|_| ())
                .map_err(|error| error.detail()),
        ),
        probe(
            "/api/roms",
            first_page.map(|_| ()).map_err(|error| error.detail()),
        ),
    ];

    let heartbeat = report.heartbeat.as_ref();
    let result = TestResult {
        server_id: report.server_id.clone(),
        reachable: heartbeat.is_some(),
        romm_version: heartbeat.map(|beat| beat.version.clone()),
        version_supported: heartbeat.is_some_and(|beat| beat.is_supported()),
        api_version: report.api.api_version.clone(),
        available_endpoints: report.api.available_endpoints.clone(),
        missing_endpoints: report.api.missing_endpoints.clone(),
        declared_read_scopes: report.api.declared_read_scopes.clone(),
        supports_pagination: report.api.supports_limit_offset_pagination,
        available_hash_fields: report.api.available_hash_fields.clone(),
        available_artwork_fields: report.api.available_artwork_fields.clone(),
        exposes_file_list: report.api.exposes_file_list,
        supports_client_tokens: report.api.supports_client_tokens,
        can_import: report.api.can_import() && authenticated_reads.iter().all(|read| read.ok),
        blocking_reason: report.api.blocking_reason(),
        notes: report.notes.clone(),
        configured_path_kind: settings.source.provider_path_kind,
        path_kind_mismatch: observed_path_kind
            .is_some_and(|observed| observed != settings.source.provider_path_kind),
        observed_path_kind,
        sample_provider_path,
        cache_modified: false,
        romm_modified: false,
        authenticated_reads,
    };

    let all_reads_ok = result.authenticated_reads.iter().all(|read| read.ok);
    context.emit(&result, || {
        let mut lines = vec![
            format!("Connected to {}", result.server_id),
            format!(
                "  RomM version:      {} ({})",
                result.romm_version.as_deref().unwrap_or("unknown"),
                if result.version_supported {
                    "supported"
                } else {
                    "not verified against this build"
                }
            ),
            format!(
                "  API version:       {}",
                result.api_version.as_deref().unwrap_or("unknown")
            ),
            format!(
                "  Endpoints present: {}",
                result.available_endpoints.join(", ")
            ),
            format!(
                "  Read scopes:       {}",
                result.declared_read_scopes.join(", ")
            ),
            format!(
                "  Pagination:        {}",
                yes_no(result.supports_pagination)
            ),
            format!(
                "  Hash fields:       {}",
                if result.available_hash_fields.is_empty() {
                    "none".to_string()
                } else {
                    result.available_hash_fields.join(", ")
                }
            ),
            format!(
                "  Artwork fields:    {}",
                if result.available_artwork_fields.is_empty() {
                    "none".to_string()
                } else {
                    result.available_artwork_fields.join(", ")
                }
            ),
            format!("  Multi-file list:   {}", yes_no(result.exposes_file_list)),
            format!(
                "  Client tokens:     {}",
                yes_no(result.supports_client_tokens)
            ),
        ];
        lines.push("  Authenticated reads:".to_string());
        for read in &result.authenticated_reads {
            lines.push(format!(
                "    {:<16} {}{}",
                read.endpoint,
                if read.ok { "ok" } else { "failed" },
                read.detail
                    .as_deref()
                    .map(|detail| format!(" - {detail}"))
                    .unwrap_or_default()
            ));
        }
        if !result.missing_endpoints.is_empty() {
            lines.push(format!(
                "  Missing:           {}",
                result.missing_endpoints.join(", ")
            ));
        }
        for note in &result.notes {
            lines.push(format!("  Note: {note}"));
        }
        lines.push(format!(
            "  Path shape:        configured {}, reported {}",
            result.configured_path_kind.slug(),
            result
                .observed_path_kind
                .map(ProviderPathKind::slug)
                .unwrap_or("unknown (no record to sample)")
        ));
        if let Some(sample) = &result.sample_provider_path {
            lines.push(format!("  Example path:      {sample}"));
        }
        if result.path_kind_mismatch {
            let observed = result
                .observed_path_kind
                .map(ProviderPathKind::slug)
                .unwrap_or("relative");
            lines.push(format!(
                "  This server's paths are {observed}, but this source is configured for {}. Run \
                 `identity source romm configure --path-kind {observed}` and write the mappings in \
                 that shape, or every record will stay unmatched.",
                result.configured_path_kind.slug()
            ));
        }
        lines.push(format!(
            "  Ready to import:   {}",
            yes_no(result.can_import)
        ));
        lines.push("Nothing was imported, cached or changed in RomM.".to_string());
        lines
    })?;

    if !all_reads_ok {
        return Err(
            "the token could not read every endpoint an import needs; check that it carries \
             platforms.read and roms.read and has not expired"
                .into(),
        );
    }
    Ok(())
}

// --- mappings -------------------------------------------------------------

#[derive(Debug, Serialize)]
struct MappingsList {
    mappings: Vec<MappingEntry>,
    trusted_roots: Vec<PathBuf>,
    /// Which shape these prefixes are written in, so a listing is unambiguous.
    path_kind: ProviderPathKind,
}

#[derive(Debug, Serialize)]
struct MappingEntry {
    romm_root: String,
    archivefs_root: PathBuf,
    /// Why this stored mapping cannot be used under the configured path shape.
    ///
    /// A listing must never fail: if a mapping and the declared shape disagree,
    /// seeing that is exactly how someone works out what to remove.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
}

fn mappings(context: &Context, mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let Some(action) = args.first().cloned() else {
        return Err("mappings requires list, add, remove or preview".into());
    };
    args.remove(0);
    match action.as_str() {
        "list" => {
            reject_extra(&args, "mappings list")?;
            let settings = context.load_settings()?;
            list_mappings(context, &settings)
        }
        "add" => add_mapping(context, args),
        "remove" => remove_mapping(context, args),
        "preview" => preview_mappings(context, args),
        other => Err(format!("unknown mappings action {other:?}").into()),
    }
}

fn list_mappings(
    context: &Context,
    settings: &ProviderSettings,
) -> Result<(), Box<dyn std::error::Error>> {
    let trusted = context.source_roots.clone();
    let kind = settings.source.provider_path_kind;

    // Each mapping is judged on its own, so one bad entry cannot hide the rest.
    // Usable ones are ordered by the engine, which is the order the rules are
    // actually applied in rather than the order they were typed.
    let usable: Vec<PathMapping> = settings
        .source
        .mappings
        .iter()
        .filter(|mapping| normalise_prefix(&mapping.provider_prefix, kind).is_ok())
        .cloned()
        .collect();
    let ordered = PathMappings::validate(&usable, &[], kind)
        .map(|validated| validated.as_slice().to_vec())
        .unwrap_or(usable);

    let mut entries: Vec<MappingEntry> = ordered
        .iter()
        .map(|mapping| MappingEntry {
            romm_root: mapping.provider_prefix.clone(),
            archivefs_root: mapping.archivefs_prefix.clone(),
            problem: None,
        })
        .collect();
    for mapping in &settings.source.mappings {
        if let Err(refusal) = normalise_prefix(&mapping.provider_prefix, kind) {
            entries.push(MappingEntry {
                romm_root: mapping.provider_prefix.clone(),
                archivefs_root: mapping.archivefs_prefix.clone(),
                problem: Some(refusal.detail()),
            });
        }
    }

    let list = MappingsList {
        path_kind: kind,
        mappings: entries,
        trusted_roots: trusted,
    };
    context.emit(&list, || {
        let usable_count = list
            .mappings
            .iter()
            .filter(|entry| entry.problem.is_none())
            .count();
        let mut lines = if list.mappings.is_empty() {
            vec![
                "No path mappings are configured.".to_string(),
                "Add one with `identity source romm mappings add --romm-root <path> \
                 --archivefs-root <path>`."
                    .to_string(),
            ]
        } else {
            let mut lines = vec![format!(
                "{usable_count} usable path mapping(s), most specific first:"
            )];
            for entry in list.mappings.iter().filter(|entry| entry.problem.is_none()) {
                lines.push(format!(
                    "  {}  ->  {}",
                    entry.romm_root,
                    entry.archivefs_root.display()
                ));
            }
            lines
        };
        let unusable: Vec<&MappingEntry> = list
            .mappings
            .iter()
            .filter(|entry| entry.problem.is_some())
            .collect();
        if !unusable.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "{} stored mapping(s) cannot be used as configured:",
                unusable.len()
            ));
            for entry in unusable {
                lines.push(format!(
                    "  {}  ->  {}",
                    entry.romm_root,
                    entry.archivefs_root.display()
                ));
                if let Some(problem) = &entry.problem {
                    lines.push(format!("    {problem}"));
                }
                lines.push(format!(
                    "    Remove it with `mappings remove --romm-root {}`.",
                    entry.romm_root
                ));
            }
        }
        lines.push(String::new());
        lines.push(format!("RomM path shape: {}", list.path_kind.label()));
        if !list.trusted_roots.is_empty() {
            lines.push(String::new());
            lines.push("Destinations must sit inside a configured source folder:".to_string());
            for root in &list.trusted_roots {
                lines.push(format!("  {}", root.display()));
            }
        }
        lines
    })
}

/// Whether a stored mapping is the one `wanted` names.
///
/// Compares the canonical forms when both normalise, and falls back to the text
/// as typed. The fallback matters: a mapping stored under one path shape and then
/// orphaned by a change of shape must still be removable, or the configuration
/// could be edited into a state with no way out.
fn prefix_matches(stored: &str, wanted: &str, kind: ProviderPathKind) -> bool {
    let lenient = |text: &str| {
        let trimmed = text.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            text.trim().to_string()
        } else {
            trimmed.to_string()
        }
    };
    match (
        normalise_prefix(stored, kind),
        normalise_prefix(wanted, kind),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => lenient(stored) == lenient(wanted),
    }
}

fn add_mapping(context: &Context, mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let romm_root =
        take_string(&mut args, "--romm-root")?.ok_or("mappings add requires --romm-root <path>")?;
    let archivefs_root = take_path(&mut args, "--archivefs-root")?
        .ok_or("mappings add requires --archivefs-root <path>")?;
    let replace = take_flag(&mut args, "--replace");
    reject_extra(&args, "mappings add")?;

    let mut settings = context.load_settings()?;
    let kind = settings.source.provider_path_kind;
    let candidate = PathMapping {
        provider_prefix: romm_root.clone(),
        archivefs_prefix: archivefs_root.clone(),
    };
    // Normalised first, so the comparison below is against the engine's own form
    // rather than the typed text - and so a prefix of the wrong shape is refused
    // with the shape named, rather than silently never matching anything.
    let normalised_prefix =
        normalise_prefix(&romm_root, kind).map_err(|refusal| refusal.detail())?;

    let existing = settings
        .source
        .mappings
        .iter()
        .position(|mapping| prefix_matches(&mapping.provider_prefix, &romm_root, kind));
    if let Some(index) = existing {
        if !replace {
            return Err(format!(
                "a mapping for {normalised_prefix} already exists (-> {}); pass --replace to \
                 change it",
                settings.source.mappings[index].archivefs_prefix.display()
            )
            .into());
        }
        settings.source.mappings.remove(index);
    }
    settings.source.mappings.push(candidate);

    // Validated as a whole, with the trusted roots applied, so a duplicate
    // destination or an escaping mapping is refused before it is stored.
    PathMappings::validate(&settings.source.mappings, &context.source_roots, kind)
        .map_err(|refusal| refusal.detail())?;
    context
        .settings
        .save(&settings)
        .map_err(|error| error.detail())?;
    list_mappings(context, &settings)
}

fn remove_mapping(
    context: &Context,
    mut args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let romm_root = take_string(&mut args, "--romm-root")?
        .ok_or("mappings remove requires --romm-root <path>")?;
    reject_extra(&args, "mappings remove")?;
    let mut settings = context.load_settings()?;
    let kind = settings.source.provider_path_kind;
    let before = settings.source.mappings.len();
    // An unnormalisable spelling is compared as typed: removing something is not
    // the moment to refuse over a prefix that is already stored.
    let target = normalise_prefix(&romm_root, kind).unwrap_or_else(|_| romm_root.clone());

    settings
        .source
        .mappings
        .retain(|mapping| !prefix_matches(&mapping.provider_prefix, &romm_root, kind));
    if settings.source.mappings.len() == before {
        return Err(format!("no mapping starts from {target}").into());
    }
    context
        .settings
        .save(&settings)
        .map_err(|error| error.detail())?;
    list_mappings(context, &settings)
}

#[derive(Debug, Serialize)]
struct PreviewReport {
    examples: Vec<PreviewExample>,
    translated: usize,
    unmatched: usize,
    refused: usize,
    /// How many of the sampled paths existed locally.
    existing_files: usize,
    /// Where the sample paths came from - the cache, or RomM.
    sample_source: &'static str,
    /// The shape the mappings are configured for.
    configured_path_kind: ProviderPathKind,
    /// The shape the samples actually had, counted.
    observed_relative: usize,
    observed_absolute: usize,
    /// Set when the samples disagree with the configured shape, which is nearly
    /// always the real cause of a preview that refuses everything.
    suggested_path_kind: Option<ProviderPathKind>,
}

#[derive(Debug, Serialize)]
struct PreviewExample {
    /// The exact string RomM sent, never a cleaned-up version of it.
    romm_path: String,
    /// The form the comparison was made against. Equal to `romm_path` unless the
    /// provider spelled it differently, in which case seeing both is the point.
    normalised_path: Option<String>,
    path_kind: Option<ProviderPathKind>,
    matched_prefix: Option<String>,
    archivefs_path: Option<PathBuf>,
    /// Which configured source folder the result landed in.
    trusted_root: Option<PathBuf>,
    /// Whether the destination is inside a configured source folder. `None` when
    /// there was no translation to check.
    inside_trusted_root: Option<bool>,
    outcome: &'static str,
    refusal: Option<String>,
    refusal_code: Option<String>,
    file_exists: Option<bool>,
    canonical_platform: Option<String>,
}

fn preview_mappings(
    context: &Context,
    mut args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let limit = take_number(&mut args, "--limit")?
        .unwrap_or(DEFAULT_PREVIEW_LIMIT)
        .clamp(1, MAX_PREVIEW_LIMIT);
    reject_extra(&args, "mappings preview")?;
    let settings = context.load_settings()?;
    let kind = settings.source.provider_path_kind;
    let engine = PathMappings::validate(&settings.source.mappings, &context.source_roots, kind)
        .map_err(|refusal| refusal.detail())?;

    // Prefer paths already in the cache: previewing against real data costs
    // nothing and needs no network. Only ask RomM when there is no cache.
    let (samples, platforms, sample_source) = match context.api.open_cache(None) {
        Ok(cache) => {
            let samples: Vec<String> = cache
                .records
                .iter()
                .take(limit)
                .map(|record| record.provider_path.clone())
                .collect();
            let platforms: Vec<Option<String>> = cache
                .records
                .iter()
                .take(limit)
                .map(|record| record.platform_candidate.clone())
                .collect();
            (samples, platforms, "cached identity")
        }
        Err(_) => {
            let source = context.validated(&settings)?;
            let transport = UreqTransport::new();
            let client =
                archivefs_core::identity_source::romm::client::RommClient::new(&source, &transport);
            context.progress("No cache yet; asking RomM for a small sample...");
            let page = client
                .roms_page(u32::try_from(limit).unwrap_or(10), 0, None)
                .map_err(|error| error.detail())?;
            // The same extraction an import uses, so the preview cannot promise a
            // translation the import would then make differently.
            let samples: Vec<String> = page
                .items
                .iter()
                .map(archivefs_core::identity_source::romm::normalise::provider_path_of)
                .filter(|path| !path.is_empty())
                .collect();
            let platforms: Vec<Option<String>> = page
                .items
                .iter()
                .map(|item| {
                    item.get("platform_slug")
                        .and_then(|value| value.as_str())
                        .and_then(archivefs_core::identity_source::romm::normalise::canonical_platform_for_romm_slug)
                        .map(str::to_string)
                })
                .collect();
            (samples, platforms, "a bounded RomM sample")
        }
    };

    let preview = MappingPreview::build(&engine, &samples);
    let examples: Vec<PreviewExample> = preview
        .translations
        .iter()
        .enumerate()
        .map(|(index, translation)| match translation {
            PathTranslation::Translated {
                provider_path,
                normalised_path,
                kind,
                archivefs_path,
                matched_prefix,
                trusted_root,
            } => PreviewExample {
                romm_path: provider_path.clone(),
                normalised_path: Some(normalised_path.clone()),
                path_kind: Some(*kind),
                matched_prefix: Some(matched_prefix.clone()),
                archivefs_path: Some(archivefs_path.clone()),
                trusted_root: trusted_root.clone(),
                // A translation only gets this far if it is inside a root, or if
                // no roots were configured to check against.
                inside_trusted_root: Some(true),
                outcome: "translated",
                refusal: None,
                refusal_code: None,
                // Metadata only. A preview never reads a file's contents.
                file_exists: Some(archivefs_path.is_file()),
                canonical_platform: platforms.get(index).cloned().flatten(),
            },
            PathTranslation::Unmatched {
                provider_path,
                normalised_path,
                kind,
            } => PreviewExample {
                romm_path: provider_path.clone(),
                normalised_path: Some(normalised_path.clone()),
                path_kind: Some(*kind),
                matched_prefix: None,
                archivefs_path: None,
                trusted_root: None,
                inside_trusted_root: None,
                outcome: "unmatched",
                refusal: None,
                refusal_code: None,
                file_exists: None,
                canonical_platform: platforms.get(index).cloned().flatten(),
            },
            PathTranslation::Refused {
                provider_path,
                refusal,
            } => PreviewExample {
                romm_path: provider_path.clone(),
                normalised_path: None,
                path_kind: None,
                matched_prefix: None,
                archivefs_path: None,
                trusted_root: None,
                inside_trusted_root: matches!(
                    refusal,
                    archivefs_core::identity_source::path_map::MappingRefusal::OutsideTrustedRoots { .. }
                )
                .then_some(false),
                outcome: "refused",
                refusal: Some(refusal.detail()),
                refusal_code: Some(refusal.code().to_string()),
                file_exists: None,
                canonical_platform: None,
            },
        })
        .collect();

    let report = PreviewReport {
        translated: preview.translated,
        unmatched: preview.unmatched,
        refused: preview.refused,
        existing_files: examples
            .iter()
            .filter(|example| example.file_exists == Some(true))
            .count(),
        sample_source,
        configured_path_kind: preview.configured_kind,
        observed_relative: preview.observed_relative,
        observed_absolute: preview.observed_absolute,
        suggested_path_kind: preview.suggested_kind(),
        examples,
    };
    context.emit(&report, || {
        let mut lines = vec![
            format!(
                "Previewing {} path(s) from {} - nothing was imported or changed.",
                report.examples.len(),
                report.sample_source
            ),
            format!(
                "Configured RomM path shape: {}",
                report.configured_path_kind.label()
            ),
        ];
        if let Some(suggested) = report.suggested_path_kind {
            lines.push(String::new());
            lines.push(format!(
                "These paths look {}, not {}. Run `identity source romm configure --path-kind {}` \
                 and rewrite the mappings to match, or nothing will translate.",
                suggested.slug(),
                report.configured_path_kind.slug(),
                suggested.slug()
            ));
        }
        lines.push(String::new());
        for example in &report.examples {
            lines.push(format!("  {}", example.romm_path));
            // Shown only when it differs, so an identical line is not noise.
            if let Some(normalised) = &example.normalised_path
                && normalised != &example.romm_path
            {
                lines.push(format!("    compared as: {normalised}"));
            }
            match (&example.archivefs_path, &example.refusal) {
                (Some(path), _) => {
                    lines.push(format!("    -> {}", path.display()));
                    lines.push(format!(
                        "       via mapping {}, file {}{}",
                        example.matched_prefix.as_deref().unwrap_or("(none)"),
                        if example.file_exists == Some(true) {
                            "present"
                        } else {
                            "missing"
                        },
                        example
                            .canonical_platform
                            .as_deref()
                            .map(|platform| format!(", {platform}"))
                            .unwrap_or_default()
                    ));
                    lines.push(format!(
                        "       trusted root: {}",
                        example
                            .trusted_root
                            .as_ref()
                            .map(|root| root.display().to_string())
                            .unwrap_or_else(
                                || "not checked (no source folders configured)".to_string()
                            )
                    ));
                }
                (None, Some(refusal)) => lines.push(format!(
                    "    refused ({}): {refusal}",
                    example.refusal_code.as_deref().unwrap_or("unknown")
                )),
                (None, None) => lines.push("    no mapping covers this path".to_string()),
            }
        }
        lines.push(String::new());
        lines.push(format!(
            "{} translated ({} of those exist locally), {} unmatched, {} refused",
            report.translated, report.existing_files, report.unmatched, report.refused
        ));
        lines.push(format!(
            "Sampled path shapes: {} relative, {} absolute",
            report.observed_relative, report.observed_absolute
        ));
        lines
    })
}

// --- import and refresh ---------------------------------------------------

#[derive(Debug, Serialize)]
struct ImportResult {
    mode: &'static str,
    sample_limit: Option<usize>,
    published: bool,
    cache_path: Option<PathBuf>,
    pages_fetched: u32,
    records_fetched: usize,
    platforms: usize,
    records: usize,
    confirmed: usize,
    strong: usize,
    probable: usize,
    ambiguous: usize,
    stale: usize,
    unmatched: usize,
    invalid_hashes: usize,
    unknown_platforms: usize,
    multi_file_groups: usize,
    elapsed_milliseconds: u128,
    peak_memory_kib: Option<u64>,
    /// What the import was asked to page with.
    configured_page_size: u32,
    /// What it ended up paging with, after any reduction.
    effective_page_size: u32,
    /// The smallest page size it ever used.
    smallest_page_size: u32,
    /// How many times the page size stepped down.
    page_size_reductions: u32,
    /// How many responses were refused for exceeding the size ceiling.
    oversized_page_retries: u32,
    /// How many times the page size stepped back up after a run of successes.
    page_size_recoveries: u32,
    /// Records imported without their per-file detail, because RomM's file list
    /// for them was too large to read.
    records_without_file_detail: Vec<String>,
    /// On failure: whether the previous cache still serves.
    previous_cache_usable: bool,
    error: Option<String>,
    error_code: Option<String>,
}

fn import(
    context: &Context,
    mut args: Vec<String>,
    is_refresh: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sample = take_number(&mut args, "--sample")?;
    reject_extra(&args, if is_refresh { "refresh" } else { "import" })?;
    if is_refresh && sample.is_some() {
        return Err("refresh does not take --sample; use `import --sample <n>` to preview".into());
    }

    let settings = context.load_settings()?;
    // Validated before the enabled check, so someone who has configured nothing is
    // told that rather than being told to enable a source that does not exist yet.
    let source = context.validated(&settings)?;
    if !settings.source.enabled {
        return Err("the RomM source is disabled; run `identity source romm enable` first".into());
    }
    let transport = UreqTransport::new();
    let page_size = settings.effective_page_size();
    let paging = ObservedPaging::new(page_size);
    let mode = if is_refresh { "refresh" } else { "import" };
    // A connection failure is reported through the same result shape as any other
    // import failure, so `--json` always yields one document to read rather than
    // nothing at all.
    let capability = match context.api.test_connection(&source, &transport, None) {
        Ok(capability) => capability,
        Err(error) => {
            let result = failed_import(
                context,
                mode,
                sample,
                error.code(),
                error.detail(),
                0,
                &paging,
            );
            context.emit(&result, || render_import(&result))?;
            return Err(error.detail().into());
        }
    };

    let scope = match sample {
        // A sample is validation only: it never publishes, so it cannot replace a
        // working cache with a partial one.
        Some(max_records) => ImportScope::Sample { max_records },
        None => ImportScope::Full,
    };
    let trusted = TrustedRoots::from_paths(&context.source_roots);
    let hashes = LocalHashCache::new();
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();

    if let ImportScope::Sample { max_records } = scope {
        // Sample mode imports and matches, then reports - and stops. Publication
        // is deliberately not reached.
        context.progress(&format!(
            "Importing a sample of up to {max_records} records..."
        ));
        let outcome = archivefs_core::identity_source::romm::import::import_identity(
            &source,
            &transport,
            scope,
            &capability,
            page_size,
            |progress| {
                paging.note(progress);
                report_progress(context, progress);
            },
            Some(&cancel),
        );
        let elapsed = started.elapsed().as_millis();
        return match outcome {
            Ok(mut outcome) => {
                archivefs_core::identity_source::matching::match_all(
                    &mut outcome.cache.records,
                    &hashes,
                    |record| facts_for(record, &trusted),
                    Some(&cancel),
                )
                .map_err(|_| "the sample import was cancelled")?;
                let counts = outcome.cache.counts();
                let groups =
                    archivefs_core::identity_source::matching::build_groups(&outcome.cache.records);
                let result = ImportResult {
                    mode: "sample",
                    sample_limit: Some(max_records),
                    published: false,
                    cache_path: None,
                    pages_fetched: outcome.progress.pages_fetched,
                    records_fetched: outcome.progress.records_fetched,
                    platforms: outcome.cache.platforms.len(),
                    records: outcome.cache.records.len(),
                    confirmed: counts.confirmed,
                    strong: counts.strong,
                    probable: counts.probable,
                    ambiguous: counts.ambiguous,
                    stale: counts.stale,
                    unmatched: counts.unmatched,
                    invalid_hashes: outcome.normalisation.rejected_hashes.len(),
                    unknown_platforms: outcome.normalisation.unknown_platforms.len(),
                    multi_file_groups: groups.len(),
                    elapsed_milliseconds: elapsed,
                    peak_memory_kib: peak_memory_kib(),
                    configured_page_size: outcome.adaptive.configured_page_size,
                    effective_page_size: outcome.adaptive.effective_page_size,
                    smallest_page_size: outcome.adaptive.smallest_page_size,
                    page_size_reductions: outcome.adaptive.reductions,
                    oversized_page_retries: outcome.adaptive.oversized_retries,
                    page_size_recoveries: outcome.adaptive.recoveries,
                    records_without_file_detail: outcome
                        .adaptive
                        .records_without_file_detail
                        .clone(),
                    previous_cache_usable: context.api.open_cache(None).is_ok(),
                    error: None,
                    error_code: None,
                };
                context.emit(&result, || render_import(&result))
            }
            Err(failure) => {
                let result = failed_import(
                    context,
                    "sample",
                    Some(max_records),
                    failure.code(),
                    failure.detail(),
                    elapsed,
                    &paging,
                );
                context.emit(&result, || render_import(&result))?;
                Err(failure.detail().into())
            }
        };
    }

    context.progress("Importing the full RomM catalogue...");
    let outcome = context.api.refresh(
        RefreshRequest {
            source: &source,
            transport: &transport,
            scope,
            capability: &capability,
            hashes: &hashes,
            page_size,
            cancel: Some(&cancel),
        },
        |record| facts_for(record, &trusted),
        |progress| {
            paging.note(progress);
            report_progress(context, progress);
        },
    );
    let elapsed = started.elapsed().as_millis();
    match outcome {
        Ok(summary) => {
            let result = ImportResult {
                mode: if is_refresh { "refresh" } else { "import" },
                sample_limit: None,
                published: true,
                cache_path: Some(summary.cache_path.clone()),
                pages_fetched: summary.progress.pages_fetched,
                records_fetched: summary.progress.records_fetched,
                platforms: summary.platforms,
                records: summary.records,
                confirmed: summary.counts.confirmed,
                strong: summary.counts.strong,
                probable: summary.counts.probable,
                ambiguous: summary.counts.ambiguous,
                stale: summary.counts.stale,
                unmatched: summary.counts.unmatched,
                invalid_hashes: summary.invalid_hashes,
                unknown_platforms: summary.unknown_platforms,
                multi_file_groups: summary.groups.len(),
                elapsed_milliseconds: elapsed,
                peak_memory_kib: peak_memory_kib(),
                configured_page_size: summary.adaptive.configured_page_size,
                effective_page_size: summary.adaptive.effective_page_size,
                smallest_page_size: summary.adaptive.smallest_page_size,
                page_size_reductions: summary.adaptive.reductions,
                oversized_page_retries: summary.adaptive.oversized_retries,
                page_size_recoveries: summary.adaptive.recoveries,
                records_without_file_detail: summary.adaptive.records_without_file_detail.clone(),
                previous_cache_usable: true,
                error: None,
                error_code: None,
            };
            context.emit(&result, || render_import(&result))
        }
        Err(failure) => {
            let result = failed_import(
                context,
                mode,
                None,
                failure.code(),
                failure.detail(),
                elapsed,
                &paging,
            );
            context.emit(&result, || render_import(&result))?;
            Err(failure.detail().into())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn failed_import(
    context: &Context,
    mode: &'static str,
    sample_limit: Option<usize>,
    code: &str,
    detail: String,
    elapsed: u128,
    paging: &ObservedPaging,
) -> ImportResult {
    ImportResult {
        mode,
        sample_limit,
        published: false,
        cache_path: None,
        pages_fetched: 0,
        records_fetched: 0,
        platforms: 0,
        records: 0,
        confirmed: 0,
        strong: 0,
        probable: 0,
        ambiguous: 0,
        stale: 0,
        unmatched: 0,
        invalid_hashes: 0,
        unknown_platforms: 0,
        multi_file_groups: 0,
        elapsed_milliseconds: elapsed,
        peak_memory_kib: peak_memory_kib(),
        configured_page_size: paging.configured,
        // Taken from what the progress stream actually showed, so an import that
        // stepped down and then failed for another reason still says so.
        effective_page_size: paging.smallest.get(),
        smallest_page_size: paging.smallest.get(),
        page_size_reductions: paging.reductions.get(),
        oversized_page_retries: paging.oversized_retries.get(),
        // Not knowable from the progress stream, which reports reductions only.
        page_size_recoveries: 0,
        records_without_file_detail: Vec::new(),
        // Asked, not assumed: the answer is whether the file on disk still loads.
        previous_cache_usable: context.api.open_cache(None).is_ok(),
        error: Some(detail),
        error_code: Some(code.to_string()),
    }
}

fn render_import(result: &ImportResult) -> Vec<String> {
    let mut lines = Vec::new();
    if result.error.is_some() {
        // The failure text itself is left to `main`, which prints every error the
        // same way. What is said here is the part `main` cannot know: whether the
        // identity you already had is still there.
        lines.push(format!(
            "The {} did not complete. Previous cache: {}.",
            result.mode,
            if result.previous_cache_usable {
                "still usable - nothing was replaced"
            } else {
                "none was published, and none existed before"
            }
        ));
        if result.page_size_reductions > 0 {
            lines.push(format!(
                "  Paging had already reduced from {} to {} over {} oversized response(s).",
                result.configured_page_size,
                result.smallest_page_size,
                result.oversized_page_retries
            ));
        }
        return lines;
    }
    lines.push(match result.sample_limit {
        Some(limit) => format!("Sample import of up to {limit} record(s) - not published"),
        None => format!("{} complete", capitalise(result.mode)),
    });
    lines.push(format!(
        "  Pages fetched:     {} ({} record(s))",
        result.pages_fetched, result.records_fetched
    ));
    if result.page_size_reductions > 0 {
        // Three different numbers, and saying only two of them reads as nonsense
        // when the size came back up: "reduced 5 times to 100" is not a reduction.
        lines.push(format!(
            "  Page size:         {} configured, {} reduction(s) down to {}, finished at {}",
            result.configured_page_size,
            result.page_size_reductions,
            result.smallest_page_size,
            result.effective_page_size
        ));
        lines.push(format!(
            "  Oversized pages:   {} response(s) refused for size and retried at the same offset",
            result.oversized_page_retries
        ));
        if result.page_size_recoveries > 0 {
            lines.push(format!(
                "  Recovered:         stepped back up {} time(s) once pages were fitting again",
                result.page_size_recoveries
            ));
        }
    } else {
        lines.push(format!(
            "  Page size:         {} (no reduction needed)",
            result.configured_page_size
        ));
    }
    // Independent of any reduction: a record can lose its file list on the very
    // first attempt if the ladder starts at one record.
    if !result.records_without_file_detail.is_empty() {
        lines.push(format!(
            "  File detail:       {} record(s) imported without their per-file list, which RomM \
             could not send within the size ceiling:",
            result.records_without_file_detail.len()
        ));
        for id in result.records_without_file_detail.iter().take(10) {
            lines.push(format!("                       RomM id {id}"));
        }
        if result.records_without_file_detail.len() > 10 {
            lines.push(format!(
                "                       and {} more, not listed separately",
                result.records_without_file_detail.len() - 10
            ));
        }
    }
    lines.push(format!("  Platforms:         {}", result.platforms));
    lines.push(format!("  Records:           {}", result.records));
    lines.push(format!("  Confirmed:         {}", result.confirmed));
    lines.push(format!("  Strong:            {}", result.strong));
    lines.push(format!("  Probable:          {}", result.probable));
    lines.push(format!("  Ambiguous:         {}", result.ambiguous));
    lines.push(format!("  Stale:             {}", result.stale));
    lines.push(format!("  Unmatched:         {}", result.unmatched));
    lines.push(format!("  Invalid hashes:    {}", result.invalid_hashes));
    lines.push(format!("  Unknown platforms: {}", result.unknown_platforms));
    lines.push(format!("  Multi-file groups: {}", result.multi_file_groups));
    lines.push(format!(
        "  Elapsed:           {} ms",
        result.elapsed_milliseconds
    ));
    if let Some(peak) = result.peak_memory_kib {
        lines.push(format!("  Peak memory:       {peak} KiB"));
    }
    match &result.cache_path {
        Some(path) => lines.push(format!("  Published to:      {}", path.display())),
        None => lines.push(
            "  Nothing was published: a sample is a preview, so your active cache is unchanged."
                .to_string(),
        ),
    }
    lines
}

/// What the progress callbacks revealed about paging.
///
/// An [`ImportFailure`] carries no adaptive state, so an import that stepped down
/// twice and then hit the deadline would otherwise report no reductions at all.
/// The progress stream already says everything needed, so it is recorded as it
/// goes past.
struct ObservedPaging {
    configured: u32,
    smallest: std::cell::Cell<u32>,
    reductions: std::cell::Cell<u32>,
    oversized_retries: std::cell::Cell<u32>,
}

impl ObservedPaging {
    fn new(configured: u32) -> Self {
        Self {
            configured,
            smallest: std::cell::Cell::new(configured),
            reductions: std::cell::Cell::new(0),
            oversized_retries: std::cell::Cell::new(0),
        }
    }

    fn note(&self, progress: ImportProgress) {
        if let Some(reduction) = progress.reduction {
            self.reductions.set(self.reductions.get() + 1);
            self.oversized_retries.set(self.oversized_retries.get() + 1);
            self.smallest.set(self.smallest.get().min(reduction.to));
        }
    }
}

fn report_progress(context: &Context, progress: ImportProgress) {
    // A reduction is its own event, reported once, naming the offset being
    // retried so it is clear no records were passed over.
    if let Some(reduction) = progress.reduction {
        context.progress(&format!(
            "  page response exceeded {} at offset {}; retrying with page size {}",
            human_bytes(reduction.ceiling_bytes),
            reduction.offset,
            reduction.to
        ));
        return;
    }
    let fraction = progress
        .fraction()
        .map(|value| format!(" ({:.0}%)", value * 100.0))
        .unwrap_or_default();
    context.progress(&format!(
        "  page {}: {} record(s){fraction}",
        progress.pages_fetched, progress.records_fetched
    ));
}

/// Local facts for one record: metadata only, and a platform ArchiveFS itself
/// determined where the registry can say so from the path.
fn facts_for(record: &ExternalIdentityRecord, _trusted: &TrustedRoots) -> LocalFileFacts {
    match record.archivefs_path.as_deref() {
        Some(path) => {
            // The platform ArchiveFS would derive from the path alone. Folder
            // evidence is not conclusive, so it is reported as weak - which is
            // what stops it from being treated as a verified local identity.
            let local = archivefs_core::platform::detect::platform_for_folder_name(
                path.parent()
                    .and_then(|parent| parent.file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or(""),
            )
            .map(|platform| platform.id);
            LocalFileFacts::observe(path).with_local_platform(
                local,
                if local.is_some() {
                    LocalEvidenceStrength::Weak
                } else {
                    LocalEvidenceStrength::None
                },
            )
        }
        None => LocalFileFacts::default(),
    }
}

// --- records and conflicts ------------------------------------------------

#[derive(Debug, Serialize)]
struct RecordPage {
    total_in_cache: usize,
    matching_filters: usize,
    offset: usize,
    limit: usize,
    records: Vec<RecordView>,
}

/// One record as the CLI shows it. A projection, not the raw provider payload:
/// nothing oversized and nothing secret.
#[derive(Debug, Serialize)]
struct RecordView {
    romm_game_id: String,
    romm_platform_id: Option<String>,
    romm_file_id: Option<String>,
    romm_path: String,
    archivefs_path: Option<PathBuf>,
    canonical_platform: Option<String>,
    romm_platform_name: Option<String>,
    title: Option<String>,
    regions: Vec<String>,
    revision: Option<String>,
    file_size_bytes: Option<u64>,
    hashes: Vec<HashView>,
    verification: ExternalVerification,
    stale: bool,
    conflicts: Vec<ConflictView>,
    related_files: Vec<String>,
    sibling_game_ids: Vec<String>,
    metadata_provider_ids: Vec<MetadataIdView>,
    artwork_reference: Option<String>,
    imported_at_unix_seconds: i64,
    romm_updated_at: Option<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize)]
struct HashView {
    algorithm: String,
    value: String,
}

#[derive(Debug, Serialize)]
struct MetadataIdView {
    provider: String,
    id: String,
}

#[derive(Debug, Serialize)]
struct ConflictView {
    field: String,
    romm: String,
    local: String,
    detail: String,
}

impl RecordView {
    fn of(record: &ExternalIdentityRecord) -> Self {
        Self {
            romm_game_id: record.provider_game_id.clone(),
            romm_platform_id: record.provider_platform_id.clone(),
            romm_file_id: record.provider_file_id.clone(),
            romm_path: record.provider_path.clone(),
            archivefs_path: record.archivefs_path.clone(),
            canonical_platform: record.platform_candidate.clone(),
            romm_platform_name: record.provider_platform_name.clone(),
            title: record.title.clone(),
            regions: record.regions.clone(),
            revision: record.revision.clone(),
            file_size_bytes: record.file_size_bytes,
            hashes: record
                .hashes
                .iter()
                .map(|hash| HashView {
                    algorithm: hash.algorithm.label().to_string(),
                    value: hash.value.clone(),
                })
                .collect(),
            verification: record.verification,
            stale: record.verification == ExternalVerification::Stale,
            conflicts: record
                .conflicts
                .iter()
                .map(|conflict| ConflictView {
                    field: conflict.field.label().to_string(),
                    romm: conflict.external.clone(),
                    local: conflict.local.clone(),
                    detail: conflict.detail.clone(),
                })
                .collect(),
            related_files: record.related_files.clone(),
            sibling_game_ids: record.sibling_game_ids.clone(),
            metadata_provider_ids: record
                .metadata_provider_ids
                .iter()
                .map(|entry| MetadataIdView {
                    provider: entry.provider.clone(),
                    id: entry.id.clone(),
                })
                .collect(),
            artwork_reference: record
                .artwork
                .as_ref()
                .map(|artwork| artwork.reference.clone()),
            imported_at_unix_seconds: record.imported_at_unix_seconds,
            romm_updated_at: record.provider_updated_at.clone(),
            evidence: record.evidence.clone(),
        }
    }
}

fn open_cache_for_reading(context: &Context) -> Result<IdentityCache, Box<dyn std::error::Error>> {
    context.api.open_cache(None).map_err(|refusal| {
        let hint = match refusal {
            CacheRefusal::Missing => " Run `identity source romm import` to build one.",
            _ => "",
        };
        format!("{}{hint}", refusal.detail()).into()
    })
}

fn records(context: &Context, mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let status_filter = take_string(&mut args, "--status")?;
    let platform_filter = take_string(&mut args, "--platform")?;
    let limit = take_number(&mut args, "--limit")?
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let offset = take_number(&mut args, "--offset")?.unwrap_or(0);
    reject_extra(&args, "records")?;

    let wanted = status_filter
        .as_deref()
        .map(parse_verification)
        .transpose()?;
    let cache = open_cache_for_reading(context)?;
    let filtered: Vec<&ExternalIdentityRecord> = cache
        .records
        .iter()
        .filter(|record| wanted.is_none_or(|wanted| record.verification == wanted))
        .filter(|record| {
            platform_filter
                .as_deref()
                .is_none_or(|platform| record.platform_candidate.as_deref() == Some(platform))
        })
        .collect();

    let start = offset.min(filtered.len());
    let end = start.saturating_add(limit).min(filtered.len());
    let page = RecordPage {
        total_in_cache: cache.records.len(),
        matching_filters: filtered.len(),
        offset: start,
        limit,
        records: filtered[start..end]
            .iter()
            .map(|record| RecordView::of(record))
            .collect(),
    };
    context.emit(&page, || {
        let mut lines = vec![format!(
            "{} of {} record(s) match; showing {}-{}",
            page.matching_filters,
            page.total_in_cache,
            page.offset,
            page.offset + page.records.len()
        )];
        for record in &page.records {
            lines.push(String::new());
            lines.extend(render_record_summary(record));
        }
        lines
    })
}

fn record(context: &Context, mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let id = args
        .first()
        .cloned()
        .ok_or("record requires a RomM ROM id")?;
    args.remove(0);
    reject_extra(&args, "record")?;
    let cache = open_cache_for_reading(context)?;
    let found = cache
        .records
        .iter()
        .find(|record| record.provider_game_id == id)
        .ok_or_else(|| format!("no cached record has RomM id {id}"))?;
    let view = RecordView::of(found);
    context.emit(&view, || render_record_detail(&view))
}

fn conflicts(context: &Context, mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let limit = take_number(&mut args, "--limit")?
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let offset = take_number(&mut args, "--offset")?.unwrap_or(0);
    reject_extra(&args, "conflicts")?;
    let cache = open_cache_for_reading(context)?;
    let all = cache.conflicts();
    let start = offset.min(all.len());
    let end = start.saturating_add(limit).min(all.len());
    let page = RecordPage {
        total_in_cache: cache.records.len(),
        matching_filters: all.len(),
        offset: start,
        limit,
        records: all[start..end]
            .iter()
            .map(|record| RecordView::of(record))
            .collect(),
    };
    // Exits 0 whether or not conflicts exist, matching `doctor --findings`:
    // reporting a fact the command was asked to look for is a success.
    context.emit(&page, || {
        if page.matching_filters == 0 {
            return vec!["No conflicts between RomM and local evidence.".to_string()];
        }
        let mut lines = vec![format!(
            "{} record(s) conflict with local evidence; showing {}-{}",
            page.matching_filters,
            page.offset,
            page.offset + page.records.len()
        )];
        for record in &page.records {
            lines.push(String::new());
            lines.extend(render_record_summary(record));
            for conflict in &record.conflicts {
                lines.push(format!("    {} disagreement:", conflict.field));
                lines.push(format!("      RomM:  {}", conflict.romm));
                lines.push(format!("      Local: {}", conflict.local));
                lines.push(format!("      {}", conflict.detail));
            }
            if record
                .evidence
                .iter()
                .any(|item| item.contains("not displaced"))
            {
                lines.push(
                    "    ArchiveFS's own identity is stronger and was not replaced.".to_string(),
                );
            }
        }
        lines
    })
}

fn render_record_summary(record: &RecordView) -> Vec<String> {
    let mut lines = vec![format!(
        "  [{}] {} - {}",
        verification_slug(record.verification),
        record.romm_game_id,
        record.title.as_deref().unwrap_or("(untitled)")
    )];
    lines.push(format!(
        "    Platform: {}{}",
        record.canonical_platform.as_deref().unwrap_or("(unmapped)"),
        record
            .romm_platform_name
            .as_deref()
            .map(|name| format!(" (RomM: {name})"))
            .unwrap_or_default()
    ));
    lines.push(format!("    RomM path: {}", record.romm_path));
    lines.push(format!(
        "    Local path: {}",
        record
            .archivefs_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(no mapping)".to_string())
    ));
    if !record.hashes.is_empty() {
        lines.push(format!(
            "    Hashes: {}",
            record
                .hashes
                .iter()
                .map(|hash| format!("{}={}", hash.algorithm, hash.value))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !record.related_files.is_empty() {
        lines.push(format!(
            "    Multi-file: {} file(s), {} sibling(s)",
            record.related_files.len(),
            record.sibling_game_ids.len()
        ));
    }
    lines
}

fn render_record_detail(record: &RecordView) -> Vec<String> {
    let mut lines = render_record_summary(record);
    if let Some(size) = record.file_size_bytes {
        lines.push(format!("    Size: {size} bytes"));
    }
    if !record.regions.is_empty() {
        lines.push(format!("    Regions: {}", record.regions.join(", ")));
    }
    if let Some(revision) = &record.revision {
        lines.push(format!("    Revision: {revision}"));
    }
    if !record.metadata_provider_ids.is_empty() {
        lines.push(format!(
            "    Metadata ids: {}",
            record
                .metadata_provider_ids
                .iter()
                .map(|entry| format!("{}={}", entry.provider, entry.id))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if let Some(artwork) = &record.artwork_reference {
        lines.push(format!("    Artwork reference: {artwork}"));
    }
    for file in &record.related_files {
        lines.push(format!("    File: {file}"));
    }
    lines.push(format!(
        "    Imported: unix {}{}",
        record.imported_at_unix_seconds,
        record
            .romm_updated_at
            .as_deref()
            .map(|updated| format!(", RomM updated {updated}"))
            .unwrap_or_default()
    ));
    for conflict in &record.conflicts {
        lines.push(format!(
            "    Conflict ({}): RomM {} vs local {} - {}",
            conflict.field, conflict.romm, conflict.local, conflict.detail
        ));
    }
    for item in &record.evidence {
        lines.push(format!("    Evidence: {item}"));
    }
    lines
}

// --- verify-hash ----------------------------------------------------------

#[derive(Debug, Serialize)]
struct HashVerification {
    path: PathBuf,
    bytes_hashed: u64,
    crc32: String,
    md5: String,
    sha1: String,
    /// The record claiming this path, when the cache has one.
    romm_game_id: Option<String>,
    /// Per-algorithm comparison against what RomM published.
    comparisons: Vec<HashComparison>,
    /// `Some(true)` only when every hash RomM published agrees. `None` when RomM
    /// published none, which is not the same as agreement.
    all_agree: Option<bool>,
    /// Whether at least one published hash disagrees. Kept separate from
    /// `all_agree` because "some agree and some do not" is its own answer: it
    /// means RomM's own metadata is inconsistent, not that the file is wrong.
    any_disagree: Option<bool>,
    verification_after: Option<ExternalVerification>,
    file_modified: bool,
}

#[derive(Debug, Serialize)]
struct HashComparison {
    algorithm: String,
    romm: String,
    local: String,
    agrees: bool,
}

fn verify_hash(context: &Context, mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let path = take_path(&mut args, "--path")?.ok_or("verify-hash requires --path <local-path>")?;
    reject_extra(&args, "verify-hash")?;

    let roots = context.source_roots.clone();
    if roots.is_empty() {
        return Err("no source folders are configured, so no path can be verified safely".into());
    }
    // `TrustedRoots` governs where a *symlink target* may lead; it does not
    // constrain the path given on the command line. So the containment check is
    // made here, before anything is opened - otherwise `--path` would be a
    // read-anything primitive, and a credential file would be a legitimate
    // target for it.
    let requested = path.clone();
    let path = confine_to_roots(&path, &roots)?;
    let trusted = TrustedRoots::from_paths(&roots);
    let cancel = AtomicBool::new(false);
    context.progress(&format!("Hashing {}...", path.display()));
    // The one place a hash is computed, and only because it was asked for.
    let hashes: LocalHashes =
        hash_file(&path, &trusted, Some(&cancel)).map_err(|refusal| refusal.detail())?;

    // Compare against the cache when there is one; a missing cache is fine, the
    // hashes are still reported.
    let cache = context.api.open_cache(None).ok();
    // Looked up by the path as typed first, then by its resolved form: a mapping
    // stores whichever spelling the configuration produced, and both name the
    // same file.
    let record = cache.as_ref().and_then(|cache| {
        cache
            .record_for_path(&requested)
            .or_else(|| cache.record_for_path(&path))
    });
    let comparisons: Vec<HashComparison> = record
        .map(|record| {
            record
                .hashes
                .iter()
                .map(|published| HashComparison {
                    algorithm: published.algorithm.label().to_string(),
                    romm: published.value.clone(),
                    local: hashes.value(published.algorithm).to_string(),
                    agrees: hashes.agrees_with(published),
                })
                .collect()
        })
        .unwrap_or_default();

    // Re-match with the freshly computed hash, so the reported verdict is what a
    // refresh would now conclude.
    let verification_after = record.map(|record| {
        let mut local_cache = LocalHashCache::new();
        local_cache.insert(hashes.clone());
        let claims = cache
            .as_ref()
            .map(|cache| PathClaims::of(&cache.records))
            .unwrap_or_default();
        archivefs_core::identity_source::matching::match_record(
            record,
            &facts_for(record, &trusted),
            &claims,
            &local_cache,
        )
        .verification
    });

    let result = HashVerification {
        path: path.clone(),
        bytes_hashed: hashes.bytes_hashed,
        crc32: hashes.crc32.clone(),
        md5: hashes.md5.clone(),
        sha1: hashes.sha1.clone(),
        romm_game_id: record.map(|record| record.provider_game_id.clone()),
        all_agree: (!comparisons.is_empty())
            .then(|| comparisons.iter().all(|comparison| comparison.agrees)),
        any_disagree: (!comparisons.is_empty())
            .then(|| comparisons.iter().any(|comparison| !comparison.agrees)),
        verification_after,
        // Hashing opens the file read-only through the shared policy.
        file_modified: false,
        comparisons,
    };
    context.emit(&result, || {
        let mut lines = vec![
            format!(
                "Hashed {} ({} bytes)",
                result.path.display(),
                result.bytes_hashed
            ),
            format!("  CRC32: {}", result.crc32),
            format!("  MD5:   {}", result.md5),
            format!("  SHA-1: {}", result.sha1),
        ];
        match &result.romm_game_id {
            Some(id) => {
                lines.push(format!("  RomM record: {id}"));
                if result.comparisons.is_empty() {
                    lines.push("  RomM published no hash to compare against.".to_string());
                }
                for comparison in &result.comparisons {
                    lines.push(format!(
                        "    {}: {}",
                        comparison.algorithm,
                        if comparison.agrees {
                            "matches"
                        } else {
                            "does NOT match"
                        }
                    ));
                    if !comparison.agrees {
                        lines.push(format!("      RomM:  {}", comparison.romm));
                        lines.push(format!("      Local: {}", comparison.local));
                    }
                }
                if result.all_agree == Some(false) && result.any_disagree == Some(true) {
                    let agreeing = result
                        .comparisons
                        .iter()
                        .filter(|comparison| comparison.agrees)
                        .count();
                    if agreeing > 0 {
                        lines.push(format!(
                            "  {agreeing} of {} published hashes agree, so RomM's own metadata is \
                             inconsistent for this file rather than the file being a different \
                             dump.",
                            result.comparisons.len()
                        ));
                    } else {
                        lines.push(
                            "  No published hash agrees: this local file is a different dump from \
                             the one RomM describes."
                                .to_string(),
                        );
                    }
                }
                if let Some(verification) = result.verification_after {
                    lines.push(format!(
                        "  Verdict with this hash: {}",
                        verification.label()
                    ));
                }
            }
            None => lines.push(
                "  No cached RomM record claims this path, so there was nothing to compare."
                    .to_string(),
            ),
        }
        lines.push("The file was opened read-only and not changed.".to_string());
        lines
    })
}

// --- lifecycle ------------------------------------------------------------

#[derive(Debug, Serialize)]
struct EnabledResult {
    enabled: bool,
    config_path: PathBuf,
    /// Stated because enabling deliberately does not connect.
    connected: bool,
    cache_available: bool,
}

fn set_enabled(context: &Context, enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut settings = context.load_settings()?;
    if settings.source.url.trim().is_empty() {
        return Err("configure a URL before enabling this source".into());
    }
    settings.source.enabled = enabled;
    let config_path = context
        .settings
        .save(&settings)
        .map_err(|error| error.detail())?;
    let result = EnabledResult {
        enabled,
        config_path,
        connected: false,
        cache_available: context.api.open_cache(None).is_ok(),
    };
    context.emit(&result, || {
        let mut lines = vec![format!(
            "RomM identity source {}",
            if result.enabled {
                "enabled"
            } else {
                "disabled"
            }
        )];
        if result.enabled {
            lines.push(
                "Nothing was contacted. Run `identity source romm test` or `... import` when \
                 you are ready."
                    .to_string(),
            );
        } else {
            lines.push(
                "Configuration and cached identity are kept; no refresh will run while disabled."
                    .to_string(),
            );
        }
        lines.push(format!(
            "  Cached identity: {}",
            if result.cache_available {
                "available"
            } else {
                "none"
            }
        ));
        lines
    })
}

#[derive(Debug, Serialize)]
struct RemovalResult {
    cache_removed: bool,
    config_removed: bool,
    /// Never removed by this command.
    token_file_kept: Option<PathBuf>,
    romm_modified: bool,
    roms_modified: bool,
}

fn remove(context: &Context, mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let confirmed = take_flag(&mut args, "--confirm");
    let keep_config = take_flag(&mut args, "--keep-config");
    reject_extra(&args, "remove")?;
    if !confirmed {
        return Err("removing cached RomM identity needs --confirm; nothing was removed".into());
    }
    let settings = context.load_settings()?;
    let cache_removed = context
        .api
        .remove_cached_identity(true)
        .map_err(|refusal| refusal.detail())?;
    let config_removed = if keep_config {
        false
    } else {
        context.settings.remove()?
    };
    let result = RemovalResult {
        cache_removed,
        config_removed,
        // The token belongs to the person, not to ArchiveFS.
        token_file_kept: settings.source.token_path.clone(),
        romm_modified: false,
        roms_modified: false,
    };
    context.emit(&result, || {
        let mut lines = vec![format!(
            "Removed cached RomM identity{}",
            if result.config_removed {
                " and its configuration"
            } else {
                ""
            }
        )];
        if !result.cache_removed {
            lines.push("  There was no cache to remove.".to_string());
        }
        if let Some(path) = &result.token_file_kept {
            lines.push(format!(
                "  Your token file was left alone: {}",
                path.display()
            ));
        }
        lines.push("  Nothing in RomM and no ROM file was changed.".to_string());
        lines
    })
}

// --- helpers --------------------------------------------------------------

/// Refuses any path that is not a real file inside a configured source folder.
///
/// Both the path as given and the path after resolution must be inside a root:
/// the first stops an unrelated path being named outright, the second stops a
/// symlink that sits inside the library from reaching out of it.
fn confine_to_roots(
    path: &std::path::Path,
    roots: &[PathBuf],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if !path.is_absolute() {
        return Err(format!(
            "{} is not an absolute path; name the file in full so there is no doubt which one is \
             meant",
            path.display()
        )
        .into());
    }
    // Canonical roots, so the comparison is not defeated by a symlinked source
    // folder - a configured root that cannot be resolved is dropped rather than
    // trusted.
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    let inside = |candidate: &std::path::Path| {
        canonical_roots
            .iter()
            .any(|root| candidate.starts_with(root))
    };

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("{} cannot be examined: {error}", path.display()))?;
    // Checked before resolution, so the answer describes the path that was typed.
    let lexical = path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .map(|parent| parent.join(path.file_name().unwrap_or_default()));
    if !lexical.as_deref().is_some_and(&inside) {
        return Err(format!(
            "{} is not inside a configured source folder, so ArchiveFS will not read it",
            path.display()
        )
        .into());
    }

    let resolved = path.canonicalize().map_err(|error| {
        if metadata.file_type().is_symlink() {
            format!(
                "{} is a symlink whose target cannot be resolved: {error}",
                path.display()
            )
        } else {
            format!("{} cannot be resolved: {error}", path.display())
        }
    })?;
    if !inside(&resolved) {
        return Err(format!(
            "{} leads out of your configured source folders, to {}; ArchiveFS will not follow it",
            path.display(),
            resolved.display()
        )
        .into());
    }
    if !resolved.is_file() {
        return Err(format!("{} is not a regular file", path.display()).into());
    }
    Ok(resolved)
}

/// Bytes as a person would say them, for a progress line. Exact when the value
/// is a whole number of MiB or KiB, which every ceiling in this crate is.
fn human_bytes(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    const KIB: usize = 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes.is_multiple_of(KIB) {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn verification_slug(verification: ExternalVerification) -> &'static str {
    match verification {
        ExternalVerification::ConfirmedExternal => "confirmed",
        ExternalVerification::StrongExternal => "strong",
        ExternalVerification::ProbableExternal => "probable",
        ExternalVerification::Ambiguous => "ambiguous",
        ExternalVerification::Stale => "stale",
        ExternalVerification::Unmatched => "unmatched",
    }
}

/// Parses a `--status` filter. Accepts the slugs the output prints, so a person
/// can copy one straight back into a filter.
pub fn parse_verification(text: &str) -> Result<ExternalVerification, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "confirmed" | "confirmedexternal" | "confirmed_external" => {
            Ok(ExternalVerification::ConfirmedExternal)
        }
        "strong" | "strongexternal" | "strong_external" => Ok(ExternalVerification::StrongExternal),
        "probable" | "probableexternal" | "probable_external" => {
            Ok(ExternalVerification::ProbableExternal)
        }
        "ambiguous" => Ok(ExternalVerification::Ambiguous),
        "stale" => Ok(ExternalVerification::Stale),
        "unmatched" => Ok(ExternalVerification::Unmatched),
        other => Err(format!(
            "unknown --status {other:?}; one of: confirmed, strong, probable, ambiguous, stale, \
             unmatched"
        )),
    }
}

/// Peak resident memory, on platforms that report it. `None` elsewhere rather
/// than a guess.
fn peak_memory_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                return rest
                    .trim()
                    .strip_suffix(" kB")
                    .and_then(|value| value.trim().parse().ok());
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let had = args.iter().any(|arg| arg == flag);
    args.retain(|arg| arg != flag);
    had
}

fn take_string(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return Ok(None);
    };
    if args.iter().filter(|arg| *arg == flag).count() > 1 {
        return Err(format!("{flag} was given more than once").into());
    }
    if index + 1 >= args.len() {
        return Err(format!("{flag} needs a value").into());
    }
    let value = args.remove(index + 1);
    args.remove(index);
    if value.starts_with("--") {
        return Err(format!("{flag} needs a value, not another flag").into());
    }
    Ok(Some(value))
}

fn take_path(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    Ok(take_string(args, flag)?.map(PathBuf::from))
}

fn take_number(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<usize>, Box<dyn std::error::Error>> {
    let Some(text) = take_string(args, flag)? else {
        return Ok(None);
    };
    let value: usize = text
        .parse()
        .map_err(|_| format!("{flag} needs a whole number, not {text:?}"))?;
    Ok(Some(value))
}

fn reject_extra(args: &[String], command: &str) -> Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Ok(());
    }
    Err(format!("identity source romm {command} does not accept {args:?}").into())
}

/// What one captured command run produced.
#[cfg(test)]
pub struct CapturedRun {
    pub error: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(test)]
impl CapturedRun {
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }

    /// The error text, or a panic naming what was produced instead - so a test
    /// that expected a refusal and got success says so.
    pub fn error_text(&self) -> &str {
        self.error.as_deref().unwrap_or_else(|| {
            panic!(
                "expected a refusal, but the command succeeded with:\n{}",
                self.stdout
            )
        })
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout was not one JSON document ({error}):\n{}",
                self.stdout
            )
        })
    }
}

/// Drives one command with its output captured, against whatever identity root
/// the caller passes in `args`.
#[cfg(test)]
pub fn run_captured(args: &[&str], source_roots: &[&std::path::Path]) -> CapturedRun {
    let output = Output::Captured {
        out: std::cell::RefCell::new(String::new()),
        err: std::cell::RefCell::new(String::new()),
    };
    let result = dispatch(
        args.iter().map(|arg| (*arg).to_string()).collect(),
        &output,
        Some(source_roots.iter().map(PathBuf::from).collect()),
    );
    let Output::Captured { out, err } = &output else {
        unreachable!("the sink was built as Captured just above")
    };
    CapturedRun {
        error: result.err().map(|error| error.to_string()),
        stdout: out.borrow().clone(),
        stderr: err.borrow().clone(),
    }
}

#[cfg(test)]
mod tests;

// --- stale-summary --------------------------------------------------------

/// Explains the stale population rather than just counting it.
///
/// Reads the published cache and probes each translated path's metadata. Makes no
/// network request, hashes nothing, and writes nothing.
fn stale_summary(
    context: &Context,
    mut args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let examples = take_number(&mut args, "--examples")?.unwrap_or(DEFAULT_EXAMPLES);
    reject_extra(&args, "stale-summary")?;
    let settings = context.load_settings()?;
    let cache = open_cache_for_reading(context)?;

    // The mappings as configured, so each group can name the rule that produced it.
    let mappings: Vec<(String, String)> = settings
        .source
        .mappings
        .iter()
        .map(|mapping| {
            (
                mapping.provider_prefix.clone(),
                mapping.archivefs_prefix.display().to_string(),
            )
        })
        .collect();

    context.progress("Checking each stale record's path (metadata only)...");
    let summary = StaleSummary::build(
        &cache,
        &mappings,
        examples,
        archivefs_core::identity_source::matching::LocalPresence::observe,
    );

    context.emit(&summary, || {
        let share = |count: usize| -> String {
            if summary.stale == 0 {
                "0%".to_string()
            } else {
                format!("{:.1}%", count as f64 * 100.0 / summary.stale as f64)
            }
        };
        let mut lines = vec![
            format!(
                "{} of {} cached record(s) are stale",
                summary.stale, summary.total_in_cache
            ),
            String::new(),
            "Why each one could not be matched to a file".to_string(),
        ];
        for reason in &summary.by_reason {
            lines.push(format!(
                "  {:6} ({:>5})  {}",
                reason.count,
                share(reason.count),
                reason.label
            ));
            lines.push(format!(
                "                     RomM itself calls {} of these missing",
                reason.romm_reports_missing
            ));
            for example in &reason.examples {
                lines.push(format!("                     e.g. {}", example.romm_path));
            }
        }

        lines.push(String::new());
        lines.push("What that adds up to".to_string());
        lines.push(format!(
            "  {} ({}) are records RomM already reports as missing on its own filesystem",
            summary.romm_reports_missing,
            share(summary.romm_reports_missing)
        ));
        lines.push(format!(
            "  {} ({}) are symlinks whose target has gone",
            summary.dangling_symlinks,
            share(summary.dangling_symlinks)
        ));
        lines.push(format!(
            "  {} ({}) are present as directories - folder-based games, not missing files",
            summary.present_as_directory,
            share(summary.present_as_directory)
        ));
        lines.push(format!(
            "  {} have no mapping to a local path",
            summary.unmapped
        ));
        lines.push(format!(
            "  {} are genuinely multi-file (RomM lists more than one file)",
            summary.multi_file
        ));
        lines.push(String::new());
        lines.push(if summary.looks_like_drift() {
            "This looks like ordinary library drift: almost all of it is either RomM's own \
             record of a missing file, or a link whose target has gone. Neither points at a \
             mapping or matching fault."
                .to_string()
        } else {
            "Enough of this is unexplained that it is worth checking the mappings: a large share \
             is neither flagged missing by RomM nor a broken link."
                .to_string()
        });

        let section = |lines: &mut Vec<String>,
                       title: &str,
                       groups: &[archivefs_core::identity_source::stale::StaleGroup],
                       omitted: usize| {
            lines.push(String::new());
            lines.push(title.to_string());
            for group in groups {
                lines.push(format!(
                    "  {:6}  {}  ({} flagged missing by RomM)",
                    group.count, group.key, group.romm_reports_missing
                ));
            }
            if omitted > 0 {
                lines.push(format!("  and {omitted} more, not listed separately"));
            }
        };
        section(
            &mut lines,
            "By platform",
            &summary.by_platform,
            summary.platforms_not_listed,
        );
        section(
            &mut lines,
            "By RomM path prefix",
            &summary.by_romm_prefix,
            summary.romm_prefixes_not_listed,
        );
        section(
            &mut lines,
            "By local folder",
            &summary.by_local_prefix,
            summary.local_prefixes_not_listed,
        );
        section(
            &mut lines,
            "By file extension",
            &summary.by_extension,
            summary.extensions_not_listed,
        );
        section(&mut lines, "By mapping used", &summary.by_mapping, 0);
        lines.push(String::new());
        lines.push(
            "Nothing was changed: no file was read, no hash computed, and RomM was not contacted."
                .to_string(),
        );
        lines
    })
}

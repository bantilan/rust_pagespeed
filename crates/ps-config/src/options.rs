//! Rewrite options: values, parsing, scopes, merge, and query adjust.
//!
//! Mirrors `RewriteOptions` (`mod_pagespeed/net/instaweb/rewriter/`):
//! scalar/composite value parsing (`ParseFromString` family), the
//! special/2-arg/3-arg directive dispatch
//! (`ParseAndSetOptionFromName{,2,3}`), `Merge`, and the query-param path
//! (`RewriteQuery::ScanNameValue` for `PageSpeed=` / `PageSpeedFilters=` /
//! `Cache-Control: no-transform` / `PageSpeed<option>` suffixes).
//!
//! Out of scope for foundations (documented follow-ups): the full
//! `DomainLawyer` (mappings are stored verbatim here), `ExperimentSpec`
//! validation (specs stored raw), `X-PSA-Client-Options` handling
//! (filters-6), and `Signature()`/`OptionSignature` computation.
#![forbid(unsafe_code)]

use crate::filter::{
    Filter, FilterState, RewriteLevel, ADD_HEAD_FILTERS, REQUIRES_SCRIPT_EXECUTION,
};
use crate::property::{
    lookup_by_id, lookup_by_name, nginx_option_scope, strip_mod_pagespeed_prefix, PropDefault,
    PropKind, Property, PROPERTIES,
};
use crate::cookies::{CookiePatternSet, CookiePolicy};
use crate::scope::Scope;
use crate::wildcard::WildcardGroup;
use std::collections::{HashMap, HashSet};

/// Master enabled state (`EnabledEnum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EnabledState {
    /// Deprecated; Apache `standby` / nginx `unplugged` equivalent.
    Off,
    /// Runs normally; query params may override.
    #[default]
    On,
    /// Fully passive; serves no `.pagespeed.` resources.
    Unplugged,
    /// Serves `.pagespeed.` but does not optimize HTML.
    Standby,
}

impl EnabledState {
    /// Parses `ParseFromString(value, EnabledEnum*)`: bool words first, then
    /// `unplugged` / `standby` (all ASCII case-insensitive).
    #[must_use]
    pub fn parse(text: &str) -> Option<EnabledState> {
        if let Some(b) = parse_bool(text) {
            return Some(if b {
                EnabledState::On
            } else {
                EnabledState::Off
            });
        }
        if text.eq_ignore_ascii_case("unplugged") {
            Some(EnabledState::Unplugged)
        } else if text.eq_ignore_ascii_case("standby") {
            Some(EnabledState::Standby)
        } else {
            None
        }
    }

    /// Canonical spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            EnabledState::Off => "Off",
            EnabledState::On => "On",
            EnabledState::Unplugged => "Unplugged",
            EnabledState::Standby => "Standby",
        }
    }
}

/// Resource category (`semantic_type::Category`, without `kUndefined`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Script,
    Image,
    Stylesheet,
    OtherResource,
    Hyperlink,
    Prefetch,
}

impl Category {
    /// Parses (`semantic_type::ParseCategory`, ASCII case-insensitive).
    #[must_use]
    pub fn parse(text: &str) -> Option<Category> {
        if text.eq_ignore_ascii_case("Script") {
            Some(Category::Script)
        } else if text.eq_ignore_ascii_case("Image") {
            Some(Category::Image)
        } else if text.eq_ignore_ascii_case("Stylesheet") {
            Some(Category::Stylesheet)
        } else if text.eq_ignore_ascii_case("OtherResource") {
            Some(Category::OtherResource)
        } else if text.eq_ignore_ascii_case("Hyperlink") {
            Some(Category::Hyperlink)
        } else if text.eq_ignore_ascii_case("Prefetch") {
            Some(Category::Prefetch)
        } else {
            None
        }
    }

    /// Canonical spelling (`GetCategoryString`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Category::Script => "Script",
            Category::Image => "Image",
            Category::Stylesheet => "Stylesheet",
            Category::OtherResource => "OtherResource",
            Category::Hyperlink => "Hyperlink",
            Category::Prefetch => "Prefetch",
        }
    }
}

/// `AllowVaryOn` value: auto mode plus per-signal flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllowVaryOn {
    /// `Auto` mode (all signals decided per-request).
    pub auto: bool,
    /// `Accept` allowed.
    pub accept: bool,
    /// `Save-Data` allowed (implied by auto).
    pub save_data: bool,
    /// `User-Agent` allowed.
    pub user_agent: bool,
}

impl AllowVaryOn {
    /// The `Auto` default (`ParseFromString(kAutoString, ...)`).
    #[must_use]
    pub const fn auto() -> AllowVaryOn {
        AllowVaryOn {
            auto: true,
            accept: false,
            save_data: false,
            user_agent: false,
        }
    }

    /// Parses (`ParseFromString(value, AllowVaryOn*)`).
    ///
    /// `None` selects nothing, `Auto` selects auto mode, otherwise a comma
    /// list (empties kept, so `""` is invalid) of `Accept`, `Save-Data`,
    /// `User-Agent` (case-insensitive, whitespace tolerated).
    #[must_use]
    pub fn parse(text: &str) -> Option<AllowVaryOn> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("None") {
            return Some(AllowVaryOn::default());
        }
        if text.eq_ignore_ascii_case("Auto") {
            return Some(AllowVaryOn::auto());
        }
        let mut out = AllowVaryOn::default();
        for part in text.split(',') {
            let part = part.trim();
            if part.eq_ignore_ascii_case("Accept") {
                out.accept = true;
            } else if part.eq_ignore_ascii_case("Save-Data") {
                out.save_data = true;
            } else if part.eq_ignore_ascii_case("User-Agent") {
                out.user_agent = true;
            } else {
                return None;
            }
        }
        Some(out)
    }
}

/// `BeaconUrl` value: beacon endpoints plus query-stripped variants.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BeaconUrl {
    /// HTTP endpoint (may carry a query).
    pub http: String,
    /// HTTPS endpoint (may carry a query).
    pub https: String,
    /// HTTP endpoint with query stripped.
    pub http_in: String,
    /// HTTPS endpoint with query stripped.
    pub https_in: String,
}

impl BeaconUrl {
    /// Parses (`ParseBeaconUrl`): one or two space-separated URLs; with one
    /// URL starting with `http:`, the https twin is derived by scheme swap,
    /// otherwise both are the same URL. A trailing `ets=` query is stripped
    /// before deriving the `*_in` variants.
    #[must_use]
    pub fn parse(text: &str) -> Option<BeaconUrl> {
        let urls: Vec<&str> = text.split(' ').filter(|t| !t.is_empty()).collect();
        if urls.is_empty() || urls.len() > 2 {
            return None;
        }
        let http = urls[0].to_string();
        let https = if urls.len() == 2 {
            urls[1].to_string()
        } else if let Some(rest) = urls[0].strip_prefix("http:") {
            format!("https:{rest}")
        } else {
            http.clone()
        };
        let http_in = strip_beacon_query(&http);
        let https_in = strip_beacon_query(&https);
        Some(BeaconUrl {
            http,
            https,
            http_in,
            https_in,
        })
    }
}

/// Strips a trailing `ets=` query (plus its `?`/`&`) and then any remaining
/// query (`StripBeaconUrlQueryParam`).
fn strip_beacon_query(url: &str) -> String {
    let mut base = url;
    if let Some(no_ets) = base.strip_suffix("ets=") {
        base = no_ets.strip_suffix(['?', '&']).unwrap_or(no_ets);
    }
    match base.find('?') {
        Some(pos) => base[..pos].to_string(),
        None => base.to_string(),
    }
}

/// Parses bools (`ParseFromString(value, bool*)`): `true/on/false/off`.
#[must_use]
pub fn parse_bool(text: &str) -> Option<bool> {
    if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("on") {
        Some(true)
    } else if text.eq_ignore_ascii_case("false") || text.eq_ignore_ascii_case("off") {
        Some(false)
    } else {
        None
    }
}

/// Parses a full-string 64-bit integer (no surrounding whitespace, as with
/// `StringToInt64`).
#[must_use]
pub fn parse_int64(text: &str) -> Option<i64> {
    if text.is_empty() {
        return None;
    }
    // Reject whitespace and partial parses: the whole token must be numeric.
    if !text
        .bytes()
        .all(|b| b.is_ascii_digit() || b == b'+' || b == b'-')
    {
        return None;
    }
    text.parse::<i64>().ok()
}

/// Parses a full-string 32-bit integer (no surrounding whitespace, as with
/// `StringToInt`).
#[must_use]
pub fn parse_int32(text: &str) -> Option<i32> {
    if text.is_empty() {
        return None;
    }
    // Reject whitespace and partial parses: the whole token must be numeric.
    if !text
        .bytes()
        .all(|b| b.is_ascii_digit() || b == b'+' || b == b'-')
    {
        return None;
    }
    text.parse::<i32>().ok()
}

/// Parses comma-separated positive doubles, sorted ascending
/// (responsive-densities grammar; empty input invalid).
#[must_use]
pub fn parse_densities(text: &str) -> Option<Vec<f64>> {
    if text.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for part in text.split(',') {
        if part.is_empty() {
            continue; // C++ split ignores empty strings.
        }
        let value: f64 = part.trim().parse().ok()?;
        // Strictly positive; NaN compares greater than nothing, so it is
        // rejected too.
        if value.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            return None;
        }
        out.push(value);
    }
    if out.is_empty() {
        return None;
    }
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(out)
}

/// Parses `InlineResourcesWithoutExplicitAuthorization` values
/// (`ParseInlineUnauthorizedResourceType`): comma list of categories, where
/// `off` clears the set.
#[must_use]
pub fn parse_resource_categories(text: &str) -> Option<HashSet<Category>> {
    let mut out = HashSet::new();
    for part in text.split(',').filter(|t| !t.is_empty()) {
        if part.eq_ignore_ascii_case("off") {
            out.clear();
        } else {
            out.insert(Category::parse(part)?);
        }
    }
    Some(out)
}

/// Validates a cache fragment (`CacheFragmentOption::SetFromString`):
/// ASCII alphanumerics, `-`, `_` only (empty allowed).
#[must_use]
pub fn valid_cache_fragment(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Validates a polyfill filename (`PolyfillFilenameOption::SetFromString`):
/// 1-255 chars of ASCII alphanumerics plus `._-`, not `.` or `..`.
#[must_use]
pub fn valid_polyfill_filename(text: &str) -> bool {
    if text.is_empty() || text.len() > 255 || text == "." || text == ".." {
        return false;
    }
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// A parsed option value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Boolean.
    Bool(bool),
    /// Integer (both `int` and `int64` kinds; `int` range-checked).
    Int(i64),
    /// String.
    Str(String),
    /// Rewrite level.
    Level(RewriteLevel),
    /// Enabled state.
    Enabled(EnabledState),
    /// Structured composite value, kept in canonical string form plus its
    /// C++ type name (validated by the type's own grammar on set).
    Composite {
        /// C++ type name (e.g. `"AllowVaryOn"`).
        type_name: &'static str,
        /// Canonical value text.
        text: String,
    },
}

impl Value {
    /// Converts a table default to a value.
    #[must_use]
    pub fn from_default(prop: &Property) -> Value {
        match prop.default {
            PropDefault::Bool(b) => Value::Bool(b),
            PropDefault::Int(i) => Value::Int(i),
            PropDefault::Str(s) => Value::Str(s.to_string()),
            PropDefault::Level(name) => Value::Level(RewriteLevel::parse(name).unwrap_or_default()),
            PropDefault::Enabled(_) => Value::Enabled(EnabledState::On),
            PropDefault::Expr(text) => Value::Composite {
                type_name: match prop.kind {
                    PropKind::Other(name) => name,
                    _ => "?",
                },
                text: text.to_string(),
            },
        }
    }
}

/// Outcome of setting an option (`OptionSettingResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOutcome {
    /// Value accepted.
    Ok,
    /// Unknown name (also used for scope violations, as in C++).
    NameUnknown,
    /// Known name, bad value.
    ValueInvalid,
}

/// File-loading policy entries (`FileLoadPolicy`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileLoadPolicy {
    /// Exact URL-prefix associations (`LoadFromFile`).
    pub exact: Vec<(String, String)>,
    /// Regexp associations (`LoadFromFileMatch`).
    pub regex: Vec<(String, String)>,
    /// Allow/disallow rules (`LoadFromFileRule[Match]`).
    pub rules: Vec<LoadRule>,
}

/// One `LoadFromFileRule[Match]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRule {
    /// Whether the rule allows (vs disallows) loading.
    pub allow: bool,
    /// Pattern text (kept verbatim; matching lands with the fetcher).
    pub pattern: String,
    /// Whether the pattern is a regexp (vs prefix).
    pub is_regexp: bool,
}

/// Domain-mapping entry (stored verbatim; full `DomainLawyer` lands later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainMapping {
    /// Mapping flavor: origin, proxy, or rewrite.
    pub kind: DomainMappingKind,
    /// Source domain.
    pub from: String,
    /// Target domain.
    pub to: String,
    /// Third arg (empty for 2-arg forms): the `Host` override for
    /// origin mappings, the CDN re-base for proxy mappings (C++
    /// `MapProxyDomain` takes `<proxy> <origin> [<cdn>]`, never a
    /// host header there).
    pub host_header: String,
}

/// Flavor of a domain mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainMappingKind {
    /// `MapOriginDomain`.
    Origin,
    /// `MapProxyDomain`.
    Proxy,
    /// `MapRewriteDomain`.
    Rewrite,
}

/// `pagespeed ProcessScriptVariables` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScriptVariables {
    /// No script variables.
    #[default]
    Off,
    /// Legacy restricted set.
    LegacyRestricted,
    /// All scriptable directives.
    All,
}

impl ScriptVariables {
    /// Parses `off` / `on` / `all` (case-insensitive, as in C++).
    #[must_use]
    pub fn parse(text: &str) -> Option<ScriptVariables> {
        if text.eq_ignore_ascii_case("off") {
            Some(ScriptVariables::Off)
        } else if text.eq_ignore_ascii_case("on") {
            Some(ScriptVariables::LegacyRestricted)
        } else if text.eq_ignore_ascii_case("all") {
            Some(ScriptVariables::All)
        } else {
            None
        }
    }
}

/// Nginx factory-owned options (stored on `NgxRewriteDriverFactory` in C++).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NginxFactoryOptions {
    /// `UseNativeFetcher` (http-only, default off).
    pub use_native_fetcher: bool,
    /// `NativeFetcherMaxKeepaliveRequests` (http-only, default 100).
    pub native_fetcher_max_keepalive: i64,
    /// `ProcessScriptVariables` mode (http-only, set-once, default off).
    pub script_variables: ScriptVariables,
    /// Whether script-variables mode was already set (second set rejected).
    pub script_variables_set: bool,
}

impl Default for NginxFactoryOptions {
    fn default() -> Self {
        NginxFactoryOptions {
            use_native_fetcher: false,
            native_fetcher_max_keepalive: 100,
            script_variables: ScriptVariables::Off,
            script_variables_set: false,
        }
    }
}

/// Nginx system options, stored per options object
/// (`SystemRewriteOptions` + `NgxRewriteOptions` subset: the nginx
/// step ports options on demand; the rest land with their owners).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NginxSystemOptions {
    /// `FileCachePath` (empty when unset). ps-nginx validates
    /// presence at server-merge time.
    pub file_cache_path: String,
    /// `LogDir` (empty when unset).
    pub log_dir: String,
    /// `StatisticsPath` (empty when unset: no statistics route).
    pub statistics_path: String,
    /// `GlobalStatisticsPath` (empty when unset).
    pub global_statistics_path: String,
    /// `ConsolePath` (empty when unset).
    pub console_path: String,
    /// `MessagesPath` (empty when unset).
    pub messages_path: String,
    /// `AdminPath` (empty when unset: no admin route).
    pub admin_path: String,
    /// `GlobalAdminPath` (empty when unset).
    pub global_admin_path: String,
    /// `PurgeMethod` (empty when unset: no purge route).
    pub purge_method: String,
    /// `StatisticsDomains` allow/disallow rules.
    pub statistics_domains: WildcardGroup,
    /// `GlobalStatisticsDomains` rules.
    pub global_statistics_domains: WildcardGroup,
    /// `MessagesDomains` rules.
    pub messages_domains: WildcardGroup,
    /// `ConsoleDomains` rules.
    pub console_domains: WildcardGroup,
    /// `AdminDomains` rules.
    pub admin_domains: WildcardGroup,
    /// `GlobalAdminDomains` rules.
    pub global_admin_domains: WildcardGroup,
    /// `FileCacheInodeLimit` target inode count (0: no limit;
    /// the factory ctor raises it to 500000).
    pub file_cache_clean_inode_limit: i64,
    /// `Statistics` collection switch (default on).
    pub statistics_enabled: bool,
    /// `StatisticsLogging` switch (default off; needs `LogDir`).
    pub statistics_logging_enabled: bool,
    /// `StatisticsLoggingIntervalMs` (default 10 minutes).
    pub statistics_logging_interval_ms: i64,
    /// `StatisticsLoggingMaxFileSizeKb` (default 1 MiB).
    pub statistics_logging_max_file_size_kb: i64,
    /// `MemcachedServers` cluster (empty when unset).
    pub memcached_servers: Vec<ExternalServerSpec>,
    /// `RedisServer` endpoint (`None` when unset).
    pub redis_server: Option<ExternalServerSpec>,
    /// `FetchProxy` (empty when unset). May carry credentials:
    /// never log this.
    pub fetcher_proxy: String,
    /// `FetchWithGzip` (default off).
    pub fetch_with_gzip: bool,
    /// `FetcherTimeOutMs` (default 5000).
    pub blocking_fetch_timeout_ms: i64,
    /// `FileCacheCleanIntervalMs` (default one hour; -1 disables).
    pub file_cache_clean_interval_ms: i64,
    /// `FileCacheSizeKb` (default 100 MiB).
    pub file_cache_clean_size_kb: i64,
    /// `LRUCacheByteLimit` (default 0: LRU disabled by size).
    pub lru_cache_byte_limit: i64,
    /// `LRUCacheKbPerProcess` (default 0: no per-process LRU).
    pub lru_cache_kb_per_process: i64,
    /// `CacheFlushFilename` (empty when unset).
    pub cache_flush_filename: String,
    /// `CompressMetadataCache` (default on).
    pub compress_metadata_cache: bool,
    /// `IproMaxResponseBytes` (default 10 MiB; 0 unlimited).
    pub ipro_max_response_bytes: i64,
    /// `IproMaxConcurrentRecordings` (default 10; 0 unlimited).
    pub ipro_max_concurrent_recordings: i64,
    /// Explicitly-set flags (`was_set`, driving merges).
    pub was_set: SystemWasSet,
}

/// Explicitly-set flags for system options (`was_set` bits).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemWasSet {
    /// `Statistics` set.
    pub statistics_enabled: bool,
    /// `StatisticsLogging` set.
    pub statistics_logging_enabled: bool,
    /// `StatisticsLoggingIntervalMs` set.
    pub statistics_logging_interval_ms: bool,
    /// `StatisticsLoggingMaxFileSizeKb` set.
    pub statistics_logging_max_file_size_kb: bool,
    /// `FetchProxy` set.
    pub fetcher_proxy: bool,
    /// `FetchWithGzip` set.
    pub fetch_with_gzip: bool,
    /// `FetcherTimeOutMs` set.
    pub blocking_fetch_timeout_ms: bool,
    /// `FileCacheCleanIntervalMs` set.
    pub clean_interval: bool,
    /// `FileCacheSizeKb` set.
    pub clean_size: bool,
    /// `FileCacheInodeLimit` set.
    pub clean_inode: bool,
    /// `LRUCacheByteLimit` set.
    pub lru_byte_limit: bool,
    /// `LRUCacheKbPerProcess` set.
    pub lru_kb_per_process: bool,
    /// `CacheFlushFilename` set.
    pub flush_filename: bool,
    /// `CompressMetadataCache` set.
    pub compress_metadata: bool,
    /// `IproMaxResponseBytes` set.
    pub ipro_max_response_bytes: bool,
    /// `IproMaxConcurrentRecordings` set.
    pub ipro_max_concurrent_recordings: bool,
}

impl Default for NginxSystemOptions {
    fn default() -> Self {
        NginxSystemOptions {
            file_cache_path: String::new(),
            log_dir: String::new(),
            statistics_path: String::new(),
            global_statistics_path: String::new(),
            console_path: String::new(),
            messages_path: String::new(),
            admin_path: String::new(),
            global_admin_path: String::new(),
            purge_method: String::new(),
            statistics_domains: WildcardGroup::default(),
            global_statistics_domains: WildcardGroup::default(),
            messages_domains: WildcardGroup::default(),
            console_domains: WildcardGroup::default(),
            admin_domains: WildcardGroup::default(),
            global_admin_domains: WildcardGroup::default(),
            file_cache_clean_inode_limit: 0,
            statistics_enabled: true,
            statistics_logging_enabled: false,
            statistics_logging_interval_ms: 600_000,
            statistics_logging_max_file_size_kb: 1024,
            memcached_servers: Vec::new(),
            redis_server: None,
            fetcher_proxy: String::new(),
            fetch_with_gzip: false,
            blocking_fetch_timeout_ms: 5000,
            file_cache_clean_interval_ms: 3_600_000,
            file_cache_clean_size_kb: 102_400,
            lru_cache_byte_limit: 0,
            lru_cache_kb_per_process: 0,
            cache_flush_filename: String::new(),
            compress_metadata_cache: true,
            ipro_max_response_bytes: 10 * 1024 * 1024,
            ipro_max_concurrent_recordings: 10,
            was_set: SystemWasSet::default(),
        }
    }
}

/// Default memcached port (`kMemcachedDefaultPort`).
pub const MEMCACHED_DEFAULT_PORT: u16 = 11211;

/// Default redis port (`kRedisDefaultPort`).
pub const REDIS_DEFAULT_PORT: u16 = 6379;

/// Parsed external cache server (`ExternalServerSpec`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalServerSpec {
    /// Server host (never empty).
    pub host: String,
    /// Server port (1-65535).
    pub port: u16,
}

/// Parses `<host>[:<port>]` (`ExternalServerSpec::SetFromString`,
/// verbatim errors).
pub fn parse_external_server(text: &str, default_port: u16) -> Result<ExternalServerSpec, String> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 1 && parts.len() != 2 {
        return Err("Expected single server in format <host>[:<port>]".to_string());
    }
    if parts[0].is_empty() {
        return Err("Server host cannot be empty".to_string());
    }
    let mut port = default_port;
    if parts.len() == 2 {
        let parsed = match parse_int32(parts[1]) {
            Some(parsed) => parsed,
            None => {
                return Err(format!(
                    "Port specified is not a valid number: '{}'",
                    parts[1]
                ));
            }
        };
        if !(1..=65535).contains(&parsed) {
            return Err(format!("Invalid port: {parsed}"));
        }
        port = parsed as u16;
    }
    Ok(ExternalServerSpec {
        host: parts[0].to_string(),
        port,
    })
}

/// Parses a comma-separated server list
/// (`ExternalClusterSpec::SetFromString`, verbatim errors).
pub fn parse_external_cluster(
    text: &str,
    default_port: u16,
) -> Result<Vec<ExternalServerSpec>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = text.split(',').collect();
    let mut detail = String::new();
    let mut servers = Vec::with_capacity(parts.len());
    for part in &parts {
        match parse_external_server(part, default_port) {
            Ok(server) => servers.push(server),
            Err(current) => {
                if parts.len() > 1 {
                    if !detail.is_empty() {
                        detail.push_str(". ");
                    }
                    detail.push_str(&format!("In server '{part}': "));
                }
                detail.push_str(&current);
            }
        }
    }
    if detail.is_empty() {
        Ok(servers)
    } else {
        Err(detail)
    }
}

/// Query-param scan outcome (`RewriteQuery::Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryStatus {
    /// Not a PageSpeed param.
    NoneFound,
    /// Applied.
    Success,
    /// Recognized but invalid value.
    Invalid,
}

/// Rewrite options: filter state, scalar values, and directive collections.
///
/// Values not explicitly set read back as their table defaults; presence in
/// the override map is the `was_set()` bit used by merge.
#[derive(Debug, Clone, Default)]
pub struct RewriteOptions {
    filters: FilterState,
    enabled_state: EnabledState,
    /// `was_set` bit for the master switch (`enabled_` is an
    /// `Option` in C++, so merge propagates it).
    enabled_set: bool,
    values: HashMap<&'static str, Value>,
    allow_resources: WildcardGroup,
    retain_comments: WildcardGroup,
    css_combining_ids: WildcardGroup,
    blocking_rewrite_referer: WildcardGroup,
    lazyload_enabled_classes: WildcardGroup,
    override_caching_wildcard: WildcardGroup,
    allow_when_inlining_resources: WildcardGroup,
    html_cache_ignore_cookies: CookiePatternSet,
    html_cache_bypass_cookies: CookiePatternSet,
    inline_unauthorized_types: Vec<Category>,
    domains: Vec<String>,
    known_domains: Vec<String>,
    proxy_suffix: String,
    downstream_purge_prefix: String,
    domain_mappings: Vec<DomainMapping>,
    shards: Vec<(String, String)>,
    resource_headers: Vec<(String, String)>,
    custom_fetch_headers: Vec<(String, String)>,
    file_load: FileLoadPolicy,
    libraries: HashMap<i64, HashMap<String, String>>,
    url_valued_attrs: Vec<(String, String, Category)>,
    experiment_specs: Vec<String>,
    experiment_index: Option<usize>,
    polyfill_filenames: HashSet<String>,
    modified: bool,
}

impl RewriteOptions {
    /// Creates default options (`HtmlWriterFilter` enabled, as in C++).
    #[must_use]
    pub fn new() -> RewriteOptions {
        let mut options = RewriteOptions::default();
        options.filters.enable(Filter::HtmlWriterFilter);
        options
    }

    /// Borrows the filter state.
    #[must_use]
    pub fn filters(&self) -> &FilterState {
        &self.filters
    }

    /// Mutably borrows the filter state.
    pub fn filters_mut(&mut self) -> &mut FilterState {
        &mut self.filters
    }

    /// Returns the master enabled state.
    #[must_use]
    pub fn enabled_state(&self) -> EnabledState {
        self.enabled_state
    }

    /// Sets the master enabled state (`set_enabled`).
    pub fn set_enabled_state(&mut self, state: EnabledState) {
        self.enabled_state = state;
        self.enabled_set = true;
        self.modified = true;
    }

    /// Fully passive (`unplugged()`): serves no `.pagespeed.` resources.
    #[must_use]
    pub fn unplugged(&self) -> bool {
        self.enabled_state == EnabledState::Unplugged
    }

    /// Idle-but-serving (`standby()`): serves `.pagespeed.` resources
    /// without optimizing HTML.
    #[must_use]
    pub fn standby(&self) -> bool {
        self.enabled_state == EnabledState::Standby
    }

    /// Whether any setting changed since construction (`modified_`).
    #[must_use]
    pub fn modified(&self) -> bool {
        self.modified
    }

    /// Reads an option value by property id (override or default).
    #[must_use]
    pub fn get_by_id(&self, id: &str) -> Option<Value> {
        let prop = lookup_by_id(id)?;
        if let Some(value) = self.values.get(prop.id) {
            return Some(value.clone());
        }
        Some(Value::from_default(prop))
    }

    /// Reads an option value by name (override or default).
    #[must_use]
    pub fn get_by_name(&self, name: &str) -> Option<Value> {
        let prop = lookup_by_name(name)?;
        self.get_by_id(prop.id)
    }

    /// Whether an option was explicitly set (`was_set()`).
    #[must_use]
    pub fn was_set(&self, id: &str) -> bool {
        self.values.contains_key(id)
    }

    /// Like `get_by_name`, failing for unknown names.
    fn require_prop(name: &str) -> Result<&'static Property, SetOutcome> {
        lookup_by_name(name).ok_or(SetOutcome::NameUnknown)
    }

    /// Sets a table option from its string form
    /// (`SetOptionFromNameInternal` with scope enforcement).
    ///
    /// Scope violations report `NameUnknown` with a scope message, exactly
    /// like C++.
    pub fn set_option(
        &mut self,
        name: &str,
        value: &str,
        max_scope: Scope,
    ) -> Result<(), SetError> {
        let prop = Self::require_prop(name).map_err(|outcome| SetError {
            outcome,
            message: String::new(),
        })?;
        if prop.scope > max_scope {
            return Err(SetError {
                outcome: SetOutcome::NameUnknown,
                message: format!(
                    "Option {name} cannot be set. Maximum allowed scope is {}",
                    scope_name(max_scope)
                ),
            });
        }
        // Failure detail (`FormatSetOptionMessage`): table parses
        // leave the detail empty, so the text ends after the
        // period plus the verbatim trailing space.
        let parsed = parse_value(prop, value).ok_or_else(|| SetError {
            outcome: SetOutcome::ValueInvalid,
            message: format!("Cannot set option {name} to {value}. "),
        })?;
        self.values.insert(prop.id, parsed);
        self.sync_special(prop.id);
        self.modified = true;
        Ok(())
    }

    /// Sets a request-scoped property by id (`set_pre_connect_url` et al).
    ///
    /// Request-only properties have no directive name; the server layer
    /// sets them per request. Anything else is rejected.
    pub fn set_request_property(&mut self, id: &str, value: &str) -> Result<(), SetError> {
        let prop = lookup_by_id(id).ok_or_else(|| SetError {
            outcome: SetOutcome::NameUnknown,
            message: String::new(),
        })?;
        if !prop.request_only {
            return Err(SetError {
                outcome: SetOutcome::NameUnknown,
                message: format!("Option {id} is not request-scoped"),
            });
        }
        let parsed = parse_value(prop, value).ok_or_else(|| SetError {
            outcome: SetOutcome::ValueInvalid,
            message: format!("Invalid value for {id}: {value}"),
        })?;
        self.values.insert(prop.id, parsed);
        self.modified = true;
        Ok(())
    }

    /// Keeps derived state (level, enabled flag, preserve flags) in sync
    /// when their backing properties are set directly.
    fn sync_special(&mut self, id: &str) {
        match id {
            // "l" RewriteLevel.
            "l" => {
                if let Some(Value::Level(level)) = self.values.get("l") {
                    self.filters.set_level(*level);
                }
            }
            // "e" EnableRewriting.
            "e" => {
                let state = match self.values.get("e") {
                    Some(Value::Enabled(state)) => Some(*state),
                    _ => None,
                };
                if let Some(state) = state {
                    self.set_enabled_state(state);
                }
            }
            // Preserve-urls flags.
            "cpu" => {
                if let Some(Value::Bool(v)) = self.values.get("cpu") {
                    self.filters.set_css_preserve_urls(*v);
                }
            }
            "jpu" => {
                if let Some(Value::Bool(v)) = self.values.get("jpu") {
                    self.filters.set_js_preserve_urls(*v);
                }
            }
            "ipu" => {
                if let Some(Value::Bool(v)) = self.values.get("ipu") {
                    self.filters.set_image_preserve_urls(*v);
                }
            }
            _ => {}
        }
    }

    /// One-arg dispatch: table options then specials
    /// (`ParseAndSetOptionFromNameWithScope`).
    ///
    /// Removed-but-recognized names are NOT accepted here: only the Apache
    /// host consults `is_deprecated_option` (warn + accept); nginx reports
    /// them unknown.
    pub fn parse_and_set_1(
        &mut self,
        name: &str,
        arg: &str,
        max_scope: Scope,
    ) -> Result<(), SetError> {
        if lookup_by_name(name).is_some() {
            return self.set_option(name, arg, max_scope);
        }
        self.parse_special_1(name, arg, max_scope)
    }

    /// Special one-arg directives (table lookup already missed).
    fn parse_special_1(&mut self, name: &str, arg: &str, max_scope: Scope) -> Result<(), SetError> {
        if name.eq_ignore_ascii_case("Allow") {
            self.allow_resources.allow(arg);
        } else if name.eq_ignore_ascii_case("DisableFilters") {
            if !self.filters.disable_by_comma_list(arg) {
                return invalid("Failed to disable some filters.");
            }
        } else if name.eq_ignore_ascii_case("Disallow") {
            self.allow_resources.disallow(arg);
        } else if name.eq_ignore_ascii_case("Domain") {
            self.domains.push(arg.to_string());
        } else if name.eq_ignore_ascii_case("HtmlCacheIgnoreCookie") {
            if let Err(error) = self.html_cache_ignore_cookies.add(arg) {
                return invalid(&format!("Invalid HtmlCacheIgnoreCookie: {}", error.message()));
            }
        } else if name.eq_ignore_ascii_case("HtmlCacheBypassCookie") {
            if let Err(error) = self.html_cache_bypass_cookies.add(arg) {
                return invalid(&format!("Invalid HtmlCacheBypassCookie: {}", error.message()));
            }
        } else if name.eq_ignore_ascii_case("ProxySuffix") {
            self.proxy_suffix = arg.to_string();
        } else if name.eq_ignore_ascii_case("DownstreamCachePurgeLocationPrefix") {
            if !is_web_url(arg) {
                return invalid("Downstream cache purge location prefix is invalid.");
            }
            if let Some(host_port) = url_host_port(arg) {
                self.known_domains.push(host_port);
            }
            self.downstream_purge_prefix = arg.to_string();
        } else if name.eq_ignore_ascii_case("EnableFilters") {
            if !self.filters.enable_by_comma_list(arg) {
                return invalid("Failed to enable some filters.");
            }
        } else if name.eq_ignore_ascii_case("ExperimentVariable") {
            let slot: i32 = arg.parse().unwrap_or(0);
            if !(1..=5).contains(&slot) {
                return invalid("must be an integer between 1 and 5");
            }
            self.values.insert("fga", Value::Int(i64::from(slot)));
        } else if name.eq_ignore_ascii_case("ExperimentSpec") {
            // Directory-level sources may not set alternate origins.
            if max_scope < Scope::Server && spec_has_alternate_origin(arg) {
                return invalid("alternate_origin_domain is not allowed at directory scope");
            }
            // Full ExperimentSpec validation lands later; the raw spec is
            // stored verbatim (non-empty required, as in C++ AddExperimentSpec
            // which rejects empty specs).
            if arg.is_empty() {
                return invalid("not a valid experiment spec");
            }
            self.experiment_specs.push(arg.to_string());
        } else if name.eq_ignore_ascii_case("ForbidFilters") {
            if !self.filters.forbid_by_comma_list(arg) {
                return invalid("Failed to forbid some filters.");
            }
        } else if name.eq_ignore_ascii_case("RetainComment") {
            self.retain_comments.allow(arg);
        } else if name.eq_ignore_ascii_case("BlockingRewriteRefererUrls") {
            self.blocking_rewrite_referer.allow(arg);
        } else if name.eq_ignore_ascii_case("PermitIdsForCssCombining") {
            self.css_combining_ids.allow(arg);
        } else {
            return Err(SetError {
                outcome: SetOutcome::NameUnknown,
                message: String::new(),
            });
        }
        self.modified = true;
        Ok(())
    }

    /// Two-arg dispatch (`ParseAndSetOptionFromName2`).
    pub fn parse_and_set_2(&mut self, name: &str, arg1: &str, arg2: &str) -> Result<(), SetError> {
        if name.eq_ignore_ascii_case("AddResourceHeader") {
            return self.add_resource_header(arg1, arg2);
        } else if name.eq_ignore_ascii_case("CustomFetchHeader") {
            self.custom_fetch_headers
                .push((arg1.to_string(), arg2.to_string()));
        } else if name.eq_ignore_ascii_case("LoadFromFile") {
            self.file_load
                .exact
                .push((arg1.to_string(), arg2.to_string()));
        } else if name.eq_ignore_ascii_case("LoadFromFileMatch") {
            // Regexp validity is enforced when the fetcher compiles it;
            // stored verbatim here.
            self.file_load
                .regex
                .push((arg1.to_string(), arg2.to_string()));
        } else if name.eq_ignore_ascii_case("LoadFromFileRule")
            || name.eq_ignore_ascii_case("LoadFromFileRuleMatch")
        {
            let is_regexp = name.eq_ignore_ascii_case("LoadFromFileRuleMatch");
            let allow = if arg1.eq_ignore_ascii_case("Allow") {
                true
            } else if arg1.eq_ignore_ascii_case("Disallow") {
                false
            } else {
                return invalid("Argument 1 must be either 'Allow' or 'Disallow'");
            };
            self.file_load.rules.push(LoadRule {
                allow,
                pattern: arg2.to_string(),
                is_regexp,
            });
        } else if name.eq_ignore_ascii_case("MapOriginDomain") {
            self.domain_mappings.push(DomainMapping {
                kind: DomainMappingKind::Origin,
                from: arg1.to_string(),
                to: arg2.to_string(),
                host_header: String::new(),
            });
        } else if name.eq_ignore_ascii_case("MapProxyDomain") {
            self.domain_mappings.push(DomainMapping {
                kind: DomainMappingKind::Proxy,
                from: arg1.to_string(),
                to: arg2.to_string(),
                host_header: String::new(),
            });
        } else if name.eq_ignore_ascii_case("MapRewriteDomain") {
            self.domain_mappings.push(DomainMapping {
                kind: DomainMappingKind::Rewrite,
                from: arg1.to_string(),
                to: arg2.to_string(),
                host_header: String::new(),
            });
        } else if name.eq_ignore_ascii_case("ShardDomain") {
            // Empty shards explicitly disable sharding (script variables).
            if !arg2.is_empty() {
                self.shards.push((arg1.to_string(), arg2.to_string()));
            }
        } else {
            return Err(SetError {
                outcome: SetOutcome::NameUnknown,
                message: String::new(),
            });
        }
        self.modified = true;
        Ok(())
    }

    /// Three-arg dispatch (`ParseAndSetOptionFromName3`).
    pub fn parse_and_set_3(
        &mut self,
        name: &str,
        arg1: &str,
        arg2: &str,
        arg3: &str,
    ) -> Result<(), SetError> {
        if name.eq_ignore_ascii_case("UrlValuedAttribute") {
            match Category::parse(arg3) {
                Some(category) => {
                    self.url_valued_attrs
                        .push((arg1.to_string(), arg2.to_string(), category))
                }
                None => {
                    return invalid(&format!("Invalid resource category: {arg3}"));
                }
            }
        } else if name.eq_ignore_ascii_case("Library") {
            let bytes = parse_int64(arg1).unwrap_or(-1);
            if bytes < 0 {
                return invalid("Library size must be a positive 64-bit integer");
            }
            if !register_library(&mut self.libraries, bytes, arg2, arg3) {
                return invalid(&format!(
                    "Format is size md5 url; bad md5 {arg2} or URL {arg3}"
                ));
            }
        } else if name.eq_ignore_ascii_case("MapOriginDomain") {
            self.domain_mappings.push(DomainMapping {
                kind: DomainMappingKind::Origin,
                from: arg1.to_string(),
                to: arg2.to_string(),
                host_header: arg3.to_string(),
            });
        } else if name.eq_ignore_ascii_case("MapProxyDomain") {
            self.domain_mappings.push(DomainMapping {
                kind: DomainMappingKind::Proxy,
                from: arg1.to_string(),
                to: arg2.to_string(),
                host_header: arg3.to_string(),
            });
        } else {
            return Err(SetError {
                outcome: SetOutcome::NameUnknown,
                message: String::new(),
            });
        }
        self.modified = true;
        Ok(())
    }

    /// Validated resource-header add (`ValidateAndAddResourceHeader`).
    fn add_resource_header(&mut self, name: &str, value: &str) -> Result<(), SetError> {
        let name = name.trim();
        let value = value.trim();
        validate_http_header(name, value)?;
        if HOP_BY_HOP_HEADERS
            .iter()
            .any(|h| h.eq_ignore_ascii_case(name))
        {
            return invalid(&format!("Rejecting hop by hop header '{name} '"));
        }
        if FIXED_RESOURCE_HEADERS
            .iter()
            .any(|h| h.eq_ignore_ascii_case(name))
        {
            return invalid(&format!("Rejecting header '{name} '"));
        }
        // C++ rejects when size already exceeds 20 (allows 21 entries).
        if self.resource_headers.len() > 20 {
            return invalid("Too many AddResourceHeader directives (max: 20)");
        }
        self.resource_headers
            .push((name.to_string(), value.to_string()));
        Ok(())
    }

    /// Merges a narrower-scope `src` over `self` (`Merge`).
    ///
    /// Filter sets union with the C++ erase rules, the forbid-all latch is
    /// enforced, set scalar values override, collections append, libraries
    /// merge per key, and the extend-cache/preserve conflicts resolve via
    /// the merge override.
    pub fn merge(&mut self, src: &RewriteOptions) {
        let mut modify = src.modified;
        let latched = self.forbid_all_disabled();
        let enabled_before: HashSet<Filter> = if latched {
            Filter::ALL
                .into_iter()
                .filter(|f| self.filters.enabled(*f))
                .collect()
        } else {
            HashSet::new()
        };

        // Preserve/extend-cache conflicts, computed before merging.
        let overrides = [
            (
                Filter::ExtendCacheCss,
                self.css_preserve(),
                src.css_preserve(),
            ),
            (
                Filter::ExtendCacheImages,
                self.image_preserve(),
                src.image_preserve(),
            ),
            (
                Filter::ExtendCacheScripts,
                self.js_preserve(),
                src.js_preserve(),
            ),
        ]
        .map(|(filter, mine, theirs)| {
            // ComputeMergeOverride(filter, src_preserve, my_preserve, src).
            if src.filters.enabled(filter) && mine {
                MergeOverride::DisablePreserve
            } else if self.filters.enabled(filter) && theirs {
                MergeOverride::DisableFilter
            } else {
                MergeOverride::None
            }
        });

        let new_forbid_all = self.forbid_all_disabled() || src.forbid_all_disabled();
        // Merge the flag itself when either side set it.
        let forbid_flag_id = lookup_by_name("ForbidAllDisabledFilters").map(|p| p.id);
        if let Some(id) = forbid_flag_id {
            if self.was_set(id) || src.was_set(id) {
                self.values.insert(id, Value::Bool(new_forbid_all));
            }
        }

        // Filter sets.
        if !latched {
            // Enabled in src clears disabled here.
            for f in src.filters.enabled_set().iter() {
                self.filters.disabled_set_mut().remove(f);
            }
        }
        for f in src.filters.enabled_set().iter() {
            let before = self.filters.enabled_set().len();
            self.filters.enabled_set_mut().insert(f);
            modify |= self.filters.enabled_set().len() != before;
        }
        for f in src.filters.disabled_set().iter() {
            let before = self.filters.disabled_set().len();
            self.filters.disabled_set_mut().insert(f);
            modify |= self.filters.disabled_set().len() != before;
        }
        // Enabled minus disabled, then minus forbidden.
        for f in Filter::ALL {
            if self.filters.disabled_set().contains(f) {
                self.filters.enabled_set_mut().remove(f);
            }
        }
        for f in src.filters.forbidden_set().iter() {
            let before = self.filters.forbidden_set().len();
            self.filters.forbidden_set_mut().insert(f);
            modify |= self.filters.forbidden_set().len() != before;
        }
        for f in Filter::ALL {
            if self.filters.forbidden_set().contains(f) {
                self.filters.enabled_set_mut().remove(f);
            }
        }

        // Master switch (`Option::MergeHelper`, verbatim): src wins
        // when set, and an unset self takes src's value (default
        // transfer). The twin values write fires only when src is
        // set, so merging two unset switches leaves `values["e"]`
        // absent exactly like the C++ `was_set_` bit.
        if src.enabled_set || !self.enabled_set {
            self.enabled_state = src.enabled_state;
            self.enabled_set = src.enabled_set;
            if src.enabled_set {
                self.values.insert("e", Value::Enabled(src.enabled_state));
            }
        }
        // Scalars override when set in src.
        for (id, value) in &src.values {
            self.values.insert(id, value.clone());
        }
        self.sync_special("l");
        self.sync_special("e");
        self.sync_special("cpu");
        self.sync_special("jpu");
        self.sync_special("ipu");

        // Collections append.
        self.resource_headers
            .extend(src.resource_headers.iter().cloned());
        self.custom_fetch_headers
            .extend(src.custom_fetch_headers.iter().cloned());
        self.url_valued_attrs
            .extend(src.url_valued_attrs.iter().cloned());
        self.experiment_specs
            .extend(src.experiment_specs.iter().cloned());
        self.domains.extend(src.domains.iter().cloned());
        self.known_domains.extend(src.known_domains.iter().cloned());
        self.domain_mappings
            .extend(src.domain_mappings.iter().cloned());
        self.shards.extend(src.shards.iter().cloned());
        self.file_load
            .exact
            .extend(src.file_load.exact.iter().cloned());
        self.file_load
            .regex
            .extend(src.file_load.regex.iter().cloned());
        self.file_load
            .rules
            .extend(src.file_load.rules.iter().cloned());
        self.allow_resources.append_from(&src.allow_resources);
        self.retain_comments.append_from(&src.retain_comments);
        self.css_combining_ids.append_from(&src.css_combining_ids);
        self.blocking_rewrite_referer
            .append_from(&src.blocking_rewrite_referer);
        self.lazyload_enabled_classes
            .append_from(&src.lazyload_enabled_classes);
        self.override_caching_wildcard
            .append_from(&src.override_caching_wildcard);
        self.allow_when_inlining_resources
            .append_from(&src.allow_when_inlining_resources);
        self.html_cache_ignore_cookies
            .append_from(&src.html_cache_ignore_cookies);
        self.html_cache_bypass_cookies
            .append_from(&src.html_cache_bypass_cookies);
        self.polyfill_filenames
            .extend(src.polyfill_filenames.iter().cloned());
        if !src.proxy_suffix.is_empty() {
            self.proxy_suffix = src.proxy_suffix.clone();
        }
        if !src.downstream_purge_prefix.is_empty() {
            self.downstream_purge_prefix = src.downstream_purge_prefix.clone();
        }
        // Libraries merge per (bytes, md5) key; src wins.
        for (bytes, map) in &src.libraries {
            self.libraries
                .entry(*bytes)
                .or_default()
                .extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        self.experiment_index = src.experiment_index;

        // Apply extend-cache/preserve overrides.
        let preserve_ids = ["cpu", "ipu", "jpu"];
        let override_filters = [
            Filter::ExtendCacheCss,
            Filter::ExtendCacheImages,
            Filter::ExtendCacheScripts,
        ];
        for (i, filter) in override_filters.into_iter().enumerate() {
            match overrides[i] {
                MergeOverride::None => {}
                MergeOverride::DisablePreserve => {
                    if self.was_set(preserve_ids[i]) {
                        self.values.insert(preserve_ids[i], Value::Bool(false));
                    }
                }
                MergeOverride::DisableFilter => {
                    self.filters.enabled_set_mut().remove(filter);
                    self.filters.disabled_set_mut().insert(filter);
                }
            }
        }
        self.sync_special("cpu");
        self.sync_special("jpu");
        self.sync_special("ipu");

        // Latched forbid-all: refuse anything the merge turned on.
        if latched {
            for filter in Filter::ALL {
                if !enabled_before.contains(&filter) && self.filters.enabled(filter) {
                    self.filters.enabled_set_mut().remove(filter);
                    self.filters.disabled_set_mut().insert(filter);
                    modify = true;
                }
            }
        }
        if modify {
            self.modified = true;
        }
    }

    /// Merges only process-scope options (`MergeOnlyProcessScopeOptions`).
    pub fn merge_process_scope_only(&mut self, src: &RewriteOptions) {
        for prop in PROPERTIES {
            if prop.scope == Scope::LegacyProcess || prop.scope == Scope::ProcessStrict {
                if let Some(value) = src.values.get(prop.id) {
                    self.values.insert(prop.id, value.clone());
                }
            }
        }
        self.modified = true;
    }

    /// Disables filters requiring script execution
    /// (`DisableFiltersRequiringScriptExecution`).
    pub fn disable_filters_requiring_script_execution(&mut self) {
        for f in REQUIRES_SCRIPT_EXECUTION {
            self.filters.disable(*f);
        }
        self.modified = true;
    }

    /// Disables filters that cannot run on Ajax responses
    /// (`DisableFiltersThatCantRunInAjax`): script-execution
    /// filters, head-element filters, critical-CSS selection, and
    /// URL modification (relative URLs may break).
    pub fn disable_filters_that_cant_run_in_ajax(&mut self) {
        self.disable_filters_requiring_script_execution();
        for f in ADD_HEAD_FILTERS {
            self.filters.disable(*f);
        }
        self.filters.disable(Filter::PrioritizeCriticalCss);
        self.set_css_preserve_urls(true);
        self.set_image_preserve_urls(true);
        self.set_js_preserve_urls(true);
    }

    /// Scans one query/header name+value pair (`RewriteQuery::ScanNameValue`).
    ///
    /// `allow_options` gates everything after the bare `PageSpeed=` switch.
    pub fn scan_query(&mut self, name: &str, value: &str, allow_options: bool) -> QueryStatus {
        let value = trim_url_quotes(value);
        if name == "ModPagespeed" || name == "PageSpeed" {
            if let Some(state) = EnabledState::parse(value) {
                self.set_enabled_state(state);
                return QueryStatus::Success;
            }
            if value.starts_with("noscript") {
                self.disable_filters_requiring_script_execution();
                self.filters.enable(Filter::HandleNoscriptRedirect);
                return QueryStatus::Success;
            }
            return QueryStatus::Invalid;
        }
        if !allow_options {
            return QueryStatus::NoneFound;
        }
        if name == "ModPagespeedFilters" || name == "PageSpeedFilters" {
            return if self.filters.adjust_by_comma_list(value) {
                QueryStatus::Success
            } else {
                QueryStatus::Invalid
            };
        }
        if name.eq_ignore_ascii_case("X-PSA-Client-Options") {
            if self.update_with_client_options(Some(value)) {
                return QueryStatus::Success;
            }
            // We don't want to return Invalid, which causes a 405
            // (kMethodNotAllowed) returned to client.
        }
        if name.eq_ignore_ascii_case("Cache-Control")
            && value.split(',').map(str::trim).any(|element| {
                element.eq_ignore_ascii_case("no-transform")
                    || element
                        .split(';')
                        .next()
                        .is_some_and(|d| d.trim().eq_ignore_ascii_case("no-transform"))
            })
        {
            self.set_enabled_state(EnabledState::Standby);
            return QueryStatus::Success;
        }
        if let Some(suffix) = name
            .strip_prefix("ModPagespeed")
            .or_else(|| name.strip_prefix("PageSpeed"))
        {
            // Query scope only (`SetOptionFromQuery`).
            return match self.set_option(suffix, value, Scope::Query) {
                Ok(()) => QueryStatus::Success,
                Err(err) if err.outcome == SetOutcome::NameUnknown => {
                    // StickyQueryParameters is request-context state, not an
                    // option; acknowledged here so callers can route it.
                    if suffix.eq_ignore_ascii_case("StickyQueryParameters") {
                        QueryStatus::Success
                    } else {
                        QueryStatus::NoneFound
                    }
                }
                Err(_) => QueryStatus::Invalid,
            };
        }
        QueryStatus::NoneFound
    }

    // -- accessors used by merge, tests, and later crates --

    /// Reads the rewrite-hyperlinks flag (`domain_rewrite_hyperlinks`).
    #[must_use]
    pub fn domain_rewrite_hyperlinks(&self) -> bool {
        self.values
            .get(
                lookup_by_name("DomainRewriteHyperlinks")
                    .map(|p| p.id)
                    .unwrap_or(""),
            )
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    /// Reads the rewrite-cookies flag (`domain_rewrite_cookies`).
    #[must_use]
    pub fn domain_rewrite_cookies(&self) -> bool {
        self.values
            .get(
                lookup_by_name("DomainRewriteCookies")
                    .map(|p| p.id)
                    .unwrap_or(""),
            )
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    /// Reads the client-rewrite flag (`client_domain_rewrite`).
    #[must_use]
    pub fn client_domain_rewrite(&self) -> bool {
        self.values
            .get(
                lookup_by_name("ClientDomainRewrite")
                    .map(|p| p.id)
                    .unwrap_or(""),
            )
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    /// Reads the preserve-hints flag (`preserve_subresource_hints`).
    #[must_use]
    pub fn preserve_subresource_hints(&self) -> bool {
        self.values
            .get(
                lookup_by_name("PreserveSubresourceHints")
                    .map(|p| p.id)
                    .unwrap_or(""),
            )
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    /// Reads the mismatched-hash publicity flag
    /// (`publicly_cache_mismatched_hashes_experimental`).
    #[must_use]
    pub fn publicly_cache_mismatched_hashes_experimental(&self) -> bool {
        self.values
            .get(
                lookup_by_name("PubliclyCacheMismatchedHashesExperimental")
                    .map(|p| p.id)
                    .unwrap_or(""),
            )
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    /// Reads the oblivious-URLs flag (`oblivious_pagespeed_urls`).
    #[must_use]
    pub fn oblivious_pagespeed_urls(&self) -> bool {
        self.values
            .get(
                lookup_by_name("ObliviousPagespeedUrls")
                    .map(|p| p.id)
                    .unwrap_or(""),
            )
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    /// Reads the forbid-all-disabled flag.
    #[must_use]
    pub fn forbid_all_disabled(&self) -> bool {
        self.values
            .get(
                lookup_by_name("ForbidAllDisabledFilters")
                    .map(|p| p.id)
                    .unwrap_or(""),
            )
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    fn css_preserve(&self) -> bool {
        self.values
            .get("cpu")
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    fn image_preserve(&self) -> bool {
        self.values
            .get("ipu")
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    fn js_preserve(&self) -> bool {
        self.values
            .get("jpu")
            .is_some_and(|v| matches!(v, Value::Bool(true)))
    }

    /// Borrows the allow/disallow resource patterns.
    #[must_use]
    pub fn allow_resources(&self) -> &WildcardGroup {
        &self.allow_resources
    }

    /// Mutably borrows the allow/disallow resource patterns.
    pub fn allow_resources_mut(&mut self) -> &mut WildcardGroup {
        &mut self.allow_resources
    }

    /// Borrows retained-comment patterns.
    #[must_use]
    pub fn retain_comments(&self) -> &WildcardGroup {
        &self.retain_comments
    }

    /// Reads the lazyload-after-onload flag (`lazyload_images_after_onload`,
    /// default true).
    #[must_use]
    pub fn lazyload_images_after_onload(&self) -> bool {
        matches!(
            self.get_by_name("LazyloadImagesAfterOnload"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the lazyload blank-image override (`lazyload_images_blank_url`,
    /// default empty, meaning the site-local blank).
    #[must_use]
    pub fn lazyload_images_blank_url(&self) -> String {
        match self.get_by_name("LazyloadImagesBlankUrl") {
            Some(Value::Str(url)) => url,
            _ => String::new(),
        }
    }

    /// Disables lazyload for a class-name pattern
    /// (`DisableLazyloadForClassName`).
    pub fn disable_lazyload_for_class_name(&mut self, pattern: &str) {
        self.lazyload_enabled_classes.disallow(pattern);
    }

    /// Whether lazyload runs for a lowercased class value
    /// (`IsLazyloadEnabledForClassName`).
    #[must_use]
    pub fn is_lazyload_enabled_for_class_name(&self, class: &str) -> bool {
        self.lazyload_enabled_classes.is_match(class, true)
    }

    /// Reads the blank-preview flag (`use_blank_image_for_inline_preview`).
    #[must_use]
    pub fn use_blank_image_for_inline_preview(&self) -> bool {
        matches!(
            self.get_by_name("UseBlankImageForInlinePreview"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the mobile-aggressive flag
    /// (`enable_aggressive_rewriters_for_mobile`, default true).
    #[must_use]
    pub fn enable_aggressive_rewriters_for_mobile(&self) -> bool {
        matches!(
            self.get_by_name("EnableAggressiveRewritersForMobile"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the lazy-highres flag (`lazyload_highres_images`).
    #[must_use]
    pub fn lazyload_highres_images(&self) -> bool {
        matches!(
            self.get_by_name("EnableLazyLoadHighResImages"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the relativity flag (`preserve_url_relativity`, default true).
    #[must_use]
    pub fn preserve_url_relativity(&self) -> bool {
        matches!(
            self.get_by_name("PreserveUrlRelativity"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the implicit cache TTL (`implicit_cache_ttl_ms`).
    #[must_use]
    pub fn implicit_cache_ttl_ms(&self) -> i64 {
        match self.get_by_name("ImplicitCacheTtlMs") {
            Some(Value::Int(ms)) => ms,
            _ => 0,
        }
    }

    /// Reads the stale-serving flag (`serve_stale_if_fetch_error`).
    #[must_use]
    pub fn serve_stale_if_fetch_error(&self) -> bool {
        matches!(
            self.get_by_name("ServeStaleIfFetchError"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the minimum rewriteable TTL (`min_resource_cache_time_to_rewrite_ms`).
    #[must_use]
    pub fn min_resource_cache_time_to_rewrite_ms(&self) -> i64 {
        match self.get_by_name("MinResourceCacheTimeToRewriteMs") {
            Some(Value::Int(ms)) => ms,
            _ => 0,
        }
    }

    /// Reads the rewritten-HTML cache cap (`max_html_cache_time_ms`,
    /// default 0: rewritten pages are never cacheable).
    #[must_use]
    pub fn max_html_cache_time_ms(&self) -> i64 {
        match self.get_by_name("MaxHtmlCacheTimeMs") {
            Some(Value::Int(ms)) => ms,
            _ => 0,
        }
    }

    /// Sets the rewritten-HTML cache cap.
    pub fn set_max_html_cache_time_ms(&mut self, value: i64) {
        self.values.insert("hc", Value::Int(value));
        self.modified = true;
    }

    /// Reads the purge flag (`enable_cache_purge`).
    #[must_use]
    pub fn enable_cache_purge(&self) -> bool {
        matches!(
            self.get_by_name("EnableCachePurge"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the flush-following flag (`follow_flushes`, default true).
    #[must_use]
    pub fn follow_flushes(&self) -> bool {
        !matches!(self.get_by_name("FollowFlushes"), Some(Value::Bool(false)))
    }

    /// Reads the forwarded-proto flag (`respect_x_forwarded_proto`).
    #[must_use]
    pub fn respect_x_forwarded_proto(&self) -> bool {
        matches!(
            self.get_by_name("RespectXForwardedProto"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the remote-configuration URL (`remote_configuration_url`,
    /// empty when unset).
    #[must_use]
    pub fn remote_configuration_url(&self) -> String {
        match self.get_by_name("RemoteConfigurationUrl") {
            Some(Value::Str(url)) => url,
            _ => String::new(),
        }
    }

    /// Reads the version header value (`x_header_value`, the
    /// `XHeaderValue` option nginx defaults to its version).
    #[must_use]
    pub fn x_header_value(&self) -> String {
        match self.get_by_name("XHeaderValue") {
            Some(Value::Str(value)) => value,
            _ => String::new(),
        }
    }

    /// Reads the remote-configuration timeout
    /// (`remote_configuration_timeout_ms`, ms).
    #[must_use]
    pub fn remote_configuration_timeout_ms(&self) -> i64 {
        match self.get_by_name("RemoteConfigurationTimeoutMs") {
            Some(Value::Int(ms)) => ms,
            _ => 1000,
        }
    }

    /// Reads the experiment-cookie lifetime
    /// (`experiment_cookie_duration_ms`, ms).
    #[must_use]
    pub fn experiment_cookie_duration_ms(&self) -> i64 {
        match self.get_by_name("ExperimentCookieDurationMs") {
            Some(Value::Int(ms)) => ms,
            _ => 604800000,
        }
    }

    /// Effective in-place `s-maxage` (`EffectiveInPlaceSMaxAgeSec`):
    /// the configured seconds when caching headers are modified,
    /// else -1 (disabled).
    #[must_use]
    pub fn effective_in_place_s_max_age_sec(&self) -> i32 {
        if !self.modify_caching_headers() {
            return -1;
        }
        self.in_place_s_maxage_sec() as i32
    }

    /// Reads the option-override token (`request_option_override`,
    /// empty when unset).
    #[must_use]
    pub fn request_option_override(&self) -> String {
        match self.get_by_name("RequestOptionOverride") {
            Some(Value::Str(token)) => token,
            _ => String::new(),
        }
    }

    /// Whether cookies may carry options
    /// (`allow_options_to_be_set_by_cookies`, default true).
    #[must_use]
    pub fn allow_options_to_be_set_by_cookies(&self) -> bool {
        !matches!(
            self.get_by_name("AllowOptionsToBeSetByCookies"),
            Some(Value::Bool(false))
        )
    }

    /// Whether an experiment is running (`running_experiment()`).
    #[must_use]
    pub fn running_experiment(&self) -> bool {
        matches!(self.get_by_name("RunExperiment"), Some(Value::Bool(true)))
    }

    /// Whether to enroll despite request options (`enroll_experiment()`:
    /// the enrolled id differs from "force no experiment").
    #[must_use]
    pub fn enroll_experiment(&self) -> bool {
        !matches!(self.get_by_name("EnrollExperiment"), Some(Value::Int(-2)))
    }

    /// Reads the rebeaconing key (`downstream_cache_rebeaconing_key`,
    /// empty when unset).
    #[must_use]
    pub fn downstream_cache_rebeaconing_key(&self) -> String {
        match self.get_by_name("DownstreamCacheRebeaconingKey") {
            Some(Value::Str(key)) => key,
            _ => String::new(),
        }
    }

    /// Whether downstream cache integration is configured
    /// (`IsDownstreamCacheIntegrationEnabled`: a purge location
    /// prefix is set).
    #[must_use]
    pub fn is_downstream_cache_integration_enabled(&self) -> bool {
        !self.downstream_cache_purge_location_prefix().is_empty()
    }

    /// Whether a key matches the configured rebeaconing key
    /// (`MatchesDownstreamCacheRebeaconingKey`): configured and
    /// case-insensitively equal.
    #[must_use]
    pub fn matches_downstream_cache_rebeaconing_key(&self, key: &str) -> bool {
        let configured = self.downstream_cache_rebeaconing_key();
        !configured.is_empty() && key.eq_ignore_ascii_case(&configured)
    }

    /// Reads the configured beacon endpoints (`beacon_url()`), if
    /// the directive was explicitly set. The host default (nginx:
    /// `/ngx_pagespeed_beacon`) is factory behavior, so unset
    /// reads back `None` and the caller supplies it.
    #[must_use]
    pub fn beacon_url(&self) -> Option<BeaconUrl> {
        if !self.was_set("bu") {
            return None;
        }
        match self.get_by_id("bu") {
            Some(Value::Composite { text, .. }) => BeaconUrl::parse(&text),
            _ => None,
        }
    }

    /// Reads the vary flag (`respect_vary`).
    #[must_use]
    pub fn respect_vary(&self) -> bool {
        matches!(self.get_by_name("RespectVary"), Some(Value::Bool(true)))
    }

    /// Reads the proactive-freshening flag (`proactive_resource_freshening`).
    #[must_use]
    pub fn proactive_resource_freshening(&self) -> bool {
        matches!(
            self.get_by_name("ProactiveResourceFreshening"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the bot-fetch flag (`disable_background_fetches_for_bots`).
    #[must_use]
    pub fn disable_background_fetches_for_bots(&self) -> bool {
        matches!(
            self.get_by_name("DisableBackgroundFetchesForBots"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the TTL override (`override_caching_ttl_ms`, -1 when unset).
    #[must_use]
    pub fn override_caching_ttl_ms(&self) -> i64 {
        match self.get_by_name("OverrideCachingTtlMs") {
            Some(Value::Int(ms)) => ms,
            _ => -1,
        }
    }

    /// Whether a URL's cache TTL is overridden (`IsCacheTtlOverridden`).
    #[must_use]
    pub fn is_cache_ttl_overridden(&self, url: &str) -> bool {
        self.override_caching_wildcard.is_match(url, false)
    }

    /// Reads the cacheable size cap (`max_cacheable_response_content_length`).
    #[must_use]
    pub fn max_cacheable_response_content_length(&self) -> i64 {
        match self.get_by_name("MaxCacheableContentLength") {
            Some(Value::Int(limit)) => limit,
            _ => -1,
        }
    }

    /// Reads the introspective-JS flag
    /// (`avoid_renaming_introspective_javascript`, default true).
    #[must_use]
    pub fn avoid_renaming_introspective_javascript(&self) -> bool {
        match self.get_by_name("AvoidRenamingIntrospectiveJavascript") {
            Some(Value::Bool(avoid)) => avoid,
            _ => true,
        }
    }

    /// Sets the introspective-JS flag.
    pub fn set_avoid_renaming_introspective_javascript(&mut self, value: bool) {
        self.values.insert("aris", Value::Bool(value));
        self.modified = true;
    }

    /// Reads the no-transform flag (`disable_rewrite_on_no_transform`).
    #[must_use]
    pub fn disable_rewrite_on_no_transform(&self) -> bool {
        matches!(
            self.get_by_name("DisableRewriteOnNoTransform"),
            Some(Value::Bool(true))
        )
    }

    /// Sets the no-transform flag.
    pub fn set_disable_rewrite_on_no_transform(&mut self, value: bool) {
        self.values.insert("drnt", Value::Bool(value));
        self.modified = true;
    }

    /// Reads the signing key (`url_signing_key`, empty disables signing).
    #[must_use]
    pub fn url_signing_key(&self) -> String {
        match self.get_by_name("UrlSigningKey") {
            Some(Value::Str(key)) => key,
            _ => String::new(),
        }
    }

    /// Reads the lenient-signing flag (`accept_invalid_signatures`).
    #[must_use]
    pub fn accept_invalid_signatures(&self) -> bool {
        matches!(
            self.get_by_name("AcceptInvalidSignatures"),
            Some(Value::Bool(true))
        )
    }

    /// Reads the staleness window (`metadata_cache_staleness_threshold_ms`).
    #[must_use]
    pub fn metadata_cache_staleness_threshold_ms(&self) -> i64 {
        match self.get_by_name("MetadataCacheStalenessThresholdMs") {
            Some(Value::Int(ms)) => ms,
            _ => 0,
        }
    }

    /// Sets the staleness window.
    pub fn set_metadata_cache_staleness_threshold_ms(&mut self, ms: i64) {
        self.values.insert("mcst", Value::Int(ms));
        self.modified = true;
    }

    /// Whether eligible HTML bodies cache (`html_cache`,
    /// default false).
    #[must_use]
    pub fn html_cache(&self) -> bool {
        match self.get_by_name("HtmlCache") {
            Some(Value::Bool(value)) => value,
            _ => false,
        }
    }

    /// Sets HTML caching.
    pub fn set_html_cache(&mut self, value: bool) {
        self.values.insert("fhc", Value::Bool(value));
        self.modified = true;
    }

    /// Whether per-request HTML cache diagnostics append
    /// (`show_cache_timestamp`, default false).
    #[must_use]
    pub fn show_cache_timestamp(&self) -> bool {
        match self.get_by_name("ShowCacheTimestamp") {
            Some(Value::Bool(value)) => value,
            _ => false,
        }
    }

    /// Sets cache-timestamp diagnostics.
    pub fn set_show_cache_timestamp(&mut self, value: bool) {
        self.values.insert("fhcs", Value::Bool(value));
        self.modified = true;
    }

    /// HTML cache lifetime cap in seconds (`html_cache_ttl_sec`,
    /// default 300).
    #[must_use]
    pub fn html_cache_ttl_sec(&self) -> i64 {
        match self.get_by_name("HtmlCacheTtlSec") {
            Some(Value::Int(value)) => value,
            _ => 300,
        }
    }

    /// Sets the HTML cache lifetime cap.
    pub fn set_html_cache_ttl_sec(&mut self, value: i64) {
        self.values.insert("fhct", Value::Int(value));
        self.modified = true;
    }

    /// Whether common analytics/tracking cookies are ignored for
    /// HTML-cache eligibility and keys
    /// (`html_cache_ignore_common_cookies`, default true).
    ///
    /// Requests carrying only allowlisted cookies cache exactly
    /// like cookie-free requests. The built-in list covers
    /// first-party analytics names (Google Analytics legacy and
    /// GA4, Google Ads, Meta Pixel, Microsoft Clarity, Hotjar)
    /// whose values never change server-generated HTML.
    #[must_use]
    pub fn html_cache_ignore_common_cookies(&self) -> bool {
        match self.get_by_name("HtmlCacheIgnoreCommonCookies") {
            Some(Value::Bool(value)) => value,
            _ => true,
        }
    }

    /// Sets the common-cookie allowlist switch.
    pub fn set_html_cache_ignore_common_cookies(&mut self, value: bool) {
        self.values.insert("fhcc", Value::Bool(value));
        self.modified = true;
    }

    /// Custom ignored cookie patterns (`HtmlCacheIgnoreCookie`,
    /// repeatable): exact names or `prefix*` wildcards extending
    /// the built-in allowlist. Only list cookies that cannot
    /// influence server-generated HTML.
    #[must_use]
    pub fn html_cache_ignore_cookies(&self) -> &CookiePatternSet {
        &self.html_cache_ignore_cookies
    }

    /// Forced-bypass cookie patterns (`HtmlCacheBypassCookie`,
    /// repeatable): exact names or `prefix*` wildcards that bypass
    /// the HTML cache even when allowlisted.
    #[must_use]
    pub fn html_cache_bypass_cookies(&self) -> &CookiePatternSet {
        &self.html_cache_bypass_cookies
    }

    /// Cookie policy view for HTML-cache eligibility.
    #[must_use]
    pub fn html_cache_cookie_policy(&self) -> CookiePolicy<'_> {
        CookiePolicy {
            common_enabled: self.html_cache_ignore_common_cookies(),
            ignore: &self.html_cache_ignore_cookies,
            bypass: &self.html_cache_bypass_cookies,
        }
    }

    /// Formats `name args` for config snippets (`FormatOption`).
    #[must_use]
    pub fn format_option(name: &str, args: &str) -> String {
        format!("{name} {args}")
    }

    /// Dumps the configuration (`OptionsToString`): version and
    /// enabled state, enabled filters, overridden options with
    /// secrets redacted, and experiment specs. Domain-lawyer,
    /// invalidation, rejected-map, and wildcard sections wait
    /// for those subsystems to live on options.
    #[must_use]
    pub fn options_to_string(&self) -> String {
        let state = match self.enabled_state() {
            EnabledState::Off => "off",
            EnabledState::On => "on",
            EnabledState::Unplugged => "unplugged",
            EnabledState::Standby => "standby",
        };
        let mut out = format!("Version: 16: {state}\n\n");
        out.push_str("Filters\n");
        for filter in Filter::ALL {
            if self.enabled(filter) {
                out.push_str(&format!("{}\t{}\n", filter.id(), filter.display_name()));
            }
        }
        out.push_str("\nOptions\n");
        let mut names = Vec::new();
        let mut values = Vec::new();
        let mut max_width = 0;
        for prop in crate::property::PROPERTIES {
            if self.was_set(prop.id) {
                let name_and_id = format!("{} ({})", prop.name, prop.id);
                max_width = max_width.max(name_and_id.len());
                names.push(name_and_id);
                let value = if SECRET_OPTIONS.contains(&prop.name) {
                    "***".to_string()
                } else {
                    self.get_by_id(prop.id)
                        .map(value_to_string)
                        .unwrap_or_default()
                };
                values.push(value);
            }
        }
        for (name, value) in names.iter().zip(values.iter()) {
            let spaces = " ".repeat(max_width - name.len() + 2);
            out.push_str(&format!("  {name}{spaces}{value}\n"));
        }
        for spec in &self.experiment_specs {
            out.push_str(&format!("Experiment {spec}\n"));
        }
        out
    }

    /// Reads the error-input recheck TTL (`metadata_input_errors_cache_ttl_ms`).
    #[must_use]
    pub fn metadata_input_errors_cache_ttl_ms(&self) -> i64 {
        match self.get_by_id("mect") {
            Some(Value::Int(ms)) => ms,
            _ => 0,
        }
    }

    /// Whether a cached URL survives purging (`IsUrlCacheValid`).
    /// Invalidation entries land with the purge machinery; unpurged
    /// setups always validate.
    #[must_use]
    pub fn is_url_cache_valid(&self, _url: &str, _time_ms: i64) -> bool {
        let _ = self.enable_cache_purge();
        true
    }

    /// Serializes configured state (`signature`).
    ///
    /// NOTE: the bytes are implementation-local (the C++ text format
    /// is not reproduced): only stability across identical configs
    /// and sensitivity to any change are contractual. Partition keys
    /// built on it never leave the local metadata cache.
    #[must_use]
    pub fn signature(&self) -> String {
        let mut parts = Vec::new();
        let mut ids: Vec<&str> = self.values.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            parts.push(format!("{id}={:?}", self.values[id]));
        }
        parts.push(format!(
            "filters={:?}",
            self.filters.without_debug_for_signature()
        ));
        parts.push(format!("irwea={:?}", self.inline_unauthorized_types));
        parts.push(format!("allow={}", self.allow_resources.signature()));
        parts.push(format!(
            "inline={}",
            self.allow_when_inlining_resources.signature()
        ));
        parts.push(format!(
            "lazy={}",
            self.lazyload_enabled_classes.signature()
        ));
        parts.push(format!("retain={}", self.retain_comments.signature()));
        parts.push(format!("domains={:?}", self.domains));
        parts.push(format!("shards={:?}", self.shards));
        parts.push(format!("maps={:?}", self.domain_mappings));
        parts.push(format!("cssid={}", self.css_combining_ids.signature()));
        parts.push(format!(
            "referer={}",
            self.blocking_rewrite_referer.signature()
        ));
        parts.push(format!(
            "ttlwild={}",
            self.override_caching_wildcard.signature()
        ));
        parts.push(format!(
            "cookieignore={}",
            self.html_cache_ignore_cookies.signature()
        ));
        parts.push(format!(
            "cookiebypass={}",
            self.html_cache_bypass_cookies.signature()
        ));
        parts.push(format!("experiment={}", self.experiment_state_str()));
        parts.join(";")
    }

    /// Reads the URL length cap (`max_url_size`).
    #[must_use]
    pub fn max_url_size(&self) -> i64 {
        match self.get_by_name("MaxUrlSize") {
            Some(Value::Int(limit)) => limit,
            _ => -1,
        }
    }

    /// Reads the preemptive-JS flag
    /// (`in_place_preemptive_rewrite_javascript`, default true).
    #[must_use]
    pub fn in_place_preemptive_rewrite_javascript(&self) -> bool {
        match self.get_by_name("InPlacePreemptiveRewriteJavascript") {
            Some(Value::Bool(preemptive)) => preemptive,
            _ => true,
        }
    }

    /// Sets the preemptive-JS flag.
    pub fn set_in_place_preemptive_rewrite_javascript(&mut self, value: bool) {
        self.values.insert("ipprj", Value::Bool(value));
        self.modified = true;
    }

    /// Reads the experimental-minifier flag
    /// (`use_experimental_js_minifier`, default true).
    #[must_use]
    pub fn use_experimental_js_minifier(&self) -> bool {
        match self.get_by_name("UseExperimentalJsMinifier") {
            Some(Value::Bool(use_new)) => use_new,
            _ => true,
        }
    }

    /// Registers a JS library for canonicalization
    /// (`RegisterLibrary`).
    pub fn register_library(&mut self, bytes: i64, md5: &str, url: &str) -> bool {
        register_library(&mut self.libraries, bytes, md5, url)
    }

    /// Reads the JS inline cap (`js_inline_max_bytes`,
    /// default 2048).
    #[must_use]
    pub fn js_inline_max_bytes(&self) -> i64 {
        match self.get_by_name("JsInlineMaxBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 2048,
        }
    }

    /// Reads the CSS inline cap (`css_inline_max_bytes`,
    /// default 2048).
    #[must_use]
    pub fn css_inline_max_bytes(&self) -> i64 {
        match self.get_by_name("CssInlineMaxBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 2048,
        }
    }

    /// Reads the font-CSS inline cap
    /// (`google_font_css_inline_max_bytes`, default 3072).
    #[must_use]
    pub fn google_font_css_inline_max_bytes(&self) -> i64 {
        match self.get_by_name("GoogleFontCssInlineMaxBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 3072,
        }
    }

    /// Sets the font-CSS inline cap.
    pub fn set_google_font_css_inline_max_bytes(&mut self, bytes: i64) {
        self.values.insert("gfci", Value::Int(bytes));
        self.modified = true;
    }

    /// Whether caching headers may be edited (`modify_caching_headers`,
    /// default true).
    #[must_use]
    pub fn modify_caching_headers(&self) -> bool {
        match self.get_by_name("ModifyCachingHeaders") {
            Some(Value::Bool(value)) => value,
            _ => true,
        }
    }

    /// Sets the caching-headers flag.
    pub fn set_modify_caching_headers(&mut self, value: bool) {
        self.values.insert("mch", Value::Bool(value));
        self.modified = true;
    }

    /// Whether non-pagespeed URLs rewrite in place
    /// (`in_place_rewriting_enabled`, default true).
    #[must_use]
    pub fn in_place_rewriting_enabled(&self) -> bool {
        match self.get_by_name("InPlaceResourceOptimization") {
            Some(Value::Bool(value)) => value,
            _ => true,
        }
    }

    /// Sets in-place rewriting.
    pub fn set_in_place_rewriting_enabled(&mut self, value: bool) {
        self.values.insert("ipro", Value::Bool(value));
        self.modified = true;
    }

    /// Whether IPRO waits for the optimized bytes
    /// (`in_place_wait_for_optimized`, default false).
    #[must_use]
    pub fn in_place_wait_for_optimized(&self) -> bool {
        match self.get_by_name("InPlaceWaitForOptimized") {
            Some(Value::Bool(value)) => value,
            _ => false,
        }
    }

    /// Sets IPRO waiting.
    pub fn set_in_place_wait_for_optimized(&mut self, value: bool) {
        self.values.insert("ipwo", Value::Bool(value));
        self.modified = true;
    }

    /// s-maxage for not-yet-optimized IPRO serves
    /// (`in_place_s_maxage_sec`, default 10).
    #[must_use]
    pub fn in_place_s_maxage_sec(&self) -> i64 {
        match self.get_by_name("InPlaceSMaxAgeSec") {
            Some(Value::Int(value)) => value,
            _ => 10,
        }
    }

    /// Sets the IPRO s-maxage.
    pub fn set_in_place_s_maxage_sec(&mut self, value: i64) {
        self.values.insert("ipsm", Value::Int(value));
        self.modified = true;
    }

    /// Effective IPRO s-maxage (`EffectiveInPlaceSMaxAgeSec`):
    /// -1 when caching headers stay untouched.
    #[must_use]
    pub fn effective_in_place_s_maxage_sec(&self) -> i64 {
        if self.modify_caching_headers() {
            self.in_place_s_maxage_sec()
        } else {
            -1
        }
    }

    /// IPRO rewrite deadline (`in_place_rewrite_deadline_ms`,
    /// default 10; sync execution has no deadlines).
    #[must_use]
    pub fn in_place_rewrite_deadline_ms(&self) -> i64 {
        match self.get_by_name("InPlaceRewriteDeadlineMs") {
            Some(Value::Int(value)) => value,
            _ => 10,
        }
    }

    /// Sets the IPRO rewrite deadline.
    pub fn set_in_place_rewrite_deadline_ms(&mut self, value: i64) {
        self.values.insert("iprdm", Value::Int(value));
        self.modified = true;
    }

    /// Whether uncacheable resources rewrite in place
    /// (`rewrite_uncacheable_resources`, default false).
    #[must_use]
    pub fn rewrite_uncacheable_resources(&self) -> bool {
        match self.get_by_name("RewriteUncacheableResources") {
            Some(Value::Bool(value)) => value,
            _ => false,
        }
    }

    /// Sets uncacheable rewriting.
    pub fn set_rewrite_uncacheable_resources(&mut self, value: bool) {
        self.values.insert("rur", Value::Bool(value));
        self.modified = true;
    }

    /// File-input cache TTL (`load_from_file_cache_ttl_ms`,
    /// default 5 minutes).
    #[must_use]
    pub fn load_from_file_cache_ttl_ms(&self) -> i64 {
        match self.get_by_name("LoadFromFileCacheTtlMs") {
            Some(Value::Int(value)) => value,
            _ => 5 * 60 * 1000,
        }
    }

    /// Sets the file-input cache TTL.
    pub fn set_load_from_file_cache_ttl_ms(&mut self, value: i64) {
        self.values.insert("lfct", Value::Int(value));
        self.modified = true;
    }

    /// Whether IE gets `Cache-Control: private` instead of Vary
    /// (`private_not_vary_for_ie`, default true).
    #[must_use]
    pub fn private_not_vary_for_ie(&self) -> bool {
        match self.get_by_name("PrivateNotVaryForIE") {
            Some(Value::Bool(value)) => value,
            _ => true,
        }
    }

    /// Sets the IE private-not-vary flag.
    pub fn set_private_not_vary_for_ie(&mut self, value: bool) {
        self.values.insert("pnvie", Value::Bool(value));
        self.modified = true;
    }

    /// Reads the downstream-cache purge prefix (empty when unset).
    #[must_use]
    pub fn downstream_cache_purge_location_prefix(&self) -> &str {
        &self.downstream_purge_prefix
    }

    /// Sets the downstream-cache purge prefix.
    pub fn set_downstream_cache_purge_location_prefix(&mut self, prefix: &str) {
        self.downstream_purge_prefix = prefix.to_string();
        self.modified = true;
    }

    /// Reads the debug-output cap (`debug_output_max_bytes`,
    /// default 1MiB; non-positive means unbounded).
    #[must_use]
    pub fn debug_output_max_bytes(&self) -> i64 {
        match self.get_by_name("DebugOutputMaxBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 1048576,
        }
    }

    /// Sets the debug-output cap.
    pub fn set_debug_output_max_bytes(&mut self, limit: i64) {
        self.values.insert("dob", Value::Int(limit));
        self.modified = true;
    }

    /// Reads the segment length cap (`max_url_segment_size`).
    #[must_use]
    pub fn max_url_segment_size(&self) -> i64 {
        match self.get_by_name("MaxSegmentLength") {
            Some(Value::Int(limit)) => limit,
            _ => -1,
        }
    }

    /// Reads the CSS outlining threshold (`css_outline_min_bytes`,
    /// default 3000).
    #[must_use]
    pub fn css_outline_min_bytes(&self) -> i64 {
        match self.get_by_name("CssOutlineMinBytes") {
            Some(Value::Int(bytes)) => bytes,
            _ => 3000,
        }
    }

    /// Sets the CSS outlining threshold
    /// (`set_css_outline_min_bytes`, directory scope).
    pub fn set_css_outline_min_bytes(&mut self, bytes: i64) {
        self.values.insert("co", Value::Int(bytes));
        self.modified = true;
    }

    /// Reads the always-rewrite-CSS flag (`always_rewrite_css`,
    /// default false).
    #[must_use]
    pub fn always_rewrite_css(&self) -> bool {
        match self.get_by_name("AlwaysRewriteCss") {
            Some(Value::Bool(always)) => always,
            _ => false,
        }
    }

    /// Sets the always-rewrite-CSS flag.
    pub fn set_always_rewrite_css(&mut self, value: bool) {
        self.values.insert("arc", Value::Bool(value));
        self.modified = true;
    }

    /// Reads the CSS parse cap (`css_parse_max_bytes`, default
    /// 2MiB; non-positive means unbounded).
    #[must_use]
    pub fn css_parse_max_bytes(&self) -> i64 {
        match self.get_by_name("CssParseMaxBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 2 * 1024 * 1024,
        }
    }

    /// Sets the CSS parse cap.
    pub fn set_css_parse_max_bytes(&mut self, bytes: i64) {
        self.values.insert("cpmb", Value::Int(bytes));
        self.modified = true;
    }

    /// Reads the flatten-imports cap (`css_flatten_max_bytes`,
    /// default 1024000).
    #[must_use]
    pub fn css_flatten_max_bytes(&self) -> i64 {
        match self.get_by_name("CssFlattenMaxBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 1024000,
        }
    }

    /// Sets the flatten-imports cap.
    pub fn set_css_flatten_max_bytes(&mut self, bytes: i64) {
        self.values.insert("cf", Value::Int(bytes));
        self.modified = true;
    }

    /// Whether any image-optimization filter runs
    /// (`ImageOptimizationEnabled`).
    #[must_use]
    pub fn image_optimization_enabled(&self) -> bool {
        use crate::filter::Filter;
        self.enabled(Filter::RecompressJpeg)
            || self.enabled(Filter::RecompressPng)
            || self.enabled(Filter::RecompressWebp)
            || self.enabled(Filter::ConvertGifToPng)
            || self.enabled(Filter::ConvertJpegToProgressive)
            || self.enabled(Filter::ConvertPngToJpeg)
            || self.enabled(Filter::ConvertJpegToWebp)
            || self.enabled(Filter::ConvertToAvif)
            || self.enabled(Filter::ConvertToWebpAnimated)
            || self.enabled(Filter::ConvertToWebpLossless)
    }

    /// Reads the CSS image-inline cap (`css_image_inline_max_bytes`,
    /// default 0).
    #[must_use]
    pub fn css_image_inline_max_bytes(&self) -> i64 {
        match self.get_by_name("CssImageInlineMaxBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 0,
        }
    }

    /// Reads the image-inline cap, gated on the inline-images filter
    /// (`ImageInlineMaxBytes`, default 3072).
    #[must_use]
    pub fn image_inline_max_bytes(&self) -> i64 {
        if !self.enabled(Filter::InlineImages) {
            return 0;
        }
        match self.get_by_name("ImageInlineMaxBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 3072,
        }
    }

    /// Sets the image-inline cap.
    pub fn set_image_inline_max_bytes(&mut self, bytes: i64) {
        self.values.insert("ii", Value::Int(bytes));
        self.modified = true;
    }

    /// Sets the CSS-inline cap.
    pub fn set_css_inline_max_bytes(&mut self, bytes: i64) {
        self.values.insert("ci", Value::Int(bytes));
        self.modified = true;
    }

    /// Sets the CSS image-inline cap.
    pub fn set_css_image_inline_max_bytes(&mut self, bytes: i64) {
        self.values.insert("cii", Value::Int(bytes));
        self.modified = true;
    }

    /// Reads the larger inline cap (`MaxImageInlineMaxBytes`).
    #[must_use]
    pub fn max_image_inline_max_bytes(&self) -> i64 {
        self.image_inline_max_bytes()
            .max(self.css_image_inline_max_bytes())
    }

    /// Reads an image int64 option by name with a fallback.
    fn image_int(&self, name: &str, fallback: i64) -> i64 {
        match self.get_by_name(name) {
            Some(Value::Int(value)) => value,
            _ => fallback,
        }
    }

    /// Sets an image int64 option by id.
    fn set_image_int(&mut self, id: &'static str, value: i64) {
        self.values.insert(id, Value::Int(value));
        self.modified = true;
    }

    /// Reads an image bool option by name with a fallback.
    fn image_bool(&self, name: &str, fallback: bool) -> bool {
        match self.get_by_name(name) {
            Some(Value::Bool(value)) => value,
            _ => fallback,
        }
    }

    /// Sets an image bool option by id.
    fn set_image_bool(&mut self, id: &'static str, value: bool) {
        self.values.insert(id, Value::Bool(value));
        self.modified = true;
    }

    /// Reads the recompress quality (`ImageRecompressionQuality`,
    /// default 85).
    #[must_use]
    pub fn image_recompress_quality(&self) -> i64 {
        self.image_int("ImageRecompressionQuality", 85)
    }

    /// Sets the recompress quality.
    pub fn set_image_recompress_quality(&mut self, quality: i64) {
        self.set_image_int("irq", quality);
    }

    /// Reads the JPEG recompress quality (`JpegRecompressionQuality`,
    /// default -1 = fall back).
    #[must_use]
    pub fn image_jpeg_recompress_quality(&self) -> i64 {
        self.image_int("JpegRecompressionQuality", -1)
    }

    /// Sets the JPEG recompress quality.
    pub fn set_image_jpeg_recompress_quality(&mut self, quality: i64) {
        self.set_image_int("iq", quality);
    }

    /// Reads the small-screen JPEG quality
    /// (`JpegRecompressionQualityForSmallScreens`, default 70).
    #[must_use]
    pub fn image_jpeg_recompress_quality_for_small_screens(&self) -> i64 {
        self.image_int("JpegRecompressionQualityForSmallScreens", 70)
    }

    /// Sets the small-screen JPEG quality.
    pub fn set_image_jpeg_recompress_quality_for_small_screens(&mut self, quality: i64) {
        self.set_image_int("iqss", quality);
    }

    /// Reads the Save-Data JPEG quality (`JpegQualityForSaveData`,
    /// default 50).
    #[must_use]
    pub fn image_jpeg_quality_for_save_data(&self) -> i64 {
        self.image_int("JpegQualityForSaveData", 50)
    }

    /// Sets the Save-Data JPEG quality.
    pub fn set_image_jpeg_quality_for_save_data(&mut self, quality: i64) {
        self.set_image_int("iqsd", quality);
    }

    /// Reads the webp recompress quality (`WebpRecompressionQuality`,
    /// default 80).
    #[must_use]
    pub fn image_webp_recompress_quality(&self) -> i64 {
        self.image_int("WebpRecompressionQuality", 80)
    }

    /// Sets the webp recompress quality.
    pub fn set_image_webp_recompress_quality(&mut self, quality: i64) {
        self.set_image_int("iw", quality);
    }

    /// Reads the small-screen webp quality
    /// (`WebpRecompressionQualityForSmallScreens`, default 70).
    #[must_use]
    pub fn image_webp_recompress_quality_for_small_screens(&self) -> i64 {
        self.image_int("WebpRecompressionQualityForSmallScreens", 70)
    }

    /// Sets the small-screen webp quality.
    pub fn set_image_webp_recompress_quality_for_small_screens(&mut self, quality: i64) {
        self.set_image_int("iwss", quality);
    }

    /// Reads the animated-webp quality
    /// (`WebpAnimatedRecompressionQuality`, default 70).
    #[must_use]
    pub fn image_webp_animated_recompress_quality(&self) -> i64 {
        self.image_int("WebpAnimatedRecompressionQuality", 70)
    }

    /// Sets the animated-webp quality.
    pub fn set_image_webp_animated_recompress_quality(&mut self, quality: i64) {
        self.set_image_int("iwa", quality);
    }

    /// Reads the Save-Data webp quality (`WebpQualityForSaveData`,
    /// default 50).
    #[must_use]
    pub fn image_webp_quality_for_save_data(&self) -> i64 {
        self.image_int("WebpQualityForSaveData", 50)
    }

    /// Sets the Save-Data webp quality.
    pub fn set_image_webp_quality_for_save_data(&mut self, quality: i64) {
        self.set_image_int("iwsd", quality);
    }

    /// Resolves the JPEG quality (`ImageJpegQuality`).
    #[must_use]
    pub fn image_jpeg_quality(&self) -> i64 {
        let quality = self.image_jpeg_recompress_quality();
        if quality < 0 {
            self.image_recompress_quality()
        } else {
            quality
        }
    }

    /// Resolves the small-screen JPEG quality.
    #[must_use]
    pub fn image_jpeg_quality_for_small_screen(&self) -> i64 {
        let quality = self.image_jpeg_recompress_quality_for_small_screens();
        if quality < 0 {
            self.image_jpeg_quality()
        } else {
            quality
        }
    }

    /// Resolves the Save-Data JPEG quality.
    #[must_use]
    pub fn image_jpeg_quality_save_data(&self) -> i64 {
        let quality = self.image_jpeg_quality_for_save_data();
        if quality < 0 {
            self.image_jpeg_quality()
        } else {
            quality
        }
    }

    /// Resolves the webp quality (`ImageWebpQuality`).
    #[must_use]
    pub fn image_webp_quality(&self) -> i64 {
        let quality = self.image_webp_recompress_quality();
        if quality < 0 {
            self.image_recompress_quality()
        } else {
            quality
        }
    }

    /// Resolves the small-screen webp quality.
    #[must_use]
    pub fn image_webp_quality_for_small_screen(&self) -> i64 {
        let quality = self.image_webp_recompress_quality_for_small_screens();
        if quality < 0 {
            self.image_webp_quality()
        } else {
            quality
        }
    }

    /// Resolves the Save-Data webp quality.
    #[must_use]
    pub fn image_webp_quality_save_data(&self) -> i64 {
        let quality = self.image_webp_quality_for_save_data();
        if quality < 0 {
            self.image_webp_quality()
        } else {
            quality
        }
    }

    /// Resolves the animated-webp quality.
    #[must_use]
    pub fn image_webp_animated_quality(&self) -> i64 {
        let quality = self.image_webp_animated_recompress_quality();
        if quality < 0 {
            self.image_webp_quality()
        } else {
            quality
        }
    }

    /// Whether small-screen qualities differ (`HasValidSmallScreenQualities`).
    #[must_use]
    pub fn has_valid_small_screen_qualities(&self) -> bool {
        self.image_webp_quality_for_small_screen() != self.image_webp_quality()
            || self.image_jpeg_quality_for_small_screen() != self.image_jpeg_quality()
    }

    /// Whether Save-Data qualities differ (`HasValidSaveDataQualities`).
    #[must_use]
    pub fn has_valid_save_data_qualities(&self) -> bool {
        self.image_webp_quality_save_data() != self.image_webp_quality()
            || self.image_jpeg_quality_save_data() != self.image_jpeg_quality()
    }

    /// Reads the progressive-scan count (`ImageJpegNumProgressiveScans`,
    /// default -1).
    #[must_use]
    pub fn image_jpeg_num_progressive_scans(&self) -> i64 {
        self.image_int("ImageJpegNumProgressiveScans", -1)
    }

    /// Sets the progressive-scan count.
    pub fn set_image_jpeg_num_progressive_scans(&mut self, scans: i64) {
        self.set_image_int("ijps", scans);
    }

    /// Reads the small-screen scan count
    /// (`ImageJpegNumProgressiveScansForSmallScreens`, default -1).
    #[must_use]
    pub fn image_jpeg_num_progressive_scans_for_small_screens(&self) -> i64 {
        self.image_int("ImageJpegNumProgressiveScansForSmallScreens", -1)
    }

    /// Sets the small-screen scan count.
    pub fn set_image_jpeg_num_progressive_scans_for_small_screens(&mut self, scans: i64) {
        self.set_image_int("ijpst", scans);
    }

    /// Resolves the small-screen scan count.
    #[must_use]
    pub fn image_jpeg_num_progressive_scans_for_small_screen(&self) -> i64 {
        let scans = self.image_jpeg_num_progressive_scans_for_small_screens();
        if scans < 0 {
            self.image_jpeg_num_progressive_scans()
        } else {
            scans
        }
    }

    /// Reads the optimized-percent floor (`ImageLimitOptimizedPercent`,
    /// default 100).
    #[must_use]
    pub fn image_limit_optimized_percent(&self) -> i64 {
        self.image_int("ImageLimitOptimizedPercent", 100)
    }

    /// Sets the optimized-percent floor.
    pub fn set_image_limit_optimized_percent(&mut self, percent: i64) {
        self.set_image_int("ip", percent);
    }

    /// Reads the resize-area floor (`ImageLimitResizeAreaPercent`,
    /// default 100).
    #[must_use]
    pub fn image_limit_resize_area_percent(&self) -> i64 {
        self.image_int("ImageLimitResizeAreaPercent", 100)
    }

    /// Sets the resize-area floor.
    pub fn set_image_limit_resize_area_percent(&mut self, percent: i64) {
        self.set_image_int("ia", percent);
    }

    /// Reads the resolution ceiling (`ImageResolutionLimitBytes`,
    /// default 32 MiB).
    #[must_use]
    pub fn image_resolution_limit_bytes(&self) -> i64 {
        self.image_int("ImageResolutionLimitBytes", 32 * 1024 * 1024)
    }

    /// Sets the resolution ceiling.
    pub fn set_image_resolution_limit_bytes(&mut self, bytes: i64) {
        self.set_image_int("irlb", bytes);
    }

    /// Reads the webp deadline (`WebpTimeoutMs`, default -1).
    #[must_use]
    pub fn image_webp_timeout_ms(&self) -> i64 {
        self.image_int("WebpTimeoutMs", -1)
    }

    /// Sets the webp deadline.
    pub fn set_image_webp_timeout_ms(&mut self, timeout_ms: i64) {
        self.set_image_int("wt", timeout_ms);
    }

    /// Reads the AVIF deadline (`AvifTimeoutMs`, default 30000).
    #[must_use]
    pub fn image_avif_timeout_ms(&self) -> i64 {
        self.image_int("AvifTimeoutMs", 30000)
    }

    /// Sets the AVIF deadline.
    pub fn set_image_avif_timeout_ms(&mut self, timeout_ms: i64) {
        self.set_image_int("at", timeout_ms);
    }

    /// Reads the AVIF concurrency bound
    /// (`AvifMaxConcurrentConversions`, default 2).
    #[must_use]
    pub fn image_avif_max_concurrent_conversions(&self) -> i64 {
        self.image_int("AvifMaxConcurrentConversions", 2)
    }

    /// Sets the AVIF concurrency bound.
    pub fn set_image_avif_max_concurrent_conversions(&mut self, max: i64) {
        self.set_image_int("ac", max);
    }

    /// Reads the preview-image index cap
    /// (`MaxInlinedPreviewImagesIndex`, default -1).
    #[must_use]
    pub fn max_inlined_preview_images_index(&self) -> i64 {
        self.image_int("MaxInlinedPreviewImagesIndex", -1)
    }

    /// Sets the preview-image index cap.
    pub fn set_max_inlined_preview_images_index(&mut self, index: i64) {
        self.set_image_int("mdii", index);
    }

    /// Reads the low-res preview floor
    /// (`MinImageSizeLowResolutionBytes`, default 3 KiB).
    #[must_use]
    pub fn min_image_size_low_resolution_bytes(&self) -> i64 {
        self.image_int("MinImageSizeLowResolutionBytes", 3 * 1024)
    }

    /// Sets the low-res preview floor.
    pub fn set_min_image_size_low_resolution_bytes(&mut self, bytes: i64) {
        self.set_image_int("nislr", bytes);
    }

    /// Reads the low-res preview ceiling
    /// (`MaxImageSizeLowResolutionBytes`, default 1 MiB).
    #[must_use]
    pub fn max_image_size_low_resolution_bytes(&self) -> i64 {
        self.image_int("MaxImageSizeLowResolutionBytes", 1024 * 1024)
    }

    /// Sets the low-res preview ceiling.
    pub fn set_max_image_size_low_resolution_bytes(&mut self, bytes: i64) {
        self.set_image_int("xislr", bytes);
    }

    /// Reads the low-res size cap (`MaxLowResImageSizeBytes`,
    /// default -1).
    #[must_use]
    pub fn max_low_res_image_size_bytes(&self) -> i64 {
        self.image_int("MaxLowResImageSizeBytes", -1)
    }

    /// Sets the low-res size cap.
    pub fn set_max_low_res_image_size_bytes(&mut self, bytes: i64) {
        self.set_image_int("lris", bytes);
    }

    /// Reads the low-res/full-res percentage cap
    /// (`MaxLowResToHighResImageSizePercentage`, default 100).
    #[must_use]
    pub fn max_low_res_to_full_res_image_size_percentage(&self) -> i64 {
        self.image_int("MaxLowResToHighResImageSizePercentage", 100)
    }

    /// Sets the low-res/full-res percentage cap.
    pub fn set_max_low_res_to_full_res_image_size_percentage(&mut self, percent: i64) {
        self.set_image_int("lrhrs", percent);
    }

    /// Reads the progressive-JPEG floor (`ProgressiveJpegMinBytes`,
    /// default 10240).
    #[must_use]
    pub fn progressive_jpeg_min_bytes(&self) -> i64 {
        self.image_int("ProgressiveJpegMinBytes", 10240)
    }

    /// Sets the progressive-JPEG floor.
    pub fn set_progressive_jpeg_min_bytes(&mut self, bytes: i64) {
        self.set_image_int("jp", bytes);
    }

    /// Whether optimized images carry no-transform
    /// (`NoTransformOptimizedImages`, default false).
    #[must_use]
    pub fn no_transform_optimized_images(&self) -> bool {
        self.image_bool("NoTransformOptimizedImages", false)
    }

    /// Sets the no-transform flag.
    pub fn set_no_transform_optimized_images(&mut self, value: bool) {
        self.set_image_bool("ntoi", value);
    }

    /// Whether only critical images inline
    /// (`InlineOnlyCriticalImages`, default true).
    #[must_use]
    pub fn inline_only_critical_images(&self) -> bool {
        self.image_bool("InlineOnlyCriticalImages", true)
    }

    /// Sets the critical-only flag.
    pub fn set_inline_only_critical_images(&mut self, value: bool) {
        self.set_image_bool("ioci", value);
    }

    /// Whether client-side critical-image detection JS inserts
    /// (`CriticalImagesBeaconEnabled`, default true).
    #[must_use]
    pub fn critical_images_beacon_enabled(&self) -> bool {
        self.image_bool("CriticalImagesBeaconEnabled", true)
    }

    /// Sets critical-image beaconing.
    pub fn set_critical_images_beacon_enabled(&mut self, value: bool) {
        self.set_image_bool("cibe", value);
    }

    /// Beacon reinstrumentation interval in seconds
    /// (`BeaconReinstrumentTimeSec`, default 5).
    #[must_use]
    pub fn beacon_reinstrument_time_sec(&self) -> i64 {
        self.image_int("BeaconReinstrumentTimeSec", 5)
    }

    /// Sets the reinstrumentation interval.
    pub fn set_beacon_reinstrument_time_sec(&mut self, value: i64) {
        self.set_image_int("brts", value);
    }

    /// Whether small images stay unrewritten
    /// (`CacheSmallImagesUnrewritten`, default false).
    #[must_use]
    pub fn cache_small_images_unrewritten(&self) -> bool {
        self.image_bool("CacheSmallImagesUnrewritten", false)
    }

    /// Sets the unrewritten flag.
    pub fn set_cache_small_images_unrewritten(&mut self, value: bool) {
        self.set_image_bool("csiu", value);
    }

    /// Sets the blank-preview flag.
    pub fn set_use_blank_image_for_inline_preview(&mut self, value: bool) {
        self.set_image_bool("biip", value);
    }

    /// Whether rewritten webp serves to any agent
    /// (`ServeRewrittenWebpUrlsToAnyAgent`, default true).
    #[must_use]
    pub fn serve_rewritten_webp_urls_to_any_agent(&self) -> bool {
        self.image_bool("ServeRewrittenWebpUrlsToAnyAgent", true)
    }

    /// Sets the serve-webp flag.
    pub fn set_serve_rewritten_webp_urls_to_any_agent(&mut self, value: bool) {
        self.set_image_bool("swaa", value);
    }

    /// Reads the Vary policy (`AllowVaryOn`, default auto).
    #[must_use]
    pub fn allow_vary_on(&self) -> AllowVaryOn {
        match self.get_by_id("avo") {
            Some(Value::Composite { text, .. }) | Some(Value::Str(text)) => {
                AllowVaryOn::parse(&text).unwrap_or_else(AllowVaryOn::auto)
            }
            _ => AllowVaryOn::auto(),
        }
    }

    /// Sets the Vary policy.
    pub fn set_allow_vary_on(&mut self, value: AllowVaryOn) {
        let text = if value.auto {
            "Auto".to_string()
        } else {
            let mut parts = Vec::new();
            if value.accept {
                parts.push("Accept");
            }
            if value.save_data {
                parts.push("Save-Data");
            }
            if value.user_agent {
                parts.push("User-Agent");
            }
            if parts.is_empty() {
                "None".to_string()
            } else {
                parts.join(",")
            }
        };
        self.values.insert(
            "avo",
            Value::Composite {
                type_name: "AllowVaryOn",
                text,
            },
        );
        self.modified = true;
    }

    /// Whether Vary auto mode is on.
    #[must_use]
    pub fn allow_vary_on_auto(&self) -> bool {
        self.allow_vary_on().auto
    }

    /// Whether Vary: Accept is allowed.
    #[must_use]
    pub fn allow_vary_on_accept(&self) -> bool {
        self.allow_vary_on().accept
    }

    /// Whether Vary: Save-Data is allowed.
    #[must_use]
    pub fn allow_vary_on_save_data(&self) -> bool {
        self.allow_vary_on().save_data
    }

    /// Whether Vary: User-Agent is allowed.
    #[must_use]
    pub fn allow_vary_on_user_agent(&self) -> bool {
        self.allow_vary_on().user_agent
    }

    /// Whether Save-Data responses vary (`SupportSaveData`).
    #[must_use]
    pub fn support_save_data(&self) -> bool {
        self.has_valid_save_data_qualities() && self.allow_vary_on_save_data()
    }

    /// Whether HTML parsing prefetches IPRO rewrites
    /// (`in_place_preemptive_rewrite_images`, default true).
    #[must_use]
    pub fn in_place_preemptive_rewrite_images(&self) -> bool {
        self.image_bool("InPlacePreemptiveRewriteImages", true)
    }

    /// Sets the preemptive-rewrite flag.
    pub fn set_in_place_preemptive_rewrite_images(&mut self, value: bool) {
        self.set_image_bool("ippri", value);
    }

    /// Whether CSS images rewrite preemptively
    /// (`in_place_preemptive_rewrite_css_images`, default true).
    #[must_use]
    pub fn in_place_preemptive_rewrite_css_images(&self) -> bool {
        self.image_bool("InPlacePreemptiveRewriteCssImages", true)
    }

    /// Sets the CSS-image preemptive-rewrite flag.
    pub fn set_in_place_preemptive_rewrite_css_images(&mut self, value: bool) {
        self.set_image_bool("ipprci", value);
    }

    /// Whether nested CSS URLs trim (`trim_urls_in_css`, hardcoded
    /// true upstream as a test seam).
    #[must_use]
    pub fn trim_urls_in_css(&self) -> bool {
        true
    }

    /// Reads the random-drop percentage
    /// (`rewrite_random_drop_percentage`, default 0).
    #[must_use]
    pub fn rewrite_random_drop_percentage(&self) -> i64 {
        match self.get_by_name("RewriteRandomDropPercentage") {
            Some(Value::Int(percent)) => percent,
            _ => 0,
        }
    }

    /// Sets the random-drop percentage.
    pub fn set_rewrite_random_drop_percentage(&mut self, percent: i64) {
        self.values.insert("rrdp", Value::Int(percent));
        self.modified = true;
    }

    /// Reads the preemptive-CSS flag
    /// (`in_place_preemptive_rewrite_css`, default true).
    #[must_use]
    pub fn in_place_preemptive_rewrite_css(&self) -> bool {
        match self.get_by_name("InPlacePreemptiveRewriteCss") {
            Some(Value::Bool(preemptive)) => preemptive,
            _ => true,
        }
    }

    /// Sets the preemptive-CSS flag.
    pub fn set_in_place_preemptive_rewrite_css(&mut self, value: bool) {
        self.values.insert("ipprc", Value::Bool(value));
        self.modified = true;
    }

    /// Reads the combined-JS cap (`max_combined_js_bytes`, default
    /// 92160; negative means unbounded).
    #[must_use]
    pub fn max_combined_js_bytes(&self) -> i64 {
        match self.get_by_name("MaxCombinedJsBytes") {
            Some(Value::Int(limit)) => limit,
            _ => 92160,
        }
    }

    /// Sets the combined-JS cap.
    pub fn set_max_combined_js_bytes(&mut self, bytes: i64) {
        self.values.insert("xcj", Value::Int(bytes));
        self.modified = true;
    }

    /// Reads the JS outlining threshold (`js_outline_min_bytes`,
    /// default 3000).
    #[must_use]
    pub fn js_outline_min_bytes(&self) -> i64 {
        match self.get_by_name("JsOutlineMinBytes") {
            Some(Value::Int(bytes)) => bytes,
            _ => 3000,
        }
    }

    /// Sets the JS outlining threshold.
    pub fn set_js_outline_min_bytes(&mut self, bytes: i64) {
        self.values.insert("jo", Value::Int(bytes));
        self.modified = true;
    }

    /// Reads the combined-CSS cap (`max_combined_css_bytes`, default
    /// -1 = unbounded).
    #[must_use]
    pub fn max_combined_css_bytes(&self) -> i64 {
        match self.get_by_name("MaxCombinedCssBytes") {
            Some(Value::Int(limit)) => limit,
            _ => -1,
        }
    }

    /// Sets the combined-CSS cap.
    pub fn set_max_combined_css_bytes(&mut self, bytes: i64) {
        self.values.insert("xcc", Value::Int(bytes));
        self.modified = true;
    }

    /// Whether ID-permitting rules exist for CSS combining
    /// (`CssCombiningMayPermitIds`).
    #[must_use]
    pub fn css_combining_may_permit_ids(&self) -> bool {
        !self.css_combining_ids.is_empty()
    }

    /// Whether an id value may combine (`IsAllowedIdForCssCombining`).
    #[must_use]
    pub fn is_allowed_id_for_css_combining(&self, value: &str) -> bool {
        self.css_combining_ids.is_match(value, false)
    }

    /// Allows an id pattern for CSS combining
    /// (`PermitIdsForCssCombining`).
    pub fn permit_ids_for_css_combining(&mut self, pattern: &str) {
        self.css_combining_ids.allow(pattern);
        self.modified = true;
    }

    /// Reads the cross-path combining flag (`combine_across_paths`,
    /// default true).
    #[must_use]
    pub fn combine_across_paths(&self) -> bool {
        match self.get_by_name("CombineAcrossPaths") {
            Some(Value::Bool(allow)) => allow,
            _ => true,
        }
    }

    /// Disallows a URL pattern (`Disallow`).
    pub fn disallow(&mut self, pattern: &str) {
        self.allow_resources.disallow(pattern);
        self.modified = true;
    }

    /// Disallows a pattern except when inlining
    /// (`AllowOnlyWhenInlining`).
    pub fn allow_only_when_inlining(&mut self, pattern: &str) {
        self.disallow(pattern);
        self.allow_when_inlining(pattern);
    }

    /// Reads the URL options flag (`add_options_to_urls`).
    #[must_use]
    pub fn add_options_to_urls(&self) -> bool {
        matches!(
            self.get_by_name("AddOptionsToUrls"),
            Some(Value::Bool(true))
        )
    }

    /// Whether a URL may rewrite while inlining (`IsAllowedWhenInlining`,
    /// default deny).
    #[must_use]
    pub fn is_allowed_when_inlining(&self, url: &str) -> bool {
        self.allow_when_inlining_resources.is_match(url, false)
    }

    /// Whether a filter id is forbidden (`Forbidden(id)`; unknown
    /// ids are never forbidden).
    #[must_use]
    pub fn forbidden_by_id(&self, id: &str) -> bool {
        Filter::from_id(id).is_some_and(|filter| self.filters.forbidden(filter))
    }

    /// Whether a filter runs (`Enabled`).
    #[must_use]
    pub fn enabled(&self, filter: Filter) -> bool {
        self.filters.enabled(filter)
    }

    /// Disables a filter (`DisableFilter`).
    pub fn disable_filter(&mut self, filter: Filter) {
        self.filters.disable(filter);
        self.modified = true;
    }

    /// Enables a filter (`EnableFilter`, reviving soft-disables).
    ///
    /// NOTE: C++ `EnableFilter` only inserts into the enabled set,
    /// leaving the disabled set sticky — yet upstream tests
    /// soft-enable a filter and later `EnableDebug()` successfully.
    /// No static reading of the option sets explains that revival,
    /// so the port revives here to match observed behavior.
    pub fn enable_filter(&mut self, filter: Filter) {
        self.filters.undisable(filter);
        self.filters.enable(filter);
        self.modified = true;
    }

    /// Enables the cache-extension filters
    /// (`EnableExtendCacheFilters`; never PDFs).
    pub fn enable_extend_cache_filters(&mut self) {
        self.enable_filter(Filter::ExtendCacheCss);
        self.enable_filter(Filter::ExtendCacheImages);
        self.enable_filter(Filter::ExtendCacheScripts);
    }

    /// Enables a filter via the level, not the explicit set
    /// (`SoftEnableFilterForTesting`): preserve-urls can still
    /// override it.
    pub fn soft_enable_filter_for_testing(&mut self, filter: Filter) {
        if self.filters.level() == RewriteLevel::AllFilters {
            self.filters.unforbid(filter);
            self.filters.undisable(filter);
        } else {
            use crate::filter::FilterSet;
            let mut already_enabled = FilterSet::new();
            already_enabled.insert(filter);
            for candidate in Filter::ALL {
                if self.filters.enabled(candidate) {
                    already_enabled.insert(candidate);
                }
            }
            self.set_rewrite_level(RewriteLevel::AllFilters);
            for candidate in Filter::ALL {
                if !already_enabled.contains(candidate) {
                    self.filters.disable(candidate);
                }
            }
        }
        self.modified = true;
    }

    /// Sets the rewrite level (`SetRewriteLevel`).
    pub fn set_rewrite_level(&mut self, level: RewriteLevel) {
        self.values.insert("l", Value::Level(level));
        self.sync_special("l");
        self.modified = true;
    }

    /// Sets the default rewrite level (`SetDefaultRewriteLevel`):
    /// applies only when no level was explicitly set, and never
    /// marks the options modified.
    pub fn set_default_rewrite_level(&mut self, level: RewriteLevel) {
        if !self.values.contains_key("l") {
            self.filters.set_level(level);
        }
    }

    /// Caps request-built size thresholds at configured values
    /// (`LimitSizeThresholdsTo`): a set threshold above the
    /// ceiling (or negative, or zero where zero means unlimited)
    /// takes the ceiling's value.
    pub fn limit_size_thresholds_to(&mut self, ceiling: &RewriteOptions) {
        for (id, zero_is_unlimited) in [
            ("ii", false),
            ("cii", false),
            ("ci", false),
            ("ji", false),
            ("gfci", false),
            ("cf", true),
        ] {
            let mine = match self.values.get(id) {
                Some(Value::Int(value)) => *value,
                _ => continue,
            };
            let cap = match ceiling.get_by_id(id) {
                Some(Value::Int(value)) => value,
                _ => continue,
            };
            if mine > cap || mine < 0 || (zero_is_unlimited && mine == 0 && cap > 0) {
                self.values.insert(id, Value::Int(cap));
            }
        }
    }

    /// Sets the css-preserve-urls flag.
    pub fn set_css_preserve_urls(&mut self, value: bool) {
        self.values.insert("cpu", Value::Bool(value));
        self.sync_special("cpu");
        self.modified = true;
    }

    /// Sets the js-preserve-urls flag.
    pub fn set_js_preserve_urls(&mut self, value: bool) {
        self.values.insert("jpu", Value::Bool(value));
        self.sync_special("jpu");
        self.modified = true;
    }

    /// Sets the image-preserve-urls flag.
    pub fn set_image_preserve_urls(&mut self, value: bool) {
        self.values.insert("ipu", Value::Bool(value));
        self.sync_special("ipu");
        self.modified = true;
    }

    /// Reads the css-preserve-urls flag.
    #[must_use]
    pub fn css_preserve_urls(&self) -> bool {
        self.filters.css_preserve_urls()
    }

    /// Reads the js-preserve-urls flag.
    #[must_use]
    pub fn js_preserve_urls(&self) -> bool {
        self.filters.js_preserve_urls()
    }

    /// Reads the image-preserve-urls flag.
    #[must_use]
    pub fn image_preserve_urls(&self) -> bool {
        self.filters.image_preserve_urls()
    }

    /// Allows inlining a category without explicit authorization
    /// (`AddInlineUnauthorizedResourceType`).
    pub fn add_inline_unauthorized_resource_type(&mut self, category: Category) {
        if !self.inline_unauthorized_types.contains(&category) {
            self.inline_unauthorized_types.push(category);
            self.modified = true;
        }
    }

    /// Whether a category inlines unauthorized
    /// (`HasInlineUnauthorizedResourceType`).
    #[must_use]
    pub fn has_inline_unauthorized_resource_type(&self, category: Category) -> bool {
        self.inline_unauthorized_types.contains(&category)
    }

    /// Whether a URL may rewrite (`IsAllowed`, default allow).
    #[must_use]
    pub fn is_allowed(&self, url: &str) -> bool {
        self.allow_resources.is_match(url, true)
    }

    /// Allows a pattern for inlining (`AllowWhenInlining`).
    pub fn allow_when_inlining(&mut self, pattern: &str) {
        self.allow_when_inlining_resources.allow(pattern);
    }

    /// Blocks known-troublesome resources and lazyload-hostile classes
    /// (`DisallowTroublesomeResources`).
    pub fn disallow_troublesome_resources(&mut self) {
        self.allow_resources.disallow("*js_tinyMCE*");
        self.allow_resources.disallow("*tiny_mce*");
        self.allow_resources.disallow("*tinymce*");
        self.allow_resources.disallow("*scriptaculous.js*");
        self.allow_resources.disallow("*ckeditor*");
        self.allow_resources.disallow("*/wp-admin/*");
        self.disable_lazyload_for_class_name("*dfcg*");
        self.disable_lazyload_for_class_name("*lazy*");
        self.disable_lazyload_for_class_name("*nivo*");
        self.disable_lazyload_for_class_name("*slider*");
    }

    /// Borrows resource headers.
    #[must_use]
    pub fn resource_headers(&self) -> &[(String, String)] {
        &self.resource_headers
    }

    /// Borrows domain mappings.
    #[must_use]
    pub fn domain_mappings(&self) -> &[DomainMapping] {
        &self.domain_mappings
    }

    /// Borrows shard pairs (`ShardDomain`).
    #[must_use]
    pub fn shards(&self) -> &[(String, String)] {
        &self.shards
    }

    /// Borrows authorized domains (`Domain`).
    #[must_use]
    pub fn domains(&self) -> &[String] {
        &self.domains
    }

    /// Authorizes a domain (`AddDomain`).
    pub fn add_domain(&mut self, domain: &str) {
        self.domains.push(domain.to_string());
        self.modified = true;
    }

    /// Shards a domain (`AddShard`).
    pub fn add_shard(&mut self, domain: &str, shards: &str) {
        if !shards.is_empty() {
            self.shards.push((domain.to_string(), shards.to_string()));
        }
        self.modified = true;
    }

    /// Maps rewrite domains (`AddRewriteDomainMapping`).
    pub fn add_rewrite_domain_mapping(&mut self, to_domain: &str, from_domains: &str) {
        self.domain_mappings.push(DomainMapping {
            kind: DomainMappingKind::Rewrite,
            from: from_domains.to_string(),
            to: to_domain.to_string(),
            host_header: String::new(),
        });
        self.modified = true;
    }

    /// Maps a proxy domain (`AddProxyDomainMapping`): origin fetches
    /// are served under the proxy prefix.
    pub fn add_proxy_domain_mapping(&mut self, proxy_domain: &str, origin_domain: &str) {
        self.domain_mappings.push(DomainMapping {
            kind: DomainMappingKind::Proxy,
            from: proxy_domain.to_string(),
            to: origin_domain.to_string(),
            host_header: String::new(),
        });
        self.modified = true;
    }

    /// Registers a custom URL-valued attribute (`AddUrlValuedAttribute`).
    pub fn add_url_valued_attribute(&mut self, element: &str, attribute: &str, category: Category) {
        self.url_valued_attrs
            .push((element.to_string(), attribute.to_string(), category));
        self.modified = true;
    }

    /// Borrows custom URL-valued attributes (`UrlValuedAttribute`) as
    /// (element, attribute, category) triples.
    #[must_use]
    pub fn url_valued_attributes(&self) -> &[(String, String, Category)] {
        &self.url_valued_attrs
    }

    /// Borrows approved polyfill filenames (`HasApprovedPolyfillFilenames`
    /// is non-emptiness; membership is exact).
    #[must_use]
    pub fn approved_polyfill_filenames(&self) -> &std::collections::HashSet<String> {
        &self.polyfill_filenames
    }

    /// Borrows file-load policy.
    #[must_use]
    pub fn file_load_policy(&self) -> &FileLoadPolicy {
        &self.file_load
    }

    /// Borrows registered JS libraries.
    #[must_use]
    pub fn libraries(&self) -> &HashMap<i64, HashMap<String, String>> {
        &self.libraries
    }

    /// Borrows experiment specs (raw; validation is a follow-up).
    #[must_use]
    pub fn experiment_specs(&self) -> &[String] {
        &self.experiment_specs
    }

    /// Returns the experiment state letter (`GetExperimentStateStr`).
    ///
    /// Foundations track the selected spec position (spec-id parsing lands
    /// with `ExperimentSpec` validation); the observable select/round-trip
    /// contract matches C++.
    #[must_use]
    pub fn experiment_state_str(&self) -> String {
        match self.experiment_index {
            Some(i) if i < 26 => ((b'a' + i as u8) as char).to_string(),
            _ => String::new(),
        }
    }

    /// Selects an experiment by one-letter index (`SetExperimentStateStr`).
    ///
    /// Only a single character naming an existing spec position takes
    /// effect; anything else is ignored, as in C++.
    pub fn set_experiment_state_str(&mut self, index: &str) {
        if index.len() == 1 {
            let slot = index.as_bytes()[0].wrapping_sub(b'a') as usize;
            if slot < self.experiment_specs.len() {
                self.experiment_index = Some(slot);
            }
        }
    }
}

/// Extend-cache/preserve merge resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeOverride {
    None,
    DisablePreserve,
    DisableFilter,
}

/// Option-setting failure with C++-style outcome + message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetError {
    /// Machine outcome.
    pub outcome: SetOutcome,
    /// Human message (scope detail or validation reason).
    pub message: String,
}

fn invalid(message: &str) -> Result<(), SetError> {
    Err(SetError {
        outcome: SetOutcome::ValueInvalid,
        message: message.to_string(),
    })
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Query => "Query",
        Scope::Directory => "Directory",
        Scope::Server => "Server",
        Scope::LegacyProcess => "LegacyProcess",
        Scope::ProcessStrict => "ProcessStrict",
    }
}

/// Removed-but-recognized options (`AddDeprecatedProperty`): accepted and
/// ignored. Kept with scopes for the scope check.
const DEPRECATED_OPTIONS: &[(&str, Scope)] = &[
    ("MaxPrefetchJsElements", Scope::Directory),
    ("DistributeFetches", Scope::Server),
    ("DistributedRewriteKey", Scope::Server),
    ("DistributedRewriteServers", Scope::Server),
    ("DistributedRewriteTimeoutMs", Scope::Server),
];

/// Whether a name is a removed-but-recognized option
/// (`IsDeprecatedOptionName`; case-insensitive). Hosts that honor these
/// (Apache) warn and accept; other hosts report unknown.
#[must_use]
pub fn is_deprecated_option(name: &str) -> bool {
    DEPRECATED_OPTIONS
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// Options excluded from signature computation (marked in C++ right after
/// registration; recorded here for the later signature step).
pub const EXCLUDED_FROM_SIGNATURE: &[&str] = &["tifrwd", "noop"];

/// Renders a value (`OptionBase::ToString`).
fn value_to_string(value: Value) -> String {
    match value {
        Value::Bool(on) => {
            if on {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Int(number) => number.to_string(),
        Value::Str(text) => text,
        Value::Level(level) => level.name().to_string(),
        Value::Enabled(state) => state.name().to_string(),
        Value::Composite { text, .. } => text,
    }
}

/// Secret-bearing options redacted from `OptionsToString()`.
pub const SECRET_OPTIONS: &[&str] = &[
    "UrlSigningKey",
    "BlockingRewriteKey",
    "DownstreamCacheRebeaconingKey",
    "RequestOptionOverride",
    "StickyQueryParameters",
];

/// Hop-by-hop headers rejected by `AddResourceHeader`
/// (`HttpAttributes::SortedHopByHopHeaders` values).
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "Alt-Svc",
    "Alternate-Protocol",
    "Connection",
    "Keep-Alive",
    "Proxy-Authenticate",
    "Proxy-Authorization",
    "Set-Cookie",
    "Set-Cookie2",
    "TE",
    "Trailers",
    "Transfer-Encoding",
    "Upgrade",
];

/// Computed headers rejected by `AddResourceHeader`
/// (`InitFixedResourceHeaders` values).
const FIXED_RESOURCE_HEADERS: &[&str] = &[
    "Accept-Ranges",
    "Cache-Control",
    "Content-Encoding",
    "Content-Length",
    "Content-Type",
    "Date",
    "Etag",
    "Expires",
    "Last-Modified",
    "Link",
    "Server",
    "Vary",
];

/// RFC 2616 separators rejected in configured header names.
const HEADER_SEPARATORS: &str = "()<>@,;:\\\"/[]?={} \t";

/// Validates a configured header (`ValidateConfiguredHttpHeader`).
fn validate_http_header(name: &str, value: &str) -> Result<(), SetError> {
    if name.is_empty() {
        return invalid("Empty field name not allowed");
    }
    // NOTE: C++ sets "Field name too long" here but forgets `return false`,
    // so over-long names fall through to the character checks and may still
    // be accepted. Mirrored exactly: no rejection on name length.
    if value.len() > 1024 {
        return invalid("Field value too long");
    }
    for byte in name.bytes() {
        if !is_non_control_ascii(byte) {
            return invalid(&format!(
                "Invalid character in field name: {}",
                byte as char
            ));
        }
        if HEADER_SEPARATORS.as_bytes().contains(&byte) {
            return invalid(&format!("Separator found in field name: {}", byte as char));
        }
    }
    for byte in value.bytes() {
        if !is_non_control_ascii(byte) {
            return invalid(&format!(
                "Invalid character in field value: {}",
                byte as char
            ));
        }
    }
    Ok(())
}

fn is_non_control_ascii(byte: u8) -> bool {
    (0x20..0x7f).contains(&byte)
}

/// Registers a JS library (`JavascriptLibraryIdentification::RegisterLibrary`).
///
/// md5 must be `[A-Za-z0-9_-]*` (empty allowed, as in C++ which only rejects
/// bad chars) and the URL must resolve against `http://www.example.com/`.
/// New entries override previous ones for the same `(bytes, md5)` key.
fn register_library(
    libraries: &mut HashMap<i64, HashMap<String, String>>,
    bytes: i64,
    md5: &str,
    url: &str,
) -> bool {
    if !md5
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return false;
    }
    if !resolves_against_example(url) {
        return false;
    }
    libraries
        .entry(bytes)
        .or_default()
        .insert(md5.to_string(), url.to_string());
    true
}

/// Conservative URL check: absolute `http(s)://host/...` or a relative
/// reference (both resolve against the example base in C++).
fn resolves_against_example(url: &str) -> bool {
    if url.is_empty() || url.bytes().any(|b| b.is_ascii_whitespace()) {
        return false;
    }
    if let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    {
        // Needs a non-empty host.
        let host = rest.split('/').next().unwrap_or("");
        return !host.is_empty() && !host.contains([' ', '\t']);
    }
    // Relative references resolve fine; reject other schemes conservatively
    // the way `GoogleUrl::IsWebValid` rejects non-web schemes.
    !url.contains("://")
}

/// Whether a string is a web-valid absolute URL (for the downstream purge
/// prefix gate; mirrors `GoogleUrl::IsWebValid` for http/https).
fn is_web_url(url: &str) -> bool {
    url.strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .is_some_and(|rest| {
            let host = rest.split('/').next().unwrap_or("");
            !host.is_empty() && !host.bytes().any(|b| b.is_ascii_whitespace())
        })
}

/// Extracts `host[:port]` from an absolute URL.
fn url_host_port(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    Some(rest.split('/').next().unwrap_or("").to_string())
}

/// Conservative `ExperimentSpec::has_alternate_origin_domains` gate: true
/// when a `;`-separated `key=value` entry names `alternate_origin_domain`.
fn spec_has_alternate_origin(spec: &str) -> bool {
    spec.split(';').any(|entry| {
        entry
            .split_once('=')
            .is_some_and(|(key, _)| key.trim() == "alternate_origin_domain")
    })
}

/// Strips excess URL quotes (`TrimUrlQuotes`).
#[must_use]
pub fn trim_url_quotes(value: &str) -> &str {
    value.trim().trim_matches('"').trim_matches('\'')
}

/// Parses a scalar/composite value for a property (`SetFromString`).
fn parse_value(prop: &Property, text: &str) -> Option<Value> {
    match prop.kind {
        PropKind::Bool => parse_bool(text).map(Value::Bool),
        PropKind::Int => {
            let value = parse_int64(text)?;
            if value < i64::from(i32::MIN) || value > i64::from(i32::MAX) {
                return None;
            }
            Some(Value::Int(value))
        }
        PropKind::Int64 => parse_int64(text).map(Value::Int),
        PropKind::Str => Some(Value::Str(text.to_string())),
        PropKind::Level => RewriteLevel::parse(text).map(Value::Level),
        PropKind::Enabled => EnabledState::parse(text).map(Value::Enabled),
        PropKind::Other(type_name) => parse_composite(type_name, text),
    }
}

/// Parses composite option values by C++ type.
fn parse_composite(type_name: &'static str, text: &str) -> Option<Value> {
    match type_name {
        "AllowVaryOn" => AllowVaryOn::parse(text).map(|v| Value::Composite {
            type_name,
            text: canonical_allow_vary_on(&v),
        }),
        "BeaconUrl" => BeaconUrl::parse(text).map(|v| Value::Composite {
            type_name,
            text: format!("{} {}", v.http, v.https),
        }),
        "CacheFragmentOption" => valid_cache_fragment(text).then(|| Value::Composite {
            type_name,
            text: text.to_string(),
        }),
        "PolyfillFilenameOption" => valid_polyfill_filename(text).then(|| Value::Composite {
            type_name,
            text: text.to_string(),
        }),
        "ResourceCategorySet" => parse_resource_categories(text).map(|set| Value::Composite {
            type_name,
            text: canonical_categories(&set),
        }),
        "ResponsiveDensities" => parse_densities(text).map(|d| Value::Composite {
            type_name,
            text: d
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        }),
        _ => None,
    }
}

fn canonical_allow_vary_on(value: &AllowVaryOn) -> String {
    if value.auto {
        return "Auto".to_string();
    }
    let mut parts = Vec::new();
    if value.accept {
        parts.push("Accept");
    }
    if value.save_data {
        parts.push("Save-Data");
    }
    if value.user_agent {
        parts.push("User-Agent");
    }
    if parts.is_empty() {
        "None".to_string()
    } else {
        parts.join(",")
    }
}

fn canonical_categories(set: &HashSet<Category>) -> String {
    let mut names: Vec<&str> = set.iter().map(|category| category.name()).collect();
    names.sort_unstable();
    names.join(",")
}

/// Applies one `pagespeed` directive line
/// (`NgxRewriteOptions::ParseAndSetOptions`, core subset).
///
/// `directive` is the word after `pagespeed`, `args` the remaining words
/// (at most 4; nginx enforces `TAKE1..TAKE5`), `scope` the context the line
/// appears in. Handles the bare on/off switch (with the nginx `off` →
/// `unplugged` mapping), `gzip`, factory options, and 1/2/3-arg core
/// dispatch after the `GetOptionScope` gate.
///
/// System/factory `ParseAndSetOption{1,2}` fallbacks (FileCachePath,
/// MemcachedServers, …) and script-variable compilation land with the
/// system-options and nginx steps; such names report `NameUnknown` here.
///
/// Enforces the nginx scope gate for names dispatched outside the
/// option parser (factory directives): strips any `ModPagespeed`
/// prefix, then the name must resolve at or below the scope.
// NOTE: `apply_nginx_directive` shares this pre-check verbatim.
pub fn check_nginx_scope(directive: &str, scope: Scope) -> Result<(), SetError> {
    let directive = strip_mod_pagespeed_prefix(directive);
    if nginx_option_scope(directive) > scope {
        return Err(SetError {
            outcome: SetOutcome::NameUnknown,
            message: nginx_config_error(directive, "cannot be set at this scope."),
        });
    }
    Ok(())
}

/// Formats an nginx config error (`ps_error_string_for_option`):
/// the directive (or full line) in double quotes, a space, then
/// the detail. An empty detail keeps its trailing space, as in
/// C++ (bool/int parse failures leave `msg` empty).
#[must_use]
pub fn nginx_config_error(quoted: &str, detail: &str) -> String {
    format!("\"{quoted}\" {detail}")
}

/// Applies the result-switch formats to an option-table error:
/// unknown names quote the directive, bad values wrap the full
/// line around the table's own detail.
fn wrap_nginx_table_error(directive: &str, full_line: &str, err: &mut SetError) {
    if err.outcome == SetOutcome::NameUnknown {
        err.message = nginx_config_error(directive, "not recognized or too many arguments");
    } else {
        let detail = std::mem::take(&mut err.message);
        err.message = nginx_config_error(full_line, &detail);
    }
}

/// Admin-path storage slot (`NgxRewriteOptions` string properties),
/// matched case-sensitively like the option table.
fn admin_path_slot<'a>(
    system: &'a mut NginxSystemOptions,
    directive: &str,
) -> Option<&'a mut String> {
    if directive == "StatisticsPath" {
        Some(&mut system.statistics_path)
    } else if directive == "GlobalStatisticsPath" {
        Some(&mut system.global_statistics_path)
    } else if directive == "ConsolePath" {
        Some(&mut system.console_path)
    } else if directive == "MessagesPath" {
        Some(&mut system.messages_path)
    } else if directive == "AdminPath" {
        Some(&mut system.admin_path)
    } else if directive == "GlobalAdminPath" {
        Some(&mut system.global_admin_path)
    } else {
        None
    }
}

/// Admin-domain group slot (`SystemRewriteOptions` wildcard groups),
/// matched case-insensitively.
fn admin_domain_slot<'a>(
    system: &'a mut NginxSystemOptions,
    directive: &str,
) -> Option<&'a mut WildcardGroup> {
    if directive.eq_ignore_ascii_case("StatisticsDomains") {
        Some(&mut system.statistics_domains)
    } else if directive.eq_ignore_ascii_case("GlobalStatisticsDomains") {
        Some(&mut system.global_statistics_domains)
    } else if directive.eq_ignore_ascii_case("MessagesDomains") {
        Some(&mut system.messages_domains)
    } else if directive.eq_ignore_ascii_case("ConsoleDomains") {
        Some(&mut system.console_domains)
    } else if directive.eq_ignore_ascii_case("AdminDomains") {
        Some(&mut system.admin_domains)
    } else if directive.eq_ignore_ascii_case("GlobalAdminDomains") {
        Some(&mut system.global_admin_domains)
    } else {
        None
    }
}

pub fn apply_nginx_directive(
    options: &mut RewriteOptions,
    factory: &mut NginxFactoryOptions,
    system: &mut NginxSystemOptions,
    directive: &str,
    args: &[&str],
    scope: Scope,
) -> Result<(), SetError> {
    // The gate runs before the arity switch in C++, so an
    // over-long mis-scoped line reports the scope error.
    check_nginx_scope(directive, scope)?;
    // The strip mutates a copy: the gate and the dispatch match
    // the stripped name, but the invalid-wrap joins the raw
    // words (a `ModPagespeed` prefix survives in it).
    let raw_directive = directive;
    let directive = strip_mod_pagespeed_prefix(directive);
    let full_line = if args.is_empty() {
        raw_directive.to_string()
    } else {
        format!("{raw_directive} {}", args.join(" "))
    };
    if args.len() > 4 {
        // C++ `CHECK`-aborts past five words; the port reports
        // the `n_args == 5` unknown-name error instead.
        return Err(SetError {
            outcome: SetOutcome::NameUnknown,
            message: nginx_config_error(directive, "not recognized or too many arguments"),
        });
    }
    // One-word lines: master switch, ClearInheritedScripts, or gzip state.
    if args.is_empty() {
        // `ClearInheritedScripts` is a per-level flag on the nginx
        // options object (`clear_inherited_scripts_`, read by
        // `ps_merge_options`); the ps-nginx directive wrapper
        // records it there, so parsing is a no-op here.
        if directive.eq_ignore_ascii_case("ClearInheritedScripts") {
            return Ok(());
        }
        return match EnabledState::parse(directive) {
            Some(EnabledState::Off) => {
                // Historical nginx mapping: "off" means "unplugged".
                options.set_enabled_state(EnabledState::Unplugged);
                Ok(())
            }
            Some(state) => {
                options.set_enabled_state(state);
                Ok(())
            }
            None => Err(SetError {
                outcome: SetOutcome::NameUnknown,
                message: nginx_config_error(directive, "not recognized or too many arguments"),
            }),
        };
    }
    // NOTE: `pagespeed gzip on|off` never reaches this dispatcher:
    // `ps_configure` handles it inline (setter call + `NGX_CONF_OK`),
    // so there is deliberately no branch for it here.
    match args.len() {
        1 => {
            // `NgxRewriteOptions::ParseAndSetOptionFromName1`: FileCachePath
            // needs the leading slash (ps-nginx runs the directory prep
            // first, so this is near-dead defense); LogDir stores as-is.
            // The detail is verbatim; the switch wraps it with the
            // full line. Names match case-sensitively: past the
            // slash check these fall through to the exact-case
            // option table.
            if directive == "FileCachePath" {
                if !args[0].starts_with('/') {
                    return Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, "must start with a slash"),
                    });
                }
                system.file_cache_path = args[0].to_string();
                return Ok(());
            }
            if directive == "LogDir" {
                system.log_dir = args[0].to_string();
                return Ok(());
            }
            // The six admin paths (`NgxRewriteOptions::AddProperties`)
            // plus `PurgeMethod` (`SystemRewriteOptions`): stored
            // verbatim, like any string property (exact-case like
            // the table they live in).
            if let Some(slot) = admin_path_slot(system, directive) {
                *slot = args[0].to_string();
                return Ok(());
            }
            if directive == "PurgeMethod" {
                system.purge_method = args[0].to_string();
                return Ok(());
            }
            if directive.eq_ignore_ascii_case("UseNativeFetcher") {
                return match parse_bool(args[0]) {
                    Some(value) => {
                        factory.use_native_fetcher = value;
                        Ok(())
                    }
                    // The helper failure leaves `msg` empty, so the
                    // switch emits the quoted line plus a bare space.
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            if directive.eq_ignore_ascii_case("NativeFetcherMaxKeepaliveRequests") {
                // `StringToInt` plus a positive check; the failure
                // detail is empty, as above.
                return match parse_int32(args[0]) {
                    Some(n) if n > 0 => {
                        factory.native_fetcher_max_keepalive = i64::from(n);
                        Ok(())
                    }
                    _ => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // `Statistics` (`SystemRewriteOptions`, server scope,
            // default on).
            if directive == "Statistics" {
                return match parse_bool(args[0]) {
                    Some(value) => {
                        system.statistics_enabled = value;
                        system.was_set.statistics_enabled = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // `StatisticsLogging` (`SystemRewriteOptions`, server
            // scope, default off; only takes effect with `LogDir`).
            if directive == "StatisticsLogging" {
                return match parse_bool(args[0]) {
                    Some(value) => {
                        system.statistics_logging_enabled = value;
                        system.was_set.statistics_logging_enabled = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // `StatisticsLoggingIntervalMs` /
            // `StatisticsLoggingMaxFileSizeKb` (`SystemRewriteOptions`,
            // server scope; C++ parses without range checks, so any
            // 64-bit value stores and the dump driver clamps).
            if directive == "StatisticsLoggingIntervalMs"
                || directive == "StatisticsLoggingMaxFileSizeKb"
            {
                return match parse_int64(args[0]) {
                    Some(value) => {
                        if directive == "StatisticsLoggingIntervalMs" {
                            system.statistics_logging_interval_ms = value;
                            system.was_set.statistics_logging_interval_ms = true;
                        } else {
                            system.statistics_logging_max_file_size_kb = value;
                            system.was_set.statistics_logging_max_file_size_kb = true;
                        }
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // `MemcachedServers` / `RedisServer`
            // (`SystemRewriteOptions`, server scope): validated
            // specs, verbatim details wrapped with the full line.
            if directive == "MemcachedServers" {
                return match parse_external_cluster(args[0], MEMCACHED_DEFAULT_PORT) {
                    Ok(servers) => {
                        system.memcached_servers = servers;
                        Ok(())
                    }
                    Err(detail) => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, &detail),
                    }),
                };
            }
            if directive == "RedisServer" {
                return match parse_external_server(args[0], REDIS_DEFAULT_PORT) {
                    Ok(server) => {
                        system.redis_server = Some(server);
                        Ok(())
                    }
                    Err(detail) => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, &detail),
                    }),
                };
            }
            // `FetchProxy` (`SystemRewriteOptions`, server scope):
            // stored verbatim, like any string property.
            if directive == "FetchProxy" {
                system.fetcher_proxy = args[0].to_string();
                system.was_set.fetcher_proxy = true;
                return Ok(());
            }
            // `FetchWithGzip` (`SystemRewriteOptions`, legacy
            // process scope, default off).
            if directive == "FetchWithGzip" {
                return match parse_bool(args[0]) {
                    Some(value) => {
                        system.fetch_with_gzip = value;
                        system.was_set.fetch_with_gzip = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // `FetcherTimeOutMs` (`RewriteOptions`, directory
            // scope in the table but server-gated for nginx,
            // default 5000): note the capital O.
            if directive == "FetcherTimeOutMs" {
                return match parse_int64(args[0]) {
                    Some(value) => {
                        system.blocking_fetch_timeout_ms = value;
                        system.was_set.blocking_fetch_timeout_ms = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // Cache-cleaning trio (`SystemRewriteOptions`, server
            // scope): interval (hour default, -1 disables), size
            // target (100 MiB default), inode limit (0 default;
            // the factory ctor raises its own copy to 500000).
            if directive == "FileCacheCleanIntervalMs" {
                return match parse_int64(args[0]) {
                    Some(value) => {
                        system.file_cache_clean_interval_ms = value;
                        system.was_set.clean_interval = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            if directive == "FileCacheSizeKb" {
                return match parse_int64(args[0]) {
                    Some(value) => {
                        system.file_cache_clean_size_kb = value;
                        system.was_set.clean_size = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // LRU sizing (`SystemRewriteOptions`, server scope,
            // both default 0).
            if directive == "LRUCacheByteLimit" {
                return match parse_int64(args[0]) {
                    Some(value) => {
                        system.lru_cache_byte_limit = value;
                        system.was_set.lru_byte_limit = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            if directive == "LRUCacheKbPerProcess" {
                return match parse_int64(args[0]) {
                    Some(value) => {
                        system.lru_cache_kb_per_process = value;
                        system.was_set.lru_kb_per_process = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // Flush/purge names (`SystemRewriteOptions`, server
            // scope): empty filename and off by default.
            if directive == "CacheFlushFilename" {
                system.cache_flush_filename = args[0].to_string();
                system.was_set.flush_filename = true;
                return Ok(());
            }
            // `EnableCachePurge` is a core property (`euci`): it
            // falls through to the table below.
            // `CompressMetadataCache` (`SystemRewriteOptions`,
            // server scope, default on).
            if directive == "CompressMetadataCache" {
                return match parse_bool(args[0]) {
                    Some(value) => {
                        system.compress_metadata_cache = value;
                        system.was_set.compress_metadata = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // `FileCacheInodeLimit` (`SystemRewriteOptions`, server
            // scope, default 0): the factory ctor raises it to
            // 500000; the config line stores a 64-bit value.
            if directive == "FileCacheInodeLimit" {
                return match parse_int64(args[0]) {
                    Some(n) => {
                        system.file_cache_clean_inode_limit = n;
                        system.was_set.clean_inode = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            // In-place recording caps (`SystemRewriteOptions`,
            // legacy process scope): response size (10 MiB
            // default, 0 unlimited) and concurrent recordings
            // (10 default, 0 unlimited).
            if directive == "IproMaxResponseBytes" {
                return match parse_int64(args[0]) {
                    Some(n) => {
                        system.ipro_max_response_bytes = n;
                        system.was_set.ipro_max_response_bytes = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            if directive == "IproMaxConcurrentRecordings" {
                return match parse_int64(args[0]) {
                    Some(n) => {
                        system.ipro_max_concurrent_recordings = n;
                        system.was_set.ipro_max_concurrent_recordings = true;
                        Ok(())
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: nginx_config_error(&full_line, ""),
                    }),
                };
            }
            if directive.eq_ignore_ascii_case("ProcessScriptVariables") {
                if scope != Scope::ProcessStrict {
                    return Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: "ProcessScriptVariables is only allowed \
                                  at the top level"
                            .to_string(),
                    });
                }
                return match ScriptVariables::parse(args[0]) {
                    Some(mode) => {
                        if factory.script_variables_set {
                            Err(SetError {
                                outcome: SetOutcome::ValueInvalid,
                                message: "pagespeed ProcessScriptVariables: \
                                          can only be set once"
                                    .to_string(),
                            })
                        } else {
                            factory.script_variables = mode;
                            factory.script_variables_set = true;
                            Ok(())
                        }
                    }
                    None => Err(SetError {
                        outcome: SetOutcome::ValueInvalid,
                        message: "pagespeed ProcessScriptVariables: invalid value".to_string(),
                    }),
                };
            }
            options
                .parse_and_set_1(directive, args[0], Scope::ProcessStrict)
                .map_err(|mut err| {
                    // Core scope checks use process scope here (nginx already
                    // applied its own gate).
                    wrap_nginx_table_error(directive, &full_line, &mut err);
                    err
                })
        }
        2 => {
            // Admin-domain lists (`SystemRewriteOptions::
            // ParseAndSetOptionFromName2`): `allow`/`disallow` plus a
            // pattern, with verbatim error text.
            if let Some(group) = admin_domain_slot(system, directive) {
                if args[0].eq_ignore_ascii_case("allow") {
                    group.allow(args[1]);
                    return Ok(());
                }
                if args[0].eq_ignore_ascii_case("disallow") {
                    group.disallow(args[1]);
                    return Ok(());
                }
                return Err(SetError {
                    outcome: SetOutcome::ValueInvalid,
                    message: nginx_config_error(
                        &full_line,
                        &format!("expected 'allow' or 'disallow', got '{}'", args[0]),
                    ),
                });
            }
            options
                .parse_and_set_2(directive, args[0], args[1])
                .map_err(|mut err| {
                    wrap_nginx_table_error(directive, &full_line, &mut err);
                    err
                })
        }
        3 => options
            .parse_and_set_3(directive, args[0], args[1], args[2])
            .map_err(|mut err| {
                wrap_nginx_table_error(directive, &full_line, &mut err);
                err
            }),
        _ => Err(SetError {
            outcome: SetOutcome::NameUnknown,
            message: nginx_config_error(directive, "not recognized or too many arguments"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_property_by_id() {
        let mut options = RewriteOptions::new();
        options.set_request_property("pcu", "x").unwrap();
        assert_eq!(options.get_by_id("pcu"), Some(Value::Str("x".to_string())));
        assert!(options.set_request_property("fh", "true").is_err());
        assert!(options.set_request_property("zz", "x").is_err());
    }
    use crate::filter::Filter;

    #[test]
    fn defaults_and_html_writer() {
        let mut options = RewriteOptions::new();
        assert!(!options.modified());
        assert_eq!(options.enabled_state(), EnabledState::On);
        assert_eq!(0, options.max_html_cache_time_ms());
        options.set_max_html_cache_time_ms(60_000);
        assert_eq!(60_000, options.max_html_cache_time_ms());
        assert!(!options.show_cache_timestamp());
        options.set_show_cache_timestamp(true);
        assert!(options.show_cache_timestamp());
        // HtmlWriterFilter is enabled by default, as in C++ Initialize().
        assert!(options.filters().enabled(Filter::HtmlWriterFilter));
        assert_eq!(
            options.get_by_name("RewriteLevel"),
            Some(Value::Level(RewriteLevel::PassThrough))
        );
        assert_eq!(options.get_by_id("ci"), Some(Value::Int(2048)));
        assert!(!options.was_set("ci"));
        assert_eq!(options.get_by_id("zz"), None);
        assert_eq!(options.get_by_name("Bogus"), None);
    }

    #[test]
    fn scalar_parsing() {
        assert_eq!(parse_bool("on"), Some(true));
        assert_eq!(parse_bool("OFF"), Some(false));
        assert_eq!(parse_bool("yes"), None);
        assert_eq!(parse_int64("2048"), Some(2048));
        assert_eq!(parse_int64("-1"), Some(-1));
        assert_eq!(parse_int64(""), None);
        assert_eq!(parse_int64("12x"), None);
        assert_eq!(parse_int64(" 12"), None);
        assert_eq!(parse_int32("8080"), Some(8080));
        assert_eq!(parse_int32("-1"), Some(-1));
        assert_eq!(parse_int32("+80"), Some(80));
        assert_eq!(parse_int32(""), None);
        assert_eq!(parse_int32("80 "), None);
        assert_eq!(parse_int32("0x10"), None);
        assert_eq!(parse_int32("99999999999"), None);
        assert_eq!(EnabledState::parse("standby"), Some(EnabledState::Standby));
        assert_eq!(
            EnabledState::parse("unplugged"),
            Some(EnabledState::Unplugged)
        );
        assert_eq!(EnabledState::parse("bogus"), None);
    }

    #[test]
    fn composite_parsing() {
        // Densities: sorted, positive, non-empty.
        assert_eq!(parse_densities("2,1.5,3.0"), Some(vec![1.5, 2.0, 3.0]));
        assert_eq!(parse_densities(""), None);
        assert_eq!(parse_densities("0"), None);
        assert_eq!(parse_densities("x"), None);
        // Resource categories with off-clearing.
        let set = parse_resource_categories("Script,Stylesheet").unwrap();
        assert!(set.contains(&Category::Script));
        assert_eq!(
            parse_resource_categories("Script,off"),
            Some(HashSet::new())
        );
        assert_eq!(parse_resource_categories("Bogus"), None);
        // AllowVaryOn.
        assert_eq!(AllowVaryOn::parse("Auto"), Some(AllowVaryOn::auto()));
        assert_eq!(AllowVaryOn::parse("None"), Some(AllowVaryOn::default()));
        let both = AllowVaryOn::parse("Accept, User-Agent").unwrap();
        assert!(both.accept && both.user_agent && !both.save_data);
        assert_eq!(AllowVaryOn::parse(""), None);
        assert_eq!(AllowVaryOn::parse("Bogus"), None);
        // Beacon URLs.
        let beacon = BeaconUrl::parse("/mod_pagespeed_beacon").unwrap();
        assert_eq!(beacon.http, "/mod_pagespeed_beacon");
        assert_eq!(beacon.https, "/mod_pagespeed_beacon");
        let pair = BeaconUrl::parse("http://a/b https://c/d").unwrap();
        assert_eq!(pair.http, "http://a/b");
        assert_eq!(pair.https, "https://c/d");
        let derived = BeaconUrl::parse("http://a/b?x=1&ets=").unwrap();
        assert_eq!(derived.https, "https://a/b?x=1&ets=");
        assert_eq!(derived.http_in, "http://a/b");
        assert_eq!(BeaconUrl::parse(""), None);
        assert_eq!(BeaconUrl::parse("a b c"), None);
        // Fragments and filenames.
        assert!(valid_cache_fragment("good_frag-1"));
        assert!(valid_cache_fragment(""));
        assert!(!valid_cache_fragment("evil.com"));
        assert!(!valid_cache_fragment("a/b"));
        assert!(valid_polyfill_filename("polyfill.min.js"));
        assert!(!valid_polyfill_filename(""));
        assert!(!valid_polyfill_filename(".."));
        assert!(!valid_polyfill_filename("a/b"));
    }

    #[test]
    fn set_option_scopes_and_sync() {
        let mut options = RewriteOptions::new();
        options
            .set_option("CssInlineMaxBytes", "4096", Scope::Directory)
            .unwrap();
        assert_eq!(options.get_by_id("ci"), Some(Value::Int(4096)));
        assert!(options.was_set("ci"));
        assert!(options.modified());
        // Scope violation reports NameUnknown, as in C++.
        let err = options
            .set_option("RemoteConfigurationTimeoutMs", "5", Scope::Directory)
            .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        assert!(err.message.contains("Maximum allowed scope"));
        // Unknown name.
        let err = options
            .set_option("Bogus", "1", Scope::ProcessStrict)
            .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        // Bad value.
        let err = options
            .set_option("CssInlineMaxBytes", "x", Scope::ProcessStrict)
            .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::ValueInvalid);
        // Setting level/preserve properties syncs filter state.
        options
            .set_option("RewriteLevel", "CoreFilters", Scope::ProcessStrict)
            .unwrap();
        assert!(options.filters().enabled(Filter::RewriteCss));
        options
            .set_option("CssPreserveURLs", "on", Scope::ProcessStrict)
            .unwrap();
        assert!(!options.filters().enabled(Filter::CombineCss));
        options
            .set_option("EnableRewriting", "standby", Scope::ProcessStrict)
            .unwrap();
        assert_eq!(options.enabled_state(), EnabledState::Standby);
    }

    #[test]
    fn special_one_arg_directives() {
        let mut options = RewriteOptions::new();
        options
            .parse_and_set_1("Allow", "*.x/*", Scope::Directory)
            .unwrap();
        options
            .parse_and_set_1("Disallow", "*.x/y/*", Scope::Directory)
            .unwrap();
        assert!(options.allow_resources().is_match("http://a.x/z", false));
        assert!(!options.allow_resources().is_match("http://a.x/y/z", true));
        options
            .parse_and_set_1("RetainComment", "*keep*", Scope::Directory)
            .unwrap();
        assert!(options
            .retain_comments()
            .is_match("please keep this", false));
        options
            .parse_and_set_1(
                "DownstreamCachePurgeLocationPrefix",
                "http://purge.local:8080/purge",
                Scope::Directory,
            )
            .unwrap();
        assert!(options
            .parse_and_set_1(
                "DownstreamCachePurgeLocationPrefix",
                "not a url",
                Scope::Directory,
            )
            .is_err());
        options
            .parse_and_set_1("ExperimentVariable", "3", Scope::Directory)
            .unwrap();
        assert_eq!(options.get_by_id("fga"), Some(Value::Int(3)));
        assert!(options
            .parse_and_set_1("ExperimentVariable", "9", Scope::Directory)
            .is_err());
        // ExperimentSpec scope gate for alternate origins.
        assert!(options
            .parse_and_set_1(
                "ExperimentSpec",
                "id=1;alternate_origin_domain=a.com",
                Scope::Directory,
            )
            .is_err());
        options
            .parse_and_set_1("ExperimentSpec", "id=1", Scope::Directory)
            .unwrap();
        assert_eq!(options.experiment_specs(), &["id=1".to_string()]);
        // Deprecated names are NOT accepted by core dispatch.
        let err = options
            .parse_and_set_1("DistributeFetches", "x", Scope::ProcessStrict)
            .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        assert!(is_deprecated_option("distributeFetches"));
    }

    #[test]
    fn two_and_three_arg_directives() {
        let mut options = RewriteOptions::new();
        options
            .parse_and_set_2("AddResourceHeader", "X-Test", "yes")
            .unwrap();
        assert_eq!(
            options.resource_headers(),
            &[("X-Test".to_string(), "yes".to_string())]
        );
        // Hop-by-hop and computed headers rejected.
        assert!(options
            .parse_and_set_2("AddResourceHeader", "Connection", "x")
            .is_err());
        assert!(options
            .parse_and_set_2("AddResourceHeader", "Cache-Control", "x")
            .is_err());
        assert!(options
            .parse_and_set_2("AddResourceHeader", "", "x")
            .is_err());
        options
            .parse_and_set_2("LoadFromFileRule", "Allow", "/safe/*")
            .unwrap();
        assert!(options
            .parse_and_set_2("LoadFromFileRule", "Maybe", "/x")
            .is_err());
        options.parse_and_set_2("ShardDomain", "a.com", "").unwrap();
        assert!(options.shards.is_empty()); // empty shard disables
        options
            .parse_and_set_2("ShardDomain", "a.com", "s1.com,s2.com")
            .unwrap();
        assert_eq!(options.shards.len(), 1);
        assert_eq!(
            options.shards(),
            &[("a.com".to_string(), "s1.com,s2.com".to_string())]
        );
        options
            .parse_and_set_3("UrlValuedAttribute", "span", "src", "Hyperlink")
            .unwrap();
        assert!(options
            .parse_and_set_3("UrlValuedAttribute", "span", "src", "Bogus")
            .is_err());
        options
            .parse_and_set_3(
                "Library",
                "43567",
                "5giEj_jl-Ag5G8",
                "http://www.example.com/url.js",
            )
            .unwrap();
        assert!(options
            .parse_and_set_3("Library", "-1", "x", "http://e.com/u.js")
            .is_err());
        assert!(options
            .parse_and_set_3("Library", "1", "bad md5!", "http://e.com/u.js")
            .is_err());
        // Override semantics: same key replaces.
        options
            .parse_and_set_3(
                "Library",
                "43567",
                "5giEj_jl-Ag5G8",
                "http://www.example.com/other.js",
            )
            .unwrap();
        assert_eq!(
            options.libraries()[&43567]["5giEj_jl-Ag5G8"],
            "http://www.example.com/other.js"
        );
    }

    #[test]
    fn nginx_directive_entry() {
        let mut options = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = NginxSystemOptions::default();
        // Bare switch with nginx off->unplugged mapping.
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "on",
            &[],
            Scope::Directory,
        )
        .unwrap();
        assert_eq!(options.enabled_state(), EnabledState::On);
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "off",
            &[],
            Scope::Directory,
        )
        .unwrap();
        assert_eq!(options.enabled_state(), EnabledState::Unplugged);
        // Scope gate: server-only names rejected at directory scope.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetchProxy",
            &["http://proxy/"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        assert_eq!(err.message, "\"FetchProxy\" cannot be set at this scope.");
        // Core option through nginx dispatch.
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "ModPagespeedCssInlineMaxBytes",
            &["1024"],
            Scope::Directory,
        )
        .unwrap();
        assert_eq!(options.get_by_id("ci"), Some(Value::Int(1024)));
        // Factory options.
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "UseNativeFetcher",
            &["on"],
            Scope::ProcessStrict,
        )
        .unwrap();
        assert!(factory.use_native_fetcher);
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "NativeFetcherMaxKeepaliveRequests",
            &["50"],
            Scope::ProcessStrict,
        )
        .unwrap();
        assert_eq!(factory.native_fetcher_max_keepalive, 50);
        assert!(apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "NativeFetcherMaxKeepaliveRequests",
            &["0"],
            Scope::ProcessStrict,
        )
        .is_err());
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "ProcessScriptVariables",
            &["all"],
            Scope::ProcessStrict,
        )
        .unwrap();
        assert_eq!(factory.script_variables, ScriptVariables::All);
        // Set-once enforced.
        assert!(apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "ProcessScriptVariables",
            &["off"],
            Scope::ProcessStrict,
        )
        .is_err());
        // Unknown names and over-arity.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "Bogus",
            &["x"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        assert_eq!(
            err.message,
            "\"Bogus\" not recognized or too many arguments"
        );
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "RewriteLevel",
            &["a", "b", "c", "d", "e"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        assert_eq!(
            err.message,
            "\"RewriteLevel\" not recognized or too many arguments"
        );
        // The gate runs before the arity check.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetchProxy",
            &["a", "b", "c", "d", "e"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"FetchProxy\" cannot be set at this scope.");
    }

    #[test]
    fn nginx_switch_formats() {
        let mut options = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = NginxSystemOptions::default();
        // Empty-detail failures quote the full line plus a bare space.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "UseNativeFetcher",
            &["maybe"],
            Scope::ProcessStrict,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::ValueInvalid);
        assert_eq!(err.message, "\"UseNativeFetcher maybe\" ");
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "NativeFetcherMaxKeepaliveRequests",
            &["0"],
            Scope::ProcessStrict,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"NativeFetcherMaxKeepaliveRequests 0\" ");
        // `StringToInt` width: fits `i64` but not `i32`.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "NativeFetcherMaxKeepaliveRequests",
            &["99999999999"],
            Scope::ProcessStrict,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"NativeFetcherMaxKeepaliveRequests 99999999999\" "
        );
        // Table value errors wrap the full line around the
        // `FormatSetOptionMessage` detail.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "CssInlineMaxBytes",
            &["lots"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::ValueInvalid);
        assert_eq!(
            err.message,
            "\"CssInlineMaxBytes lots\" Cannot set option CssInlineMaxBytes to lots. "
        );
        // Admin-domain verb errors wrap the same way.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "StatisticsDomains",
            &["frobnicate", "x"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"StatisticsDomains frobnicate x\" expected 'allow' or 'disallow', got 'frobnicate'"
        );
        // Inode limit: server scope, 64-bit store, wrapped failure.
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FileCacheInodeLimit",
            &["1000"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(system.file_cache_clean_inode_limit, 1000);
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FileCacheInodeLimit",
            &["1000"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"FileCacheInodeLimit\" cannot be set at this scope."
        );
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FileCacheInodeLimit",
            &["lots"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"FileCacheInodeLimit lots\" ");
        // `Statistics`: default on, server scope, bool grammar.
        assert!(system.statistics_enabled);
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "Statistics",
            &["off"],
            Scope::Server,
        )
        .unwrap();
        assert!(!system.statistics_enabled);
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "Statistics",
            &["off"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"Statistics\" cannot be set at this scope.");
        // `StatisticsLogging` family: server scope, C++ defaults.
        assert!(!system.statistics_logging_enabled);
        assert_eq!(600_000, system.statistics_logging_interval_ms);
        assert_eq!(1024, system.statistics_logging_max_file_size_kb);
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "StatisticsLogging",
            &["on"],
            Scope::Server,
        )
        .unwrap();
        assert!(system.statistics_logging_enabled);
        assert!(system.was_set.statistics_logging_enabled);
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "StatisticsLoggingIntervalMs",
            &["5000"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(5000, system.statistics_logging_interval_ms);
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "StatisticsLoggingMaxFileSizeKb",
            &["2048"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(2048, system.statistics_logging_max_file_size_kb);
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "StatisticsLogging",
            &["on"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"StatisticsLogging\" cannot be set at this scope."
        );
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "StatisticsLogging",
            &["maybe"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::ValueInvalid);
    }

    #[test]
    fn fetcher_options() {
        let mut options = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = NginxSystemOptions::default();
        assert_eq!(5000, system.blocking_fetch_timeout_ms);
        // Proxy stores verbatim at server scope.
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetchProxy",
            &["http://proxy:3128/"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(system.fetcher_proxy, "http://proxy:3128/");
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetchProxy",
            &["http://proxy:3128/"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"FetchProxy\" cannot be set at this scope.");
        // Gzip flag is main-only with bool grammar.
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetchWithGzip",
            &["on"],
            Scope::ProcessStrict,
        )
        .unwrap();
        assert!(system.fetch_with_gzip);
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetchWithGzip",
            &["on"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"FetchWithGzip\" cannot be set at this scope."
        );
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetchWithGzip",
            &["sometimes"],
            Scope::ProcessStrict,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"FetchWithGzip sometimes\" ");
        // Timeout is a 64-bit value (note the capital O).
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetcherTimeOutMs",
            &["2500"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(2500, system.blocking_fetch_timeout_ms);
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetcherTimeOutMs",
            &["soon"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"FetcherTimeOutMs soon\" ");
        // The lowercase-o spelling gates but never matches.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetcherTimeoutMs",
            &["2500"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"FetcherTimeoutMs\" cannot be set at this scope."
        );
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FetcherTimeoutMs",
            &["2500"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
    }

    #[test]
    fn cache_sizing_options() {
        let mut options = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = NginxSystemOptions::default();
        assert_eq!(3_600_000, system.file_cache_clean_interval_ms);
        assert_eq!(102_400, system.file_cache_clean_size_kb);
        let apply = |options: &mut RewriteOptions,
                     factory: &mut NginxFactoryOptions,
                     system: &mut NginxSystemOptions,
                     directive: &str,
                     args: &[&str],
                     scope: Scope| {
            apply_nginx_directive(options, factory, system, directive, args, scope)
        };
        for (directive, value) in [
            ("FileCacheCleanIntervalMs", "-1"),
            ("FileCacheSizeKb", "2048"),
            ("LRUCacheByteLimit", "4096"),
            ("LRUCacheKbPerProcess", "8192"),
            ("CacheFlushFilename", "flush.me"),
            ("EnableCachePurge", "on"),
        ] {
            apply(
                &mut options,
                &mut factory,
                &mut system,
                directive,
                &[value],
                Scope::Server,
            )
            .unwrap();
        }
        assert_eq!(-1, system.file_cache_clean_interval_ms);
        assert!(system.was_set.clean_interval);
        assert_eq!(2048, system.file_cache_clean_size_kb);
        assert!(system.was_set.clean_size);
        assert_eq!(4096, system.lru_cache_byte_limit);
        assert_eq!(8192, system.lru_cache_kb_per_process);
        assert_eq!("flush.me", system.cache_flush_filename);
        assert!(system.was_set.flush_filename);
        assert!(options.enable_cache_purge());
        assert!(!system.was_set.statistics_enabled);
        // Directory scope rejects; bad values wrap empty.
        let err = apply(
            &mut options,
            &mut factory,
            &mut system,
            "LRUCacheKbPerProcess",
            &["1"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"LRUCacheKbPerProcess\" cannot be set at this scope."
        );
        let err = apply(
            &mut options,
            &mut factory,
            &mut system,
            "FileCacheSizeKb",
            &["lots"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"FileCacheSizeKb lots\" ");
    }

    #[test]
    fn ipro_recording_caps() {
        let mut options = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = NginxSystemOptions::default();
        assert_eq!(10 * 1024 * 1024, system.ipro_max_response_bytes);
        assert_eq!(10, system.ipro_max_concurrent_recordings);
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "IproMaxResponseBytes",
            &["1024"],
            Scope::ProcessStrict,
        )
        .unwrap();
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "IproMaxConcurrentRecordings",
            &["3"],
            Scope::ProcessStrict,
        )
        .unwrap();
        assert_eq!(1024, system.ipro_max_response_bytes);
        assert!(system.was_set.ipro_max_response_bytes);
        assert_eq!(3, system.ipro_max_concurrent_recordings);
        assert!(system.was_set.ipro_max_concurrent_recordings);
        // Server scope rejects; bad values wrap empty.
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "IproMaxResponseBytes",
            &["1024"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"IproMaxResponseBytes\" cannot be set at this scope."
        );
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "IproMaxConcurrentRecordings",
            &["many"],
            Scope::ProcessStrict,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"IproMaxConcurrentRecordings many\" ");
    }

    #[test]
    fn external_server_specs() {
        // Single server: default port, explicit port, errors.
        assert_eq!(
            parse_external_server("cache", REDIS_DEFAULT_PORT).unwrap(),
            ExternalServerSpec {
                host: "cache".to_string(),
                port: 6379
            }
        );
        assert_eq!(
            parse_external_server("cache:6380", REDIS_DEFAULT_PORT).unwrap(),
            ExternalServerSpec {
                host: "cache".to_string(),
                port: 6380
            }
        );
        assert_eq!(
            parse_external_server("a:b:c", 1).unwrap_err(),
            "Expected single server in format <host>[:<port>]"
        );
        assert_eq!(
            parse_external_server(":11211", 1).unwrap_err(),
            "Server host cannot be empty"
        );
        assert_eq!(
            parse_external_server("h:lots", 1).unwrap_err(),
            "Port specified is not a valid number: 'lots'"
        );
        assert_eq!(
            parse_external_server("h:0", 1).unwrap_err(),
            "Invalid port: 0"
        );
        assert_eq!(
            parse_external_server("h:-1", 1).unwrap_err(),
            "Invalid port: -1"
        );
        // Clusters: empty clears, failures join with prefixes.
        assert_eq!(
            parse_external_cluster("", MEMCACHED_DEFAULT_PORT).unwrap(),
            Vec::new()
        );
        let cluster = parse_external_cluster("a,b:11212", MEMCACHED_DEFAULT_PORT).unwrap();
        assert_eq!(2, cluster.len());
        assert_eq!(11211, cluster[0].port);
        assert_eq!(11212, cluster[1].port);
        assert_eq!(
            parse_external_cluster("a,,:1", MEMCACHED_DEFAULT_PORT).unwrap_err(),
            "In server '': Server host cannot be empty. In server ':1': Server host cannot be \
             empty"
        );
        // Config arms store; failures wrap.
        let mut options = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = NginxSystemOptions::default();
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "MemcachedServers",
            &["a:1,b"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(2, system.memcached_servers.len());
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "RedisServer",
            &["r"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(
            Some(ExternalServerSpec {
                host: "r".to_string(),
                port: 6379
            }),
            system.redis_server
        );
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "RedisServer",
            &[":1"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "\"RedisServer :1\" Server host cannot be empty"
        );
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "MemcachedServers",
            &["h:0"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.message, "\"MemcachedServers h:0\" Invalid port: 0");
    }

    #[test]
    fn cache_path_and_state_helpers() {
        let mut options = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = NginxSystemOptions::default();
        assert!(!options.unplugged());
        assert!(!options.standby());
        options.set_enabled_state(EnabledState::Unplugged);
        assert!(options.unplugged());
        options.set_enabled_state(EnabledState::Standby);
        assert!(options.standby());
        // FileCachePath needs the leading slash, verbatim text.
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FileCachePath",
            &["/var/cache/pagespeed"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(system.file_cache_path, "/var/cache/pagespeed");
        let err = apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "FileCachePath",
            &["relative/path"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::ValueInvalid);
        assert_eq!(
            err.message,
            "\"FileCachePath relative/path\" must start with a slash"
        );
        // LogDir stores as-is.
        apply_nginx_directive(
            &mut options,
            &mut factory,
            &mut system,
            "LogDir",
            &["/var/log/ps"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(system.log_dir, "/var/log/ps");
    }

    #[test]
    fn nginx_admin_paths_and_purge() {
        let mut options = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = NginxSystemOptions::default();
        let apply = |options: &mut RewriteOptions,
                     factory: &mut NginxFactoryOptions,
                     system: &mut NginxSystemOptions,
                     directive: &str,
                     args: &[&str],
                     scope: Scope| {
            apply_nginx_directive(options, factory, system, directive, args, scope)
        };
        // The six admin paths store verbatim (exact-case names).
        apply(
            &mut options,
            &mut factory,
            &mut system,
            "StatisticsPath",
            &["/ngx_pagespeed_statistics"],
            Scope::Server,
        )
        .unwrap();
        assert_eq!(system.statistics_path, "/ngx_pagespeed_statistics");
        // Mixed-case table names are not recognized.
        let err = apply(
            &mut options,
            &mut factory,
            &mut system,
            "statisticsPath",
            &["/x"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        for (directive, value) in [
            ("GlobalStatisticsPath", "/ngx_pagespeed_global_statistics"),
            ("ConsolePath", "/pagespeed_console"),
            ("MessagesPath", "/ngx_pagespeed_message"),
            ("AdminPath", "/pagespeed_admin"),
            ("GlobalAdminPath", "/pagespeed_global_admin"),
            ("PurgeMethod", "PURGE"),
        ] {
            apply(
                &mut options,
                &mut factory,
                &mut system,
                directive,
                &[value],
                Scope::ProcessStrict,
            )
            .unwrap();
        }
        assert_eq!(
            system.global_statistics_path,
            "/ngx_pagespeed_global_statistics"
        );
        assert_eq!(system.console_path, "/pagespeed_console");
        assert_eq!(system.messages_path, "/ngx_pagespeed_message");
        assert_eq!(system.admin_path, "/pagespeed_admin");
        assert_eq!(system.global_admin_path, "/pagespeed_global_admin");
        assert_eq!(system.purge_method, "PURGE");
        // Scope gates: server paths rejected at directory scope, global
        // paths rejected below http scope (exact-case gate: wrong case
        // falls through to directory, like the C++ `all_options` loop).
        let err = apply(
            &mut options,
            &mut factory,
            &mut system,
            "AdminPath",
            &["/admin"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        let err = apply(
            &mut options,
            &mut factory,
            &mut system,
            "GlobalAdminPath",
            &["/global"],
            Scope::Server,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        let err = apply(
            &mut options,
            &mut factory,
            &mut system,
            "adminpath",
            &["/lower"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::NameUnknown);
        // Domain lists: allow/disallow append, bad verbs rejected verbatim.
        apply(
            &mut options,
            &mut factory,
            &mut system,
            "AdminDomains",
            &["allow", "*.example.com"],
            Scope::Directory,
        )
        .unwrap();
        apply(
            &mut options,
            &mut factory,
            &mut system,
            "AdminDomains",
            &["disallow", "evil.example.com"],
            Scope::Directory,
        )
        .unwrap();
        assert!(system.admin_domains.is_match("www.example.com", false));
        assert!(!system.admin_domains.is_match("evil.example.com", false));
        assert!(!system.admin_domains.is_match("other.org", false));
        assert!(system.statistics_domains.is_empty());
        let err = apply(
            &mut options,
            &mut factory,
            &mut system,
            "StatisticsDomains",
            &["maybe", "x"],
            Scope::Directory,
        )
        .unwrap_err();
        assert_eq!(err.outcome, SetOutcome::ValueInvalid);
        assert_eq!(
            err.message,
            "\"StatisticsDomains maybe x\" expected 'allow' or 'disallow', got 'maybe'"
        );
        // Beacon endpoints: unset reads None (host supplies its default),
        // set reads the parsed pair.
        assert_eq!(None, options.beacon_url());
        apply(
            &mut options,
            &mut factory,
            &mut system,
            "BeaconUrl",
            &["http://a/b https://c/d"],
            Scope::Directory,
        )
        .unwrap();
        let beacon = options.beacon_url().unwrap();
        assert_eq!(beacon.http, "http://a/b");
        assert_eq!(beacon.https, "https://c/d");
    }

    #[test]
    fn merge_propagates_master_switch() {
        // Set src wins over unset self.
        let mut base = RewriteOptions::new();
        let mut child = RewriteOptions::new();
        child.set_enabled_state(EnabledState::Unplugged);
        base.merge(&child);
        assert_eq!(base.enabled_state(), EnabledState::Unplugged);
        // Unset src never clears a set self.
        let mut other = RewriteOptions::new();
        other.set_enabled_state(EnabledState::Standby);
        other.merge(&RewriteOptions::new());
        assert_eq!(other.enabled_state(), EnabledState::Standby);
        // Two unset switches merge without observable change.
        let mut fresh = RewriteOptions::new();
        fresh.merge(&RewriteOptions::new());
        assert_eq!(fresh.enabled_state(), EnabledState::On);
        assert!(!fresh.was_set("e"));
        // The table path (`EnableRewriting`) latches the bit too.
        let mut via_table = RewriteOptions::new();
        via_table
            .set_option("EnableRewriting", "off", Scope::ProcessStrict)
            .unwrap();
        let mut target = RewriteOptions::new();
        target.merge(&via_table);
        assert_eq!(target.enabled_state(), EnabledState::Off);
    }

    #[test]
    fn request_time_option_reads() {
        let mut options = RewriteOptions::new();
        // Defaults: flushes followed, nothing else on.
        assert!(options.follow_flushes());
        assert!(!options.respect_x_forwarded_proto());
        assert!(options.remote_configuration_url().is_empty());
        assert!(!options.running_experiment());
        assert!(!options.enroll_experiment());
        assert!(!options.is_downstream_cache_integration_enabled());
        assert!(!options.matches_downstream_cache_rebeaconing_key("k"));
        options
            .set_option("FollowFlushes", "false", Scope::ProcessStrict)
            .unwrap();
        assert!(!options.follow_flushes());
        options
            .set_option("RespectXForwardedProto", "true", Scope::ProcessStrict)
            .unwrap();
        assert!(options.respect_x_forwarded_proto());
        options
            .set_option(
                "RemoteConfigurationUrl",
                "http://x/rc",
                Scope::ProcessStrict,
            )
            .unwrap();
        assert_eq!(options.remote_configuration_url(), "http://x/rc");
        options
            .set_option("RunExperiment", "true", Scope::ProcessStrict)
            .unwrap();
        assert!(options.running_experiment());
        options
            .set_option("EnrollExperiment", "3", Scope::ProcessStrict)
            .unwrap();
        assert!(options.enroll_experiment());
        // Rebeaconing needs the key configured; matching ignores case.
        options
            .set_option(
                "DownstreamCacheRebeaconingKey",
                "s3cret",
                Scope::ProcessStrict,
            )
            .unwrap();
        assert!(options.matches_downstream_cache_rebeaconing_key("S3CRET"));
        assert!(!options.matches_downstream_cache_rebeaconing_key("other"));
        assert!(!options.is_downstream_cache_integration_enabled());
        options.set_downstream_cache_purge_location_prefix("http://cdn/purge/");
        assert!(options.is_downstream_cache_integration_enabled());
    }

    #[test]
    fn ajax_default_level_and_thresholds() {
        let mut options = RewriteOptions::new();
        options.filters.enable(Filter::LazyloadImages);
        options.filters.enable(Filter::AddHead);
        options.filters.enable(Filter::PrioritizeCriticalCss);
        options.filters.enable(Filter::RewriteCss);
        options.disable_filters_that_cant_run_in_ajax();
        assert!(!options.filters.enabled(Filter::LazyloadImages));
        assert!(!options.filters.enabled(Filter::AddHead));
        assert!(!options.filters.enabled(Filter::PrioritizeCriticalCss));
        assert!(options.filters.enabled(Filter::RewriteCss));
        assert!(options.css_preserve());
        assert!(options.image_preserve());
        assert!(options.js_preserve());
        // Default level applies only when unset, without modifying.
        let mut fresh = RewriteOptions::new();
        assert!(!fresh.modified());
        fresh.set_default_rewrite_level(RewriteLevel::CoreFilters);
        assert_eq!(RewriteLevel::CoreFilters, fresh.filters().level());
        assert!(!fresh.modified());
        assert!(!fresh.was_set("l"));
        let mut leveled = RewriteOptions::new();
        leveled
            .set_option("RewriteLevel", "PassThrough", Scope::ProcessStrict)
            .unwrap();
        leveled.set_default_rewrite_level(RewriteLevel::CoreFilters);
        assert_eq!(RewriteLevel::PassThrough, leveled.filters().level());
        // Thresholds cap at the ceiling.
        let mut ceiling = RewriteOptions::new();
        ceiling
            .set_option("CssInlineMaxBytes", "100", Scope::ProcessStrict)
            .unwrap();
        let mut request = RewriteOptions::new();
        request
            .set_option("CssInlineMaxBytes", "999", Scope::ProcessStrict)
            .unwrap();
        request.limit_size_thresholds_to(&ceiling);
        assert_eq!(
            Some(Value::Int(100)),
            request.get_by_name("CssInlineMaxBytes")
        );
        // Unset thresholds stay unset.
        let mut bare = RewriteOptions::new();
        bare.limit_size_thresholds_to(&ceiling);
        assert!(!bare.was_set("ci"));
        // Override token reads back.
        assert!(options.request_option_override().is_empty());
        assert_eq!(1000, options.remote_configuration_timeout_ms());
        // Effective s-maxage gates on header modification.
        assert_eq!(10, options.effective_in_place_s_max_age_sec());
        options.set_modify_caching_headers(false);
        assert_eq!(-1, options.effective_in_place_s_max_age_sec());
    }

    #[test]
    fn merge_semantics() {
        // Narrower scope overrides scalars.
        let mut base = RewriteOptions::new();
        base.set_option("CssInlineMaxBytes", "100", Scope::ProcessStrict)
            .unwrap();
        let mut child = RewriteOptions::new();
        child
            .set_option("CssInlineMaxBytes", "200", Scope::ProcessStrict)
            .unwrap();
        base.merge(&child);
        assert_eq!(base.get_by_id("ci"), Some(Value::Int(200)));
        // Enabled-in-src clears disabled-here; forbidden is sticky.
        let mut base = RewriteOptions::new();
        base.filters_mut().disable(Filter::RewriteCss);
        let mut child = RewriteOptions::new();
        child.filters_mut().enable(Filter::RewriteCss);
        base.merge(&child);
        assert!(base.filters().enabled(Filter::RewriteCss));
        let mut base = RewriteOptions::new();
        base.filters_mut().forbid(Filter::RewriteCss);
        let mut child = RewriteOptions::new();
        child.filters_mut().enable(Filter::RewriteCss);
        base.merge(&child);
        assert!(!base.filters().enabled(Filter::RewriteCss));
        // Latched forbid-all refuses newly enabled filters.
        let mut base = RewriteOptions::new();
        base.set_option("ForbidAllDisabledFilters", "on", Scope::ProcessStrict)
            .unwrap();
        base.filters_mut().disable(Filter::CollapseWhitespace);
        let mut child = RewriteOptions::new();
        child.filters_mut().enable(Filter::CollapseWhitespace);
        base.merge(&child);
        assert!(!base.filters().enabled(Filter::CollapseWhitespace));
        assert!(base.forbid_all_disabled());
        // Extend-cache vs preserve override: src enables the filter while
        // base preserves -> preserve is switched off.
        let mut base = RewriteOptions::new();
        base.set_option("CssPreserveURLs", "on", Scope::ProcessStrict)
            .unwrap();
        let mut child = RewriteOptions::new();
        child.filters_mut().enable(Filter::ExtendCacheCss);
        base.merge(&child);
        assert_eq!(base.get_by_id("cpu"), Some(Value::Bool(false)));
        // Collections append; libraries merge per key.
        let mut base = RewriteOptions::new();
        base.parse_and_set_2("CustomFetchHeader", "A", "1").unwrap();
        let mut child = RewriteOptions::new();
        child
            .parse_and_set_2("CustomFetchHeader", "B", "2")
            .unwrap();
        base.merge(&child);
        assert_eq!(base.custom_fetch_headers.len(), 2);
    }

    #[test]
    fn query_scan() {
        let mut options = RewriteOptions::new();
        assert_eq!(
            options.scan_query("PageSpeed", "off", true),
            QueryStatus::Success
        );
        assert_eq!(options.enabled_state(), EnabledState::Off);
        // noscript disables script-dependent filters + adds the redirect.
        options
            .set_option("RewriteLevel", "AllFilters", Scope::ProcessStrict)
            .unwrap();
        assert_eq!(
            options.scan_query("PageSpeed", "noscript", true),
            QueryStatus::Success
        );
        assert!(!options.filters().enabled(Filter::DeferJavascript));
        assert!(options.filters().enabled(Filter::HandleNoscriptRedirect));
        assert_eq!(
            options.scan_query("PageSpeed", "bogus", true),
            QueryStatus::Invalid
        );
        // Filters param resets to listed-only.
        assert_eq!(
            options.scan_query("PageSpeedFilters", "rewrite_css", true),
            QueryStatus::Success
        );
        assert!(options.filters().enabled(Filter::RewriteCss));
        assert!(!options.filters().enabled(Filter::CombineCss));
        // Query-scope options settable via suffix; others unknown.
        assert_eq!(
            options.scan_query("PageSpeedCssInlineMaxBytes", "512", true),
            QueryStatus::Success
        );
        assert_eq!(options.get_by_id("ci"), Some(Value::Int(512)));
        assert_eq!(
            options.scan_query("PageSpeedRewriteLevel", "AllFilters", true),
            QueryStatus::NoneFound // Directory scope, not Query
        );
        // no-transform forces standby.
        assert_eq!(
            options.scan_query("Cache-Control", "no-cache, no-transform", true),
            QueryStatus::Success
        );
        assert_eq!(options.enabled_state(), EnabledState::Standby);
        // Gated when options are disallowed.
        assert_eq!(
            options.scan_query("PageSpeedFilters", "rewrite_css", false),
            QueryStatus::NoneFound
        );
        assert_eq!(
            options.scan_query("Unrelated", "x", true),
            QueryStatus::NoneFound
        );
    }

    #[test]
    fn signature_and_secret_tables() {
        assert!(EXCLUDED_FROM_SIGNATURE.contains(&"tifrwd"));
        assert!(EXCLUDED_FROM_SIGNATURE.contains(&"noop"));
        for name in SECRET_OPTIONS {
            assert!(
                lookup_by_name(name).is_some(),
                "{name} must be a real option"
            );
        }
        // Experiment letter round-trip.
        let mut options = RewriteOptions::new();
        options
            .parse_and_set_1("ExperimentSpec", "id=7", Scope::Server)
            .unwrap();
        options
            .parse_and_set_1("ExperimentSpec", "id=9", Scope::Server)
            .unwrap();
        assert_eq!(options.experiment_state_str(), "");
        options.set_experiment_state_str("b");
        assert_eq!(options.experiment_state_str(), "b");
        options.set_experiment_state_str("z"); // out of range: ignored
        assert_eq!(options.experiment_state_str(), "b");
        options.set_experiment_state_str("bogus"); // ignored
        assert_eq!(options.experiment_state_str(), "b");
    }

    #[test]
    fn domain_rewrite_flags() {
        use super::Scope;
        let mut options = RewriteOptions::new();
        assert!(!options.domain_rewrite_hyperlinks());
        assert!(!options.domain_rewrite_cookies());
        assert!(!options.client_domain_rewrite());
        options
            .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
            .unwrap();
        options
            .set_option("DomainRewriteCookies", "on", Scope::ProcessStrict)
            .unwrap();
        options
            .set_option("ClientDomainRewrite", "on", Scope::ProcessStrict)
            .unwrap();
        assert!(options.domain_rewrite_hyperlinks());
        assert!(options.domain_rewrite_cookies());
        assert!(options.client_domain_rewrite());
    }

    #[test]
    fn preserve_subresource_hints_flag() {
        use super::Scope;
        let mut options = RewriteOptions::new();
        assert!(!options.preserve_subresource_hints());
        options
            .set_option("PreserveSubresourceHints", "on", Scope::ProcessStrict)
            .unwrap();
        assert!(options.preserve_subresource_hints());
    }

    #[test]
    fn lazyload_options() {
        use super::Scope;
        let mut options = RewriteOptions::new();
        assert!(options.lazyload_images_after_onload());
        assert!(options.lazyload_images_blank_url().is_empty());
        assert!(options.is_lazyload_enabled_for_class_name("gallery"));
        options
            .set_option("LazyloadImagesAfterOnload", "off", Scope::ProcessStrict)
            .unwrap();
        options
            .set_option(
                "LazyloadImagesBlankUrl",
                "http://blank.com/1.gif",
                Scope::ProcessStrict,
            )
            .unwrap();
        assert!(!options.lazyload_images_after_onload());
        assert_eq!(
            options.lazyload_images_blank_url(),
            "http://blank.com/1.gif"
        );
        options.disallow_troublesome_resources();
        assert!(!options.is_lazyload_enabled_for_class_name("123 dfcg-metabox"));
        assert!(!options.is_lazyload_enabled_for_class_name("nivo_sl"));
        assert!(!options.is_lazyload_enabled_for_class_name("my_slider"));
        assert!(options.is_lazyload_enabled_for_class_name("gallery"));
        assert!(!options
            .allow_resources()
            .is_match("http://x/tiny_mce.js", true));
    }

    #[test]
    fn delay_images_options() {
        use super::Scope;
        let mut options = RewriteOptions::new();
        assert!(!options.use_blank_image_for_inline_preview());
        assert!(options.enable_aggressive_rewriters_for_mobile());
        assert!(!options.lazyload_highres_images());
        options
            .set_option("UseBlankImageForInlinePreview", "on", Scope::ProcessStrict)
            .unwrap();
        options
            .set_option(
                "EnableAggressiveRewritersForMobile",
                "off",
                Scope::ProcessStrict,
            )
            .unwrap();
        options
            .set_option("EnableLazyLoadHighResImages", "on", Scope::ProcessStrict)
            .unwrap();
        assert!(options.use_blank_image_for_inline_preview());
        assert!(!options.enable_aggressive_rewriters_for_mobile());
        assert!(options.lazyload_highres_images());
    }

    #[test]
    fn oblivious_pagespeed_urls_flag() {
        use super::Scope;
        let mut options = RewriteOptions::new();
        assert!(!options.oblivious_pagespeed_urls());
        options
            .set_option("ObliviousPagespeedUrls", "on", Scope::ProcessStrict)
            .unwrap();
        assert!(options.oblivious_pagespeed_urls());
    }

    #[test]
    fn html_cache_cookie_directives() {
        use super::Scope;
        // Built-in allowlist defaults on: existing installs benefit
        // with no configuration.
        let mut options = RewriteOptions::new();
        assert!(options.html_cache_ignore_common_cookies());
        assert!(options.html_cache_ignore_cookies().is_empty());
        assert!(options.html_cache_bypass_cookies().is_empty());
        options
            .set_option("HtmlCacheIgnoreCommonCookies", "off", Scope::Server)
            .unwrap();
        assert!(!options.html_cache_ignore_common_cookies());
        options
            .set_option("HtmlCacheIgnoreCommonCookies", "on", Scope::Server)
            .unwrap();
        assert!(options.html_cache_ignore_common_cookies());
        assert!(options
            .set_option("HtmlCacheIgnoreCommonCookies", "maybe", Scope::Server)
            .is_err());
        // Custom patterns accumulate across declarations and extend
        // (never replace) the built-in list.
        let base_signature = options.signature();
        options
            .parse_and_set_1("HtmlCacheIgnoreCookie", "tracker", Scope::Server)
            .unwrap();
        options
            .parse_and_set_1("HtmlCacheIgnoreCookie", "_pk_*", Scope::Server)
            .unwrap();
        options
            .parse_and_set_1("HtmlCacheBypassCookie", "session*", Scope::Server)
            .unwrap();
        assert_eq!(
            options.html_cache_ignore_cookies().patterns(),
            &["tracker".to_string(), "_pk_*".to_string()]
        );
        assert_eq!(
            options.html_cache_bypass_cookies().patterns(),
            &["session*".to_string()]
        );
        assert_ne!(base_signature, options.signature());
        // Invalid patterns reject the directive, keeping prior state.
        assert!(options
            .parse_and_set_1("HtmlCacheIgnoreCookie", "a=b", Scope::Server)
            .is_err());
        assert!(options
            .parse_and_set_1("HtmlCacheBypassCookie", "", Scope::Server)
            .is_err());
        assert_eq!(2, options.html_cache_ignore_cookies().len());
        assert_eq!(1, options.html_cache_bypass_cookies().len());
        // Scope merge appends both lists.
        let mut child = RewriteOptions::new();
        child
            .parse_and_set_1("HtmlCacheIgnoreCookie", "child_only", Scope::Server)
            .unwrap();
        child
            .parse_and_set_1("HtmlCacheBypassCookie", "no_cache_me", Scope::Server)
            .unwrap();
        options.merge(&child);
        assert_eq!(3, options.html_cache_ignore_cookies().len());
        assert_eq!(2, options.html_cache_bypass_cookies().len());
        // The policy view reflects every input.
        let policy = options.html_cache_cookie_policy();
        assert!(policy.common_enabled);
        assert!(policy.ignore.matches("_pk_id.1.2"));
        assert!(policy.bypass.matches("session_abc"));
        // Case-insensitive directive names, like every other option.
        let mut lower = RewriteOptions::new();
        lower
            .parse_and_set_1("htmlcacheignorecookie", "x", Scope::Server)
            .unwrap();
        assert_eq!(1, lower.html_cache_ignore_cookies().len());
    }
}

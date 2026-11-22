//! Option property metadata: kinds, defaults, scopes, lookups.
//!
//! The [`PROPERTIES`] table transcribes `RewriteOptions::AddProperties`
//! (`mod_pagespeed/net/instaweb/rewriter/rewrite_options.cc`) entry for
//! entry, in C++ order, resolving option-name strings, default constants,
//! and help text. Name lookup mirrors `LookupOptionByName`
//! (case-insensitive after `GetEffectiveOptionName` rename mapping);
//! id lookup mirrors `LookupOptionById`.
#![forbid(unsafe_code)]

use crate::scope::Scope;

/// Value kind of an option (from the C++ `Option<T>` member type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropKind {
    /// `Option<bool>`; accepts `on/off/true/false` (case-insensitive).
    Bool,
    /// `Option<int>` / `Option<int32>`; 64-bit range checked on parse.
    Int,
    /// `Option<int64>`.
    Int64,
    /// `Option<string>`; free-form value.
    Str,
    /// `Option<RewriteLevel>`; `ParseRewriteLevel` spellings.
    Level,
    /// `Option<EnabledEnum>`; `on/off/true/false/unplugged/standby`.
    Enabled,
    /// Composite C++ type, holding its C++ type name
    /// (e.g. `"BeaconUrl"`, `"ResponsiveDensities"`). Values are kept as
    /// raw strings until their dedicated parsers land; defaults that are
    /// plain strings are still usable via [`PropDefault::Str`].
    Other(&'static str),
}

/// Default value of an option, resolved from the C++ default expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropDefault {
    /// Boolean default.
    Bool(bool),
    /// Integer default (both `int` and `int64` kinds).
    Int(i64),
    /// String default.
    Str(&'static str),
    /// Rewrite-level default, canonical spelling (e.g. `"PassThrough"`).
    Level(&'static str),
    /// Enabled-enum default, canonical spelling (`"On"`).
    Enabled(&'static str),
    /// Composite default kept as descriptive text (C++ meaning preserved
    /// verbatim, e.g. `"{1.5, 2.0, 3.0}"`).
    Expr(&'static str),
}

/// One transcribed property row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Property {
    /// Short option id used in URLs and signatures (e.g. `"ci"`).
    pub id: &'static str,
    /// Config-file option name (e.g. `"CssInlineMaxBytes"`); empty for
    /// request-only properties that `SetOptionFromName` cannot address.
    pub name: &'static str,
    /// Value kind.
    pub kind: PropKind,
    /// Default value.
    pub default: PropDefault,
    /// Minimum scope at which the option may be set.
    pub scope: Scope,
    /// Help text (`None` for NULL-help entries).
    pub help: Option<&'static str>,
    /// Whether the value is safe to print in logs / admin pages.
    pub safe_to_print: bool,
    /// Request-only properties (`AddRequestProperty`): no name, process
    /// scope, excluded from name lookup.
    pub request_only: bool,
}

#[path = "properties_table.inc.rs"]
mod table;

pub use table::PROPERTIES;

/// Number of transcribed core properties.
pub const NUM_PROPERTIES: usize = PROPERTIES.len();

/// Deprecated option names mapped to their replacements
/// (`kRenamedOptionNameData`; matched case-insensitively).
pub const RENAMED_OPTIONS: &[(&str, &str)] = &[
    ("ImageWebpRecompressionQuality", "WebpRecompressionQuality"),
    (
        "ImageWebpRecompressionQualityForSmallScreens",
        "WebpRecompressionQualityForSmallScreens",
    ),
];

/// Applies the rename map (`GetEffectiveOptionName`).
#[must_use]
pub fn effective_option_name(name: &str) -> &str {
    for (old, new) in RENAMED_OPTIONS {
        if old.eq_ignore_ascii_case(name) {
            return new;
        }
    }
    name
}

/// Strips a leading `ModPagespeed` prefix (nginx config parsing strips it
/// before lookup via case-insensitive `StringCaseStartsWith`, even when the
/// whole directive is exactly `ModPagespeed`).
#[must_use]
pub fn strip_mod_pagespeed_prefix(name: &str) -> &str {
    if name.len() >= "ModPagespeed".len() && name[..12].eq_ignore_ascii_case("ModPagespeed") {
        &name[12..]
    } else {
        name
    }
}

/// Looks up a property by config-file name (`LookupOptionByName`).
///
/// Applies the `ModPagespeed` prefix strip and the (case-insensitive)
/// rename map, then matches the table case-**sensitively** like the
/// C++ name map. Request-only (nameless) properties never match.
#[must_use]
pub fn lookup_by_name(name: &str) -> Option<&'static Property> {
    let name = strip_mod_pagespeed_prefix(name);
    if name.is_empty() {
        return None;
    }
    let effective = effective_option_name(name);
    PROPERTIES
        .iter()
        .find(|p| !p.name.is_empty() && p.name == effective)
}

/// Looks up a property by short id (`LookupOptionById`, case-sensitive).
#[must_use]
pub fn lookup_by_id(id: &str) -> Option<&'static Property> {
    PROPERTIES.iter().find(|p| p.id == id)
}

/// Looks up the option name for a short id (`LookupOptionNameById`).
#[must_use]
pub fn name_for_id(id: &str) -> Option<&'static str> {
    lookup_by_id(id).map(|p| p.name)
}

/// Whether a name addresses a known option (`IsValidOptionName`).
#[must_use]
pub fn is_valid_option_name(name: &str) -> bool {
    lookup_by_name(name).is_some()
}

/// Options rejected below server scope by the nginx scope gate
/// (`server_only_options` in `ngx_rewrite_options.cc`).
pub const SERVER_ONLY_OPTIONS: &[&str] = &[
    "FetcherTimeoutMs",
    "FetchProxy",
    "ForceCaching",
    "GeneratedFilePrefix",
    "ImgMaxRewritesAtOnce",
    "InheritVHostConfig",
    "InstallCrashHandler",
    "MessageBufferSize",
    "NumRewriteThreads",
    "NumExpensiveRewriteThreads",
    "StaticAssetPrefix",
    "TrackOriginalContentLength",
    "UsePerVHostStatistics",
    "BlockingRewriteRefererUrls",
    "CreateSharedMemoryMetadataCache",
    "LoadFromFile",
    "LoadFromFileMatch",
    "LoadFromFileRule",
    "LoadFromFileRuleMatch",
    "UseNativeFetcher",
    "NativeFetcherMaxKeepaliveRequests",
];

/// Options rejected below http scope (`main_only` in `ngx_rewrite_options.cc`).
pub const MAIN_ONLY_OPTIONS: &[&str] = &["UseNativeFetcher", "NativeFetcherMaxKeepaliveRequests"];

/// Nginx-only property scopes (`NgxRewriteOptions::AddProperties` plus
/// the `SystemRewriteOptions` names ported on demand). These live
/// outside the shared property table (which stays pinned at its
/// audited shape), so the gate consults this list with the same
/// case-**sensitive** match the C++ `all_options()` loop uses.
pub const NGINX_PROPERTY_SCOPES: &[(&str, Scope)] = &[
    ("StatisticsPath", Scope::Server),
    ("GlobalStatisticsPath", Scope::ProcessStrict),
    ("ConsolePath", Scope::Server),
    ("MessagesPath", Scope::Server),
    ("AdminPath", Scope::Server),
    ("GlobalAdminPath", Scope::ProcessStrict),
    ("PurgeMethod", Scope::Server),
    ("FileCacheInodeLimit", Scope::Server),
    ("Statistics", Scope::Server),
    ("StatisticsLogging", Scope::Server),
    ("StatisticsLoggingIntervalMs", Scope::Server),
    ("StatisticsLoggingMaxFileSizeKb", Scope::Server),
    ("MemcachedServers", Scope::Server),
    ("RedisServer", Scope::Server),
    ("FetchWithGzip", Scope::ProcessStrict),
    ("FileCacheCleanIntervalMs", Scope::Server),
    ("FileCacheSizeKb", Scope::Server),
    ("LRUCacheByteLimit", Scope::Server),
    ("LRUCacheKbPerProcess", Scope::Server),
    ("CacheFlushFilename", Scope::Server),
    ("EnableCachePurge", Scope::Server),
    ("CompressMetadataCache", Scope::Server),
    ("IproMaxResponseBytes", Scope::ProcessStrict),
    ("IproMaxConcurrentRecordings", Scope::ProcessStrict),
];

/// Full nginx scope resolution (`NgxRewriteOptions::GetOptionScope`).
///
/// Main-only names resolve to process scope, server-only names to server
/// scope (both case-insensitive); otherwise the property table (plus the
/// nginx-only property scopes) is consulted with a case-**sensitive**
/// name match, treating legacy-process scope as process-strict; unknown
/// names default to directory scope. Callers pass the directive with any
/// `ModPagespeed` prefix already stripped.
#[must_use]
pub fn nginx_option_scope(name: &str) -> Scope {
    if let Some(scope) = nginx_min_scope(name) {
        return scope;
    }
    if let Some(prop) = PROPERTIES.iter().find(|p| p.name == name) {
        return if prop.scope == Scope::LegacyProcess {
            Scope::ProcessStrict
        } else {
            prop.scope
        };
    }
    if let Some((_, scope)) = NGINX_PROPERTY_SCOPES.iter().find(|(n, _)| *n == name) {
        return *scope;
    }
    Scope::Directory
}

/// Minimum scope for a name through the nginx gate, if the gate constrains it.
#[must_use]
pub fn nginx_min_scope(name: &str) -> Option<Scope> {
    let name = strip_mod_pagespeed_prefix(name);
    if MAIN_ONLY_OPTIONS
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
    {
        return Some(Scope::ProcessStrict);
    }
    if SERVER_ONLY_OPTIONS
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
    {
        return Some(Scope::Server);
    }
    None
}

/// Nginx-only one-arg directive (`ClearInheritedScripts`, any scope).
pub const NGINX_ONLY_ANY_SCOPE: &[&str] = &["ClearInheritedScripts"];

/// Nginx-only two-arg http-only directives with their defaults.
pub const NGINX_ONLY_HTTP: &[(&str, &str)] = &[
    ("UseNativeFetcher", "off"),
    ("NativeFetcherMaxKeepaliveRequests", "100"),
    ("ProcessScriptVariables", "off"),
];

/// Special one-arg directives handled outside the property table
/// (`ParseAndSetOptionFromNameWithScope`).
pub const SPECIAL_ONE_ARG: &[&str] = &[
    "Allow",
    "DisableFilters",
    "Disallow",
    "Domain",
    "ProxySuffix",
    "DownstreamCachePurgeLocationPrefix",
    "EnableFilters",
    "ExperimentVariable",
    "ExperimentSpec",
    "ForbidFilters",
    "RetainComment",
    "BlockingRewriteRefererUrls",
    "PermitIdsForCssCombining",
];

/// Two-arg directives (`ParseAndSetOptionFromName2`).
pub const TWO_ARG: &[&str] = &[
    "AddResourceHeader",
    "CustomFetchHeader",
    "LoadFromFile",
    "LoadFromFileMatch",
    "LoadFromFileRule",
    "LoadFromFileRuleMatch",
    "MapOriginDomain",
    "MapProxyDomain",
    "MapRewriteDomain",
    "ShardDomain",
];

/// Three-arg directives (`ParseAndSetOptionFromName3`).
pub const THREE_ARG: &[&str] = &[
    "UrlValuedAttribute",
    "Library",
    "MapOriginDomain",
    "MapProxyDomain",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_count_and_uniqueness() {
        // 167 `AddBaseProperty`/`AddRequestProperty` calls in C++ order,
        // plus the port's `HtmlCacheIgnoreCommonCookies`.
        assert_eq!(NUM_PROPERTIES, 168);
        let ids: HashSet<&str> = PROPERTIES.iter().map(|p| p.id).collect();
        assert_eq!(ids.len(), NUM_PROPERTIES, "duplicate option id");
        let names: HashSet<&str> = PROPERTIES
            .iter()
            .map(|p| p.name)
            .filter(|n| !n.is_empty())
            .collect();
        // Request-only rows share the empty name; the rest are unique.
        let named = PROPERTIES.iter().filter(|p| !p.name.is_empty()).count();
        assert_eq!(names.len(), named, "duplicate option name");
    }

    #[test]
    fn golden_defaults() {
        let get = |id: &str| lookup_by_id(id).unwrap();
        assert_eq!(get("l").default, PropDefault::Level("PassThrough"));
        assert_eq!(get("cf").default, PropDefault::Int(1024000));
        assert_eq!(get("cpmb").default, PropDefault::Int(2097152)); // 2<<20
        assert_eq!(get("dob").default, PropDefault::Int(1048576)); // 1<<20
        assert_eq!(get("ci").default, PropDefault::Int(2048));
        assert_eq!(get("gfci").default, PropDefault::Int(3072));
        assert_eq!(get("ii").default, PropDefault::Int(3072));
        assert_eq!(get("ji").default, PropDefault::Int(2048));
        assert_eq!(get("jp").default, PropDefault::Int(10240));
        assert_eq!(get("e").default, PropDefault::Enabled("On"));
        assert_eq!(get("ict").default, PropDefault::Int(300000)); // 5min
        assert_eq!(get("bfto").default, PropDefault::Int(5000)); // 5s
        assert_eq!(get("fhct").default, PropDefault::Int(300));
        assert_eq!(get("fhcc").default, PropDefault::Bool(true));
        assert_eq!(get("rcfgt").default, PropDefault::Int(1000)); // 1s
        assert_eq!(get("rbls").default, PropDefault::Int(403));
        assert_eq!(get("eeid").default, PropDefault::Int(-2));
        assert_eq!(get("avo").default, PropDefault::Str("Auto"));
    }

    #[test]
    fn golden_scopes_and_kinds() {
        let get = |id: &str| lookup_by_id(id).unwrap();
        assert_eq!(get("l").scope, Scope::Directory);
        assert_eq!(get("cf").scope, Scope::Query);
        assert_eq!(get("rcfgt").scope, Scope::Server);
        assert_eq!(get("mect").scope, Scope::ProcessStrict);
        assert!(get("mect").request_only);
        assert_eq!(get("mect").name, "");
        assert_eq!(get("cf").kind, PropKind::Int64);
        assert_eq!(get("e").kind, PropKind::Enabled);
        assert_eq!(get("l").kind, PropKind::Level);
        assert_eq!(get("bu").kind, PropKind::Other("BeaconUrl"));
        assert_eq!(get("rid").kind, PropKind::Other("ResponsiveDensities"));
        assert_eq!(get("e").help, None); // NULL help in C++
    }

    #[test]
    fn help_text_spot_check() {
        let get = |id: &str| lookup_by_id(id).unwrap();
        assert_eq!(
            get("cf").help,
            Some("Number of bytes below which stylesheets will be flattened.")
        );
        // Escaped quotes in the C++ source render as plain quotes.
        assert_eq!(
            get("avo").help,
            Some(
                "\"Auto\", \"None\", or comma separated list of strings \
                 chosen from \"Save-Data\", \"User-Agent\", and \"Accept\"."
            )
        );
    }

    #[test]
    fn name_lookup_prefix_rename_case() {
        assert_eq!(lookup_by_name("CssInlineMaxBytes").unwrap().id, "ci");
        // The table match is case-sensitive (only the rename map
        // and the scope lists fold case).
        assert_eq!(lookup_by_name("cssinlinemaxbytes"), None);
        assert_eq!(
            lookup_by_name("ModPagespeedCssInlineMaxBytes").unwrap().id,
            "ci"
        );
        // Renames resolve to the new option...
        assert_eq!(
            lookup_by_name("ImageWebpRecompressionQuality")
                .unwrap()
                .name,
            "WebpRecompressionQuality"
        );
        // ...and the old spelling is not itself an option.
        assert!(PROPERTIES
            .iter()
            .all(|p| p.name != "ImageWebpRecompressionQuality"));
        assert_eq!(lookup_by_name(""), None);
        assert_eq!(lookup_by_name("BogusOption"), None);
        // Request-only rows are invisible to name lookup.
        assert!(lookup_by_name("mect").is_none());
        assert!(is_valid_option_name("RewriteLevel"));
        assert!(!is_valid_option_name("BogusOption"));
        assert_eq!(name_for_id("ci"), Some("CssInlineMaxBytes"));
        assert_eq!(name_for_id("zz"), None);
    }

    #[test]
    fn scope_gates() {
        assert_eq!(
            nginx_min_scope("UseNativeFetcher"),
            Some(Scope::ProcessStrict)
        );
        assert_eq!(
            nginx_min_scope("ModPagespeedFetchProxy"),
            Some(Scope::Server)
        );
        assert_eq!(nginx_min_scope("RewriteLevel"), None);
        assert_eq!(SERVER_ONLY_OPTIONS.len(), 21);
        assert_eq!(MAIN_ONLY_OPTIONS.len(), 2);
        // Every main-only option is also server-only (C++ lists overlap).
        for name in MAIN_ONLY_OPTIONS {
            assert!(SERVER_ONLY_OPTIONS.contains(name));
        }
        // Full scope resolution.
        assert_eq!(nginx_option_scope("UseNativeFetcher"), Scope::ProcessStrict);
        assert_eq!(nginx_option_scope("FetchProxy"), Scope::Server);
        assert_eq!(nginx_option_scope("CssInlineMaxBytes"), Scope::Query);
        // Property fallback is case-sensitive: wrong case falls to default.
        assert_eq!(nginx_option_scope("cssinlinemaxbytes"), Scope::Directory);
        assert_eq!(nginx_option_scope("BogusOption"), Scope::Directory);
        // Legacy-process options resolve as process-strict
        // (`ImageMaxRewritesAtOnce` is the known legacy-scope option).
        let legacy = lookup_by_name("ImageMaxRewritesAtOnce").unwrap();
        assert_eq!(legacy.scope, Scope::LegacyProcess);
        assert_eq!(
            nginx_option_scope("ImageMaxRewritesAtOnce"),
            Scope::ProcessStrict
        );
        // Prefix strip fires even on the bare word.
        assert_eq!(strip_mod_pagespeed_prefix("ModPagespeed"), "");
        assert_eq!(
            strip_mod_pagespeed_prefix("modpagespeedRewriteLevel"),
            "RewriteLevel"
        );
        assert_eq!(strip_mod_pagespeed_prefix("RewriteLevel"), "RewriteLevel");
    }

    #[test]
    fn directive_tables() {
        assert_eq!(SPECIAL_ONE_ARG.len(), 13);
        assert_eq!(TWO_ARG.len(), 10);
        assert_eq!(THREE_ARG.len(), 4);
        assert!(NGINX_ONLY_ANY_SCOPE.contains(&"ClearInheritedScripts"));
        assert_eq!(NGINX_ONLY_HTTP.len(), 3);
    }
}

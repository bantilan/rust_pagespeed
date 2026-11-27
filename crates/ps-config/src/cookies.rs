//! HTML-cache cookie policy: which request cookies may be ignored.
//!
//! Browsers attach cookies to every request, including analytics and
//! tracking cookies that never change the HTML a server generates. The
//! historical rule bypassed the HTML cache for any `Cookie` header, so
//! real-world pages (Google Analytics alone sets cookies on most
//! sites) never cached. This module implements the allowlist rule:
//! requests carrying only ignorable cookies cache exactly like
//! cookie-free requests, with cookie values excluded from the cache
//! key. Two public preferences (`wp_lang` and `trx_addons_is_retina`)
//! instead select distinct cached variants. Unknown, authentication,
//! session, cart and other personalization cookies still bypass.
//!
//! Security contract: an ignored cookie MUST NOT influence
//! server-generated HTML. The built-in list covers first-party
//! public bookkeeping, analytics/tracking and load-balancer names under
//! their intended usage. Administrators extending the list with
//! `HtmlCacheIgnoreCookie` take responsibility for that invariant;
//! when in doubt, leave the cookie bypassing (the default) or force
//! it with `HtmlCacheBypassCookie`. Only the two public preference values
//! are read beyond parsing for opaque variant digests; values are never
//! logged and never enter diagnostics — only
//! cookie names appear in bypass reasons.
//!
//! Matching is allocation-free on the accept path: the built-in list
//! uses static exact/prefix/suffix matchers, and custom patterns compile at
//! config time into exact, wildcard and numeric-suffix tables (no regular
//! expressions anywhere near the request path).
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

/// Maximum cookies honored per request (more fail closed).
pub const MAX_COOKIES_PER_REQUEST: usize = 256;

/// Maximum cookie-name characters kept in a bypass reason.
pub const MAX_REASON_NAME_LEN: usize = 128;

/* Built-in policy: static matchers, no request-time compilation. */

/// Policy generation participates in effective options signatures.
pub const COOKIE_POLICY_VERSION: &str = "public-cookies-v3";

macro_rules! exact_cookies {
    ($($name:literal),* $(,)?) => {
        /// Default exact names, including retained legacy analytics entries.
        pub const BUILTIN_IGNORE_EXACT: &[&str] = &[$($name),*];
        fn builtin_exact(name: &str) -> bool { matches!(name, $($name)|*) }
    };
}
exact_cookies! {
    "tk_qs",
    "wordpress_test_cookie",
    "mage-cache-sessid",
    "_ga",
    "_gid",
    "_gat",
    "__utma",
    "__utmb",
    "__utmc",
    "__utmt",
    "__utmz",
    "__utmv",
    "_gcl_au",
    "_gcl_aw",
    "_gcl_dc",
    "_gcl_gb",
    "_gcl_gs",
    "_gaexp",
    "_opt_awcid",
    "_opt_awmid",
    "_opt_awgid",
    "_opt_awkid",
    "_opt_utmc",
    "_fbp",
    "_fbc",
    "_clck",
    "_clsk",
    "_uetsid",
    "_uetvid",
    "_uetmsclkid",
    "_MicrosoftApplicationInsightsSession",
    "ai_user",
    "ai_session",
    "_hjFirstSeen",
    "_hjAbsoluteSessionInProgress",
    "_hjIncludedInPageviewSample",
    "_hjDonePolls",
    "_hjMinimizedPolls",
    "_hjShownFeedbackMessage",
    "_hjRecordingEnabled",
    "_hjRecordingLastActivity",
    "__hstc",
    "__hssc",
    "__hssrc",
    "hubspotutk",
    "_ttp",
    "_tt_enable_cookie",
    "_tt_sessionId",
    "_tt_pixel_session_index",
    "s_cc",
    "s_sq",
    "s_vi",
    "s_fid",
    "ajs_anonymous_id",
    "amplitude_id",
    "plausible_ignore",
    "__qca",
    "_scid",
    "_scid_r",
    "_pin_unauth",
    "_pinterest_ct_ua",
    "_rdt_uuid",
    "_twclid",
    "AnalyticsSyncHistory",
    "li_fat_id",
    "li_sugr",
    "_ym_uid",
    "_ym_d",
    "_ym_isad",
    "_ym_visorc",
    "_ce.s",
    "_ce.clock_data",
    "_ce.clock_event",
    "_ce.irv",
    "fs_uid",
    "fs_lua",
    "mf_user",
    "_lo_uid",
    "_lo_v",
    "__lotl",
    "is_unique",
    "sc_is_visitor_unique",
    "__atuvc",
    "__atuvs",
    "NREUM",
    "NRBA_SESSION",
    "RT",
    "AWSALB",
    "AWSALBCORS",
    "AWSALBTG",
    "AWSALBTGCORS",
    "ARRAffinity",
    "ARRAffinitySameSite",
    "__utmx",
    "__utmxx",
    "__gads",
    "__gac",
    "__gpi",
    "_fplc",
    "_FPAU",
    "_FPGS",
    "_FPGCLAW",
    "_FPGCLDC",
    "_FPGCLGB",
    "_hjCachedUserAttributes",
    "_hjViewportId",
    "_hjTLDTest",
    "hjClosedSurveyInvites",
    "hjDonePolls",
    "hjMinimizedPolls",
    "hjDoneTestUsers",
    "hjIncludedInSample",
    "hjShownFeedbackMessage",
}

/// Narrow vendor prefixes (without the trailing wildcard).
pub const BUILTIN_IGNORE_PREFIXES: &[&str] = &[
    "_ga_",
    "_gat_",
    "_gac_",
    "_hjSession_",
    "_hjSessionUser_",
    "_hjIncludedInSessionSample_",
    "_pk_id.",
    "_pk_ses.",
    "_pk_ref.",
    "_pk_cvar.",
    "_pk_testcookie.",
    "_pk_hsr.",
    "AMCV_",
    "AMCVS_",
    "amplitude_id_",
    "AMP_",
    "AMP_MKTG_",
    "mf_",
];

/// Public preferences vary HTML instead of having their values ignored.
/// Fixed order provides a canonical identity across cookie/header orderings.
pub const BUILTIN_PREFERENCE_COOKIES: &[&str] = &["wp_lang", "trx_addons_is_retina"];

fn preference_index(name: &str) -> Option<usize> {
    BUILTIN_PREFERENCE_COOKIES.iter().position(|p| *p == name)
}

/// Numeric WordPress settings suffixes: one or more ASCII digits only.
pub const BUILTIN_IGNORE_NUMERIC: &[&str] = &["wp-settings-", "wp-settings-time-"];
/// Vendor cookies anchored at both ends, rather than broad vendor prefixes.
pub const BUILTIN_IGNORE_INFIX: &[(&str, &str)] = &[("mp_", "_mixpanel"), ("ph_", "_posthog")];

fn numeric_suffix(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()))
}

/// Default name matching is case sensitive and does not inspect values.
#[must_use]
pub fn builtin_ignored(name: &str) -> bool {
    builtin_exact(name)
        || BUILTIN_IGNORE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || BUILTIN_IGNORE_NUMERIC
            .iter()
            .any(|prefix| numeric_suffix(name, prefix))
        || BUILTIN_IGNORE_INFIX.iter().any(|(prefix, suffix)| {
            name.len() >= prefix.len() + suffix.len()
                && name.starts_with(prefix)
                && name.ends_with(suffix)
        })
}

/// Default patterns for administration; never called on the request path.
#[must_use]
pub fn builtin_ignore_patterns() -> Vec<String> {
    BUILTIN_IGNORE_EXACT
        .iter()
        .map(|s| (*s).to_owned())
        .chain(BUILTIN_IGNORE_PREFIXES.iter().map(|s| format!("{s}*")))
        .chain(BUILTIN_IGNORE_NUMERIC.iter().map(|s| format!("{s}[0-9]+")))
        .chain(BUILTIN_IGNORE_INFIX.iter().map(|(a, b)| format!("{a}*{b}")))
        .collect()
}

/// Known personalized families always win, even against a custom `*` allow.
/// Explicit analytics session/bookkeeping names retain their intended meaning.
#[must_use]
pub fn protected_cookie(name: &str) -> bool {
    // These exact, predefined analytics/bookkeeping names include intended
    // session identifiers (ai_session, NRBA_SESSION, etc.), not login sessions.
    if builtin_exact(name) {
        return false;
    }
    if [
        "sid",
        "connect.sid",
        "frontend",
        "frontend_cid",
        "persistent_shopping_cart",
    ]
    .iter()
    .any(|p| name.eq_ignore_ascii_case(p))
    {
        return true;
    }
    let starts = |prefix: &str| {
        name.get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
    };
    if [
        "wordpress_",
        "wp-postpass_",
        "wp_woocommerce_session_",
        "SESS",
        "SSESS",
        "session",
        "auth_",
        "csrf",
        "xsrf",
        ".AspNet.",
        ".AspNetCore.",
    ]
    .iter()
    .any(|p| starts(p))
    {
        return true;
    }
    name.split(['_', '-', '.']).any(|part| {
        [
            "auth",
            "authentication",
            "authorization",
            "session",
            "sessionid",
            "sessid",
            "phpsessid",
            "jsessionid",
            "csrf",
            "csrftoken",
            "xsrf",
            "token",
            "cart",
            "basket",
            "loggedin",
            "login",
            "remember",
        ]
        .iter()
        .any(|p| part.eq_ignore_ascii_case(p))
    })
}

/* Custom pattern sets (`HtmlCacheIgnoreCookie` / `HtmlCacheBypassCookie`). */

/// Why a cookie pattern was rejected at config time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookiePatternError {
    /// Empty pattern.
    Empty,
    /// Forbidden character (`;`, `=`, `,`, whitespace, control).
    BadChar(char),
    /// Numeric syntax must be a literal prefix followed by `[0-9]+`.
    MisplacedWildcard,
}

impl CookiePatternError {
    /// Human-readable detail for the config error.
    #[must_use]
    pub fn message(self) -> String {
        match self {
            CookiePatternError::Empty => "cookie pattern must not be empty".to_string(),
            CookiePatternError::BadChar(char) => {
                format!("cookie pattern must not contain {char:?}")
            }
            CookiePatternError::MisplacedWildcard => {
                "numeric cookie pattern must be a literal prefix followed by [0-9]+".to_string()
            }
        }
    }
}

/// Compiled cookie-name patterns: exact names, wildcard and numeric suffix
/// matchers. Matching borrows the candidate (no allocation);
/// compilation happens once per config change, never per request.
///
/// Patterns are case-sensitive; `*` matches arbitrary bytes, `[0-9]+`
/// matches a numeric suffix after a literal prefix. A lone `*`
/// matches every cookie. Only allow cookies that cannot influence
/// server-generated HTML.
#[derive(Debug, Clone, Default)]
pub struct CookiePatternSet {
    exact: BTreeSet<String>,
    prefixes: Vec<String>,
    numeric: Vec<String>,
    globs: Vec<Vec<String>>,
    raw: Vec<String>,
}

impl CookiePatternSet {
    /// Empty set (matches nothing).
    #[must_use]
    pub fn new() -> Self {
        CookiePatternSet::default()
    }

    /// Adds a pattern, compiling it into the appropriate matcher table
    /// (duplicates ignored). Fails without mutating on invalid
    /// patterns.
    pub fn add(&mut self, pattern: &str) -> Result<(), CookiePatternError> {
        if pattern.is_empty() {
            return Err(CookiePatternError::Empty);
        }
        if let Some(char) = pattern.chars().find(|char| {
            *char == ';'
                || *char == '='
                || *char == ','
                || char.is_whitespace()
                || char.is_control()
        }) {
            return Err(CookiePatternError::BadChar(char));
        }
        if pattern.contains("[0-9]+") {
            let prefix = pattern
                .strip_suffix("[0-9]+")
                .filter(|s| !s.is_empty() && !s.contains(['*', '[', ']']))
                .ok_or(CookiePatternError::MisplacedWildcard)?;
            if !self.numeric.iter().any(|p| p == prefix) {
                self.numeric.push(prefix.to_owned());
            }
        } else if pattern.contains('*') {
            if let Some(prefix) = pattern.strip_suffix('*').filter(|s| !s.contains('*')) {
                if !self.prefixes.iter().any(|p| p == prefix) {
                    self.prefixes.push(prefix.to_owned());
                }
            } else if !self.raw.iter().any(|p| p == pattern) {
                self.globs
                    .push(pattern.split('*').map(str::to_owned).collect());
            }
        } else {
            self.exact.insert(pattern.to_owned());
        }
        if !self.raw.iter().any(|raw| raw == pattern) {
            self.raw.push(pattern.to_string());
        }
        Ok(())
    }

    /// Whether a cookie name matches (bypass and allow sets share
    /// this; precedence is decided by the caller).
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        if self.exact.contains(name) {
            return true;
        }
        self.prefixes.iter().any(|prefix| name.starts_with(prefix))
            || self
                .numeric
                .iter()
                .any(|prefix| numeric_suffix(name, prefix))
            || self.globs.iter().any(|parts| {
                let Some(mut rest) = name.strip_prefix(parts[0].as_str()) else {
                    return false;
                };
                for part in &parts[1..parts.len() - 1] {
                    let Some(index) = rest.find(part.as_str()) else {
                        return false;
                    };
                    rest = &rest[index + part.len()..];
                }
                rest.ends_with(parts.last().unwrap().as_str())
            })
    }

    /// Whether the set holds no patterns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Number of distinct patterns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    /// As-typed patterns, in declaration order (admin display).
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.raw
    }

    /// Appends another set's patterns (scope merge).
    pub fn append_from(&mut self, other: &CookiePatternSet) {
        for pattern in &other.raw {
            // Already-validated patterns cannot fail here.
            let _ignored = self.add(pattern);
        }
    }

    /// Canonical signature fragment (sorted exacts, declared
    /// prefixes): config changes shift HTML-cache keys.
    #[must_use]
    pub fn signature(&self) -> String {
        let mut out = String::new();
        for name in &self.exact {
            out.push_str(name);
            out.push(',');
        }
        out.push(';');
        for prefix in &self.prefixes {
            out.push_str(prefix);
            out.push_str("*,");
        }
        for prefix in &self.numeric {
            out.push_str(prefix);
            out.push_str("[0-9]+,");
        }
        for parts in &self.globs {
            out.push_str(&parts.join("*"));
            out.push(',');
        }
        out
    }
}

/* Request evaluation. */

/// Cookie policy view over the HTML-cache options.
#[derive(Debug, Clone, Copy)]
pub struct CookiePolicy<'a> {
    /// Built-in common-cookie allowlist enabled
    /// (`HtmlCacheIgnoreCommonCookies`, default on).
    pub common_enabled: bool,
    /// Custom ignored names (`HtmlCacheIgnoreCookie`).
    pub ignore: &'a CookiePatternSet,
    /// Forced-bypass names (`HtmlCacheBypassCookie`, wins over both
    /// allowlists).
    pub bypass: &'a CookiePatternSet,
}

/// Why cookies forced an HTML-cache bypass (names only, values never
/// leave the parser).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieBypass {
    /// `Cookie2` present (obsolete RFC 2965 header; fail closed).
    Cookie2,
    /// A `Cookie` header failed strict parsing (fail closed).
    Malformed,
    /// First offending cookie name (truncated to
    /// [`MAX_REASON_NAME_LEN`] characters).
    Cookie(String),
}

/// Cookie verdict for one request's headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieDecision {
    /// No `Cookie` header present.
    Absent,
    /// Every cookie allowlisted (eligible like a cookie-free
    /// request; `count` feeds the allowlist statistics).
    Allowlisted {
        /// Number of ignored cookies on the request.
        count: usize,
    },
    /// Bypass, with the reason.
    Bypass(CookieBypass),
}

impl CookieDecision {
    /// Whether the request clears the cookie gate.
    #[must_use]
    pub fn allowed(&self) -> bool {
        !matches!(self, CookieDecision::Bypass(_))
    }

    /// Allowlisted cookie count (0 unless accepted with cookies).
    #[must_use]
    pub fn allowlisted_count(&self) -> usize {
        match self {
            CookieDecision::Allowlisted { count } => *count,
            _ => 0,
        }
    }
}

/// Evaluates `Cookie`/`Cookie2` headers against the policy.
/// `Authorization`, request `Vary`, and every other header are the
/// caller's business (unchanged strict gates).
#[must_use]
pub fn evaluate_cookie_headers(headers: &[(&str, &str)], policy: &CookiePolicy) -> CookieDecision {
    let mut count = 0;
    let mut saw_cookie_header = false;
    let mut offending = None;
    let mut preferences_seen = 0u8;
    for (header_name, value) in headers {
        if header_name.eq_ignore_ascii_case("Cookie2") {
            return CookieDecision::Bypass(CookieBypass::Cookie2);
        }
        if !header_name.eq_ignore_ascii_case("Cookie") {
            continue;
        }
        saw_cookie_header = true;
        if scan_cookie_pairs(value, |name, _| {
            if let Some(index) = preference_index(name) {
                let bit = 1 << index;
                if preferences_seen & bit != 0 {
                    return Err(());
                }
                preferences_seen |= bit;
            }
            count += 1;
            if count > MAX_COOKIES_PER_REQUEST {
                return Err(());
            }
            if offending.is_none()
                && (protected_cookie(name)
                    || policy.bypass.matches(name)
                    || !((policy.common_enabled
                        && (builtin_ignored(name) || preference_index(name).is_some()))
                        || policy.ignore.matches(name)))
            {
                offending = Some(name);
            }
            Ok(())
        })
        .is_err()
        {
            return CookieDecision::Bypass(CookieBypass::Malformed);
        }
    }
    if !saw_cookie_header {
        return CookieDecision::Absent;
    }
    if let Some(name) = offending {
        return CookieDecision::Bypass(CookieBypass::Cookie(truncate_name(name)));
    }
    CookieDecision::Allowlisted { count }
}

/// Scan borrowed names without retaining or allocating cookie values/names.
/// Validate the complete header even after a disallowed name is found.
fn scan_cookie_pairs<'a>(
    value: &'a str,
    mut visit: impl FnMut(&'a str, &'a str) -> Result<(), ()>,
) -> Result<(), ()> {
    let bytes = value.as_bytes();
    let mut start = 0;
    let mut in_quotes = false;
    for index in 0..=bytes.len() {
        let boundary = index == bytes.len() || (!in_quotes && bytes[index] == b';');
        if index < bytes.len() && bytes[index] == b'"' {
            in_quotes = !in_quotes;
        }
        if boundary {
            let piece = value[start..index].trim_start_matches([' ', '\t']);
            if !piece.is_empty() {
                let (name, cookie_value) = piece.split_once('=').ok_or(())?;
                if name.is_empty()
                    || name
                        .bytes()
                        .any(|b| !b.is_ascii_alphanumeric() && !b"!#$%&'*+-.^_`|~".contains(&b))
                {
                    return Err(());
                }
                visit(name, cookie_value)?;
            }
            start = index + 1;
        }
    }
    if in_quotes {
        return Err(());
    }
    Ok(())
}

/// Borrow preference values using the same validated parser as eligibility.
/// Duplicates fail closed because upstream applications can select first/last
/// values differently. Callers must still apply the complete eligibility policy.
pub fn preference_cookie_values<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<[Option<&'a str>; 2], CookieBypass> {
    let mut values = [None; 2];
    let mut count = 0;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("Cookie2") {
            return Err(CookieBypass::Cookie2);
        }
        if !name.eq_ignore_ascii_case("Cookie") {
            continue;
        }
        scan_cookie_pairs(value, |name, value| {
            count += 1;
            if count > MAX_COOKIES_PER_REQUEST {
                return Err(());
            }
            if let Some(index) = preference_index(name) {
                if values[index].is_some() {
                    return Err(());
                }
                values[index] = Some(value);
            }
            Ok(())
        })
        .map_err(|_| CookieBypass::Malformed)?;
    }
    Ok(values)
}

/// Truncates a cookie name for diagnostics (char-boundary safe).
fn truncate_name(name: &str) -> String {
    if name.len() <= MAX_REASON_NAME_LEN {
        return name.to_string();
    }
    name.chars().take(MAX_REASON_NAME_LEN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_policy<'a>(
        common_enabled: bool,
        ignore: &'a CookiePatternSet,
        bypass: &'a CookiePatternSet,
    ) -> CookiePolicy<'a> {
        CookiePolicy {
            common_enabled,
            ignore,
            bypass,
        }
    }

    fn default_policy<'a>(
        ignore: &'a CookiePatternSet,
        bypass: &'a CookiePatternSet,
    ) -> CookiePolicy<'a> {
        make_policy(true, ignore, bypass)
    }

    #[test]
    fn preference_cookies_are_default_eligible_and_duplicates_fail_closed() {
        let empty = CookiePatternSet::new();
        let policy = default_policy(&empty, &empty);
        for name in BUILTIN_PREFERENCE_COOKIES {
            let header = format!("tk_qs=track; {name}=one");
            assert!(evaluate_cookie_headers(&[("Cookie", &header)], &policy).allowed());
            assert!(!evaluate_cookie_headers(
                &[("Cookie", &header)],
                &make_policy(false, &empty, &empty)
            )
            .allowed());
            let mut bypass = CookiePatternSet::new();
            bypass.add(name).unwrap();
            assert!(!evaluate_cookie_headers(
                &[("Cookie", &header)],
                &default_policy(&empty, &bypass)
            )
            .allowed());
        }
        let headers = [
            ("Cookie", "wp_lang=en_US; tk_qs=x"),
            ("Cookie", "trx_addons_is_retina=1"),
        ];
        assert_eq!(
            preference_cookie_values(headers).unwrap(),
            [Some("en_US"), Some("1")]
        );
        assert_eq!(
            preference_cookie_values([("Cookie", "wp_lang=; trx_addons_is_retina=0")]).unwrap(),
            [Some(""), Some("0")]
        );
        assert_eq!(
            preference_cookie_values([("Cookie", "wp_lang=en_US ")]).unwrap(),
            [Some("en_US "), None]
        );
        assert_eq!(
            preference_cookie_values([("Cookie", "wp_lang =en_US")]),
            Err(CookieBypass::Malformed)
        );
        for headers in [
            vec![("Cookie", "wp_lang=en_US; wp_lang=fr_FR")],
            vec![
                ("Cookie", "trx_addons_is_retina=1"),
                ("Cookie", "trx_addons_is_retina=1"),
            ],
        ] {
            assert_eq!(
                evaluate_cookie_headers(&headers, &policy),
                CookieDecision::Bypass(CookieBypass::Malformed)
            );
            assert_eq!(
                preference_cookie_values(headers),
                Err(CookieBypass::Malformed)
            );
        }
    }

    #[test]
    fn supplied_defaults_and_optional_entries() {
        let empty = CookiePatternSet::new();
        let defaults = include_str!("../tests/fixtures/html_cache_default_cookies.txt");
        let optional = include_str!("../tests/fixtures/html_cache_optional_cookies.txt");
        for pattern in defaults.lines() {
            let name = pattern.replace("[0-9]+", "123").replace('*', "fixture");
            let header = format!("{name}=first");
            assert!(
                evaluate_cookie_headers(&[("Cookie", &header)], &default_policy(&empty, &empty))
                    .allowed(),
                "default {pattern}"
            );
        }
        for pattern in optional.lines() {
            let name = pattern.replace('*', "fixture");
            let header = format!("{name}=first");
            assert!(
                !evaluate_cookie_headers(&[("Cookie", &header)], &default_policy(&empty, &empty))
                    .allowed(),
                "optional unexpectedly default: {pattern}"
            );
            let mut ignore = CookiePatternSet::new();
            ignore.add(pattern).unwrap();
            assert!(
                evaluate_cookie_headers(&[("Cookie", &header)], &default_policy(&ignore, &empty))
                    .allowed(),
                "opt-in {pattern}"
            );
            let mut bypass = CookiePatternSet::new();
            bypass.add(pattern).unwrap();
            assert!(!evaluate_cookie_headers(
                &[("Cookie", &header)],
                &default_policy(&ignore, &bypass)
            )
            .allowed());
        }
    }

    #[test]
    fn protected_names_win_over_all_wildcards() {
        let mut ignore = CookiePatternSet::new();
        ignore.add("*").unwrap();
        let empty = CookiePatternSet::new();
        for name in [
            "PHPSESSID",
            "JSESSIONID",
            "sessionid",
            "session",
            "sid",
            "connect.sid",
            "SESS0123456789abcdef",
            "SSESS0123456789abcdef",
            "sessionToken123",
            "frontend",
            "frontend_cid",
            "persistent_shopping_cart",
            "wordpress_logged_in_123",
            "wordpress_sec_123",
            "wp-postpass_123",
            "wp_woocommerce_session_123",
            "woocommerce_cart_hash",
            "cart",
            "csrftoken",
            "XSRF-TOKEN",
            "laravel_session",
            ".AspNetCore.Cookies",
            "mf_auth",
            "AMP_cart",
            "ph_session_posthog",
            "_hjSession_auth",
        ] {
            let header = format!("_ga=public; {name}=secret");
            assert!(
                !evaluate_cookie_headers(&[("Cookie", &header)], &default_policy(&ignore, &empty))
                    .allowed(),
                "protected {name}"
            );
        }
    }

    #[test]
    fn narrow_defaults_numeric_suffix_and_compiled_globs() {
        for name in [
            "wp-settings-",
            "wp-settings-time-",
            "wp-settings-abc",
            "wp-settings-12x",
            "wp-settings-１２",
            "_gcl_unknown",
            "_hjSessionUnexpected",
            "_hjDoneAnything",
            "mp_x_other",
            "mp_x_mixpanel_extra",
            "ph_x_other",
            "_pk_unknown.1",
        ] {
            assert!(!builtin_ignored(name), "near miss {name}");
        }
        let mut patterns = CookiePatternSet::new();
        for p in ["mp_*_mixpanel", "wp-settings-[0-9]+", "*tail", "a*b*c"] {
            patterns.add(p).unwrap();
        }
        for name in [
            "mp_abc_mixpanel",
            "mp__mixpanel",
            "wp-settings-123",
            "tail",
            "longtail",
            "axybzc",
        ] {
            assert!(patterns.matches(name), "{name}");
        }
        for name in [
            "mp_abc_mixpanelx",
            "wp-settings-",
            "wp-settings-1x",
            "wp-settings-１２",
            "tails",
            "acb",
        ] {
            assert!(!patterns.matches(name), "{name}");
        }
        let mut copy = CookiePatternSet::new();
        copy.append_from(&patterns);
        assert_eq!(patterns.signature(), copy.signature());
        assert!(copy.matches("mp_another_mixpanel"));
        assert!(copy.matches("wp-settings-9"));
        let before = copy.signature();
        assert!(copy.add("x*[0-9]+").is_err());
        assert_eq!(before, copy.signature());
    }

    #[test]
    fn builtin_covers_required_vendors() {
        // One exact and one prefix sample per required vendor.
        for name in [
            "__utma",                       // Legacy GA exact.
            "_ga",                          // UA/GA4 exact.
            "_ga_ABC123",                   // GA4 container prefix.
            "_gid",                         // UA/GA4.
            "_gat_gtag_UA_1",               // UA per-tracker prefix.
            "_gac_UA_1",                    // Ads campaign prefix.
            "_gcl_au",                      // Ads click prefix.
            "__gads",                       // Ads exact.
            "_fplc",                        // Ads first-party exact.
            "_FPGCLAW",                     // Ads first-party exact.
            "_fbp",                         // Meta exact.
            "_fbc",                         // Meta exact.
            "_clck",                        // Clarity exact.
            "_clsk",                        // Clarity exact.
            "_hjFirstSeen",                 // Hotjar exact.
            "_hjSession_9",                 // Hotjar prefix.
            "_hjSessionUser_9",             // Hotjar prefix.
            "_hjIncludedInSessionSample_9", // Hotjar prefix.
            "hjDonePolls",                  // Hotjar legacy exact.
        ] {
            assert!(builtin_ignored(name), "{name}");
        }
    }

    #[test]
    fn builtin_rejects_personalization_and_case_variants() {
        for name in [
            "sessionid",
            "PHPSESSID",
            "JSESSIONID",
            "cart",
            "wordpress_logged_in_x",
            "csrftoken",
            "auth",
            "_gax",  // Near-miss: no such GA cookie.
            "_gatx", // Near-miss of the `_gat_` prefix.
            "_GA",   // Cookie names are case-sensitive.
            "_Ga_ABC",
            "ga",
            "",
        ] {
            assert!(!builtin_ignored(name), "{name}");
        }
    }

    #[test]
    fn evaluate_absent_and_allowlisted() {
        let ignore = CookiePatternSet::new();
        let bypass = CookiePatternSet::new();
        let policy = default_policy(&ignore, &bypass);
        assert_eq!(
            evaluate_cookie_headers(&[], &policy),
            CookieDecision::Absent
        );
        assert_eq!(
            evaluate_cookie_headers(&[("Accept", "text/html")], &policy),
            CookieDecision::Absent
        );
        // Only allowlisted cookies: accepted like a cookie-free request.
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "_ga=1; _gid=2; _ga_ABC=3")], &policy),
            CookieDecision::Allowlisted { count: 3 }
        );
        // Values never matter.
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "_ga=completely-different-value")], &policy),
            CookieDecision::Allowlisted { count: 1 }
        );
        // Multiple Cookie lines combine.
        assert_eq!(
            evaluate_cookie_headers(
                &[("Cookie", "_ga=1"), ("Cookie", "_fbp=2; _clck=3")],
                &policy
            ),
            CookieDecision::Allowlisted { count: 3 }
        );
        // Empty segments and OWS are benign.
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "  _ga=1 ; ; _gid=2; ")], &policy),
            CookieDecision::Allowlisted { count: 2 }
        );
        // Quoted values may contain semicolons.
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "_ga=\"1;2\"; _gid=3")], &policy),
            CookieDecision::Allowlisted { count: 2 }
        );
    }

    #[test]
    fn evaluate_bypasses_unknown_and_mixed() {
        let ignore = CookiePatternSet::new();
        let bypass = CookiePatternSet::new();
        let policy = default_policy(&ignore, &bypass);
        // Unknown cookies bypass with the offending name (never value).
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "sessionid=abc")], &policy),
            CookieDecision::Bypass(CookieBypass::Cookie("sessionid".to_string()))
        );
        // Mixed allowlisted + unknown bypasses on the unknown one.
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "_ga=1; cart=full; _gid=2")], &policy),
            CookieDecision::Bypass(CookieBypass::Cookie("cart".to_string()))
        );
    }

    #[test]
    fn evaluate_fails_closed() {
        let ignore = CookiePatternSet::new();
        let bypass = CookiePatternSet::new();
        let policy = default_policy(&ignore, &bypass);
        // Valueless pairs, empty names, and unterminated quotes bypass.
        for header in ["oops", "_ga=1; oops", "=1", "_ga=1; =2", "_ga=\"1"] {
            assert_eq!(
                evaluate_cookie_headers(&[("Cookie", header)], &policy),
                CookieDecision::Bypass(CookieBypass::Malformed),
                "{header}"
            );
        }
        // Obsolete Cookie2 bypasses outright.
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie2", "$Version=1")], &policy),
            CookieDecision::Bypass(CookieBypass::Cookie2)
        );
        // Cookie-count bombs bypass.
        let flood: Vec<String> = (0..MAX_COOKIES_PER_REQUEST + 1)
            .map(|index| format!("_ga{index}=1"))
            .collect();
        let joined = flood.join("; ");
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", &joined)], &policy),
            CookieDecision::Bypass(CookieBypass::Malformed)
        );
    }

    #[test]
    fn custom_lists_extend_override_and_disable() {
        let mut ignore = CookiePatternSet::new();
        ignore.add("tracking_pixel").unwrap();
        ignore.add("_pk_*").unwrap();
        let mut bypass = CookiePatternSet::new();
        bypass.add("_ga").unwrap();
        // Custom entries extend the built-in list …
        let policy = default_policy(&ignore, &bypass);
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "tracking_pixel=1; _pk_id=2")], &policy),
            CookieDecision::Allowlisted { count: 2 }
        );
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "_gid=1; tracking_pixel=2")], &policy),
            CookieDecision::Allowlisted { count: 2 }
        );
        // … while bypass wins over both allowlists.
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "_ga=1")], &policy),
            CookieDecision::Bypass(CookieBypass::Cookie("_ga".to_string()))
        );
        // Disabling the common list keeps custom entries working.
        let empty_bypass = CookiePatternSet::new();
        let policy = make_policy(false, &ignore, &empty_bypass);
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "tracking_pixel=1")], &policy),
            CookieDecision::Allowlisted { count: 1 }
        );
        assert_eq!(
            evaluate_cookie_headers(&[("Cookie", "_ga=1")], &policy),
            CookieDecision::Bypass(CookieBypass::Cookie("_ga".to_string()))
        );
    }

    #[test]
    fn patterns_validate_match_and_sign() {
        let mut set = CookiePatternSet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        set.add("exact_name").unwrap();
        set.add("_pk_*").unwrap();
        assert!(!set.is_empty());
        assert_eq!(set.len(), 2);
        assert!(set.matches("exact_name"));
        assert!(!set.matches("exact_name_x"));
        assert!(set.matches("_pk_id.1.2"));
        assert!(!set.matches("_pk"));
        // Lone `*` matches everything (explicit admin choice).
        set.add("*").unwrap();
        assert!(set.matches("sessionid"));
        assert!(set.matches("anything"));
        // Duplicates collapse.
        set.add("exact_name").unwrap();
        set.add("_pk_*").unwrap();
        set.add("*").unwrap();
        assert_eq!(set.len(), 3);
        // Invalid patterns fail without mutating.
        for bad in ["", "x[0-9]+y", "a b", "a=b", "a;b", "a,b", "a\tb"] {
            assert!(set.add(bad).is_err(), "{bad:?}");
        }
        assert_eq!(set.len(), 3);
        assert!(!CookiePatternError::Empty.message().is_empty());
        assert!(!CookiePatternError::BadChar(';').message().is_empty());
        assert!(!CookiePatternError::MisplacedWildcard.message().is_empty());
        // Merge appends; signatures are canonical and sensitive.
        let mut merged = CookiePatternSet::new();
        merged.append_from(&set);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged.signature(), set.signature());
        let mut other = CookiePatternSet::new();
        other.add("zzz").unwrap();
        other.add("aaa").unwrap();
        assert_ne!(other.signature(), set.signature());
        // Case-sensitive: admin case must match the cookie case.
        assert!(!other.matches("AAA"));
        assert!(other.matches("aaa"));
    }

    #[test]
    fn bypass_names_truncate_and_lists_render() {
        let long = "n".repeat(MAX_REASON_NAME_LEN + 50);
        let header = format!("{long}=1");
        let ignore = CookiePatternSet::new();
        let bypass = CookiePatternSet::new();
        let policy = default_policy(&ignore, &bypass);
        match evaluate_cookie_headers(&[("Cookie", &header)], &policy) {
            CookieDecision::Bypass(CookieBypass::Cookie(name)) => {
                assert_eq!(name.len(), MAX_REASON_NAME_LEN);
                assert!(!name.contains('='));
            }
            other => panic!("must bypass, got {other:?}"),
        }
        // Display helpers cover exacts and prefixes.
        let patterns = builtin_ignore_patterns();
        assert_eq!(
            patterns.len(),
            BUILTIN_IGNORE_EXACT.len()
                + BUILTIN_IGNORE_PREFIXES.len()
                + BUILTIN_IGNORE_NUMERIC.len()
                + BUILTIN_IGNORE_INFIX.len()
        );
        assert!(patterns.iter().any(|pattern| pattern == "_ga"));
        assert!(patterns.iter().any(|pattern| pattern == "_ga_*"));
        // The match arms and the slice stay in sync.
        for name in BUILTIN_IGNORE_EXACT {
            assert!(builtin_ignored(name), "{name}");
        }
        for prefix in BUILTIN_IGNORE_PREFIXES {
            assert!(builtin_ignored(&format!("{prefix}probe")));
        }
    }
}

//! HTML-cache cookie policy: which request cookies may be ignored.
//!
//! Browsers attach cookies to every request, including analytics and
//! tracking cookies that never change the HTML a server generates. The
//! historical rule bypassed the HTML cache for any `Cookie` header, so
//! real-world pages (Google Analytics alone sets cookies on most
//! sites) never cached. This module implements the allowlist rule:
//! requests carrying only ignorable cookies cache exactly like
//! cookie-free requests, with cookie values excluded from the cache
//! key, while unknown, authentication, session, cart, and
//! personalization cookies still bypass.
//!
//! Security contract: an allowlisted cookie MUST NOT influence
//! server-generated HTML. The built-in list covers first-party
//! analytics/tracking names only (values are consumed client-side by
//! third-party scripts). Administrators extending the list with
//! `HtmlCacheIgnoreCookie` take responsibility for that invariant;
//! when in doubt, leave the cookie bypassing (the default) or force
//! it with `HtmlCacheBypassCookie`. Cookie values are never read
//! beyond parsing, never logged, and never enter diagnostics — only
//! cookie names appear in bypass reasons.
//!
//! Matching is allocation-free on the accept path: the built-in list
//! is a `match` plus prefix checks, and custom patterns compile at
//! config time into exact-name sets plus prefix lists (no regular
//! expressions anywhere near the request path).
#![forbid(unsafe_code)]

use std::collections::BTreeSet;

/// Maximum cookies honored per request (more fail closed).
pub const MAX_COOKIES_PER_REQUEST: usize = 256;

/// Maximum cookie-name characters kept in a bypass reason.
pub const MAX_REASON_NAME_LEN: usize = 128;

/* Built-in allowlist (first-party analytics/tracking names). */

/// Built-in ignored cookie names (exact, case-sensitive).
pub const BUILTIN_IGNORE_EXACT: &[&str] = &[
    // Legacy Google Analytics (ga.js / urchin.js).
    "__utma", "__utmb", "__utmc", "__utmt", "__utmv", "__utmx", "__utmxx", "__utmz",
    // Universal Analytics (analytics.js) and GA4 (gtag.js) base names.
    "_ga", "_gat", "_gid",
    // Google Ads first-party (`__gpi` gates ad personalization
    // client-side; it never changes server HTML).
    "__gads", "__gac", "__gpi", "_fplc", "_FPAU", "_FPGS", "_FPGCLAW", "_FPGCLDC",
    "_FPGCLGB",
    // Meta Pixel first-party.
    "_fbp", "_fbc",
    // Microsoft Clarity first-party.
    "_clck", "_clsk",
    // Hotjar fixed names (modern `_hj*` plus legacy `hj*`).
    "_hjFirstSeen",
    "_hjAbsoluteSessionInProgress",
    "_hjCachedUserAttributes",
    "_hjViewportId",
    "_hjTLDTest",
    "hjClosedSurveyInvites",
    "hjDonePolls",
    "hjMinimizedPolls",
    "hjDoneTestUsers",
    "hjIncludedInSample",
    "hjShownFeedbackMessage",
];

/// Built-in ignored cookie-name prefixes (matched without the `*`).
pub const BUILTIN_IGNORE_PREFIXES: &[&str] = &[
    // GA4 container cookies (`_ga_<container-id>`).
    "_ga_",
    // Universal Analytics per-tracker gate (`_gat_gtag_<id>`).
    "_gat_",
    // Google Ads campaign (`_gac_<property>`, `_gac_gb_<id>`).
    "_gac_",
    // Google Ads click IDs (`_gcl_aw/dc/gb/gs/au`).
    "_gcl_",
    // Hotjar session/sample/survey/poll state (`_hjSession_<id>`,
    // `_hjSessionUser_<id>`, `_hjIncludedInSessionSample_<id>`, …).
    "_hjSession",
    "_hjIncludedIn",
    "_hjClosed",
    "_hjDone",
    "_hjMinimized",
    "_hjShown",
];

/// Whether the built-in allowlist ignores a cookie name
/// (case-sensitive: cookie names are case-sensitive per RFC 6265,
/// so `_GA` is a different cookie from `_ga` and still bypasses).
#[must_use]
pub fn builtin_ignored(name: &str) -> bool {
    if BUILTIN_IGNORE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return true;
    }
    matches!(
        name,
        "__utma"
            | "__utmb"
            | "__utmc"
            | "__utmt"
            | "__utmv"
            | "__utmx"
            | "__utmxx"
            | "__utmz"
            | "_ga"
            | "_gat"
            | "_gid"
            | "__gads"
            | "__gac"
            | "__gpi"
            | "_fplc"
            | "_FPAU"
            | "_FPGS"
            | "_FPGCLAW"
            | "_FPGCLDC"
            | "_FPGCLGB"
            | "_fbp"
            | "_fbc"
            | "_clck"
            | "_clsk"
            | "_hjFirstSeen"
            | "_hjAbsoluteSessionInProgress"
            | "_hjCachedUserAttributes"
            | "_hjViewportId"
            | "_hjTLDTest"
            | "hjClosedSurveyInvites"
            | "hjDonePolls"
            | "hjMinimizedPolls"
            | "hjDoneTestUsers"
            | "hjIncludedInSample"
            | "hjShownFeedbackMessage"
    )
}

/// Built-in allowlist as display patterns (prefixes with `*`).
#[must_use]
pub fn builtin_ignore_patterns() -> Vec<String> {
    let mut patterns: Vec<String> = BUILTIN_IGNORE_EXACT
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    patterns.extend(
        BUILTIN_IGNORE_PREFIXES
            .iter()
            .map(|prefix| format!("{prefix}*")),
    );
    patterns
}

/* Custom pattern sets (`HtmlCacheIgnoreCookie` / `HtmlCacheBypassCookie`). */

/// Why a cookie pattern was rejected at config time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookiePatternError {
    /// Empty pattern.
    Empty,
    /// Forbidden character (`;`, `=`, `,`, whitespace, control).
    BadChar(char),
    /// `*` anywhere but the final position.
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
                "wildcard '*' is only allowed as the final character".to_string()
            }
        }
    }
}

/// Compiled cookie-name patterns: exact names plus `prefix*`
/// matchers. Matching borrows the candidate (no allocation);
/// compilation happens once per config change, never per request.
///
/// Patterns are case-sensitive exact names, a trailing `*` making
/// the rest a prefix (`_pk_*` ignores `_pk_id.1.x`). A lone `*`
/// matches every cookie. Only allow cookies that cannot influence
/// server-generated HTML.
#[derive(Debug, Clone, Default)]
pub struct CookiePatternSet {
    exact: BTreeSet<String>,
    prefixes: Vec<String>,
    raw: Vec<String>,
}

impl CookiePatternSet {
    /// Empty set (matches nothing).
    #[must_use]
    pub fn new() -> Self {
        CookiePatternSet::default()
    }

    /// Adds a pattern, compiling it into the exact or prefix table
    /// (duplicates ignored). Fails without mutating on invalid
    /// patterns.
    pub fn add(&mut self, pattern: &str) -> Result<(), CookiePatternError> {
        if pattern.is_empty() {
            return Err(CookiePatternError::Empty);
        }
        if let Some(char) = pattern
            .chars()
            .find(|char| *char == ';' || *char == '=' || *char == ',' || char.is_whitespace() || char.is_control())
        {
            return Err(CookiePatternError::BadChar(char));
        }
        let (body, is_prefix) = match pattern.strip_suffix('*') {
            Some(body) => (body, true),
            None => (pattern, false),
        };
        if body.contains('*') {
            return Err(CookiePatternError::MisplacedWildcard);
        }
        if body.is_empty() && !is_prefix {
            return Err(CookiePatternError::Empty);
        }
        if is_prefix {
            if !self.prefixes.iter().any(|prefix| prefix == body) {
                self.prefixes.push(body.to_string());
            }
        } else if !self.exact.contains(body) {
            self.exact.insert(body.to_string());
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
pub fn evaluate_cookie_headers(
    headers: &[(&str, &str)],
    policy: &CookiePolicy,
) -> CookieDecision {
    let mut names: Vec<&str> = Vec::new();
    let mut saw_cookie_header = false;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("Cookie2") {
            return CookieDecision::Bypass(CookieBypass::Cookie2);
        }
        if !name.eq_ignore_ascii_case("Cookie") {
            continue;
        }
        saw_cookie_header = true;
        if parse_cookie_names(value, &mut names).is_err() {
            return CookieDecision::Bypass(CookieBypass::Malformed);
        }
    }
    if !saw_cookie_header {
        return CookieDecision::Absent;
    }
    if names.len() > MAX_COOKIES_PER_REQUEST {
        return CookieDecision::Bypass(CookieBypass::Malformed);
    }
    for name in &names {
        // Explicit bypass wins over both allowlists.
        if policy.bypass.matches(name) {
            return CookieDecision::Bypass(CookieBypass::Cookie(truncate_name(name)));
        }
        let allowlisted =
            (policy.common_enabled && builtin_ignored(name)) || policy.ignore.matches(name);
        if !allowlisted {
            return CookieDecision::Bypass(CookieBypass::Cookie(truncate_name(name)));
        }
    }
    CookieDecision::Allowlisted { count: names.len() }
}

/// Parses one `Cookie` header value into names (RFC 6265
/// `cookie-pair *( ";" SP cookie-pair )`): splits on `;` outside
/// double quotes, drops empties, and requires `name=value` with a
/// non-empty name. Values are skipped, never retained.
fn parse_cookie_names<'a>(value: &'a str, names: &mut Vec<&'a str>) -> Result<(), ()> {
    let bytes = value.as_bytes();
    let mut start = 0;
    let mut in_quotes = false;
    let mut index = 0;
    while index <= bytes.len() {
        let boundary = index == bytes.len()
            || (!in_quotes && bytes[index] == b';');
        if index < bytes.len() && bytes[index] == b'"' {
            in_quotes = !in_quotes;
        }
        if boundary {
            let piece = value[start..index].trim_matches([' ', '\t']);
            if !piece.is_empty() {
                let name = piece.split('=').next().unwrap_or("");
                let name = name.trim_matches([' ', '\t']);
                if !piece.contains('=') || name.is_empty() {
                    return Err(());
                }
                names.push(name);
            }
            start = index + 1;
        }
        index += 1;
    }
    if in_quotes {
        return Err(());
    }
    Ok(())
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
    fn builtin_covers_required_vendors() {
        // One exact and one prefix sample per required vendor.
        for name in [
            "__utma",      // Legacy GA exact.
            "_ga",         // UA/GA4 exact.
            "_ga_ABC123",  // GA4 container prefix.
            "_gid",        // UA/GA4.
            "_gat_gtag_UA_1", // UA per-tracker prefix.
            "_gac_UA_1",   // Ads campaign prefix.
            "_gcl_au",     // Ads click prefix.
            "__gads",      // Ads exact.
            "_fplc",       // Ads first-party exact.
            "_FPGCLAW",    // Ads first-party exact.
            "_fbp",        // Meta exact.
            "_fbc",        // Meta exact.
            "_clck",       // Clarity exact.
            "_clsk",       // Clarity exact.
            "_hjFirstSeen", // Hotjar exact.
            "_hjSession_9", // Hotjar prefix.
            "_hjSessionUser_9", // Hotjar prefix.
            "_hjIncludedInSessionSample_9", // Hotjar prefix.
            "hjDonePolls", // Hotjar legacy exact.
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
        for bad in ["", "a*b", "*x", "a b", "a=b", "a;b", "a,b", "a\tb"] {
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
            BUILTIN_IGNORE_EXACT.len() + BUILTIN_IGNORE_PREFIXES.len()
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

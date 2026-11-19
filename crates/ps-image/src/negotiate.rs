//! Browser-aware format negotiation.
//!
//! Ports the `Accept`-header parsing (`AcceptsImageFormat`,
//! `ExplicitlyRejectsImageFormat`, `ParseQualityParameter`), the
//! `UserAgentMatcher` WebP/AVIF rules (wildcard tables, version floors,
//! `IsKnownAvifUnsupported`), the `PS-CapabilityList` downstream gate, the
//! device/request support predicates, `SetLibWebpLevel`, and the
//! `SetWebpCompressionOptions` mapping into [`crate::image`] options.

use crate::image::{CompressionOptions, PreferredWebp};

/// Trims HTTP whitespace (spaces and tabs) from both ends.
fn trim_http_whitespace(value: &str) -> &str {
    value.trim_matches(|c| c == ' ' || c == '\t')
}

/// Parses one `q=value` parameter (`ParseQualityParameter`).
///
/// Returns true when the parameter names `q` (case-insensitive); the value
/// is set to the parsed double, or 0 when the number is malformed, NaN, or
/// outside 0..=1 — including unparseable numbers, which therefore count as
/// explicit rejects downstream.
fn parse_quality_parameter(parameter: &str, quality: &mut f64) -> bool {
    let Some(equals) = parameter.find('=') else {
        return false;
    };
    if !trim_http_whitespace(&parameter[..equals]).eq_ignore_ascii_case("q") {
        return false;
    }
    let text = trim_http_whitespace(&parameter[equals + 1..]);
    // strtod parity: full-string doubles only; hex/inf/nan spellings that
    // Rust rejects (or accepts out of range) collapse to 0 either way.
    let parsed = text.parse::<f64>().unwrap_or(f64::NAN);
    if parsed.is_nan() || parsed < 0.0 || parsed > 1.0 {
        *quality = 0.0;
    } else {
        *quality = parsed;
    }
    true
}

/// Splits a header value on commas, honoring no quoting (like
/// `SplitStringPieceToVector` with skip-empty).
fn split_commas(value: &str) -> Vec<&str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// True when any `Accept` value lists `mime_type` with quality above zero.
pub fn accepts_image_format(accept_values: &[&str], mime_type: &str) -> bool {
    for value in accept_values {
        for entry in split_commas(value) {
            let parts: Vec<&str> = entry.split(';').collect();
            if parts.is_empty() || !trim_http_whitespace(parts[0]).eq_ignore_ascii_case(mime_type) {
                continue;
            }
            let mut quality = 1.0;
            for part in &parts[1..] {
                parse_quality_parameter(part, &mut quality);
            }
            if quality > 0.0 {
                return true;
            }
        }
    }
    false
}

/// True when any `Accept` value lists `mime_type` with an explicit zero
/// quality (including malformed q-values, which parse to 0).
pub fn explicitly_rejects_image_format(accept_values: &[&str], mime_type: &str) -> bool {
    for value in accept_values {
        for entry in split_commas(value) {
            let parts: Vec<&str> = entry.split(';').collect();
            if parts.is_empty() || !trim_http_whitespace(parts[0]).eq_ignore_ascii_case(mime_type) {
                continue;
            }
            for part in &parts[1..] {
                let mut quality = 1.0;
                if parse_quality_parameter(part, &mut quality) && quality == 0.0 {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Wildcard matching (`*` runs, `?` singles, full-string match).
// ---------------------------------------------------------------------------

/// Matches `text` against a `*`/`?` pattern over the whole string.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    // Classic two-pointer backtracker for `*`.
    let (mut px, mut tx) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while tx < text.len() {
        if px < pattern.len() && (pattern[px] == b'?' || pattern[px] == text[tx]) {
            px += 1;
            tx += 1;
        } else if px < pattern.len() && pattern[px] == b'*' {
            star = Some(px);
            mark = tx;
            px += 1;
        } else if let Some(star_pos) = star {
            px = star_pos + 1;
            mark += 1;
            tx = mark;
        } else {
            return false;
        }
    }
    while px < pattern.len() && pattern[px] == b'*' {
        px += 1;
    }
    px == pattern.len()
}

/// Allow/disallow pattern group: the LAST-inserted matching pattern wins;
/// `default` applies when nothing matches (`WildcardGroup::Match`).
struct WildcardGroup {
    patterns: Vec<(&'static str, bool)>,
}

impl WildcardGroup {
    fn new(patterns: &[(&'static str, bool)]) -> Self {
        WildcardGroup {
            patterns: patterns.to_vec(),
        }
    }

    fn matches(&self, text: &str, default: bool) -> bool {
        for (pattern, allow) in self.patterns.iter().rev() {
            if wildcard_match(pattern, text) {
                return *allow;
            }
        }
        default
    }
}

// ---------------------------------------------------------------------------
// User-agent matching (WebP/AVIF subset of `UserAgentMatcher`).
// ---------------------------------------------------------------------------

const LEGACY_WEBP_ALLOW: [&str; 1] = ["*Android *"];
const LEGACY_WEBP_BLOCK: [&str; 10] = [
    "*Android 0.*",
    "*Android 1.*",
    "*Android 2.*",
    "*Android 3.*",
    "*Firefox/*",
    "*Edge/*",
    "*Trident/*",
    "*Windows Phone*",
    "*Chrome/*",
    "*CriOS/*",
];

const LOSSLESS_ALPHA_ALLOW: [&str; 10] = [
    "*Chrome/??.*",
    "*Chrome/???.*",
    "*CriOS/??.*",
    "*Edg/*",
    "*EdgA/*",
    "*EdgiOS/*",
    "*OPR/*",
    "webp-la",
    "webp-animated",
    "*Firefox/*",
];
const LOSSLESS_ALPHA_BLOCK: [&str; 23] = [
    "*Chrome/?.*",
    "*Chrome/1?.*",
    "*Chrome/20.*",
    "*Chrome/21.*",
    "*Chrome/22.*",
    "*CriOS/1?.*",
    "*CriOS/20.*",
    "*CriOS/21.*",
    "*CriOS/22.*",
    "*CriOS/23.*",
    "*CriOS/24.*",
    "*CriOS/25.*",
    "*CriOS/26.*",
    "*CriOS/27.*",
    "*CriOS/28.*",
    "*Firefox/?.*",
    "*Firefox/1?.*",
    "*Firefox/2?.*",
    "*Firefox/3?.*",
    "*Firefox/4?.*",
    "*Firefox/5?.*",
    "*Firefox/60.*",
    "*Firefox/61.*",
    // NOTE: the C++ table continues with 62/63/64 below; kept together here.
];

const ANIMATED_ALLOW: [&str; 8] = [
    "*Chrome/??.*",
    "*CriOS/??.*",
    "*Edg/*",
    "*EdgA/*",
    "*EdgiOS/*",
    "*OPR/*",
    "webp-animated",
    "*Firefox/*",
];
const ANIMATED_BLOCK: [&str; 20] = [
    "*Chrome/?.*",
    "*Chrome/1?.*",
    "*Chrome/2?.*",
    "*Chrome/30.*",
    "*Chrome/31.*",
    "*CriOS/?.*",
    "*CriOS/1?.*",
    "*CriOS/2?.*",
    "*CriOS/30.*",
    "*CriOS/31.*",
    "*Firefox/?.*",
    "*Firefox/1?.*",
    "*Firefox/2?.*",
    "*Firefox/3?.*",
    "*Firefox/4?.*",
    "*Firefox/5?.*",
    "*Firefox/60.*",
    "*Firefox/61.*",
    "*Firefox/62.*",
    "*Firefox/63.*",
];

const IE_PATTERNS: [&str; 4] = ["*MSIE *", "*rv:11.?) like Gecko*", "*IE 1*", "*Trident/7*"];

fn contains_token(ua: &str, token: &str) -> bool {
    ua.contains(token)
}

/// Parses `major[.minor]` after the first `token` (`ParseVersionAfter`).
fn parse_version_after(ua: &str, token: &str) -> Option<(u32, u32)> {
    let pos = ua.find(token)?;
    let mut version = &ua[pos + token.len()..];
    let first = version.as_bytes().first()?;
    if !first.is_ascii_digit() {
        return None;
    }
    let mut major: u32 = 0;
    while let Some(digit) = version.as_bytes().first() {
        if !digit.is_ascii_digit() {
            break;
        }
        major = major
            .saturating_mul(10)
            .saturating_add(u32::from(digit - b'0'));
        version = &version[1..];
    }
    let mut minor: u32 = 0;
    if let Some(sep) = version.as_bytes().first() {
        if *sep == b'.' || *sep == b'_' {
            version = &version[1..];
            while let Some(digit) = version.as_bytes().first() {
                if !digit.is_ascii_digit() {
                    break;
                }
                minor = minor
                    .saturating_mul(10)
                    .saturating_add(u32::from(digit - b'0'));
                version = &version[1..];
            }
        }
    }
    Some((major, minor))
}

fn version_at_least(version: (u32, u32), required: (u32, u32)) -> bool {
    version.0 > required.0 || (version.0 == required.0 && version.1 >= required.1)
}

fn is_ios_user_agent(ua: &str) -> bool {
    contains_token(ua, "iPhone")
        || contains_token(ua, "iPad")
        || contains_token(ua, "iPod")
        || contains_token(ua, "CPU OS ")
}

fn ios_version_at_least_16(ua: &str) -> bool {
    (parse_version_after(ua, "iPhone OS ").or_else(|| parse_version_after(ua, "CPU OS ")))
        .is_some_and(|version| version_at_least(version, (16, 0)))
}

/// IE detection for the Vary-private rule (`IsIe`).
pub fn is_ie(ua: &str) -> bool {
    IE_PATTERNS
        .iter()
        .any(|pattern| wildcard_match(pattern, ua))
}

/// Legacy Android WebP allowlist (`LegacyWebp`).
pub fn legacy_webp(ua: &str) -> bool {
    if LEGACY_WEBP_BLOCK
        .iter()
        .any(|pattern| wildcard_match(pattern, ua))
    {
        return false;
    }
    LEGACY_WEBP_ALLOW
        .iter()
        .any(|pattern| wildcard_match(pattern, ua))
}

/// WebP support from the UA alone (`SupportsWebpRewrittenUrls`).
pub fn ua_supports_webp(ua: &str) -> bool {
    if legacy_webp(ua) {
        return true;
    }
    if contains_token(ua, "Firefox/") {
        return parse_version_after(ua, "Firefox/")
            .is_some_and(|version| version_at_least(version, (65, 0)));
    }
    if contains_token(ua, "Chrome/") || contains_token(ua, "Chromium/") {
        let token = if contains_token(ua, "Chrome/") {
            "Chrome/"
        } else {
            "Chromium/"
        };
        return parse_version_after(ua, token)
            .is_some_and(|version| version_at_least(version, (32, 0)));
    }
    if contains_token(ua, "OPR/") {
        return parse_version_after(ua, "OPR/")
            .is_some_and(|version| version_at_least(version, (19, 0)));
    }
    if contains_token(ua, "Version/") && contains_token(ua, "Safari/") && is_ios_user_agent(ua) {
        return (parse_version_after(ua, "iPhone OS ")
            .or_else(|| parse_version_after(ua, "CPU OS ")))
        .is_some_and(|version| version_at_least(version, (14, 0)));
    }
    if contains_token(ua, "Version/")
        && contains_token(ua, "Safari/")
        && !contains_token(ua, "Chrome/")
        && !contains_token(ua, "CriOS/")
        && !contains_token(ua, "Edg")
    {
        let modern_macos = contains_token(ua, "Mac OS X 11_")
            || contains_token(ua, "Mac OS X 12_")
            || contains_token(ua, "Mac OS X 13_")
            || contains_token(ua, "Mac OS X 14_")
            || contains_token(ua, "Mac OS X 15_")
            || contains_token(ua, "Mac OS X 26_");
        return modern_macos
            && parse_version_after(ua, "Version/")
                .is_some_and(|version| version_at_least(version, (14, 0)));
    }
    false
}

fn lossless_alpha_group() -> WildcardGroup {
    let mut patterns = Vec::new();
    for pattern in LOSSLESS_ALPHA_ALLOW {
        patterns.push((pattern, true));
    }
    // Blocked last so it wins on overlap (insertion order in the C++).
    for pattern in LOSSLESS_ALPHA_BLOCK {
        patterns.push((pattern, false));
    }
    for pattern in ["*Firefox/62.*", "*Firefox/63.*", "*Firefox/64.*"] {
        patterns.push((pattern, false));
    }
    WildcardGroup::new(&patterns)
}

fn animated_group() -> WildcardGroup {
    let mut patterns = Vec::new();
    for pattern in ANIMATED_ALLOW {
        patterns.push((pattern, true));
    }
    for pattern in ANIMATED_BLOCK {
        patterns.push((pattern, false));
    }
    patterns.push(("*Firefox/64.*", false));
    WildcardGroup::new(&patterns)
}

/// Lossless/alpha WebP support from the UA alone.
pub fn ua_supports_webp_lossless_alpha(ua: &str) -> bool {
    lossless_alpha_group().matches(ua, false)
}

/// Animated WebP support from the UA alone.
pub fn ua_supports_webp_animated(ua: &str) -> bool {
    animated_group().matches(ua, false)
}

/// AVIF support from the UA alone (`SupportsAvifRewrittenUrls`).
pub fn ua_supports_avif(ua: &str) -> bool {
    if is_known_avif_unsupported(ua) {
        return false;
    }
    if contains_token(ua, "Opera Mini") || contains_token(ua, "KaiOS") {
        return false;
    }
    if contains_token(ua, "Opera Mobi") || contains_token(ua, "Opera Mobile") {
        return (parse_version_after(ua, "Opera Mobile/")
            .is_some_and(|version| version_at_least(version, (80, 0))))
            || (parse_version_after(ua, "Opera Mobi/")
                .is_some_and(|version| version_at_least(version, (80, 0))));
    }
    if contains_token(ua, "EdgiOS/")
        || (is_ios_user_agent(ua) && (contains_token(ua, "CriOS/") || contains_token(ua, "FxiOS/")))
    {
        return ios_version_at_least_16(ua);
    }
    if contains_token(ua, "SamsungBrowser/") {
        return parse_version_after(ua, "SamsungBrowser/")
            .is_some_and(|version| version_at_least(version, (14, 0)));
    }
    if contains_token(ua, "UCBrowser/") {
        return parse_version_after(ua, "UCBrowser/")
            .is_some_and(|version| version_at_least(version, (15, 5)));
    }
    if contains_token(ua, "BaiduBrowser/") {
        return parse_version_after(ua, "BaiduBrowser/")
            .is_some_and(|version| version_at_least(version, (13, 52)));
    }
    if contains_token(ua, "Version/")
        && contains_token(ua, "Safari/")
        && !contains_token(ua, "Chrome/")
        && !contains_token(ua, "CriOS/")
        && !contains_token(ua, "Edg")
    {
        if is_ios_user_agent(ua) {
            return ios_version_at_least_16(ua);
        }
        return parse_version_after(ua, "Version/")
            .is_some_and(|version| version_at_least(version, (16, 4)));
    }
    if contains_token(ua, "Firefox/") {
        return parse_version_after(ua, "Firefox/")
            .is_some_and(|version| version_at_least(version, (93, 0)));
    }
    if contains_token(ua, "OPR/") {
        return parse_version_after(ua, "OPR/")
            .is_some_and(|version| version_at_least(version, (71, 0)));
    }
    if contains_token(ua, "Edg/") {
        return parse_version_after(ua, "Edg/")
            .is_some_and(|version| version_at_least(version, (121, 0)));
    }
    if contains_token(ua, "EdgA/") {
        return parse_version_after(ua, "EdgA/")
            .is_some_and(|version| version_at_least(version, (121, 0)));
    }
    if contains_token(ua, "EdgiOS/") {
        return ios_version_at_least_16(ua);
    }
    if contains_token(ua, "CriOS/") {
        return if is_ios_user_agent(ua) {
            ios_version_at_least_16(ua)
        } else {
            parse_version_after(ua, "CriOS/")
                .is_some_and(|version| version_at_least(version, (85, 0)))
        };
    }
    if contains_token(ua, "Chrome/") || contains_token(ua, "Chromium/") {
        let token = if contains_token(ua, "Chrome/") {
            "Chrome/"
        } else {
            "Chromium/"
        };
        return parse_version_after(ua, token)
            .is_some_and(|version| version_at_least(version, (85, 0)));
    }
    false
}

/// UAs known to lack AVIF (`IsKnownAvifUnsupported`), in C++ order.
pub fn is_known_avif_unsupported(ua: &str) -> bool {
    if ua.is_empty() {
        return false;
    }
    if is_ie(ua) || contains_token(ua, "Edge/") || contains_token(ua, "Opera Mini") {
        return true;
    }
    if contains_token(ua, "KaiOS") {
        return true;
    }
    if contains_token(ua, "AndroidBrowser/") || contains_token(ua, "Android Browser") {
        return true;
    }
    if contains_token(ua, "Android ")
        && !contains_token(ua, "Chrome/")
        && !contains_token(ua, "Chromium/")
    {
        if let Some(version) = parse_version_after(ua, "Android ") {
            if version.0 < 5 || (version.0 == 4 && version.1 <= 4) {
                return true;
            }
        }
    }
    if contains_token(ua, "Opera Mobi") || contains_token(ua, "Opera Mobile") {
        if parse_version_after(ua, "Opera Mobi/").is_some()
            || parse_version_after(ua, "Opera Mobile/").is_some()
        {
            let version = parse_version_after(ua, "Opera Mobi/")
                .or_else(|| parse_version_after(ua, "Opera Mobile/"))
                .unwrap();
            return !version_at_least(version, (80, 0));
        }
        if !contains_token(ua, "OPR/") {
            if let Some((major, minor)) = parse_version_after(ua, "Version/") {
                return major < 13 || (major == 12 && minor <= 1);
            }
        }
    }
    if contains_token(ua, "Opera/") && !contains_token(ua, "OPR/") {
        if let Some((major, minor)) = parse_version_after(ua, "Version/") {
            return major < 13 || (major == 12 && minor <= 1);
        }
        return true;
    }
    if (contains_token(ua, "EdgiOS/")
        || (is_ios_user_agent(ua)
            && (contains_token(ua, "CriOS/") || contains_token(ua, "FxiOS/"))))
        && !ios_version_at_least_16(ua)
    {
        return true;
    }
    if contains_token(ua, "Version/") && contains_token(ua, "Safari/") && is_ios_user_agent(ua) {
        // Decisive either way: iOS Safari never reaches the desktop branch.
        return !ios_version_at_least_16(ua);
    }
    if contains_token(ua, "Version/")
        && contains_token(ua, "Safari/")
        && !contains_token(ua, "Chrome/")
        && !contains_token(ua, "CriOS/")
        && !contains_token(ua, "Edg")
    {
        return parse_version_after(ua, "Version/")
            .is_none_or(|version| !version_at_least(version, (16, 4)));
    }
    if contains_token(ua, "Safari/")
        && !contains_token(ua, "Chrome/")
        && !contains_token(ua, "CriOS/")
        && !contains_token(ua, "Edg")
        && !contains_token(ua, "OPR/")
        && !contains_token(ua, "Version/")
    {
        return true;
    }
    if contains_token(ua, "Edg/") {
        if let Some(version) = parse_version_after(ua, "Edg/") {
            return !version_at_least(version, (121, 0));
        }
    }
    if contains_token(ua, "EdgA/") {
        if let Some(version) = parse_version_after(ua, "EdgA/") {
            return !version_at_least(version, (121, 0));
        }
    }
    if contains_token(ua, "Firefox/") {
        if let Some(version) = parse_version_after(ua, "Firefox/") {
            return !version_at_least(version, (93, 0));
        }
    }
    if contains_token(ua, "OPR/") {
        if let Some(version) = parse_version_after(ua, "OPR/") {
            return !version_at_least(version, (71, 0));
        }
    }
    if contains_token(ua, "Chrome/") || contains_token(ua, "Chromium/") {
        let token = if contains_token(ua, "Chrome/") {
            "Chrome/"
        } else {
            "Chromium/"
        };
        if let Some(version) = parse_version_after(ua, token) {
            return !version_at_least(version, (85, 0));
        }
    }
    if contains_token(ua, "UCBrowser/") {
        return parse_version_after(ua, "UCBrowser/")
            .is_none_or(|version| !version_at_least(version, (15, 5)));
    }
    if contains_token(ua, "BaiduBrowser/") {
        return parse_version_after(ua, "BaiduBrowser/")
            .is_none_or(|version| !version_at_least(version, (13, 52)));
    }
    if contains_token(ua, "QQBrowser/") {
        // Only 14.9 is called out (a version with broken AVIF).
        return parse_version_after(ua, "QQBrowser/").is_some_and(|version| version == (14, 9));
    }
    if contains_token(ua, "SamsungBrowser/") {
        return parse_version_after(ua, "SamsungBrowser/")
            .is_none_or(|version| !version_at_least(version, (14, 0)));
    }
    false
}

// ---------------------------------------------------------------------------
// Device and request support.
// ---------------------------------------------------------------------------

/// Device-level format support (Accept + UA combined).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSupport {
    pub webp_rewritten_urls: bool,
    pub avif_rewritten_urls: bool,
    pub webp_lossless_alpha: bool,
    pub webp_animated: bool,
}

/// Computes device support from `Accept` values and the UA string.
pub fn device_support(accept_values: &[&str], user_agent: &str) -> DeviceSupport {
    let accepts_webp = accepts_image_format(accept_values, "image/webp");
    let accepts_avif = accepts_image_format(accept_values, "image/avif");
    let webp_rejected = explicitly_rejects_image_format(accept_values, "image/webp");
    let avif_rejected = explicitly_rejects_image_format(accept_values, "image/avif");
    DeviceSupport {
        webp_rewritten_urls: !webp_rejected && (accepts_webp || ua_supports_webp(user_agent)),
        avif_rewritten_urls: !avif_rejected
            && !is_known_avif_unsupported(user_agent)
            && (accepts_avif || ua_supports_avif(user_agent)),
        webp_lossless_alpha: accepts_webp && ua_supports_webp_lossless_alpha(user_agent),
        webp_animated: accepts_webp && ua_supports_webp_animated(user_agent),
    }
}

/// Downstream `PS-CapabilityList` gate. Absent (or `NoCapabilitiesSpecified`)
/// means everything is supported; otherwise the filter-ID tokens (`jw`,
/// `ws`, `wa`) must appear as whole comma-separated entries, with optional
/// `:suffix` data allowed.
#[derive(Debug, Clone)]
pub struct CapabilityList {
    capabilities: String,
}

impl CapabilityList {
    /// No header sent: every capability is supported.
    #[must_use]
    pub fn absent() -> Self {
        CapabilityList {
            capabilities: "NoCapabilitiesSpecified".to_string(),
        }
    }

    /// Parses the raw header value.
    #[must_use]
    pub fn parse(header_value: &str) -> Self {
        CapabilityList {
            capabilities: header_value.to_string(),
        }
    }

    fn supports(&self, token: &str) -> bool {
        let list = self.capabilities.as_str();
        if list == "NoCapabilitiesSpecified" || list == token {
            return true;
        }
        if list.starts_with(&format!("{token}:")) || list.starts_with(&format!("{token},")) {
            return true;
        }
        list.contains(&format!(",{token}:")) || list.contains(&format!(",{token},"))
    }

    /// Downstream webp support (`jw` token).
    #[must_use]
    pub fn supports_webp(&self) -> bool {
        self.supports("jw")
    }

    /// Downstream lossless/alpha webp support (`ws` token).
    #[must_use]
    pub fn supports_webp_lossless_alpha(&self) -> bool {
        self.supports("ws")
    }

    /// Downstream animated webp support (`wa` token).
    #[must_use]
    pub fn supports_webp_animated(&self) -> bool {
        self.supports("wa")
    }
}

/// Request-level format support (device AND downstream capabilities; AVIF
/// has no downstream gate, like the C++).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestSupport {
    pub webp_rewritten_urls: bool,
    pub avif_rewritten_urls: bool,
    pub webp_lossless_alpha: bool,
    pub webp_animated: bool,
}

/// Combines device support with the capability list.
#[must_use]
pub fn request_support(device: DeviceSupport, capabilities: &CapabilityList) -> RequestSupport {
    RequestSupport {
        webp_rewritten_urls: capabilities.supports_webp() && device.webp_rewritten_urls,
        avif_rewritten_urls: device.avif_rewritten_urls,
        webp_lossless_alpha: capabilities.supports_webp_lossless_alpha()
            && device.webp_lossless_alpha,
        webp_animated: capabilities.supports_webp_animated() && device.webp_animated,
    }
}

/// WebP level for cache keys and URL encoding (`ResourceContext::LibWebpLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibWebpLevel {
    #[default]
    None,
    LossyOnly,
    LossyLosslessAlpha,
    Animated,
}

/// Enabled image filters relevant to level selection.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebpFilterFlags {
    pub recompress_webp: bool,
    pub convert_to_webp_animated: bool,
    pub convert_to_webp_lossless: bool,
    pub convert_jpeg_to_webp: bool,
}

/// Selects the libwebp level (`ImageUrlEncoder::SetLibWebpLevel`).
#[must_use]
pub fn select_libwebp_level(support: RequestSupport, filters: WebpFilterFlags) -> LibWebpLevel {
    if support.webp_animated && (filters.recompress_webp || filters.convert_to_webp_animated) {
        LibWebpLevel::Animated
    } else if support.webp_lossless_alpha
        && (filters.recompress_webp || filters.convert_to_webp_lossless)
    {
        LibWebpLevel::LossyLosslessAlpha
    } else if support.webp_rewritten_urls
        && (filters.recompress_webp
            || filters.convert_to_webp_lossless
            || filters.convert_jpeg_to_webp)
    {
        LibWebpLevel::LossyOnly
    } else {
        LibWebpLevel::None
    }
}

/// Applies the negotiated level to compression options
/// (`SetWebpCompressionOptions`).
pub fn apply_webp_level(
    level: LibWebpLevel,
    convert_to_webp_animated: bool,
    convert_to_webp_lossless: bool,
    options: &mut CompressionOptions,
) {
    match level {
        LibWebpLevel::None => {
            options.preferred_webp = PreferredWebp::None;
            options.allow_webp_alpha = false;
        }
        LibWebpLevel::LossyOnly => {
            options.preferred_webp = PreferredWebp::Lossy;
            options.allow_webp_alpha = false;
        }
        LibWebpLevel::Animated => {
            if convert_to_webp_animated {
                options.preferred_webp = PreferredWebp::Animated;
                options.allow_webp_animated = true;
                options.allow_webp_alpha = true;
                return;
            }
            options.allow_webp_alpha = true;
            set_lossy_or_lossless(convert_to_webp_lossless, options);
        }
        LibWebpLevel::LossyLosslessAlpha => {
            options.allow_webp_alpha = true;
            set_lossy_or_lossless(convert_to_webp_lossless, options);
        }
    }
}

fn set_lossy_or_lossless(convert_to_webp_lossless: bool, options: &mut CompressionOptions) {
    // Falls through from Animated when the animated filter is off, exactly
    // like the C++ FALLTHROUGH_INTENDED.
    if convert_to_webp_lossless {
        options.preferred_webp = PreferredWebp::Lossless;
    } else {
        options.preferred_webp = PreferredWebp::Lossy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_basics() {
        assert!(wildcard_match(
            "*Chrome/*",
            "Mozilla/5.0 Chrome/120.0 Safari/537.36"
        ));
        assert!(!wildcard_match("*Chrome/*", "Mozilla/5.0 Safari/537.36"));
        assert!(wildcard_match("*Chrome/??.*", "xx Chrome/32.1 yy"));
        assert!(!wildcard_match("*Chrome/??.*", "xx Chrome/3.1 yy"));
        assert!(wildcard_match("webp-la", "webp-la"));
        assert!(!wildcard_match("webp-la", "webp-lax"));
        assert!(wildcard_match("*", "anything"));
    }

    #[test]
    fn group_last_inserted_wins() {
        let group = WildcardGroup::new(&[("*Chrome/*", true), ("*Chrome/1?.*", false)]);
        assert!(!group.matches("A Chrome/15.0 B", false));
        assert!(group.matches("A Chrome/120.0 B", false));
        assert!(!group.matches("no match here", false));
        assert!(group.matches("no match here", true));
    }

    #[test]
    fn accept_quality_rules() {
        assert!(accepts_image_format(
            &["text/html, image/webp"],
            "image/webp"
        ));
        assert!(accepts_image_format(&["image/webp;q=0.5"], "image/webp"));
        assert!(!accepts_image_format(&["image/webp;q=0"], "image/webp"));
        assert!(!accepts_image_format(&["text/html"], "image/webp"));
        // Case-insensitive MIME with whitespace and extra params.
        assert!(accepts_image_format(
            &["Image/WebP ; q=1.0; foo=bar"],
            "image/webp"
        ));
        // Malformed q collapses to 0: not accepted, explicitly rejected.
        assert!(!accepts_image_format(
            &["image/webp;q=banana"],
            "image/webp"
        ));
        assert!(explicitly_rejects_image_format(
            &["image/webp;q=banana"],
            "image/webp"
        ));
        assert!(explicitly_rejects_image_format(
            &["image/webp;q=0"],
            "image/webp"
        ));
        assert!(!explicitly_rejects_image_format(
            &["image/webp"],
            "image/webp"
        ));
    }

    #[test]
    fn capability_list_tokens() {
        assert!(CapabilityList::absent().supports_webp());
        assert!(CapabilityList::parse("jw,wa,ws").supports_webp_animated());
        assert!(CapabilityList::parse("xx,jw:1,yy").supports_webp());
        assert!(!CapabilityList::parse("jw2,wsx").supports_webp());
        assert!(!CapabilityList::parse("").supports_webp());
        // Trailing bare tokens never match (C++ quirk, ported exactly).
        assert!(!CapabilityList::parse("jw,ws").supports_webp_lossless_alpha());
    }
}

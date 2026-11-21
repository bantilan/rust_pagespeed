//! Whole-HTML cache (`proxy_fetch.cc`, `signature.h`, closes caching-gap-1).
//!
//! Keys are `optimized-html-v1:` + a 27-char HMAC-SHA1 signature over
//! the identity string; values are `<created_ms>\n<expires_ms>\n<body>`.
//! Eligibility inputs mirror `ShouldCache` (method, status, cookies,
//! experiments, critical-CSS state, forbidden request headers,
//! origin cacheability); storage and validation mirror
//! `HtmlCacheLookup*` exactly, including the 1MB cap and the purge-set
//! check at serve time.
#![forbid(unsafe_code)]

use crate::base64::web64_encode;
use crate::purge::PurgeSet;

/// HTML-cache key prefix (before the signature).
pub const HTML_KEY_PREFIX: &str = "optimized-html-v1:";
/// HMAC signing key (`"pagespeed-optimized-html-v1"`).
pub const HTML_SIGNING_KEY: &str = "pagespeed-optimized-html-v1";
/// Signature length for 20 HMAC bytes (`ceil(20*4/3)` = 27).
pub const SIGNATURE_CHARS: usize = 27;
/// Maximum cached HTML body (`kMaxHtmlCacheBodySize` = 1MB).
pub const MAX_HTML_BODY_BYTES: usize = 1024 * 1024;
/// Maximum TTL, seconds (1 year, enforced at config time).
pub const MAX_HTML_TTL_SEC: i64 = 31_536_000;

/* Signatures (`SHA1Signature`). */

/// Signs data under a key: web64(HMAC-SHA1) truncated to
/// [`SIGNATURE_CHARS`] (`SHA1Signature::Sign`).
#[must_use]
pub fn sign_url(key: &str, data: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha1::Sha1;
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes()).expect("HMAC accepts any key");
    mac.update(data.as_bytes());
    let mut signature = web64_encode(&mac.finalize().into_bytes());
    signature.truncate(SIGNATURE_CHARS);
    signature
}

/// Builds an HTML-cache key for an identity string.
#[must_use]
pub fn html_cache_key(identity: &str) -> String {
    format!("{HTML_KEY_PREFIX}{}", sign_url(HTML_SIGNING_KEY, identity))
}

/* Eligibility inputs (`ShouldCache` parameters). */

/// Cacheability inputs for one response.
#[derive(Debug, Clone)]
pub struct HtmlEligibility {
    /// Caching enabled + positive TTL within a year.
    pub html_cache_enabled: bool,
    /// `html_cache_ttl_sec`.
    pub ttl_sec: i64,
    /// Request method is GET.
    pub is_get: bool,
    /// Origin status is 200.
    pub origin_ok: bool,
    /// No Set-Cookie(2) on the origin response.
    pub no_origin_cookies: bool,
    /// Response varies on nothing (Vary absent).
    pub vary_absent: bool,
    /// No experiment active.
    pub no_experiment: bool,
    /// PrioritizeCriticalCss not evaluating/admin.
    pub critical_css_ok: bool,
    /// Request carries no forbidden headers.
    pub request_headers_ok: bool,
    /// Origin proxy-cacheable (Vary-insensitive check).
    pub origin_cacheable: bool,
    /// Origin not already expired.
    pub origin_fresh: bool,
    /// Beacon enabled with no nonce in body.
    pub beacon_ok: bool,
}

impl HtmlEligibility {
    /// Whether the response may be cached (`ShouldCache`).
    #[must_use]
    pub fn cacheable(&self) -> bool {
        self.html_cache_enabled
            && self.ttl_sec > 0
            && self.ttl_sec <= MAX_HTML_TTL_SEC
            && self.is_get
            && self.origin_ok
            && self.no_origin_cookies
            && self.vary_absent
            && self.no_experiment
            && self.critical_css_ok
            && self.request_headers_ok
            && self.origin_cacheable
            && self.origin_fresh
            && self.beacon_ok
    }
}

/// Request headers that disqualify HTML caching (method aside).
pub const FORBIDDEN_REQUEST_HEADERS: &[&str] = &["Cookie", "Cookie2", "Authorization"];

/// Whether request headers pass (`method == GET` handled by callers via
/// `is_get`; cookies/auth/Vary checked here).
#[must_use]
pub fn request_headers_cacheable(headers: &[(&str, &str)]) -> bool {
    !headers.iter().any(|(name, _)| {
        FORBIDDEN_REQUEST_HEADERS
            .iter()
            .any(|forbidden| forbidden.eq_ignore_ascii_case(name))
            || name.eq_ignore_ascii_case("Vary")
    })
}

/* Value codec + validation (`CompleteLookup`, `HtmlCacheLookupComplete`). */

/// Encoded value: `<created_ms>\n<expires_ms>\n<body>`, capped at 1MB.
#[must_use]
pub fn encode_value(created_ms: i64, expires_ms: i64, body: &[u8]) -> Option<Vec<u8>> {
    if body.len() > MAX_HTML_BODY_BYTES {
        return None;
    }
    let mut out = format!("{created_ms}\n{expires_ms}\n").into_bytes();
    out.extend_from_slice(body);
    Some(out)
}

/// Decoded + validated value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlCacheEntry {
    /// Creation time, ms.
    pub created_ms: i64,
    /// Absolute expiry, ms.
    pub expires_ms: i64,
    /// Cached body.
    pub body: Vec<u8>,
}

/// Validates a stored value (`HtmlCacheLookup::ValidateCandidate`).
///
/// Rejects overlong header lines, unparseable bounds, non-positive or
/// future creation, expired or over-TTL windows, bodies over 1MB or
/// empty, stale origins, and purge-invalidated entries (wildcard
/// search on).
#[must_use]
pub fn validate_value(
    blob: &[u8],
    url: &str,
    now_ms: i64,
    ttl_ms: i64,
    origin_expiry_ms: i64,
    purge_set: &PurgeSet,
) -> Option<HtmlCacheEntry> {
    let first = blob.iter().position(|byte| *byte == b'\n')?;
    if first > 20 {
        return None;
    }
    let second = blob[first + 1..].iter().position(|byte| *byte == b'\n')? + first + 1;
    if second - first > 21 {
        return None;
    }
    let created: i64 = std::str::from_utf8(&blob[..first]).ok()?.parse().ok()?;
    let expires: i64 = std::str::from_utf8(&blob[first + 1..second])
        .ok()?
        .parse()
        .ok()?;
    let body = blob[second + 1..].to_vec();
    if created <= 0
        || created > now_ms
        || expires <= now_ms
        || expires <= created
        || expires - created > ttl_ms
        || origin_expiry_ms <= now_ms
        || body.len() > MAX_HTML_BODY_BYTES
        || !purge_set.is_valid(url, created)
    {
        return None;
    }
    if body.is_empty() {
        return None;
    }
    Some(HtmlCacheEntry {
        created_ms: created,
        expires_ms: expires,
        body,
    })
}

/// Computes the store expiry (`min(origin_expiry, now + ttl)`, with
/// `max_html_cache_time_ms` applied by callers when non-zero).
#[must_use]
pub fn store_expiry_ms(origin_expiry_ms: i64, now_ms: i64, ttl_ms: i64) -> i64 {
    origin_expiry_ms.min(now_ms + ttl_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_shape_and_determinism() {
        let key = html_cache_key("http://x/\n\nsig\nreq\n");
        assert!(key.starts_with(HTML_KEY_PREFIX));
        let signature = &key[HTML_KEY_PREFIX.len()..];
        assert_eq!(signature.len(), SIGNATURE_CHARS);
        assert!(signature
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
        assert_eq!(key, html_cache_key("http://x/\n\nsig\nreq\n"));
        assert_ne!(key, html_cache_key("http://x/other\n\nsig\nreq\n"));
    }

    #[test]
    fn hmac_golden() {
        // RFC 2202 HMAC-SHA1 case 2, web64-encoded + truncated
        // (verified against Python hmac+hashlib+base64).
        let signature = sign_url("Jefe", "what do ya want for nothing?");
        assert_eq!(&signature, "7_zfauXrL6LSdBbV8YTfnCWafHk");
    }

    #[test]
    fn value_round_trip_and_rejections() {
        let set = PurgeSet::new(crate::purge::CACHE_PURGE_BYTES);
        let blob = encode_value(1000, 5000, b"<html>").unwrap();
        assert_eq!(blob, b"1000\n5000\n<html>");
        let check = |blob: &[u8], now: i64, ttl: i64, origin: i64, set: &PurgeSet| {
            validate_value(blob, "http://x/", now, ttl, origin, set)
        };
        let entry = check(&blob, 2000, 60_000, 9000, &set).unwrap();
        assert_eq!(entry.created_ms, 1000);
        assert_eq!(entry.body, b"<html>");
        // Oversize bodies refused at encode time.
        assert!(encode_value(0, 1, &vec![0u8; MAX_HTML_BODY_BYTES + 1]).is_none());
        // Malformed/empty/expired/over-TTL/stale-origin/purged rejected.
        assert!(check(b"bogus", 2000, 60_000, 9000, &set).is_none());
        assert!(check(b"1000\n5000\n", 2000, 60_000, 9000, &set).is_none());
        assert!(check(&blob, 6000, 60_000, 9000, &set).is_none());
        assert!(check(&blob, 2000, 1000, 9000, &set).is_none());
        assert!(check(&blob, 2000, 60_000, 1500, &set).is_none());
        assert!(check(&blob, 500, 60_000, 9000, &set).is_none()); // created > now
        let overlong = format!("{}\n5000\n<html>", "1".repeat(21));
        assert!(check(overlong.as_bytes(), 2000, 60_000, 9000, &set).is_none());
        let mut set = PurgeSet::new(crate::purge::CACHE_PURGE_BYTES);
        set.put("http://x/", 1500);
        assert!(check(&blob, 2000, 60_000, 9000, &set).is_none());
    }

    #[test]
    fn eligibility_matrix() {
        let eligible = HtmlEligibility {
            html_cache_enabled: true,
            ttl_sec: 60,
            is_get: true,
            origin_ok: true,
            no_origin_cookies: true,
            vary_absent: true,
            no_experiment: true,
            critical_css_ok: true,
            request_headers_ok: true,
            origin_cacheable: true,
            origin_fresh: true,
            beacon_ok: true,
        };
        assert!(eligible.cacheable());
        // Each input vetoes independently.
        type Veto = Box<dyn Fn(&mut HtmlEligibility)>;
        let vetoes: Vec<Veto> = vec![
            Box::new(|e| e.html_cache_enabled = false),
            Box::new(|e| e.ttl_sec = 0),
            Box::new(|e| e.ttl_sec = MAX_HTML_TTL_SEC + 1),
            Box::new(|e| e.is_get = false),
            Box::new(|e| e.origin_ok = false),
            Box::new(|e| e.no_origin_cookies = false),
            Box::new(|e| e.vary_absent = false),
            Box::new(|e| e.no_experiment = false),
            Box::new(|e| e.critical_css_ok = false),
            Box::new(|e| e.request_headers_ok = false),
            Box::new(|e| e.origin_cacheable = false),
            Box::new(|e| e.origin_fresh = false),
            Box::new(|e| e.beacon_ok = false),
        ];
        for veto in vetoes {
            let mut entry = eligible.clone();
            veto(&mut entry);
            assert!(!entry.cacheable());
        }
        assert!(request_headers_cacheable(&[("Accept", "text/html")]));
        assert!(!request_headers_cacheable(&[("Cookie", "a=1")]));
        assert!(!request_headers_cacheable(&[("Vary", "Cookie")]));
        assert_eq!(store_expiry_ms(9000, 1000, 60_000), 9000);
        assert_eq!(store_expiry_ms(999_000, 1000, 60_000), 61_000);
    }
}

//! Caching-header policy and surgery (`ngx_caching_header_policy.*`,
//! `ngx_cache_control_merge.*`, `GenerateDisabledCacheControl`,
//! `ResponseHeaders::ApplySMaxAge`).
//!
//! All decisions here are pure over header values: which caching
//! headers the module keeps, which privacy limits travel onto the
//! rewritten page's `Cache-Control`, the disabled-page value, and
//! the `s-maxage` rewrite for in-place recording.

/// Which caching headers the module keeps from the server's own
/// response (`PreserveCachingHeaders`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreserveCachingHeaders {
    /// Keep `Cache-Control`, `ETag`, `Last-Modified`, etc.
    PreserveAll,
    /// Keep `Cache-Control` only.
    PreserveOnlyCacheControl,
    /// Keep none (the module's headers describe the response).
    #[default]
    DontPreserve,
}

/// Inputs to the preserve decision (`NgxCachingHeaderRequest`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachingHeaderRequest {
    /// `ModifyCachingHeaders` option for the request.
    pub modify_caching_headers: bool,
    /// Downstream cache integration is configured.
    pub downstream_cache_integration: bool,
    /// The request names a `.pagespeed.` resource.
    pub pagespeed_resource: bool,
    /// The request targets an admin-style handler.
    pub admin_handler: bool,
    /// The request carries the configured rebeaconing key.
    pub rebeaconing_key_matches: bool,
}

impl Default for CachingHeaderRequest {
    fn default() -> Self {
        CachingHeaderRequest {
            modify_caching_headers: true,
            downstream_cache_integration: false,
            pagespeed_resource: false,
            admin_handler: false,
            rebeaconing_key_matches: false,
        }
    }
}

/// Decides which caching headers to keep
/// (`NgxPreserveCachingHeadersFor`).
#[must_use]
pub const fn preserve_caching_headers_for(
    request: &CachingHeaderRequest,
) -> PreserveCachingHeaders {
    if request.pagespeed_resource {
        return PreserveCachingHeaders::DontPreserve;
    }
    if !request.modify_caching_headers {
        return PreserveCachingHeaders::PreserveAll;
    }
    if !request.downstream_cache_integration {
        return PreserveCachingHeaders::DontPreserve;
    }
    if request.admin_handler {
        return PreserveCachingHeaders::DontPreserve;
    }
    if request.rebeaconing_key_matches {
        return PreserveCachingHeaders::DontPreserve;
    }
    PreserveCachingHeaders::PreserveOnlyCacheControl
}

/// Privacy limits noted from the page's own `Cache-Control`
/// (`NgxCacheControlLimits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheControlLimits {
    /// A `private` directive was seen.
    pub is_private: bool,
    /// A `no-store` directive was seen.
    pub no_store: bool,
}

/// Splits a field value on a delimiter, omitting empty pieces
/// (`SplitStringPieceToVector` with `omit_empty_strings`).
fn split_omitting_empty(value: &str, delimiter: char) -> Vec<&str> {
    value
        .split(delimiter)
        .filter(|piece| !piece.is_empty())
        .collect()
}

/// Notes the `private`/`no-store` directives of one `Cache-Control`
/// field value (`NgxNoteCacheControlLimits`): directive names
/// compare case-insensitively, and a qualified `private="field"`
/// counts as private.
pub fn note_cache_control_limits(field_value: &str, limits: &mut CacheControlLimits) {
    for element in split_omitting_empty(field_value, ',') {
        let name = element.split('=').next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case("private") {
            limits.is_private = true;
        } else if name.eq_ignore_ascii_case("no-store") {
            limits.no_store = true;
        }
    }
}

/// Carries noted limits onto the module's value
/// (`NgxKeepCacheControlLimits`): appends `", private"` / `",
/// no-store"` for each noted directive the value lacks.
#[must_use]
pub fn keep_cache_control_limits(module_value: &str, limits: &CacheControlLimits) -> String {
    let mut present = CacheControlLimits::default();
    note_cache_control_limits(module_value, &mut present);
    let mut result = module_value.to_string();
    if limits.is_private && !present.is_private {
        if !result.is_empty() {
            result.push_str(", ");
        }
        result.push_str("private");
    }
    if limits.no_store && !present.no_store {
        if !result.is_empty() {
            result.push_str(", ");
        }
        result.push_str("no-store");
    }
    result
}

/// Disabled-page base value (`HttpAttributes::kNoCacheMaxAge0`).
pub const NO_CACHE_MAX_AGE_0: &str = "max-age=0, no-cache";

/// Builds the disabled-page `Cache-Control`
/// (`GenerateDisabledCacheControl`) from the looked-up values
/// (comma-split, trimmed): every directive survives except
/// `no-cache`, `max-age`, `private`, and `public` — note
/// `no-store` is carried over.
#[must_use]
pub fn disabled_cache_control(cache_control_values: &[&str]) -> String {
    let mut out = NO_CACHE_MAX_AGE_0.to_string();
    for piece in cache_control_values {
        let name = piece.split('=').next().unwrap_or("").trim();
        if !name.eq_ignore_ascii_case("no-cache")
            && !name.eq_ignore_ascii_case("max-age")
            && !name.eq_ignore_ascii_case("private")
            && !name.eq_ignore_ascii_case("public")
        {
            out.push_str(", ");
            out.push_str(piece);
        }
    }
    out
}

/// Whether an element carries a directive (`ElementHasDirective`):
/// any `;`-separated piece equal to the name, or starting with it
/// followed by `=`, space, or tab.
fn element_has_directive(element: &str, name: &str) -> bool {
    for piece in split_omitting_empty(element, ';') {
        let piece = piece.trim();
        if piece.eq_ignore_ascii_case(name) {
            return true;
        }
        if piece.len() > name.len()
            && piece.as_bytes()[..name.len()].eq_ignore_ascii_case(name.as_bytes())
        {
            let next = piece.as_bytes()[name.len()];
            if next == b'=' || next == b' ' || next == b'\t' {
                return true;
            }
        }
    }
    false
}

/// A parsed lifetime directive (`LifetimeDirective`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Lifetime {
    /// Which lifetime, if any.
    pub(crate) name: LifetimeName,
    /// Whether the seconds value parsed cleanly.
    pub(crate) readable: bool,
    /// Parsed seconds (when readable).
    pub(crate) seconds: i32,
}

/// Lifetime directive names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifetimeName {
    /// Not a lifetime.
    None,
    /// `max-age`.
    MaxAge,
    /// `s-maxage`.
    SMaxAge,
}

/// Parses lifetime seconds (`ParseLifetimeSeconds`): optional
/// surrounding quotes, all digits, clamped to `i32::MAX`.
fn parse_lifetime_seconds(value: &str) -> Option<i32> {
    let mut value = value;
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value = &value[1..value.len() - 1];
    }
    if value.is_empty() {
        return None;
    }
    let mut result: i64 = 0;
    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        if result <= i64::from(i32::MAX) {
            result = result * 10 + i64::from(byte - b'0');
        }
    }
    Some(result.min(i64::from(i32::MAX)) as i32)
}

/// Reads one segment's lifetime directive (`ReadLifetimeDirective`).
pub(crate) fn read_lifetime_directive(element: &str) -> Lifetime {
    let element = element.trim();
    let joined = element.contains(';');
    let equals = element.find('=');
    let name = match equals {
        Some(pos) => &element[..pos],
        None => element,
    };
    let trimmed_name = name.trim();
    let mut result = Lifetime {
        name: LifetimeName::None,
        readable: false,
        seconds: 0,
    };
    let exact = if trimmed_name.len() >= 7
        && trimmed_name.as_bytes()[..7].eq_ignore_ascii_case(b"max-age")
    {
        result.name = LifetimeName::MaxAge;
        Some("max-age")
    } else if trimmed_name.len() >= 8
        && trimmed_name.as_bytes()[..8].eq_ignore_ascii_case(b"s-maxage")
    {
        result.name = LifetimeName::SMaxAge;
        Some("s-maxage")
    } else {
        None
    };
    if let Some(exact) = exact {
        // The seconds scan untrimmed, like the C++: `max-age= 60`
        // is unreadable (spaces fail the digit scan).
        let seconds = match equals {
            Some(pos) => parse_lifetime_seconds(&element[pos + 1..]),
            None => None,
        };
        result.readable =
            !joined && equals.is_some() && name.eq_ignore_ascii_case(exact) && seconds.is_some();
        result.seconds = seconds.unwrap_or(0);
        return result;
    }
    if joined || element.contains('"') {
        let lower = element.to_ascii_lowercase();
        if lower.contains("max-age") {
            result.name = LifetimeName::MaxAge;
        } else if lower.contains("s-maxage") {
            result.name = LifetimeName::SMaxAge;
        }
    }
    result
}

/// Rewrites a `Cache-Control` value to carry `s-maxage`
/// (`ResponseHeaders::ApplySMaxAge`): `None` when the value must
/// not be touched (`no-transform`, uncacheable directives, an
/// unreadable lifetime, or an existing lifetime already at or
/// below the target).
#[must_use]
pub fn apply_s_max_age(s_maxage_sec: i32, existing_cache_control: &str) -> Option<String> {
    let existing = existing_cache_control.trim();
    let s_maxage_str = format!("s-maxage={s_maxage_sec}");
    if existing.is_empty() {
        return Some(s_maxage_str);
    }
    let segments: Vec<&str> = split_omitting_empty(existing, ',');
    for segment in &segments {
        let segment = segment.trim();
        if element_has_directive(segment, "no-transform") {
            return None;
        }
        if element_has_directive(segment, "no-cache")
            || element_has_directive(segment, "no-store")
            || element_has_directive(segment, "private")
        {
            return None;
        }
        let lifetime = read_lifetime_directive(segment);
        if lifetime.name != LifetimeName::None && !lifetime.readable {
            return None;
        }
    }
    // Lower every oversized s-maxage in place.
    let mut found_s_maxage = false;
    let mut updated = false;
    let mut rewritten: Vec<String> = Vec::with_capacity(segments.len());
    for segment in &segments {
        let trimmed = segment.trim();
        let lifetime = read_lifetime_directive(trimmed);
        if lifetime.name == LifetimeName::SMaxAge {
            found_s_maxage = true;
            if lifetime.seconds > s_maxage_sec {
                rewritten.push(s_maxage_str.clone());
                updated = true;
                continue;
            }
        }
        rewritten.push(trimmed.to_string());
    }
    if found_s_maxage {
        return if updated {
            Some(rewritten.join(", "))
        } else {
            None
        };
    }
    // No s-maxage: add one only below the lowest max-age.
    let mut found_max_age = false;
    let mut lowest = s_maxage_sec.saturating_add(1);
    for segment in &segments {
        let lifetime = read_lifetime_directive(segment.trim());
        if lifetime.name == LifetimeName::MaxAge && lifetime.seconds < lowest {
            found_max_age = true;
            lowest = lifetime.seconds;
        }
    }
    if found_max_age && lowest <= s_maxage_sec {
        return None;
    }
    Some(format!("{existing}, {s_maxage_str}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserve_policy_matrix() {
        let base = CachingHeaderRequest::default();
        // Module resources always carry module headers.
        let resource = CachingHeaderRequest {
            pagespeed_resource: true,
            ..base
        };
        assert_eq!(
            PreserveCachingHeaders::DontPreserve,
            preserve_caching_headers_for(&resource)
        );
        // Operator headers stay when modification is off.
        let frozen = CachingHeaderRequest {
            modify_caching_headers: false,
            ..base
        };
        assert_eq!(
            PreserveCachingHeaders::PreserveAll,
            preserve_caching_headers_for(&frozen)
        );
        // No downstream integration: drop the originals.
        assert_eq!(
            PreserveCachingHeaders::DontPreserve,
            preserve_caching_headers_for(&base)
        );
        // Admin handlers drop even with integration on.
        let admin = CachingHeaderRequest {
            downstream_cache_integration: true,
            admin_handler: true,
            ..base
        };
        assert_eq!(
            PreserveCachingHeaders::DontPreserve,
            preserve_caching_headers_for(&admin)
        );
        // Integration + rebeaconing key: drop for the no-cache serve.
        let rebeacon = CachingHeaderRequest {
            downstream_cache_integration: true,
            rebeaconing_key_matches: true,
            ..base
        };
        assert_eq!(
            PreserveCachingHeaders::DontPreserve,
            preserve_caching_headers_for(&rebeacon)
        );
        // Integration alone: keep Cache-Control only.
        let integrated = CachingHeaderRequest {
            downstream_cache_integration: true,
            ..base
        };
        assert_eq!(
            PreserveCachingHeaders::PreserveOnlyCacheControl,
            preserve_caching_headers_for(&integrated)
        );
    }

    #[test]
    fn limits_note_and_keep() {
        let mut limits = CacheControlLimits::default();
        note_cache_control_limits("max-age=60, private=\"x\"", &mut limits);
        assert!(limits.is_private);
        assert!(!limits.no_store);
        note_cache_control_limits("NO-STORE", &mut limits);
        assert!(limits.no_store);
        // Directives on any line stay noted.
        note_cache_control_limits("public", &mut limits);
        assert!(limits.is_private && limits.no_store);
        // Keep appends the missing limits only.
        assert_eq!(
            "max-age=0, no-cache, private, no-store",
            keep_cache_control_limits("max-age=0, no-cache", &limits)
        );
        assert_eq!(
            "private",
            keep_cache_control_limits(
                "private",
                &CacheControlLimits {
                    is_private: true,
                    no_store: false
                }
            )
        );
        assert_eq!(
            "x",
            keep_cache_control_limits("x", &CacheControlLimits::default())
        );
    }

    #[test]
    fn disabled_cache_control_carries() {
        // No origin value: the bare disabled line.
        assert_eq!("max-age=0, no-cache", disabled_cache_control(&[]));
        // Lifetimes and publicity dropped; the rest (incl. no-store)
        // carried over verbatim.
        assert_eq!(
            "max-age=0, no-cache, no-store, must-revalidate",
            disabled_cache_control(&["max-age=60", "no-store", "must-revalidate"])
        );
        assert_eq!(
            "max-age=0, no-cache",
            disabled_cache_control(&["no-cache", "private", "public"])
        );
    }

    #[test]
    fn smaxage_rewrite_matrix() {
        // Empty input takes the bare directive (whitespace trimmed).
        assert_eq!(Some("s-maxage=60".to_string()), apply_s_max_age(60, "  "));
        // Untouchable values refused.
        assert_eq!(None, apply_s_max_age(60, "no-transform"));
        assert_eq!(None, apply_s_max_age(60, "max-age=600, no-store"));
        assert_eq!(None, apply_s_max_age(60, "private, max-age=600"));
        assert_eq!(None, apply_s_max_age(60, "no-cache, max-age=600"));
        // Unreadable lifetimes refused (untrimmed seconds, joined).
        assert_eq!(None, apply_s_max_age(60, "max-age= 60"));
        assert_eq!(None, apply_s_max_age(60, "max-age"));
        assert_eq!(None, apply_s_max_age(60, "foo; max-age=60"));
        // Oversized s-maxage lowered in place (all of them).
        assert_eq!(
            Some("s-maxage=60".to_string()),
            apply_s_max_age(60, "s-maxage=600")
        );
        assert_eq!(
            Some("public, s-maxage=60".to_string()),
            apply_s_max_age(60, "public, s-maxage=600")
        );
        // Small-enough s-maxage left alone (no update, no append).
        assert_eq!(None, apply_s_max_age(600, "s-maxage=60"));
        // No s-maxage: appended only below the lowest max-age.
        assert_eq!(
            Some("max-age=600, s-maxage=60".to_string()),
            apply_s_max_age(60, "max-age=600")
        );
        assert_eq!(None, apply_s_max_age(600, "max-age=60"));
        assert_eq!(
            Some("public, s-maxage=60".to_string()),
            apply_s_max_age(60, "public")
        );
        // Quoted and huge seconds parse (clamped).
        assert_eq!(
            Some("s-maxage=60".to_string()),
            apply_s_max_age(60, "s-maxage=\"600\"")
        );
        assert_eq!(
            Some("s-maxage=60".to_string()),
            apply_s_max_age(60, "s-maxage=99999999999999999999")
        );
    }
}

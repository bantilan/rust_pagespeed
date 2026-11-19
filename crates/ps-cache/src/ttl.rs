//! TTL policy (`rewrite_options.cc`, `http_cache_failure.{h,cc}`,
//! `server_context.cc`, `rewrite_context.cc`).
//!
//! Default time-to-live set plus the fetch-failure table and the metadata
//! recheck rule (`AddRecheckDependency`).
#![forbid(unsafe_code)]

use crate::timer::{MINUTE_MS, SECOND_MS, YEAR_MS};

/// Default TTL for resources lacking cache headers
/// (`kDefaultImplicitCacheTtlMs` = 5 min).
pub const IMPLICIT_CACHE_TTL_MS: i64 = 5 * MINUTE_MS;
/// Default TTL for `LoadFromFile` entries (5 min).
pub const LOAD_FROM_FILE_CACHE_TTL_MS: i64 = 5 * MINUTE_MS;
/// Default TTL for metadata input errors (5 min).
pub const METADATA_INPUT_ERRORS_CACHE_TTL_MS: i64 = 5 * MINUTE_MS;
/// Max age served for generated resources (`kGeneratedMaxAgeMs` = 1 year).
pub const GENERATED_MAX_AGE_MS: i64 = YEAR_MS;
/// TTL for mismatched content (`kCacheTtlForMismatchedContentMs` = 5 min).
pub const MISMATCHED_CONTENT_TTL_MS: i64 = 5 * MINUTE_MS;

/// Fetch outcome classification (`FetchResponseStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FetchStatus {
    /// Unset.
    NotSet = 0,
    /// Successful fetch.
    Ok = 1,
    /// 200 that must not be cached.
    Uncacheable200 = 2,
    /// Uncacheable error.
    UncacheableError = 3,
    /// 4xx error.
    Error4xx = 4,
    /// Any other error.
    OtherError = 5,
    /// Dropped under load.
    Dropped = 6,
    /// Empty response.
    Empty = 7,
}

/// Remembered-failure TTLs in seconds (`HttpCacheFailurePolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailurePolicy {
    ttl_sec_for_status: [i32; 8],
}

impl FailurePolicy {
    /// Compiled-in defaults: 300s everywhere except dropped fetches (10s).
    #[must_use]
    pub const fn new() -> FailurePolicy {
        FailurePolicy {
            ttl_sec_for_status: [300, 300, 300, 300, 300, 300, 10, 300],
        }
    }

    /// TTL for a status (`failure_caching_ttl_sec`).
    #[must_use]
    pub fn ttl_sec(&self, status: FetchStatus) -> i32 {
        self.ttl_sec_for_status[status as usize]
    }

    /// Overrides one status TTL (`set_failure_caching_ttl_sec`).
    pub fn set_ttl_sec(&mut self, status: FetchStatus, ttl_sec: i32) {
        self.ttl_sec_for_status[status as usize] = ttl_sec;
    }
}

impl Default for FailurePolicy {
    fn default() -> Self {
        FailurePolicy::new()
    }
}

/// Metadata recheck TTL (`AddRecheckDependency`).
///
/// `Ok` fetches recheck at `max(implicit_ttl, input_expiry - now)`;
/// 4xx errors use the metadata-errors TTL (dangling references recheck
/// rarely); anything else uses the failure-policy TTL for its status.
#[must_use]
pub fn recheck_ttl_ms(
    status: FetchStatus,
    implicit_ttl_ms: i64,
    input_expiry_ms: i64,
    now_ms: i64,
    metadata_errors_ttl_ms: i64,
    failure_policy: &FailurePolicy,
) -> i64 {
    match status {
        FetchStatus::Ok => implicit_ttl_ms.max(input_expiry_ms - now_ms),
        FetchStatus::Error4xx => metadata_errors_ttl_ms,
        _ => i64::from(failure_policy.ttl_sec(status)) * SECOND_MS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_ttls() {
        assert_eq!(IMPLICIT_CACHE_TTL_MS, 300_000);
        assert_eq!(LOAD_FROM_FILE_CACHE_TTL_MS, 300_000);
        assert_eq!(METADATA_INPUT_ERRORS_CACHE_TTL_MS, 300_000);
        assert_eq!(GENERATED_MAX_AGE_MS, 31_536_000_000);
        assert_eq!(MISMATCHED_CONTENT_TTL_MS, 300_000);
        // Cross-check against the ps-config option table.
        assert_eq!(
            ps_config::property::lookup_by_id("ict").unwrap().default,
            ps_config::property::PropDefault::Int(300_000)
        );
    }

    #[test]
    fn failure_policy_table() {
        let mut policy = FailurePolicy::new();
        assert_eq!(policy.ttl_sec(FetchStatus::Ok), 300);
        assert_eq!(policy.ttl_sec(FetchStatus::Uncacheable200), 300);
        assert_eq!(policy.ttl_sec(FetchStatus::UncacheableError), 300);
        assert_eq!(policy.ttl_sec(FetchStatus::Error4xx), 300);
        assert_eq!(policy.ttl_sec(FetchStatus::OtherError), 300);
        assert_eq!(policy.ttl_sec(FetchStatus::Dropped), 10);
        assert_eq!(policy.ttl_sec(FetchStatus::Empty), 300);
        policy.set_ttl_sec(FetchStatus::Dropped, 60);
        assert_eq!(policy.ttl_sec(FetchStatus::Dropped), 60);
    }

    #[test]
    fn recheck_rules() {
        let policy = FailurePolicy::new();
        // Ok: max(implicit, expiry-now).
        assert_eq!(
            recheck_ttl_ms(
                FetchStatus::Ok,
                300_000,
                1_000_000,
                100_000,
                300_000,
                &policy
            ),
            900_000
        );
        assert_eq!(
            recheck_ttl_ms(FetchStatus::Ok, 300_000, 100_000, 90_000, 300_000, &policy),
            300_000
        );
        // 4xx: metadata-errors TTL regardless of expiry.
        assert_eq!(
            recheck_ttl_ms(
                FetchStatus::Error4xx,
                300_000,
                9_000_000,
                100_000,
                111_000,
                &policy
            ),
            111_000
        );
        // Dropped: failure policy (10s).
        assert_eq!(
            recheck_ttl_ms(FetchStatus::Dropped, 300_000, 0, 0, 300_000, &policy),
            10_000
        );
    }
}

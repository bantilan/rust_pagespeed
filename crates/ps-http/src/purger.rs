//! Downstream cache purge requests (`downstream_cache_purger.{h,cc}`).
//!
//! Builds and issues downstream purge requests for rewritten resources.
//! The driver surface C++ consults (options, rewrite counts, request
//! shape) is captured in [`PurgeInputs`] so this logic stays testable
//! without the driver; `RewriteDriver` wiring lands with Step 4.
#![forbid(unsafe_code)]

use crate::fetch::AsyncFetcher;
use crate::request::{Method, RequestHeaders};

/// Loop-prevention marker (`kPsaPurgeRequest`).
pub const PSA_PURGE_REQUEST: &str = "X-PSA-Purge-Request";
/// Default purge method (`kDefaultDownstreamCachePurgeMethod`).
pub const DEFAULT_PURGE_METHOD: &str = "PURGE";
/// Default rewritten-success threshold, percent
/// (`kDefaultDownstreamCacheRewrittenPercentageThreshold`).
pub const DEFAULT_REWRITTEN_THRESHOLD: i32 = 95;

/// Inputs the purger reads from the driver and request.
#[derive(Debug, Clone)]
pub struct PurgeInputs {
    /// Downstream integration enabled (prefix configured).
    pub integration_enabled: bool,
    /// Purge method, e.g. `PURGE`.
    pub purge_method: String,
    /// Purge location prefix (validated web URL).
    pub purge_location_prefix: String,
    /// Rewritten-success threshold percent.
    pub rewritten_threshold: i32,
    /// Number of initiated rewrites.
    pub num_initiated_rewrites: i32,
    /// Number detached (still relevant) rewrites.
    pub num_detached_rewrites: i32,
}

impl PurgeInputs {
    /// Creates inputs with C++ defaults (integration off).
    #[must_use]
    pub fn new() -> PurgeInputs {
        PurgeInputs {
            integration_enabled: false,
            purge_method: DEFAULT_PURGE_METHOD.to_string(),
            purge_location_prefix: String::new(),
            rewritten_threshold: DEFAULT_REWRITTEN_THRESHOLD,
            num_initiated_rewrites: 0,
            num_detached_rewrites: 0,
        }
    }
}

impl Default for PurgeInputs {
    fn default() -> Self {
        PurgeInputs::new()
    }
}

/// A constructed purge request (`DownstreamCachePurger::GeneratePurgeRequestParameters`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeRequest {
    /// Target URL.
    pub url: String,
    /// Purge method.
    pub method: String,
    /// `X-PSA-Purge-Request: true`.
    pub marker: (String, String),
}

impl PurgeRequest {
    /// Fully-qualified target: `<prefix><stripped-url>`, where stripping
    /// drops the scheme (`://` split) so `http://host/path` re-attaches
    /// under the purge server.
    #[must_use]
    pub fn target(prefix: &str, url: &str) -> Option<String> {
        let (scheme, rest) = url.split_once("://")?;
        if scheme.is_empty() || rest.is_empty() || !prefix.ends_with('/') {
            return None;
        }
        Some(format!("{prefix}{rest}"))
    }
}

/// Decides and issues a purge (`DownstreamCachePurger::MaybeIssuePurge`).
///
/// Issues only for GET requests without the loop marker, on web-valid
/// URLs, when integration is on, every rewrite initiated was detached,
/// and the origin was fully rewritten (or the threshold is non-positive,
/// or no rewrites were initiated).
pub fn maybe_issue_purge<F: AsyncFetcher + ?Sized>(
    inputs: &PurgeInputs,
    method: Method,
    request_headers: &RequestHeaders,
    url: &str,
    fully_rewritten: bool,
    fetcher: &F,
    fetch: &mut dyn crate::fetch::AsyncFetch,
) -> bool {
    if method != Method::Get || request_headers.headers().has(PSA_PURGE_REQUEST) || !is_web_url(url)
    {
        return false;
    }
    if !inputs.integration_enabled
        || inputs.purge_method.is_empty()
        || inputs.num_initiated_rewrites != inputs.num_detached_rewrites
    {
        return false;
    }
    let threshold_ok =
        fully_rewritten || inputs.rewritten_threshold <= 0 || inputs.num_initiated_rewrites == 0;
    if !threshold_ok {
        return false;
    }
    let Some(target) = PurgeRequest::target(&inputs.purge_location_prefix, url) else {
        return false;
    };
    // Loop marker plus the purge method (other configured methods are
    // accepted but not actually sent yet: TODO in C++).
    fetch
        .request_headers_mut()
        .headers_mut()
        .add(PSA_PURGE_REQUEST, "1");
    if inputs.purge_method == "PURGE" {
        fetch.request_headers_mut().set_method(Method::Purge);
    }
    fetcher.fetch(&target, fetch);
    true
}

/// Absolute http(s) URL check (purge targets must be web-valid).
fn is_web_url(url: &str) -> bool {
    url.strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .is_some_and(|rest| {
            let host = rest.split('/').next().unwrap_or("");
            !host.is_empty() && !host.bytes().any(|b| b.is_ascii_whitespace())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::AsyncFetch;
    use crate::fetch::BufferingFetch;
    use crate::mock::MockUrlFetcher;
    use crate::request::RequestContext;
    use crate::response::HttpOptions;

    fn inputs() -> PurgeInputs {
        PurgeInputs {
            integration_enabled: true,
            purge_method: "PURGE".to_string(),
            purge_location_prefix: "http://purge.local:8080/".to_string(),
            rewritten_threshold: 95,
            num_initiated_rewrites: 2,
            num_detached_rewrites: 2,
        }
    }

    fn request() -> RequestHeaders {
        let mut headers = RequestHeaders::new();
        headers.set_method(Method::Get);
        headers
    }

    #[test]
    fn target_and_gates() {
        assert_eq!(
            PurgeRequest::target("http://purge.local/", "http://x/a"),
            Some("http://purge.local/x/a".to_string())
        );
        // Prefix must end in '/', URL must carry a scheme + host.
        assert_eq!(
            PurgeRequest::target("http://purge.local", "http://x/a"),
            None
        );
        assert_eq!(PurgeRequest::target("http://purge.local/", "notaurl"), None);

        let fetcher = MockUrlFetcher::new();
        // Happy path issues exactly one fetch to the purge target.
        let mut fetch = BufferingFetch::new(
            "http://x/a",
            RequestContext::new(HttpOptions::default_for_tests()),
        );
        assert!(maybe_issue_purge(
            &inputs(),
            Method::Get,
            &request(),
            "http://x/a",
            true,
            &fetcher,
            &mut fetch
        ));
        assert_eq!(fetcher.fetch_count(), 1);
        assert_eq!(
            fetch.request_headers().headers().lookup1(PSA_PURGE_REQUEST),
            Some("1".to_string())
        );
        assert_eq!(fetch.request_headers().method(), Method::Purge);
        // Non-GET, loop marker, bad URL, disabled integration, pending
        // rewrites, and partial rewrites all skip silently.
        let mut looped = request();
        looped.headers_mut().add(PSA_PURGE_REQUEST, "true");
        for (inputs, method, headers, url, rewritten) in [
            (inputs(), Method::Post, request(), "http://x/a", true),
            (inputs(), Method::Get, looped, "http://x/a", true),
            (inputs(), Method::Get, request(), "notaurl", true),
            (
                PurgeInputs {
                    integration_enabled: false,
                    ..inputs()
                },
                Method::Get,
                request(),
                "http://x/a",
                true,
            ),
            (
                PurgeInputs {
                    num_detached_rewrites: 1,
                    ..inputs()
                },
                Method::Get,
                request(),
                "http://x/a",
                true,
            ),
            (inputs(), Method::Get, request(), "http://x/a", false),
        ] {
            let mut fetch = BufferingFetch::new(
                "http://x/a",
                RequestContext::new(HttpOptions::default_for_tests()),
            );
            assert!(!maybe_issue_purge(
                &inputs, method, &headers, url, rewritten, &fetcher, &mut fetch
            ));
        }
        assert_eq!(fetcher.fetch_count(), 1);
    }
}

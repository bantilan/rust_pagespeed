//! In-memory scripted fetcher (`mock_url_fetcher.{h,cc}`, core subset).
//!
//! Maps URLs to canned (headers, body) responses with conditional-request
//! (304) handling and configurable failure injection. Serves both tests
//! and the compatibility harness as a deterministic origin server.
#![forbid(unsafe_code)]

use crate::fetch::{AsyncFetch, AsyncFetcher};
use crate::names::header;
use crate::response::ResponseHeaders;
use std::collections::HashMap;
use std::sync::Mutex;

/// In-memory origin server (`MockUrlFetcher`).
#[derive(Debug)]
pub struct MockUrlFetcher {
    state: Mutex<MockState>,
}

#[derive(Debug, Default)]
struct MockState {
    responses: HashMap<String, (ResponseHeaders, Vec<u8>)>,
    enabled: bool,
    error_message: Vec<u8>,
    fail_after_headers: bool,
    split_writes: bool,
    omit_empty_writes: bool,
    update_date_headers: bool,
    now_ms_override: Option<i64>,
    strip_query_params: bool,
    fetch_count: u64,
    last_referer: Option<String>,
    last_accept_encoding: String,
    verify_pagespeed_header_off: bool,
    last_had_pagespeed_off: Option<bool>,
    waiting: bool,
    pending_urls: Vec<String>,
}

impl Default for MockUrlFetcher {
    fn default() -> Self {
        MockUrlFetcher {
            state: Mutex::new(MockState {
                enabled: true,
                ..MockState::default()
            }),
        }
    }
}

impl MockUrlFetcher {
    /// Creates an empty fetcher.
    #[must_use]
    pub fn new() -> MockUrlFetcher {
        MockUrlFetcher::default()
    }

    /// Disables all fetches (`Disable`): every fetch fails.
    pub fn disable(&self) {
        lock(&self.state).enabled = false;
    }

    /// Re-enables fetches after [`disable`](Self::disable).
    pub fn enable(&self) {
        lock(&self.state).enabled = true;
    }

    /// Body written on failures (`set_error_message`).
    pub fn set_error_message(&self, message: &[u8]) {
        lock(&self.state).error_message = message.to_vec();
    }

    /// Last request's `Referer`, if any (`last_referer`).
    #[must_use]
    pub fn last_referer(&self) -> Option<String> {
        lock(&self.state).last_referer.clone()
    }

    /// Requires `PageSpeed: off` on fetches
    /// (`set_verify_pagespeed_header_off`); mismatches record
    /// instead of failing, for the test to assert.
    pub fn set_verify_pagespeed_header_off(&self, verify: bool) {
        lock(&self.state).verify_pagespeed_header_off = verify;
    }

    /// Whether the last fetch carried `PageSpeed: off` (`None` before
    /// any fetch, or while verification is off).
    #[must_use]
    pub fn last_had_pagespeed_off(&self) -> Option<bool> {
        lock(&self.state).last_had_pagespeed_off
    }

    /// Scripts a response (`SetResponse`).
    pub fn set_response(&self, url: &str, headers: ResponseHeaders, body: &[u8]) {
        lock(&self.state)
            .responses
            .insert(url.to_string(), (headers, body.to_vec()));
    }

    /// Adds a header to a scripted response (`AddToResponse`).
    pub fn add_response_header(&self, url: &str, name: &str, value: &str) {
        if let Some((headers, _)) = lock(&self.state).responses.get_mut(url) {
            headers.headers_mut().add(name, value);
            headers.compute_caching();
        }
    }

    /// Scripts a plain-200 response with a content type.
    pub fn set_simple_response(&self, url: &str, content_type: &str, body: &[u8]) {
        let mut headers = ResponseHeaders::new(crate::response::HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers
            .headers_mut()
            .add(header::CONTENT_TYPE, content_type);
        self.set_response(url, headers, body);
    }

    /// Fails after headers complete (`set_fail_after_headers`).
    pub fn set_fail_after_headers(&self, fail: bool) {
        lock(&self.state).fail_after_headers = fail;
    }

    /// Splits body writes byte-by-byte (`set_split_writes`).
    pub fn set_split_writes(&self, split: bool) {
        lock(&self.state).split_writes = split;
    }

    /// Skips zero-length writes (`set_omit_empty_writes`).
    pub fn set_omit_empty_writes(&self, omit: bool) {
        lock(&self.state).omit_empty_writes = omit;
    }

    /// Refreshes Date/Expires on serve (`set_update_date_headers`).
    pub fn set_update_date_headers(&self, update: bool) {
        lock(&self.state).update_date_headers = update;
    }

    /// Pins the clock for date updates (`set_timer` equivalent):
    /// `None` restores wall time.
    pub fn set_now_ms(&self, now_ms: Option<i64>) {
        lock(&self.state).now_ms_override = now_ms;
    }

    /// Strips query strings before matching (`set_strip_query_params`).
    pub fn set_strip_query_params(&self, strip: bool) {
        lock(&self.state).strip_query_params = strip;
    }

    /// Number of fetch attempts served.
    #[must_use]
    pub fn fetch_count(&self) -> u64 {
        lock(&self.state).fetch_count
    }

    /// Whether a URL is scripted.
    #[must_use]
    pub fn has_response(&self, url: &str) -> bool {
        lock(&self.state).responses.contains_key(url)
    }

    /// Holds fetches uncompleted (`SetupWaitFetcher`): `fetch`
    /// records the URL and returns with the fetch untouched, so
    /// loads observe `!done` (pending, never failed).
    pub fn set_waiting(&self, waiting: bool) {
        lock(&self.state).waiting = waiting;
    }

    /// Releases held fetches (`CallFetcherCallbacksForDriver`).
    ///
    /// The sync model cannot complete the borrowed fetch objects
    /// after the fact, so the pump only unblocks future fetches;
    /// later loads fetch fresh (from scripted responses) instead of
    /// completing the held ones. Returns the held URL count.
    pub fn call_callbacks(&self) -> usize {
        let mut state = lock(&self.state);
        state.waiting = false;
        let count = state.pending_urls.len();
        state.pending_urls.clear();
        count
    }

    /// URLs held while waiting (test introspection).
    #[must_use]
    pub fn pending_urls(&self) -> Vec<String> {
        lock(&self.state).pending_urls.clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// Strips the query string (`AllExceptQuery` equivalent for matching).
fn strip_query(url: &str) -> &str {
    match url.find(['?', '#']) {
        Some(pos) => &url[..pos],
        None => url,
    }
}

impl AsyncFetcher for MockUrlFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        let (
            enabled,
            error_message,
            response,
            fail_after_headers,
            split_writes,
            omit_empty,
            update_dates,
            now_override,
            waiting,
        ) = {
            let mut state = lock(&self.state);
            state.fetch_count += 1;
            let waiting = state.waiting;
            if waiting {
                state.pending_urls.push(url.to_string());
            }
            if state.enabled {
                state.last_referer = fetch.request_headers().headers().lookup1(header::REFERER);
                state.last_accept_encoding = fetch
                    .request_headers()
                    .headers()
                    .lookup_joined(header::ACCEPT_ENCODING);
                if state.verify_pagespeed_header_off {
                    state.last_had_pagespeed_off = Some(
                        fetch
                            .request_headers()
                            .headers()
                            .has_value("PageSpeed", "off"),
                    );
                }
            }
            let key = if state.strip_query_params {
                strip_query(url).to_string()
            } else {
                url.to_string()
            };
            let found = state.responses.get(&key).cloned();
            (
                state.enabled,
                state.error_message.clone(),
                found,
                state.fail_after_headers,
                state.split_writes,
                state.omit_empty_writes,
                state.update_date_headers,
                state.now_ms_override,
                waiting,
            )
        };
        if waiting {
            // Held: the fetch object stays untouched (`!done`), so
            // the load observes pending rather than failure.
            return;
        }
        if !enabled {
            // Disabled fetches fail, reporting a 500 with the error
            // message when one is set.
            if !error_message.is_empty() {
                fetch.response_headers_mut().set_status_and_reason(500);
                fetch.headers_complete();
                fetch.handle_write(&error_message);
            }
            fetch.handle_done(false);
            return;
        }
        let Some((mut headers, body)) = response else {
            // Unset URLs fail the fetch (C++ Done(false)).
            fetch.handle_done(false);
            return;
        };
        // Conditional requests: matching ETag/Last-Modified yields 304.
        if conditional_match(fetch.request_headers(), &headers) {
            headers.set_status_and_reason(304);
            headers.headers_mut().remove_all(header::CONTENT_LENGTH);
            *fetch.response_headers_mut() = headers;
            fetch.headers_complete();
            fetch.handle_done(true);
            return;
        }
        if update_dates {
            let now_ms = now_override.or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|age| age.as_millis() as i64)
                    .ok()
            });
            if let Some(now_ms) = now_ms {
                headers.set_date(now_ms);
            }
        }
        *fetch.response_headers_mut() = headers;
        fetch.headers_complete();
        if fail_after_headers {
            fetch.handle_done(false);
            return;
        }
        if split_writes {
            for byte in body {
                if !fetch.handle_write(&[byte]) {
                    fetch.handle_done(false);
                    return;
                }
            }
        } else if (!body.is_empty() || !omit_empty) && !fetch.handle_write(&body) {
            fetch.handle_done(false);
            return;
        }
        fetch.handle_done(true);
    }
}

/// Whether request validators match the response (`If-Modified-Since` /
/// `If-None-Match` handling in `MockUrlFetcher::Fetch`).
fn conditional_match(request: &crate::request::RequestHeaders, response: &ResponseHeaders) -> bool {
    if let Some(since) = request.headers().lookup1(header::IF_MODIFIED_SINCE) {
        if let Some(last_modified) = response.headers().lookup1(header::LAST_MODIFIED) {
            if since == last_modified {
                return true;
            }
        }
    }
    if let Some(match_tag) = request.headers().lookup1(header::IF_NONE_MATCH) {
        if let Some(etag) = response.headers().lookup1(header::ETAG) {
            if match_tag == etag {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::BufferingFetch;
    use crate::request::RequestContext;
    use crate::response::HttpOptions;

    fn fetch_via(fetcher: &MockUrlFetcher, url: &str) -> BufferingFetch {
        let mut fetch =
            BufferingFetch::new(url, RequestContext::new(HttpOptions::default_for_tests()));
        fetcher.fetch(url, &mut fetch);
        fetch
    }

    #[test]
    fn serve_and_count() {
        let fetcher = MockUrlFetcher::new();
        fetcher.set_simple_response("http://x/a.css", "text/css", b"a{}");
        let fetch = fetch_via(&fetcher, "http://x/a.css");
        assert!(fetch.done() && fetch.success());
        assert_eq!(fetch.response_headers().status_code(), 200);
        assert_eq!(fetch.body(), b"a{}");
        assert_eq!(fetcher.fetch_count(), 1);
        // Unset URLs fail.
        let fetch = fetch_via(&fetcher, "http://x/missing");
        assert!(fetch.done() && !fetch.success());
        assert_eq!(fetcher.fetch_count(), 2);
    }

    #[test]
    fn conditional_304() {
        let fetcher = MockUrlFetcher::new();
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add(header::ETAG, "\"v1\"");
        headers.headers_mut().add(header::CONTENT_LENGTH, "3");
        fetcher.set_response("http://x/a", headers, b"abc");
        let mut fetch = BufferingFetch::new(
            "http://x/a",
            RequestContext::new(HttpOptions::default_for_tests()),
        );
        fetch
            .request_headers_mut()
            .headers_mut()
            .add(header::IF_NONE_MATCH, "\"v1\"");
        fetcher.fetch("http://x/a", &mut fetch);
        assert!(fetch.done() && fetch.success());
        assert_eq!(fetch.response_headers().status_code(), 304);
        assert!(fetch.body().is_empty());
        assert!(!fetch
            .response_headers()
            .headers()
            .has(header::CONTENT_LENGTH));
    }

    #[test]
    fn failure_injection() {
        let fetcher = MockUrlFetcher::new();
        fetcher.set_simple_response("http://x/a", "text/css", b"abc");
        fetcher.set_split_writes(true);
        let fetch = fetch_via(&fetcher, "http://x/a");
        assert_eq!(fetch.body(), b"abc"); // byte-wise writes reassemble
        fetcher.set_fail_after_headers(true);
        let fetch = fetch_via(&fetcher, "http://x/a");
        assert!(fetch.done() && !fetch.success());
        assert!(fetch.headers_complete());
        assert!(fetch.body().is_empty());
    }
}

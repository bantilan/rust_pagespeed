//! Whole-HTML cache sessions (`HtmlPageCacheWriter` driver half).
//!
//! Eligibility, identity, lookup, store, and diagnostics for one HTML
//! response. The storage codec lives in `ps-cache::html`; this drives
//! it through the metadata cache: lookup before rewriting serves
//! validated bodies (skipping the parse), while completed rewrites
//! buffer up to 1MB and publish on success with a `PS_HTML_DETAIL`
//! history line. `ShowCacheTimestamp` appends the C++ HIT/MISS/BYPASS
//! comment. Counter bumps (`html_cache_*`) go through the shared
//! registry, so every worker reports global totals.
//!
//! Divergences from C++, all deliberate: the port's stricter
//! `vary_absent` gate replaces C++'s `Vary: *`-only check;
//! per-request `Vary` on the request side follows the ported
//! `request_headers_cacheable` helper verbatim; detached-rewrite
//! tracking is omitted (port rewrites are synchronous, so no
//! rewrite ever detaches resource work past a deadline).
#![forbid(unsafe_code)]

use ps_cache::html as html_codec;
use ps_cache::purge::PurgeSet;
use ps_http::request::RequestHeaders;
use ps_http::response::{ResponseHeaders, ValidatorOption, VaryOption};
use std::rc::Rc;

/// Origin headers excluded from the identity string (fresh
/// Date/Age/Expires, framing, connection tokens, and our own
/// weak ETags must not churn the key).
const IDENTITY_EXCLUDED_HEADERS: &[&str] = &[
    "Date",
    "Age",
    "Expires",
    "Content-Length",
    "Vary",
    "Connection",
    "Keep-Alive",
];

/// Our weak ETag prefix (excluded from identity like C++).
const PSA_ETAG_PREFIX: &str = "W/\"PSA-";

/// Whole-HTML cache session for one response.
pub struct HtmlCacheSession {
    driver: Rc<super::server_context::ResourceDriver>,
    url: String,
    device: String,
    formats: String,
    device_suffix: String,
    state: SessionState,
}

/// Session state machine.
#[derive(Debug)]
enum SessionState {
    /// Lookup not run yet.
    Pending,
    /// Ineligible or disqualified (show renders BYPASS).
    Bypass,
    /// Lookup missed; buffering the rewrite for store.
    Armed {
        /// Cache key.
        key: String,
        /// TTL in ms.
        ttl_ms: i64,
        /// Origin expiry in ms.
        origin_expiry_ms: i64,
        /// Buffered rewritten bytes.
        body: Vec<u8>,
        /// Lookup-time beacon evidence identity (`None` when
        /// beaconing is inactive).
        beacon_identity: Option<Vec<u8>>,
    },
    /// Lookup hit; the caller serves the returned entry.
    Hit,
    /// Finished (store attempted or hit served).
    Done,
}

/// Store/show outcome of [`HtmlCacheSession::finish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlCacheFinish {
    /// History line for the message buffer ( publications only).
    pub detail_line: Option<String>,
    /// `ShowCacheTimestamp` comment bytes, if enabled.
    pub show_comment: Option<Vec<u8>>,
}

impl HtmlCacheSession {
    /// Binds a session to a rewrite driver and URL, snapshotting
    /// the device/format identity inputs (request properties are
    /// fully installed by the time the sink exists).
    #[must_use]
    pub fn new(driver: Rc<super::server_context::ResourceDriver>, url: &str) -> Self {
        let mut properties = driver.request_properties();
        let device_type = properties.device_type();
        let device =
            ps_http::user_agent_matcher::UserAgentMatcher::device_type_string(device_type)
                .to_string();
        let device_suffix =
            ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(device_type)
                .to_string();
        let formats = if properties.supports_avif_rewritten_urls() {
            "AVIF capable"
        } else if properties.supports_webp_rewritten_urls() {
            "WebP capable"
        } else {
            "Original formats"
        }
        .to_string();
        HtmlCacheSession {
            driver,
            url: url.to_string(),
            device,
            formats,
            device_suffix,
            state: SessionState::Pending,
        }
    }

    /// Looks up a cached rewrite (`Lookup`): on hit returns the
    /// entry to serve; on miss arms the session to buffer and
    /// store. Bumps bypasses/hits/misses. `file_mtime` is the
    /// origin file's modification time for file-mapped URLs: our
    /// filter runs upstream of nginx's `expires` handling, so
    /// same-server statics carry no visible freshness — the mtime
    /// (keyed into the identity) makes change detection exact
    /// where C++ reads final response headers instead. Visible
    /// prohibitions still veto file-backed entries.
    pub fn lookup(
        &mut self,
        origin: &ResponseHeaders,
        request: &RequestHeaders,
        purge: &PurgeSet,
        file_mtime: Option<(u64, u32)>,
        now_ms: i64,
    ) -> Option<html_codec::HtmlCacheEntry> {
        let options = self.driver.options();
        let enabled = options.html_cache();
        let Some((key, ttl_ms, origin_expiry_ms, beacon_identity)) =
            self.eligibility_key(origin, request, &options, file_mtime, now_ms)
        else {
            if enabled {
                self.count("html_cache_bypasses", 1);
            }
            self.state = SessionState::Bypass;
            return None;
        };
        let lookup = self.driver.server_context().metadata_cache().get(&key);
        let blob = match lookup {
            ps_cache::cache::Lookup {
                state: ps_cache::cache::KeyState::Available,
                value: Some(blob),
            } => blob,
            _ => {
                self.count("html_cache_misses", 1);
                self.state = SessionState::Armed {
                    key,
                    ttl_ms,
                    origin_expiry_ms,
                    body: Vec::new(),
                    beacon_identity,
                };
                return None;
            }
        };
        match html_codec::validate_value(&blob, &self.url, now_ms, ttl_ms, origin_expiry_ms, purge)
        {
            Some(entry) => {
                self.count("html_cache_hits", 1);
                self.count("html_cache_bytes_served", entry.body.len() as i64);
                self.state = SessionState::Hit;
                Some(entry)
            }
            None => {
                self.count("html_cache_misses", 1);
                self.state = SessionState::Armed {
                    key,
                    ttl_ms,
                    origin_expiry_ms,
                    body: Vec::new(),
                    beacon_identity,
                };
                None
            }
        }
    }

    /// Formats the HIT comment for an entry served now (`None`
    /// unless `ShowCacheTimestamp`).
    #[must_use]
    pub fn hit_comment(&self, entry: &html_codec::HtmlCacheEntry, now_ms: i64) -> Option<Vec<u8>> {
        if !self.driver.options().show_cache_timestamp() {
            return None;
        }
        Some(show_comment(true, entry.created_ms, now_ms).into_bytes())
    }

    /// Buffers rewritten bytes for a later store (over-cap bodies
    /// disqualify, like C++).
    pub fn write(&mut self, bytes: &[u8]) {
        if let SessionState::Armed { body, .. } = &mut self.state {
            if body.len() + bytes.len() <= html_codec::MAX_HTML_BODY_BYTES {
                body.extend_from_slice(bytes);
            } else {
                body.clear();
                self.state = SessionState::Bypass;
            }
        }
    }

    /// Completes the session (`Finish`): hits report bytes served,
    /// armed rewrites store (empty bodies, sampled beacon
    /// initializations, and mid-render evidence changes excluded)
    /// with a history line, and `ShowCacheTimestamp` renders the
    /// HIT/MISS/BYPASS comment. Idempotent.
    pub fn finish(&mut self, now_ms: i64) -> HtmlCacheFinish {
        let options = self.driver.options();
        let show = options.show_cache_timestamp();
        match std::mem::replace(&mut self.state, SessionState::Done) {
            // Hits serve at lookup time (the origin body is
            // swallowed, so this never runs for them); reaching
            // here anyway stays quiet and uncounted.
            SessionState::Hit => HtmlCacheFinish {
                detail_line: None,
                show_comment: None,
            },
            SessionState::Armed {
                key,
                ttl_ms,
                origin_expiry_ms,
                body,
                beacon_identity,
            } => {
                let mut created_ms = 0;
                let mut detail_line = None;
                // Skip rewrites whose beacon evidence changed while
                // filters ran (the beacon filter snapshots the
                // post-render identity at end of document).
                let evidence_settled = match (&beacon_identity, self.driver.beacon_evidence_identity()) {
                    (Some(then), Some(now)) => then == &now,
                    (Some(_), None) => false,
                    (None, _) => true,
                };
                if !body.is_empty() && evidence_settled && !contains_sampled_beacon_init(&body) {
                    let expires_ms = html_codec::store_expiry_ms(origin_expiry_ms, now_ms, ttl_ms);
                    if expires_ms > now_ms {
                        if let Some(value) = html_codec::encode_value(now_ms, expires_ms, &body) {
                            self.driver
                                .server_context()
                                .metadata_cache()
                                .put(&key, &value);
                            self.count("html_cache_inserts", 1);
                            created_ms = now_ms;
                            if self.url.len() <= 4096 {
                                detail_line = Some(format!(
                                    "PS_HTML_DETAIL\t{created_ms}\t{expires_ms}\t{}\t{}\t{}\t{}\t{}",
                                    body.len(),
                                    self.device,
                                    self.formats,
                                    key,
                                    ps_cache::base64::web64_encode(self.url.as_bytes()),
                                ));
                            }
                        }
                    }
                }
                // A store means this response missed (created==0
                // renders BYPASS for skipped stores).
                let show_comment =
                    show.then(|| show_comment(false, created_ms, now_ms).into_bytes());
                HtmlCacheFinish {
                    detail_line,
                    show_comment,
                }
            }
            _ => {
                let show_comment = show.then(|| show_comment(false, 0, now_ms).into_bytes());
                HtmlCacheFinish {
                    detail_line: None,
                    show_comment,
                }
            }
        }
    }

    /// Whether the lookup hit (serve cached, swallow the origin).
    #[must_use]
    pub fn is_hit(&self) -> bool {
        matches!(self.state, SessionState::Hit)
    }

    /// Computes eligibility plus the identity key (`ShouldCache`
    /// plus identity construction), or `None` when bypassing. The
    /// trailing element snapshots the lookup-time beacon evidence
    /// (`None` when beaconing is inactive).
    fn eligibility_key(
        &self,
        origin: &ResponseHeaders,
        request: &RequestHeaders,
        options: &ps_config::options::RewriteOptions,
        file_mtime: Option<(u64, u32)>,
        now_ms: i64,
    ) -> Option<(String, i64, i64, Option<Vec<u8>>)> {
        use ps_config::filter::Filter;
        let request_pairs: Vec<(&str, &str)> = request.headers().iter().collect();
        // Origin cacheability on a Vary-stripped policy copy, as in C++.
        let mut policy = origin.clone();
        policy.headers_mut().remove_all("Vary");
        policy.compute_caching();
        let origin_cacheable = policy.is_proxy_cacheable(
            request.properties(),
            VaryOption::RespectVaryOnResources,
            ValidatorOption::HasValidator,
        );
        let explicitly_fresh = policy.cache_expiration_time_ms() > now_ms;
        // File-backed URLs stand in for invisible freshness (nginx
        // applies `expires` downstream of this filter): content
        // changes track the mtime, which enters the identity. An
        // explicitly prohibitive origin still vetoes.
        let file_ok = file_mtime.is_some() && !has_cache_prohibition(origin);
        let eligibility = html_codec::HtmlEligibility {
            html_cache_enabled: options.html_cache(),
            ttl_sec: options.html_cache_ttl_sec(),
            is_get: request.method() == ps_http::request::Method::Get,
            origin_ok: origin.status_code() == 200,
            no_origin_cookies: !origin.headers().has("Set-Cookie")
                && !origin.headers().has("Set-Cookie2"),
            vary_absent: !origin.headers().has("Vary"),
            no_experiment: !options.running_experiment(),
            critical_css_ok: !options.enabled(Filter::PrioritizeCriticalCss),
            request_headers_ok: html_codec::request_headers_cacheable(&request_pairs),
            origin_cacheable: origin_cacheable || file_ok,
            origin_fresh: explicitly_fresh || file_ok,
            // Structural beacon flag only; learning gates (sampling
            // due, missing evidence) apply below with identity.
            beacon_ok: true,
        };
        if !eligibility.cacheable() {
            return None;
        }
        let ttl_ms = options.html_cache_ttl_sec() * 1000;
        let origin_expiry_ms = if explicitly_fresh {
            policy.cache_expiration_time_ms()
        } else {
            // No visible expiry (mtime-keyed file): the TTL alone bounds.
            i64::MAX
        };
        let mut identity = format!(
            "{}\n{}\n{}\n{}\n",
            self.url,
            self.driver.cache_fragment(),
            options.signature(),
            request.to_http_string(),
        );
        identity.push_str(&format!("origin-ttl:{}\n", policy.cache_ttl_ms()));
        if let Some((secs, nanos)) = file_mtime {
            identity.push_str(&format!("file-mtime:{secs}.{nanos:09}\n"));
        }
        for (name, value) in origin.headers().iter() {
            if IDENTITY_EXCLUDED_HEADERS
                .iter()
                .any(|excluded| name.eq_ignore_ascii_case(excluded))
            {
                continue;
            }
            if name.eq_ignore_ascii_case("Etag") && value.starts_with(PSA_ETAG_PREFIX) {
                continue;
            }
            identity.push_str(name);
            identity.push(':');
            identity.push_str(value);
            identity.push('\n');
        }
        // Beacon learning participation: when beaconing drives
        // rewrites, sampling-due requests and pre-learning
        // variants bypass, while learned evidence (including LCP)
        // joins the identity so each accepted beacon invalidates
        // the variant.
        let mut beacon_identity = None;
        if super::beacon_finder::beaconing_active(options) {
            let server = self.driver.server_context();
            let oh = server.hasher().hash(options.signature().as_bytes());
            let finder =
                super::beacon_finder::BeaconFinder::new(server.metadata_cache());
            let lookup = finder.load(&self.url, &oh, &self.device_suffix);
            match &lookup {
                super::beacon_finder::EvidenceLookup::Found(_) => {
                    self.count(super::beacon_finder::VALID_COUNT, 1);
                }
                super::beacon_finder::EvidenceLookup::Corrupt => {
                    self.count(super::beacon_finder::EXPIRED_COUNT, 1);
                }
                super::beacon_finder::EvidenceLookup::Missing => {
                    self.count(super::beacon_finder::NOT_FOUND_COUNT, 1);
                }
            }
            let record = lookup.record();
            let header = request.headers().lookup1("PS-ShouldBeacon");
            let downstream = super::beacon_finder::downstream_beacon_gate(
                options,
                header.as_deref(),
            );
            let learned = record.is_some_and(|record| record.valid_beacons >= 1);
            if !learned
                || super::beacon_finder::should_beacon(now_ms, record, downstream)
            {
                return None;
            }
            let record = record.expect("learned evidence has a record");
            let encoded = super::beacon_finder::encode_evidence(record);
            identity.push_str("\nbeacon:");
            identity.push_str(
                &String::from_utf8(encoded.clone()).expect("evidence codec emits UTF-8"),
            );
            beacon_identity = Some(encoded);
        }
        Some((
            html_codec::html_cache_key(&identity),
            ttl_ms,
            origin_expiry_ms,
            beacon_identity,
        ))
    }

    /// Bumps a counter, creating it when absent.
    fn count(&self, name: &str, delta: i64) {
        self.driver.server_context().variable(name).add(delta);
    }
}

/// Whether visible origin headers prohibit caching outright
/// (`no-store`/`private`/`no-cache` or `Pragma: no-cache`), which
/// vetoes even file-backed entries.
fn has_cache_prohibition(origin: &ResponseHeaders) -> bool {
    if origin.headers().lookup("Pragma").is_some_and(|values| {
        values
            .iter()
            .any(|value| value.eq_ignore_ascii_case("no-cache"))
    }) {
        return true;
    }
    origin
        .headers()
        .lookup("Cache-Control")
        .is_some_and(|values| {
            values.iter().any(|value| {
                value.split(',').any(|token| {
                    let token = token.trim();
                    token.eq_ignore_ascii_case("no-store")
                        || token.eq_ignore_ascii_case("private")
                        || token.eq_ignore_ascii_case("no-cache")
                })
            })
        })
}

/// Sampled beacon initialization markers (never freeze these
/// into a cached document, including the boundary case where
/// sampling becomes due during the rewrite).
const SAMPLED_BEACON_MARKERS: &[&[u8]] = &[
    b"pagespeed.CriticalImages.Run(",
    b"CriticalCssBeacon",
];

/// Whether a body carries sampled beacon initialization.
fn contains_sampled_beacon_init(body: &[u8]) -> bool {
    SAMPLED_BEACON_MARKERS.iter().any(|marker| {
        body.windows(marker.len())
            .any(|window| window == *marker)
    })
}

/// Formats the served-timestamp comment (`show_cache_timestamp`):
/// HIT/MISS with creation time and age, or BYPASS when nothing was
/// created nor served.
fn show_comment(hit: bool, created_ms: i64, now_ms: i64) -> String {
    if created_ms == 0 {
        return "<!-- PageSpeed Cache: BYPASS | Created: N/A | Age: N/A -->".to_string();
    }
    let (year, month, day, hour, minute, second) = ps_http::time::utc_ymd_hms(created_ms);
    format!(
        "<!-- PageSpeed Cache: {} | Created: {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC | Age: {}s -->",
        if hit { "HIT" } else { "MISS" },
        year,
        month,
        day,
        hour,
        minute,
        second,
        (now_ms - created_ms).max(0) / 1000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_cache::cache::MemoryCache;
    use ps_config::filter::Filter;
    use ps_config::options::RewriteOptions;
    use ps_http::mock::MockUrlFetcher;
    use ps_http::request::Method;
    use ps_http::response::HttpOptions;
    use std::cell::RefCell;
    use std::sync::Arc;

    const PAGE_URL: &str = "http://cache.test/page.html";
    const START_MS: i64 = 1_700_000_000_000;

    /// Harness: server with memory metadata, options, driver, purge set.
    struct Harness {
        options: Rc<RefCell<RewriteOptions>>,
        driver: Rc<crate::server_context::ResourceDriver>,
        purge: PurgeSet,
    }

    impl Harness {
        fn new() -> Self {
            let metadata: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
            let http: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
            let server = Rc::new(crate::server_context::ServerContext::new_with_metadata(
                http,
                metadata,
                Rc::new(MockUrlFetcher::new()),
                START_MS,
            ));
            let options = Rc::new(RefCell::new(RewriteOptions::new()));
            options
                .borrow_mut()
                .set_option("HtmlCache", "on", ps_config::scope::Scope::Server)
                .unwrap();
            options
                .borrow_mut()
                .set_option("HtmlCacheTtlSec", "60", ps_config::scope::Scope::Server)
                .unwrap();
            let driver = Rc::new(crate::server_context::ResourceDriver::new(
                server,
                Rc::clone(&options),
                PAGE_URL,
            ));
            Harness {
                options,
                driver,
                purge: PurgeSet::new(25_000),
            }
        }

        fn session(&self) -> HtmlCacheSession {
            HtmlCacheSession::new(Rc::clone(&self.driver), PAGE_URL)
        }

        fn get_request(&self) -> RequestHeaders {
            let mut request = RequestHeaders::new();
            request.set_method(Method::Get);
            request.set_version(1, 1);
            request.headers_mut().add("Host", "cache.test");
            request.headers_mut().add("Accept", "text/html");
            request
        }

        fn ok_origin(&self) -> ResponseHeaders {
            let mut origin = ResponseHeaders::new(HttpOptions::default_for_tests());
            origin.set_status_and_reason(200);
            origin.headers_mut().add("Content-Type", "text/html");
            origin
                .headers_mut()
                .add("Date", &ps_http::time::format_http_date(START_MS));
            origin
                .headers_mut()
                .add("Cache-Control", "public, max-age=300");
            origin.headers_mut().add("Content-Length", "4");
            origin.compute_caching();
            origin
        }

        fn counter(&self, name: &str) -> i64 {
            self.driver.server_context().variable(name).get()
        }
    }

    #[test]
    fn miss_stores_and_hit_serves() {
        let harness = Harness::new();
        let request = harness.get_request();
        let origin = harness.ok_origin();
        // First lookup misses and arms.
        let mut first = harness.session();
        assert!(first
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(1, harness.counter("html_cache_misses"));
        // Streaming the rewrite then finishing stores it.
        first.write(b"<html>");
        first.write(b"</html>");
        let finish = first.finish(START_MS + 1000);
        assert_eq!(1, harness.counter("html_cache_inserts"));
        let line = finish.detail_line.expect("stored line");
        assert!(line.starts_with("PS_HTML_DETAIL\t"), "{line}");
        assert_eq!(8, line.split('\t').count(), "{line}");
        assert!(finish.show_comment.is_none());
        // A fresh session hits the stored entry.
        let mut second = harness.session();
        let entry = second
            .lookup(&origin, &request, &harness.purge, None, START_MS + 2000)
            .expect("hit");
        assert_eq!(b"<html></html>", entry.body.as_slice());
        assert_eq!(1, harness.counter("html_cache_hits"));
        assert!(second.is_hit());
        // Show comments render HIT/MISS shapes.
        harness
            .options
            .borrow_mut()
            .set_option("ShowCacheTimestamp", "on", ps_config::scope::Scope::Server)
            .unwrap();
        let comment = second.hit_comment(&entry, START_MS + 61000);
        let comment = String::from_utf8(comment.expect("hit comment")).unwrap();
        assert!(comment.contains("PageSpeed Cache: HIT"), "{comment}");
        // Created at finish (START+1s), read at START+61s.
        assert!(comment.contains("Age: 60s"), "{comment}");
    }

    /// One bypass case: name plus its eligibility tweak.
    type BypassCase<'a> = (
        &'a str,
        Box<dyn Fn(&Harness, &mut RequestHeaders, &mut ResponseHeaders) + 'a>,
    );

    #[test]
    fn each_gate_bypasses_alone() {
        // Every eligibility input vetoes independently (bypass counted,
        // nothing stored, BYPASS comment when shown).
        let cases: Vec<BypassCase<'_>> = vec![
            (
                "post",
                Box::new(|_, request, _| request.set_method(Method::Post)),
            ),
            (
                "status",
                Box::new(|harness, _, origin| {
                    let _ = harness;
                    origin.set_status_and_reason(404);
                    origin.compute_caching();
                }),
            ),
            (
                "set-cookie",
                Box::new(|_, _, origin| {
                    origin.headers_mut().add("Set-Cookie", "a=1");
                    origin.compute_caching();
                }),
            ),
            (
                "vary",
                Box::new(|_, _, origin| {
                    origin.headers_mut().add("Vary", "Accept-Encoding");
                    origin.compute_caching();
                }),
            ),
            (
                "cookie",
                Box::new(|_, request, _| {
                    request.headers_mut().add("Cookie", "a=1");
                }),
            ),
            (
                "auth",
                Box::new(|_, request, _| {
                    request.headers_mut().add("Authorization", "x");
                }),
            ),
            (
                "no-store",
                Box::new(|_, _, origin| {
                    origin.headers_mut().remove_all("Cache-Control");
                    origin.headers_mut().add("Cache-Control", "no-store");
                    origin.compute_caching();
                }),
            ),
            (
                "expired",
                Box::new(|_, _, origin| {
                    origin.headers_mut().remove_all("Cache-Control");
                    origin.headers_mut().add("Cache-Control", "max-age=0");
                    origin.compute_caching();
                }),
            ),
            (
                "cache-off",
                Box::new(|harness, _, _| {
                    harness
                        .options
                        .borrow_mut()
                        .set_option("HtmlCache", "off", ps_config::scope::Scope::Server)
                        .unwrap();
                }),
            ),
            (
                "ttl-zero",
                Box::new(|harness, _, _| {
                    harness
                        .options
                        .borrow_mut()
                        .set_option("HtmlCacheTtlSec", "0", ps_config::scope::Scope::Server)
                        .unwrap();
                }),
            ),
            (
                "prioritize",
                Box::new(|harness, _, _| {
                    harness
                        .options
                        .borrow_mut()
                        .soft_enable_filter_for_testing(Filter::PrioritizeCriticalCss);
                }),
            ),
        ];
        for (name, tweak) in cases {
            let harness = Harness::new();
            let mut request = harness.get_request();
            let mut origin = harness.ok_origin();
            tweak(&harness, &mut request, &mut origin);
            let mut session = harness.session();
            assert!(
                session
                    .lookup(&origin, &request, &harness.purge, None, START_MS)
                    .is_none(),
                "{name} must bypass"
            );
            // Bypass counts only when the feature is on (C++ skips the
            // bump while disabled so idle servers stay quiet).
            let bypasses = if name == "cache-off" { 0 } else { 1 };
            assert_eq!(bypasses, harness.counter("html_cache_bypasses"), "{name}");
            assert_eq!(0, harness.counter("html_cache_misses"), "{name}");
        }
    }

    #[test]
    fn experiment_bypasses() {
        let harness = Harness::new();
        harness
            .options
            .borrow_mut()
            .set_option(
                "RunExperiment",
                "true",
                ps_config::scope::Scope::ProcessStrict,
            )
            .unwrap();
        let mut session = harness.session();
        assert!(session
            .lookup(
                &harness.ok_origin(),
                &harness.get_request(),
                &harness.purge,
                None,
                START_MS
            )
            .is_none());
        assert_eq!(1, harness.counter("html_cache_bypasses"));
    }

    #[test]
    fn oversize_and_sampled_bodies_do_not_store() {
        let harness = Harness::new();
        let request = harness.get_request();
        let origin = harness.ok_origin();
        // Over-cap bodies disqualify mid-stream.
        let mut big = harness.session();
        assert!(big
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        big.write(&vec![b'x'; html_codec::MAX_HTML_BODY_BYTES + 1]);
        let finish = big.finish(START_MS + 1000);
        assert!(finish.detail_line.is_none());
        assert_eq!(0, harness.counter("html_cache_inserts"));
        // Sampled beacon initializations never freeze.
        for marker in [
            "pagespeed.CriticalImages.Run('",
            "CriticalCssBeacon",
        ] {
            let mut sampled = harness.session();
            assert!(sampled
                .lookup(&origin, &request, &harness.purge, None, START_MS)
                .is_none());
            sampled.write(b"<html><script>");
            sampled.write(marker.as_bytes());
            sampled.write(b"</script></html>");
            let finish = sampled.finish(START_MS + 1000);
            assert!(finish.detail_line.is_none(), "{marker}");
        }
        assert_eq!(0, harness.counter("html_cache_inserts"));
    }

    #[test]
    fn identity_splits_user_agents_and_purges() {
        let harness = Harness::new();
        let request = harness.get_request();
        let origin = harness.ok_origin();
        let mut first = harness.session();
        assert!(first
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        first.write(b"<html/>");
        first.finish(START_MS + 1000);
        assert_eq!(1, harness.counter("html_cache_inserts"));
        // A different UA is a different variant (identity miss).
        let mut other_ua = harness.get_request();
        other_ua.headers_mut().add("User-Agent", "Other/1.0");
        let mut second = harness.session();
        assert!(second
            .lookup(&origin, &other_ua, &harness.purge, None, START_MS + 2000)
            .is_none());
        assert_eq!(2, harness.counter("html_cache_misses"));
        // Purging the URL invalidates the stored entry.
        let mut purged = harness;
        purged.purge.put(PAGE_URL, START_MS + 3000);
        let mut third = purged.session();
        assert!(third
            .lookup(&origin, &request, &purged.purge, None, START_MS + 4000)
            .is_none());
    }

    #[test]
    fn file_mtime_stands_in_for_invisible_freshness() {
        let harness = Harness::new();
        // No Cache-Control at all (nginx applies `expires`
        // downstream of the lookup): explicit freshness absent.
        let mut origin = ResponseHeaders::new(HttpOptions::default_for_tests());
        origin.set_status_and_reason(200);
        origin.headers_mut().add("Content-Type", "text/html");
        origin.compute_caching();
        let request = harness.get_request();
        let mtime = Some((1_700_000_000u64, 0u32));
        let mut first = harness.session();
        assert!(first
            .lookup(&origin, &request, &harness.purge, mtime, START_MS)
            .is_none());
        first.write(b"<html/>");
        let finish = first.finish(START_MS + 1000);
        assert!(finish.detail_line.is_some());
        assert_eq!(1, harness.counter("html_cache_inserts"));
        // Same mtime hits.
        let mut second = harness.session();
        assert!(second
            .lookup(&origin, &request, &harness.purge, mtime, START_MS + 2000)
            .is_some());
        // Changed mtime busts (different identity).
        let mut third = harness.session();
        assert!(third
            .lookup(
                &origin,
                &request,
                &harness.purge,
                Some((1_700_000_001, 0)),
                START_MS + 2000
            )
            .is_none());
        // A visible prohibition vetoes even file-backed entries.
        let mut banned = harness.session();
        let mut no_store = origin.clone();
        no_store.headers_mut().add("Cache-Control", "no-store");
        no_store.compute_caching();
        assert!(banned
            .lookup(&no_store, &request, &harness.purge, mtime, START_MS + 2000)
            .is_none());
        assert_eq!(1, harness.counter("html_cache_bypasses"));
    }

    /// Seeds one accepted stamped beacon for the harness page,
    /// returning the stored evidence. Repeatable: each seed
    /// simulates a due sampling followed by its beacon, leaving
    /// sampling settled in the future.
    fn seed_learned_beacon(harness: &Harness) -> crate::beacon_finder::BeaconEvidence {
        use crate::beacon_finder as finder;
        let server = harness.driver.server_context();
        let oh = server
            .hasher()
            .hash(harness.driver.options().signature().as_bytes());
        let cohort = finder::BeaconFinder::new(server.metadata_cache());
        let mut evidence = cohort.load(PAGE_URL, &oh, "@Desktop").into_record();
        evidence.next_beacon_ms = 0;
        let nonce = finder::prepare_beacon(&mut evidence, START_MS, 5, 77).expect("due");
        let hash = finder::url_hash("http://cache.test/hero.png");
        finder::update_candidates(
            &mut evidence,
            &[hash.clone()].into_iter().collect(),
            false,
        );
        let report = finder::BeaconReport {
            html_hashes: Some([hash].into_iter().collect()),
            lcp_kind: finder::LCP_KIND_IMG.to_string(),
            lcp_image_url: String::new(),
            nonce,
        };
        assert_eq!(
            finder::accept_beacon(&mut evidence, &report, START_MS),
            finder::AcceptOutcome::Accepted
        );
        // Settle sampling so lookups are not due.
        evidence.next_beacon_ms = START_MS + 1_000_000;
        cohort.store(PAGE_URL, &oh, "@Desktop", &evidence);
        evidence
    }

    /// Snapshots the current evidence identity into the driver,
    /// mirroring the beacon filter's end-of-document write.
    fn settle_driver_identity(harness: &Harness) {
        use crate::beacon_finder as finder;
        let server = harness.driver.server_context();
        let oh = server
            .hasher()
            .hash(harness.driver.options().signature().as_bytes());
        let cohort = finder::BeaconFinder::new(server.metadata_cache());
        let evidence = cohort.load(PAGE_URL, &oh, "@Desktop").into_record();
        harness
            .driver
            .set_beacon_evidence_identity(finder::encode_evidence(&evidence));
    }

    #[test]
    fn beacon_learning_gates_lookup_store_and_identity() {
        use crate::beacon_finder as finder;
        // Default options drive no beacon learning.
        assert!(!finder::beaconing_active(
            &Harness::new().driver.options()
        ));
        let harness = Harness::new();
        harness
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::PrioritizeLcpImages);
        assert!(finder::beaconing_active(&harness.driver.options()));
        let request = harness.get_request();
        let origin = harness.ok_origin();
        // Pre-learning variants bypass (nothing stored to hit).
        let mut cold = harness.session();
        assert!(cold
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(1, harness.counter("html_cache_bypasses"));
        assert_eq!(1, harness.counter(finder::NOT_FOUND_COUNT));
        // A learned, settled variant misses, stores, and hits.
        seed_learned_beacon(&harness);
        let mut first = harness.session();
        assert!(first
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(1, harness.counter("html_cache_misses"));
        assert_eq!(1, harness.counter(finder::VALID_COUNT));
        first.write(b"<html/>");
        settle_driver_identity(&harness);
        let finish = first.finish(START_MS + 1000);
        assert!(finish.detail_line.is_some());
        assert_eq!(1, harness.counter("html_cache_inserts"));
        let mut second = harness.session();
        assert!(second
            .lookup(&origin, &request, &harness.purge, None, START_MS + 2000)
            .is_some());
        assert_eq!(1, harness.counter("html_cache_hits"));
        // A new accepted beacon invalidates the variant (miss).
        seed_learned_beacon(&harness);
        let mut third = harness.session();
        assert!(third
            .lookup(&origin, &request, &harness.purge, None, START_MS + 3000)
            .is_none());
        assert_eq!(2, harness.counter("html_cache_misses"));
    }

    #[test]
    fn sampling_due_and_unsettled_renders_bypass() {
        // Sampling-due variants bypass even with learned evidence.
        let harness = Harness::new();
        harness
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::PrioritizeLcpImages);
        let mut evidence = seed_learned_beacon(&harness);
        evidence.next_beacon_ms = 0;
        let server = harness.driver.server_context();
        let oh = server
            .hasher()
            .hash(harness.driver.options().signature().as_bytes());
        crate::beacon_finder::BeaconFinder::new(server.metadata_cache()).store(
            PAGE_URL,
            &oh,
            "@Desktop",
            &evidence,
        );
        let mut due = harness.session();
        assert!(due
            .lookup(
                &harness.ok_origin(),
                &harness.get_request(),
                &harness.purge,
                None,
                START_MS
            )
            .is_none());
        assert_eq!(1, harness.counter("html_cache_bypasses"));
        // Renders whose evidence changed mid-flight do not store.
        let settled = Harness::new();
        settled
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::PrioritizeLcpImages);
        seed_learned_beacon(&settled);
        let mut render = settled.session();
        assert!(render
            .lookup(
                &settled.ok_origin(),
                &settled.get_request(),
                &settled.purge,
                None,
                START_MS
            )
            .is_none());
        render.write(b"<html/>");
        // No driver snapshot (or a divergent one): skip the store.
        let finish = render.finish(START_MS + 1000);
        assert!(finish.detail_line.is_none());
        assert_eq!(0, settled.counter("html_cache_inserts"));
    }

    #[test]
    fn show_comment_shapes() {
        // 2023-11-14 06:13:20 UTC.
        let created = 1_699_942_400_000;
        let hit = show_comment(true, created, created + 65_000);
        assert_eq!(
            hit,
            "<!-- PageSpeed Cache: HIT | Created: 2023-11-14 06:13:20 UTC | Age: 65s -->"
        );
        let miss = show_comment(false, created, created + 1000);
        assert!(miss.contains("MISS"), "{miss}");
        assert!(miss.contains("Age: 1s"), "{miss}");
        assert_eq!(
            show_comment(false, 0, created),
            "<!-- PageSpeed Cache: BYPASS | Created: N/A | Age: N/A -->"
        );
    }
}

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
//! Divergences from C++, all deliberate: only `Vary: *`
//! bypasses (named Vary fields address per-variant entries,
//! with explicit origin Vary preserved for normalized navigation headers);
//! per-request `Vary` on the request side follows the ported
//! `request_headers_cacheable` helper verbatim; request cookies
//! pass through the allowlist policy in `ps_config::cookies`
//! (common analytics/tracking cookies cache like cookie-free
//! requests, with values excluded from the cache key) instead of
//! bypassing outright; unlearned beacon variants cache (C++
//! bypasses until first learning) under an unlearned identity
//! marker, so later evidence versions them away instead of
//! blocking publication.
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
    early_policy: Option<(String, Vec<u8>)>,
}

/// Why a lookup bypassed (cookie names only; cookie values never
/// leave the parser).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlCacheBypassReason {
    /// Non-cookie gate (method, status, `Vary: *`, freshness,
    /// beacon sampling, oversize …).
    Other,
    /// First offending request-cookie name (truncated to
    /// [`ps_config::cookies::MAX_REASON_NAME_LEN`] characters).
    Cookie(String),
    /// A `Cookie` header failed strict parsing.
    MalformedCookie,
    /// A forbidden request header (`Authorization`, request `Vary`,
    /// or `Cookie2`).
    ForbiddenHeader(&'static str),
}

/// Eligible lookup inputs plus the allowlisted-cookie count.
struct Eligibility {
    /// Cache key.
    key: String,
    /// TTL in ms.
    ttl_ms: i64,
    /// Origin expiry in ms.
    origin_expiry_ms: i64,
    /// Lookup-time beacon evidence identity (`None` when
    /// beaconing is inactive).
    beacon_identity: Option<Vec<u8>>,
    /// Cookies accepted through the allowlist (0 when cookie-free).
    allowlisted_cookies: usize,
}

/// Session state machine.
#[derive(Debug)]
enum SessionState {
    /// Lookup not run yet.
    Pending,
    /// Ineligible or disqualified (show renders BYPASS).
    Bypass {
        /// Attributed reason (drives bypass history lines).
        reason: HtmlCacheBypassReason,
    },
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
    /// History line for the message buffer (publications only).
    pub detail_line: Option<String>,
    /// History line for cookie-attributed bypasses (`None` for
    /// stores and non-cookie bypasses).
    pub bypass_line: Option<String>,
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
        let device = ps_http::user_agent_matcher::UserAgentMatcher::device_type_string(device_type)
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
            early_policy: None,
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
        let eligibility = match self.eligibility_key(origin, request, &options, file_mtime, now_ms)
        {
            Ok(eligibility) => eligibility,
            Err(reason) => {
                if enabled {
                    self.count("html_cache_bypasses", 1);
                }
                self.state = SessionState::Bypass { reason };
                return None;
            }
        };
        if eligibility.allowlisted_cookies > 0 {
            self.count("html_cache_cookie_allowlisted", 1);
        }
        let Eligibility {
            key,
            ttl_ms,
            origin_expiry_ms,
            beacon_identity,
            ..
        } = eligibility;
        let lookup = self.driver.server_context().metadata_cache().get(&key);
        if let ps_cache::cache::Lookup {
            state: ps_cache::cache::KeyState::Available,
            value: Some(blob),
        } = lookup
        {
            if let Some(entry) = html_codec::validate_value(
                &blob,
                &self.url,
                now_ms,
                ttl_ms,
                origin_expiry_ms,
                purge,
            ) {
                self.count("html_cache_hits", 1);
                self.count("html_cache_bytes_served", entry.body.len() as i64);
                self.state = SessionState::Hit;
                return Some(entry);
            }
        }
        // Only a MISS can publish a new early-lookup hint. Avoid building its
        // key and cloning/serializing origin headers for an existing HIT.
        self.early_policy = self.early_key(request, file_mtime).and_then(|hint_key| {
            let bytes = origin.clone().write_as_binary();
            (bytes.len() <= 65000).then_some((hint_key, bytes))
        });
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

    /// Origin-independent candidate key. File-backed routes use the current
    /// regular-file mtime; public navigation headers are normalized and
    /// cookie policy is checked before constructing variant inputs. This is a hint, never an authorization to serve alone.
    fn early_key(
        &self,
        request: &RequestHeaders,
        file_mtime: Option<(u64, u32)>,
    ) -> Option<String> {
        if !self.driver.options().html_cache_early_lookup()
            || !self.driver.options().html_cache()
            || request.method() != ps_http::request::Method::Get
            || request.headers().iter().any(|(name, value)| {
                [
                    "Authorization",
                    "Range",
                    "If-Range",
                    "If-None-Match",
                    "If-Modified-Since",
                    "If-Match",
                    "If-Unmodified-Since",
                    "Pragma",
                    "Vary",
                ]
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n))
                    || (name.eq_ignore_ascii_case("Cache-Control")
                        && !(self.driver.options().html_cache_public_browser_grouping()
                            && public_refresh_directive(value)))
            })
        {
            return None;
        }
        // Match the ordinary lookup's configured cookie policy before
        // omitting cookie values from the hint identity. A late HIT must
        // not require origin work solely for approved analytics cookies.
        let headers: Vec<(&str, &str)> = request.headers().iter().collect();
        cookie_verdict(&self.driver.options(), &headers).ok()?;
        Some(format!(
            "early-html-v2:{}",
            html_codec::html_cache_key(&format!(
                "{}\n{}\n{}\n{}\n{:?}",
                self.url,
                self.driver.cache_fragment(),
                self.driver.options_signature(),
                self.request_identity(request, None),
                file_mtime
            ))
        ))
    }

    /// Attempts a fully validated HIT using previously published origin policy.
    /// Missing/corrupt hints never arm a rewrite or count as ordinary misses.
    pub fn lookup_early(
        &mut self,
        request: &RequestHeaders,
        purge: &PurgeSet,
        file_mtime: Option<(u64, u32)>,
        now_ms: i64,
    ) -> Option<(html_codec::HtmlCacheEntry, ResponseHeaders)> {
        let hint_key = self.early_key(request, file_mtime)?;
        let cache = self.driver.server_context().metadata_cache();
        let hint = cache.get(&hint_key);
        if hint.state != ps_cache::cache::KeyState::Available {
            return None;
        }
        let blob = hint.value?;
        if blob.len() > 65536 {
            return None;
        }
        let policy = html_codec::validate_value(
            &blob,
            &self.url,
            now_ms,
            self.driver.options().html_cache_ttl_sec() * 1000,
            i64::MAX,
            purge,
        )?;
        let mut origin = ResponseHeaders::new(ps_http::response::HttpOptions::default());
        origin.read_from_binary(&policy.body).ok()?;
        let eligible = self
            .eligibility_key(&origin, request, &self.driver.options(), file_mtime, now_ms)
            .ok()?;
        let cached = cache.get(&eligible.key);
        if cached.state != ps_cache::cache::KeyState::Available {
            return None;
        }
        let body = cached.value?;
        let entry = html_codec::validate_value(
            &body,
            &self.url,
            now_ms,
            eligible.ttl_ms,
            eligible.origin_expiry_ms,
            purge,
        )?;
        if eligible.allowlisted_cookies > 0 {
            self.count("html_cache_cookie_allowlisted", 1);
        }
        self.count("html_cache_hits", 1);
        self.count("html_cache_bytes_served", entry.body.len() as i64);
        self.state = SessionState::Hit;
        Some((entry, origin))
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
                self.state = SessionState::Bypass {
                    reason: HtmlCacheBypassReason::Other,
                };
            }
        }
    }

    /// Completes the session (`Finish`): hits report bytes served,
    /// armed rewrites store (empty bodies, sampled beacon
    /// initializations, mid-render evidence changes, and requests
    /// failing the publication cookie re-check excluded) with a
    /// history line, cookie-attributed bypasses emit a bypass
    /// history line, and `ShowCacheTimestamp` renders the
    /// HIT/MISS/BYPASS comment. Idempotent.
    pub fn finish(&mut self, request: &RequestHeaders, now_ms: i64) -> HtmlCacheFinish {
        let options = self.driver.options();
        let show = options.show_cache_timestamp();
        // Publication re-check: the store path enforces the same
        // request policy as lookup (fail closed).
        let request_pairs: Vec<(&str, &str)> = request.headers().iter().collect();
        let publication_ok = request_eligible(&options, &request_pairs).is_ok();
        match std::mem::replace(&mut self.state, SessionState::Done) {
            // Hits serve at lookup time (the origin body is
            // swallowed, so this never runs for them); reaching
            // here anyway stays quiet and uncounted.
            SessionState::Hit => HtmlCacheFinish {
                detail_line: None,
                bypass_line: None,
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
                let evidence_settled =
                    match (&beacon_identity, self.driver.beacon_evidence_identity()) {
                        (Some(then), Some(now)) => then == &now,
                        (Some(_), None) => false,
                        (None, _) => true,
                    };
                // Detached rewrites (deadline-pending inputs serving
                // fallback content) never publish: the background
                // completion warms the caches for a complete later
                // rewrite instead.
                if !body.is_empty()
                    && evidence_settled
                    && publication_ok
                    && self.driver.detached_rewrites() == 0
                    && !contains_sampled_beacon_init(&body)
                {
                    let expires_ms = html_codec::store_expiry_ms(origin_expiry_ms, now_ms, ttl_ms);
                    if expires_ms > now_ms {
                        if let Some(value) = html_codec::encode_value(now_ms, expires_ms, &body) {
                            self.driver
                                .server_context()
                                .metadata_cache()
                                .put(&key, &value);
                            if let Some((hint_key, bytes)) = self.early_policy.take() {
                                if let Some(hint) =
                                    html_codec::encode_value(now_ms, expires_ms, &bytes)
                                {
                                    self.driver
                                        .server_context()
                                        .metadata_cache()
                                        .put(&hint_key, &hint);
                                }
                            }
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
                let show_comment = show.then(|| {
                    let mut comment = show_comment(false, created_ms, now_ms);
                    if created_ms == 0 {
                        let reason = if body.is_empty() {
                            "empty-body"
                        } else if !publication_ok {
                            "request-policy"
                        } else if !evidence_settled {
                            "beacon-evidence-changed"
                        } else if self.driver.detached_rewrites() != 0 {
                            "resource-optimization-pending"
                        } else if contains_sampled_beacon_init(&body) {
                            "beacon-sampling"
                        } else {
                            "expired-or-unencodable"
                        };
                        comment.push_str(&format!("\n<!-- PageSpeed Cache Detail: {reason} -->"));
                    }
                    comment.into_bytes()
                });
                HtmlCacheFinish {
                    detail_line,
                    bypass_line: None,
                    show_comment,
                }
            }
            SessionState::Bypass { reason } => {
                let show_comment = show.then(|| show_comment(false, 0, now_ms).into_bytes());
                HtmlCacheFinish {
                    detail_line: None,
                    bypass_line: bypass_history_line(&reason, &self.url, now_ms),
                    show_comment,
                }
            }
            SessionState::Pending | SessionState::Done => {
                let show_comment = show.then(|| show_comment(false, 0, now_ms).into_bytes());
                HtmlCacheFinish {
                    detail_line: None,
                    bypass_line: None,
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

    /// The attributed bypass reason (`None` unless bypassed).
    #[must_use]
    pub fn bypass_reason(&self) -> Option<&HtmlCacheBypassReason> {
        match &self.state {
            SessionState::Bypass { reason } => Some(reason),
            _ => None,
        }
    }

    /// Computes eligibility plus the identity key (`ShouldCache`
    /// plus identity construction), or the attributed bypass reason.
    /// The identity drops `Cookie` lines (requests that reach key
    /// generation hold only ignored cookies/public preferences), adds opaque
    /// preference selectors, and snapshots the
    /// lookup-time beacon evidence (`None` when beaconing is
    /// inactive).
    fn eligibility_key(
        &self,
        origin: &ResponseHeaders,
        request: &RequestHeaders,
        options: &ps_config::options::RewriteOptions,
        file_mtime: Option<(u64, u32)>,
        now_ms: i64,
    ) -> Result<Eligibility, HtmlCacheBypassReason> {
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
        // Strict request gates first (attributed precisely), then
        // the cookie policy; the matrix below keeps the full gate
        // set in one place.
        let strict = strict_request_headers_ok(&request_pairs);
        let cookies = cookie_verdict(options, &request_pairs);
        let eligibility = html_codec::HtmlEligibility {
            html_cache_enabled: options.html_cache(),
            ttl_sec: options.html_cache_ttl_sec(),
            is_get: request.method() == ps_http::request::Method::Get,
            origin_ok: origin.status_code() == 200,
            no_origin_cookies: !origin.headers().has("Set-Cookie")
                && !origin.headers().has("Set-Cookie2"),
            vary_star_absent: !origin_varies_star(origin),
            no_experiment: !options.running_experiment(),
            critical_css_ok: !options.enabled(Filter::PrioritizeCriticalCss),
            request_headers_ok: strict.is_ok(),
            cookies_ok: cookies.is_ok(),
            origin_cacheable: origin_cacheable || file_ok,
            origin_fresh: explicitly_fresh || file_ok,
            // Structural beacon flag only; learning gates (sampling
            // due, missing evidence) apply below with identity.
            beacon_ok: true,
        };
        if !eligibility.cacheable() {
            if let Err(header) = strict {
                return Err(HtmlCacheBypassReason::ForbiddenHeader(header));
            }
            if let Err(reason) = &cookies {
                return Err(reason.clone());
            }
            return Err(HtmlCacheBypassReason::Other);
        }
        let allowlisted_cookies = cookies.unwrap_or(0);
        let ttl_ms = options.html_cache_ttl_sec() * 1000;
        let origin_expiry_ms = if explicitly_fresh {
            policy.cache_expiration_time_ms()
        } else {
            // No visible expiry (mtime-keyed file): the TTL alone bounds.
            i64::MAX
        };
        let mut identity = format!(
            "public-html-v2\n{}\n{}\n{}\n{}\n",
            self.url,
            self.driver.cache_fragment(),
            self.driver.options_signature(),
            self.request_identity(request, Some(origin)),
        );
        identity.push_str(&format!(
            "debug:{}\norigin-ttl:{}\n",
            options.enabled(Filter::Debug),
            policy.cache_ttl_ms()
        ));
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
        // Beacon learning participation: learned evidence
        // (including LCP) joins the identity so each accepted
        // beacon versions the variant away; sampling-due requests
        // bypass so collection keeps running. Unlike C++ (which
        // bypasses until first learning), unlearned variants cache
        // under the empty-evidence marker: their first rewrite
        // samples through the beacon filter (whose nonce keeps that
        // rewrite out of the store), and arriving evidence rekeys
        // later requests instead of blocking publication.
        let mut beacon_identity = None;
        if super::beacon_finder::beaconing_active(options) {
            let server = self.driver.server_context();
            let oh = server
                .hasher()
                .hash(self.driver.options_signature().as_bytes());
            let finder = super::beacon_finder::BeaconFinder::new(server.metadata_cache());
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
            let downstream =
                super::beacon_finder::downstream_beacon_gate(options, header.as_deref());
            // Sampling cadence applies to recorded evidence, and only
            // when this request could actually contribute one: clients
            // that cannot beacon (curl, bots, latency probes) would
            // otherwise bypass forever on a perpetually-due record
            // they can never settle. Virgin variants stay eligible
            // (gated downstream integration still decides for itself).
            let can_sample = self
                .driver
                .request_properties()
                .supports_critical_images_beacon();
            let due = match downstream {
                super::beacon_finder::DownstreamBeacon::MustMatch(_) => {
                    super::beacon_finder::should_beacon(now_ms, record, downstream)
                }
                _ => {
                    can_sample
                        && record.is_some_and(|record| {
                            super::beacon_finder::should_beacon(now_ms, Some(record), downstream)
                        })
                }
            };
            if due {
                return Err(HtmlCacheBypassReason::Other);
            }
            let empty;
            let effective = match record {
                Some(record) => record,
                // Missing/corrupt evidence keys as the empty record
                // (matching the filter's snapshot); first evidence
                // rekeys the variant.
                None => {
                    empty = super::beacon_finder::BeaconEvidence::default();
                    &empty
                }
            };
            let encoded = super::beacon_finder::encode_evidence(effective);
            identity.push_str("\nbeacon:");
            identity
                .push_str(&String::from_utf8(encoded.clone()).expect("evidence codec emits UTF-8"));
            beacon_identity = Some(encoded);
        }
        // Font learning participation: the confirmed set joins the
        // identity so learned confirmations version the variant away
        // (content-addressed refresh). Unlearned variants cache under
        // the empty marker, matching the filter's snapshot; beacon
        // merges rekey later requests instead of blocking publication.
        // No settled check: worst case orphans one variant, and the
        // filter snapshots inputs per render so output stays coherent.
        if options.enabled(ps_config::filter::Filter::PreloadCriticalFonts) {
            use ps_cache::cache::KeyState;
            let server = self.driver.server_context();
            let oh = server
                .hasher()
                .hash(self.driver.options_signature().as_bytes());
            let key = super::font_store::confirmed_key(&self.url, &oh, &self.device_suffix);
            let blob = match server.metadata_cache().get(&key) {
                ps_cache::cache::Lookup {
                    state: KeyState::Available,
                    value: Some(blob),
                } => blob,
                _ => Vec::new(),
            };
            identity.push_str("\nfont-beacon:");
            identity.push_str(&String::from_utf8_lossy(&blob));
        }
        Ok(Eligibility {
            key: html_codec::html_cache_key(&identity),
            ttl_ms,
            origin_expiry_ms,
            beacon_identity,
            allowlisted_cookies,
        })
    }

    fn request_identity(
        &self,
        request: &RequestHeaders,
        origin: Option<&ResponseHeaders>,
    ) -> String {
        let mut identity = self.request_identity_without_preferences(request, origin);
        // Tracking cookies stay omitted. These two public preferences can
        // affect generated HTML, so both normal and early keys must vary.
        // Only opaque digests join identity; raw values never reach keys/logs.
        match ps_config::cookies::preference_cookie_values(request.headers().iter()) {
            Ok(values) => {
                for (name, value) in ps_config::cookies::BUILTIN_PREFERENCE_COOKIES
                    .iter()
                    .zip(values)
                {
                    if let Some(value) = value {
                        identity.push_str("\npublic-cookie-preference:");
                        identity.push_str(name);
                        identity.push(':');
                        identity.push_str(&html_codec::sign_url(
                            "pagespeed-public-cookie-preference-v1",
                            value,
                        ));
                    }
                }
            }
            // Eligibility rejects malformed/duplicate cookies before key use.
            Err(_) => identity.push_str("\ninvalid-cookie-preference"),
        }
        identity
    }

    fn request_identity_without_preferences(
        &self,
        request: &RequestHeaders,
        origin: Option<&ResponseHeaders>,
    ) -> String {
        if !self.driver.options().html_cache_public_browser_grouping() {
            return public_request_identity(request, origin);
        }
        let mut normalized = request.clone();
        // Explicit public-site contract overrides these origin UA/Accept
        // dimensions only. Other explicit Vary fields remain unchanged;
        // public transport/refresh/privacy normalization below honors them.
        for name in [
            "User-Agent",
            "Accept",
            "Sec-CH-UA",
            "Sec-CH-UA-Full-Version",
            "Sec-CH-UA-Full-Version-List",
            "Sec-CH-UA-Mobile",
            "Sec-CH-UA-Platform",
        ] {
            normalized.headers_mut().remove_all(name);
        }
        // These do not select public content unless the origin declares
        // Vary. Only normalize the cache-key copy: privacy signals and
        // transport/request directives are still forwarded to the origin.
        for name in ["Connection", "Keep-Alive", "TE", "Sec-GPC", "DNT"] {
            if !origin_varies_on(origin, name) {
                normalized.headers_mut().remove_all(name);
            }
        }
        if !origin_varies_on(origin, "Cache-Control")
            && normalized
                .headers()
                .lookup("Cache-Control")
                .is_some_and(|values| values.iter().all(|value| public_refresh_directive(value)))
        {
            normalized.headers_mut().remove_all("Cache-Control");
        }
        let mut p = self.driver.request_properties();
        // In-place resource negotiation is not an HTML output capability.
        // Rewritten-image support above includes UA inference and q=0 vetoes.
        let caps = [
            p.supports_avif_rewritten_urls(),
            p.supports_webp_rewritten_urls(),
            p.supports_webp_lossless_alpha(),
            (self
                .driver
                .options()
                .enabled(ps_config::filter::Filter::ConvertToWebpAnimated)
                || self
                    .driver
                    .options()
                    .enabled(ps_config::filter::Filter::RecompressWebp))
                && p.supports_webp_animated(),
            p.supports_image_inlining(),
            p.supports_lazyload_images(),
            p.supports_js_defer(false),
            p.supports_js_defer(true),
            p.supports_dns_prefetch(),
            p.supports_dns_prefetch_using_rel_prefetch(),
            p.supports_critical_css(),
            p.supports_critical_css_beacon(),
            p.supports_critical_images_beacon(),
            p.forbid_webp_inlining(),
            p.requests_save_data(),
            p.has_via_header(),
            p.is_bot(),
            p.accepts_gzip(),
        ];
        // Header names/order are transport representation, not content.
        // Preserve duplicate-field value order while canonicalizing names.
        let raw = public_request_identity(&normalized, origin);
        let mut lines = raw.split("\r\n");
        let first = lines.next().unwrap_or_default();
        let mut fields: Vec<(String, &str)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value))
            .collect();
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        let mut canonical = format!("{first}\r\n");
        for (name, value) in fields {
            canonical.push_str(&name);
            canonical.push(':');
            canonical.push_str(value);
            canonical.push_str("\r\n");
        }
        format!(
            "public-browser-v2:{:?}:{:?}\n{}",
            p.device_type(),
            caps,
            canonical
        )
    }

    /// Bumps a counter, creating it when absent.
    fn count(&self, name: &str, delta: i64) {
        self.driver.server_context().variable(name).add(delta);
    }
}

/// Public cache mode treats a plain browser refresh as public navigation.
/// Any additional directive (especially no-cache/no-store) keeps origin work.
fn public_refresh_directive(value: &str) -> bool {
    value.split(',').all(|directive| {
        directive.split_once('=').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("max-age") && value.trim() == "0"
        })
    })
}

fn origin_varies_on(origin: Option<&ResponseHeaders>, name: &str) -> bool {
    origin.is_some_and(|origin| {
        origin.headers().lookup("Vary").is_some_and(|values| {
            values.iter().any(|value| {
                value
                    .split(',')
                    .any(|field| field.trim().eq_ignore_ascii_case(name))
            })
        })
    })
}

/// Navigation provenance and scheduling do not define public content unless
/// the origin explicitly declares otherwise. All other variant inputs remain.
fn public_request_identity(request: &RequestHeaders, origin: Option<&ResponseHeaders>) -> String {
    let mut normalized = request.clone();
    for name in ["Referer", "Sec-Fetch-Site", "Priority"] {
        let varies = origin_varies_on(origin, name);
        if !varies {
            normalized.headers_mut().remove_all(name);
        }
    }
    normalized.to_http_string_without_cookies()
}

/// Whether an origin response varies on everything (`Vary: *`):
/// any `Vary` line carrying a `*` token vetoes caching (named
/// fields are safe — explicitly named headers remain variant inputs —
/// but `*` names no keyable dimension). Token-based
/// rather than C++'s whole-value match, so technically-invalid
/// `Vary: <fields>, *` combinations fail closed too.
fn origin_varies_star(origin: &ResponseHeaders) -> bool {
    origin.headers().lookup("Vary").is_some_and(|values| {
        values
            .iter()
            .any(|value| value.split(',').any(|token| token.trim() == "*"))
    })
}

/// Strict request gates (`Authorization`, request `Vary`): any
/// presence bypasses, naming the header.
fn strict_request_headers_ok(headers: &[(&str, &str)]) -> Result<(), &'static str> {
    for (name, _) in headers {
        if name.eq_ignore_ascii_case("Authorization") {
            return Err("Authorization");
        }
        if name.eq_ignore_ascii_case("Vary") {
            return Err("Vary");
        }
    }
    Ok(())
}

/// Cookie gate verdict plus the allowlisted-cookie count (shared by
/// lookup and the publication re-check).
fn cookie_verdict(
    options: &ps_config::options::RewriteOptions,
    headers: &[(&str, &str)],
) -> Result<usize, HtmlCacheBypassReason> {
    use ps_config::cookies::{CookieBypass, CookieDecision};
    match ps_config::cookies::evaluate_cookie_headers(headers, &options.html_cache_cookie_policy())
    {
        CookieDecision::Absent => Ok(0),
        CookieDecision::Allowlisted { count } => Ok(count),
        CookieDecision::Bypass(CookieBypass::Cookie2) => {
            Err(HtmlCacheBypassReason::ForbiddenHeader("Cookie2"))
        }
        CookieDecision::Bypass(CookieBypass::Malformed) => {
            Err(HtmlCacheBypassReason::MalformedCookie)
        }
        CookieDecision::Bypass(CookieBypass::Cookie(name)) => {
            Err(HtmlCacheBypassReason::Cookie(name))
        }
    }
}

/// Full request verdict for lookup and publication (strict gates
/// first, then the cookie policy).
fn request_eligible(
    options: &ps_config::options::RewriteOptions,
    headers: &[(&str, &str)],
) -> Result<usize, HtmlCacheBypassReason> {
    strict_request_headers_ok(headers).map_err(HtmlCacheBypassReason::ForbiddenHeader)?;
    cookie_verdict(options, headers)
}

/// History line for cookie-attributed bypasses (`PS_HTML_BYPASS`,
/// cookie names only, values never): `None` for non-cookie reasons
/// (no new noise) and overlong URLs.
fn bypass_history_line(reason: &HtmlCacheBypassReason, url: &str, now_ms: i64) -> Option<String> {
    let token = match reason {
        HtmlCacheBypassReason::Cookie(name) => format!("cookie:{}", sanitize_reason_name(name)),
        HtmlCacheBypassReason::MalformedCookie => "malformed-cookie".to_string(),
        HtmlCacheBypassReason::ForbiddenHeader("Cookie2") => "header:Cookie2".to_string(),
        _ => return None,
    };
    if url.len() > 4096 {
        return None;
    }
    Some(format!(
        "PS_HTML_BYPASS\t{now_ms}\t{token}\t{}",
        ps_cache::base64::web64_encode(url.as_bytes()),
    ))
}

/// Sanitizes a cookie name for history lines (control characters,
/// whitespace, and TSV separators become `?`).
fn sanitize_reason_name(name: &str) -> String {
    name.chars()
        .map(|char| {
            if char.is_ascii_graphic() && char != '\t' {
                char
            } else {
                '?'
            }
        })
        .collect()
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
const SAMPLED_BEACON_MARKERS: &[&[u8]] = &[b"pagespeed.CriticalImages.Run(", b"CriticalCssBeacon"];

/// Whether a body carries sampled beacon initialization.
fn contains_sampled_beacon_init(body: &[u8]) -> bool {
    SAMPLED_BEACON_MARKERS
        .iter()
        .any(|marker| body.windows(marker.len()).any(|window| window == *marker))
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
                Arc::new(MockUrlFetcher::new()),
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
    fn font_confirmed_set_versions_cache_key() {
        let harness = Harness::new();
        harness
            .options
            .borrow_mut()
            .filters_mut()
            .enable(Filter::PreloadCriticalFonts);
        let session = harness.session();
        let origin = harness.ok_origin();
        let request = harness.get_request();
        let options = harness.options.borrow().clone();
        let before = session
            .eligibility_key(&origin, &request, &options, None, START_MS)
            .expect("eligible")
            .key;
        // Learning a confirmation rekeys the variant.
        let server = harness.driver.server_context();
        let oh = server
            .hasher()
            .hash(harness.driver.options_signature().as_bytes());
        let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
            harness.driver.request_properties().device_type(),
        );
        let mut confirmed = std::collections::HashSet::new();
        confirmed.insert((
            "manrope".to_string(),
            400,
            crate::font_discovery::FontStyle::Normal,
        ));
        server.metadata_cache().put(
            &crate::font_store::confirmed_key(PAGE_URL, &oh, &suffix),
            crate::font_store::encode_confirmed(&confirmed).as_bytes(),
        );
        let after = session
            .eligibility_key(&origin, &request, &options, None, START_MS)
            .expect("eligible")
            .key;
        assert_ne!(before, after);
        // Disabled filter: confirmations do not perturb keys.
        let mut plain = harness.options.borrow().clone();
        plain.filters_mut().disable(Filter::PreloadCriticalFonts);
        let unkeyed = session
            .eligibility_key(&origin, &request, &plain, None, START_MS)
            .expect("eligible")
            .key;
        server.metadata_cache().put(
            &crate::font_store::confirmed_key(PAGE_URL, &oh, &suffix),
            b"font-confirmed/1\n1\nmanrope\t700\tn\n",
        );
        let unkeyed_after = session
            .eligibility_key(&origin, &request, &plain, None, START_MS)
            .expect("eligible")
            .key;
        assert_eq!(unkeyed, unkeyed_after);
    }

    #[test]
    fn public_browser_grouping_defaults_on_and_preserves_variants() {
        let h = Harness::new();
        let firefox =
            "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0";
        let chrome = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36";
        let make = |ua: &str, accept: &str| {
            let mut r = h.get_request();
            r.headers_mut().add("User-Agent", ua);
            r.headers_mut().replace("Accept", accept);
            r
        };
        let mut f = make(
            firefox,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        );
        f.headers_mut().add("Sec-GPC", "1");
        f.headers_mut().add("Connection", "keep-alive");
        f.headers_mut().add("TE", "trailers");
        let mut c = make(chrome, "text/html,image/webp,image/avif,*/*;q=0.8");
        c.headers_mut().add("Cache-Control", "max-age=0");
        c.headers_mut().add("Sec-CH-UA", "Chromium");
        let identity = |r: &RequestHeaders| {
            h.driver
                .set_user_agent(&r.headers().lookup1("User-Agent").unwrap());
            h.driver.set_request_headers(r.clone());
            h.session().request_identity(r, Some(&h.ok_origin()))
        };
        assert!(h.options.borrow().html_cache_public_browser_grouping());
        assert_eq!(identity(&f), identity(&c));
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCachePublicBrowserGrouping",
                "off",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        assert_ne!(identity(&f), identity(&c));
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCachePublicBrowserGrouping",
                "on",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        assert_eq!(identity(&f), identity(&c));
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCacheEarlyLookup",
                "on",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let mut origin = h.ok_origin();
        origin.headers_mut().add("Vary", "User-Agent, Accept");
        identity(&f);
        let mut first = h.session();
        assert!(first
            .lookup(&origin, &f, &h.purge, None, START_MS)
            .is_none());
        first.write(b"<html>public browser group</html>");
        first.finish(&f, START_MS);
        identity(&c);
        assert!(h
            .session()
            .lookup_early(&c, &h.purge, None, START_MS + 1)
            .is_some());
        assert!(h
            .session()
            .lookup(&origin, &c, &h.purge, None, START_MS + 1)
            .is_some());

        // Identical browser generations share even when raw UA/Accept differ.
        let c2 = make(
            &chrome.replace("158.0.0.0", "159.0.0.0"),
            "text/html,image/webp,image/avif",
        );
        assert_eq!(identity(&c), identity(&c2));
        let mut language = c.clone();
        language.headers_mut().add("Accept-Language", "fr");
        assert_ne!(identity(&c), identity(&language));
        let mut no_avif = c.clone();
        no_avif
            .headers_mut()
            .replace("Accept", "text/html,image/webp,image/avif;q=0");
        assert_ne!(identity(&c), identity(&no_avif));
        let mobile = make("Mozilla/5.0 (Linux; Android 13; Mobile) AppleWebKit/537.36 Chrome/158.0.0.0 Mobile Safari/537.36", "text/html,image/avif,image/webp");
        assert_ne!(identity(&c), identity(&mobile));
    }

    #[test]
    fn public_browser_headers_share_unless_origin_varies() {
        for (name, value) in [
            ("Sec-GPC", "1"),
            ("DNT", "1"),
            ("Connection", "keep-alive"),
            ("Keep-Alive", "timeout=5"),
            ("TE", "trailers"),
            ("Cache-Control", "max-age=0"),
        ] {
            for varies in [false, true] {
                let h = Harness::new();
                h.options
                    .borrow_mut()
                    .set_option(
                        "HtmlCacheEarlyLookup",
                        "on",
                        ps_config::scope::Scope::Server,
                    )
                    .unwrap();
                let base = h.get_request();
                let mut other = base.clone();
                other.headers_mut().add(name, value);
                let mut origin = h.ok_origin();
                if varies {
                    origin.headers_mut().add("Vary", name);
                }
                let mut first = h.session();
                assert!(first
                    .lookup(&origin, &base, &h.purge, None, START_MS)
                    .is_none());
                first.write(b"<html>public</html>");
                first.finish(&base, START_MS);
                assert_eq!(
                    h.session()
                        .lookup_early(&other, &h.purge, None, START_MS + 1)
                        .is_some(),
                    !varies,
                    "early {name} Vary={varies}"
                );
                assert_eq!(
                    h.session()
                        .lookup(&origin, &other, &h.purge, None, START_MS + 1)
                        .is_some(),
                    !varies,
                    "normal {name} Vary={varies}"
                );
                assert_eq!(
                    other.headers().lookup1(name).as_deref(),
                    Some(value),
                    "original headers preserved"
                );
            }
        }
    }

    #[test]
    fn public_refresh_keeps_revalidation_controls_and_exact_mode() {
        let h = Harness::new();
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCacheEarlyLookup",
                "on",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let base = h.get_request();
        for value in ["max-age=0", "MAX-AGE = 0", "max-age=0, max-age=0"] {
            let mut refresh = base.clone();
            refresh.headers_mut().add("Cache-Control", value);
            assert_eq!(
                h.session().early_key(&base, None),
                h.session().early_key(&refresh, None)
            );
        }
        for value in [
            "no-cache",
            "no-store",
            "max-age=0, no-cache",
            "max-age=0, no-store",
            "max-age=1",
            "",
            "max-age=0,",
        ] {
            let mut refresh = base.clone();
            refresh.headers_mut().add("Cache-Control", value);
            assert!(h.session().early_key(&refresh, None).is_none(), "{value}");
        }
        let mut refresh = base.clone();
        refresh.headers_mut().add("Pragma", "no-cache");
        assert!(h.session().early_key(&refresh, None).is_none());
        refresh.headers_mut().remove_all("Pragma");
        refresh.headers_mut().add("Cache-Control", "max-age=0");
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCachePublicBrowserGrouping",
                "off",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        assert!(h.session().early_key(&refresh, None).is_none());
        assert_ne!(
            h.session().request_identity(&base, None),
            h.session().request_identity(&refresh, None)
        );
    }

    #[test]
    fn public_navigation_shares_hits_but_explicit_vary_separates() {
        for vary in [
            None,
            Some("Referer"),
            Some("Sec-Fetch-Site"),
            Some("Priority"),
        ] {
            let h = Harness::new();
            h.options
                .borrow_mut()
                .set_option(
                    "HtmlCacheEarlyLookup",
                    "on",
                    ps_config::scope::Scope::Server,
                )
                .unwrap();
            let mut direct = h.get_request();
            direct.headers_mut().add("Sec-Fetch-Site", "none");
            direct.headers_mut().add("Priority", "u=0");
            let mut internal = h.get_request();
            internal.headers_mut().add("Sec-Fetch-Site", "same-origin");
            internal.headers_mut().add("Priority", "u=1");
            internal
                .headers_mut()
                .add("Referer", "http://cache.test/another-page");
            let mut origin = h.ok_origin();
            if let Some(vary) = vary {
                origin.headers_mut().add("Vary", vary);
            }
            let mut render = h.session();
            assert!(render
                .lookup(&origin, &direct, &h.purge, None, START_MS)
                .is_none());
            render.write(b"<html>public</html>");
            render.finish(&direct, START_MS);
            assert!(h
                .session()
                .lookup_early(&direct, &h.purge, None, START_MS + 1)
                .is_some());
            assert_eq!(
                h.session()
                    .lookup_early(&internal, &h.purge, None, START_MS + 1)
                    .is_some(),
                vary.is_none(),
                "{vary:?}"
            );
            assert_eq!(
                h.session()
                    .lookup(&origin, &internal, &h.purge, None, START_MS + 1)
                    .is_some(),
                vary.is_none(),
                "{vary:?}"
            );
            let mut language = direct.clone();
            language.headers_mut().add("Accept-Language", "fr");
            assert!(h
                .session()
                .lookup_early(&language, &h.purge, None, START_MS + 1)
                .is_none());
        }
    }

    #[test]
    fn early_lookup_uses_configured_cookie_policy() {
        let h = Harness::new();
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCacheEarlyLookup",
                "on",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = h.get_request();
        let origin = h.ok_origin();
        let approved = cookie_request(&h, "_ga=first; _gid=second");
        assert!(h
            .session()
            .lookup_early(&approved, &h.purge, None, START_MS)
            .is_none());
        // A render with approved cookies publishes the same public variant.
        let mut render = h.session();
        assert!(render
            .lookup(&origin, &approved, &h.purge, None, START_MS)
            .is_none());
        render.write(b"<html>public</html>");
        render.finish(&approved, START_MS);
        for r in [
            request.clone(),
            approved,
            cookie_request(&h, "_ga=different; _ga=another"),
        ] {
            assert_eq!(
                h.session()
                    .lookup_early(&r, &h.purge, None, START_MS + 1)
                    .unwrap()
                    .0
                    .body,
                b"<html>public</html>"
            );
        }
        assert_eq!(h.counter("html_cache_cookie_allowlisted"), 3);
        for value in [
            "wordpress_logged_in_x=secret",
            "_ga=x; session=secret",
            "unknown=x",
            "_ga",
            "_GA=x",
        ] {
            let r = cookie_request(&h, value);
            assert!(h.session().early_key(&r, None).is_none(), "{value}");
            assert!(h
                .session()
                .lookup_early(&r, &h.purge, None, START_MS + 1)
                .is_none());
        }
        let mut cookie2 = request.clone();
        cookie2.headers_mut().add("Cookie2", "_ga=x");
        assert!(h.session().early_key(&cookie2, None).is_none());
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCacheIgnoreCommonCookies",
                "off",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        assert!(h
            .session()
            .early_key(&cookie_request(&h, "_ga=x"), None)
            .is_none());
        h.options
            .borrow_mut()
            .parse_and_set_1(
                "HtmlCacheIgnoreCookie",
                "tracker",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        assert!(h
            .session()
            .early_key(&cookie_request(&h, "tracker=x"), None)
            .is_some());
        h.options
            .borrow_mut()
            .parse_and_set_1(
                "HtmlCacheBypassCookie",
                "tracker",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        assert!(h
            .session()
            .early_key(&cookie_request(&h, "tracker=x"), None)
            .is_none());
    }

    #[test]
    fn preference_identity_is_opaque_and_tracking_values_are_ignored() {
        let h = Harness::new();
        let first = cookie_request(
            &h,
            "tk_qs=one; wp_lang=private-language-value; trx_addons_is_retina=1",
        );
        let other = cookie_request(
            &h,
            "trx_addons_is_retina=1; tk_qs=another; wp_lang=private-language-value",
        );
        let key = h.session().request_identity(&first, None);
        assert!(!key.contains("private-language-value"));
        assert!(!key.contains("tk_qs"));
        assert_eq!(key, h.session().request_identity(&other, None));
        assert_ne!(
            key,
            h.session().request_identity(
                &cookie_request(&h, "wp_lang=another-language; trx_addons_is_retina=1"),
                None
            )
        );
    }

    #[test]
    fn early_lookup_requires_publication_and_preserves_variants() {
        let h = Harness::new();
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCacheEarlyLookup",
                "on",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = h.get_request();
        let origin = h.ok_origin();
        let mtime = Some((12, 34));
        assert!(h
            .session()
            .lookup_early(&request, &h.purge, mtime, START_MS)
            .is_none());
        let mut first = h.session();
        assert!(first
            .lookup(&origin, &request, &h.purge, mtime, START_MS)
            .is_none());
        first.write(b"<html>cached</html>");
        assert!(h
            .session()
            .lookup_early(&request, &h.purge, mtime, START_MS)
            .is_none());
        first.finish(&request, START_MS);
        let (entry, replay) = h
            .session()
            .lookup_early(&request, &h.purge, mtime, START_MS + 1)
            .unwrap();
        assert_eq!(entry.body, b"<html>cached</html>");
        assert_eq!(
            replay.headers().lookup1("Content-Type").as_deref(),
            Some("text/html")
        );
        assert!(h
            .session()
            .lookup_early(&request, &h.purge, Some((13, 34)), START_MS + 1)
            .is_none());
        assert!(h
            .session()
            .lookup_early(&request, &h.purge, mtime, START_MS + 60001)
            .is_none());
        for header in [
            "Cookie",
            "Authorization",
            "Range",
            "If-None-Match",
            "Cache-Control",
            "Pragma",
        ] {
            let mut variant = request.clone();
            variant.headers_mut().add(header, "x");
            assert!(
                h.session()
                    .lookup_early(&variant, &h.purge, mtime, START_MS + 1)
                    .is_none(),
                "{header}"
            );
        }
        let mut variant = request.clone();
        variant.headers_mut().add("Accept-Language", "fr");
        assert!(h
            .session()
            .lookup_early(&variant, &h.purge, mtime, START_MS + 1)
            .is_none());
        let mut post = request.clone();
        post.set_method(Method::Post);
        assert!(h
            .session()
            .lookup_early(&post, &h.purge, mtime, START_MS + 1)
            .is_none());
        let mut purge = PurgeSet::new(25_000);
        purge.put(PAGE_URL, START_MS + 1);
        assert!(h
            .session()
            .lookup_early(&request, &purge, mtime, START_MS + 2)
            .is_none());
        h.options
            .borrow_mut()
            .set_option("HtmlCacheTtlSec", "30", ps_config::scope::Scope::Server)
            .unwrap();
        assert!(h
            .session()
            .lookup_early(&request, &h.purge, mtime, START_MS + 1)
            .is_none());
    }

    #[test]
    fn corrupt_html_miss_republishes_early_lookup_policy() {
        let h = Harness::new();
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCacheEarlyLookup",
                "on",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = h.get_request();
        let origin = h.ok_origin();
        let mut first = h.session();
        assert!(first
            .lookup(&origin, &request, &h.purge, None, START_MS)
            .is_none());
        let SessionState::Armed { key, .. } = &first.state else {
            panic!("MISS must arm");
        };
        let key = key.clone();
        first.write(b"old HTML");
        first.finish(&request, START_MS);
        h.driver
            .server_context()
            .metadata_cache()
            .put(&key, b"corrupt HTML entry");
        assert!(h
            .session()
            .lookup_early(&request, &h.purge, None, START_MS + 1)
            .is_none());
        let mut repair = h.session();
        assert!(repair
            .lookup(&origin, &request, &h.purge, None, START_MS + 1)
            .is_none());
        repair.write(b"repaired HTML");
        repair.finish(&request, START_MS + 1);
        let (entry, _) = h
            .session()
            .lookup_early(&request, &h.purge, None, START_MS + 2)
            .expect("invalid cached HTML must still republish its early policy after repair");
        assert_eq!(entry.body, b"repaired HTML");
    }

    #[test]
    fn early_lookup_rejects_corrupt_hint_and_private_origin() {
        let h = Harness::new();
        h.options
            .borrow_mut()
            .set_option(
                "HtmlCacheEarlyLookup",
                "on",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = h.get_request();
        let key = h.session().early_key(&request, None).unwrap();
        h.driver
            .server_context()
            .metadata_cache()
            .put(&key, b"garbage");
        assert!(h
            .session()
            .lookup_early(&request, &h.purge, None, START_MS)
            .is_none());
        let mut origin = h.ok_origin();
        origin.headers_mut().add("Set-Cookie", "login=secret");
        let mut session = h.session();
        session.lookup(&origin, &request, &h.purge, None, START_MS);
        session.write(b"private");
        session.finish(&request, START_MS);
        assert!(h
            .session()
            .lookup_early(&request, &h.purge, None, START_MS)
            .is_none());
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
        let finish = first.finish(&request, START_MS + 1000);
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
                "vary-star",
                Box::new(|_, _, origin| {
                    origin.headers_mut().add("Vary", "*");
                    origin.compute_caching();
                }),
            ),
            (
                "vary-star-embedded",
                Box::new(|_, _, origin| {
                    origin.headers_mut().add("Vary", "Accept-Encoding, *");
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
        let finish = big.finish(&request, START_MS + 1000);
        assert!(finish.detail_line.is_none());
        assert_eq!(0, harness.counter("html_cache_inserts"));
        // Sampled beacon initializations never freeze.
        for marker in ["pagespeed.CriticalImages.Run('", "CriticalCssBeacon"] {
            let mut sampled = harness.session();
            assert!(sampled
                .lookup(&origin, &request, &harness.purge, None, START_MS)
                .is_none());
            sampled.write(b"<html><script>");
            sampled.write(marker.as_bytes());
            sampled.write(b"</script></html>");
            let finish = sampled.finish(&request, START_MS + 1000);
            assert!(finish.detail_line.is_none(), "{marker}");
        }
        assert_eq!(0, harness.counter("html_cache_inserts"));
    }

    #[test]
    fn pending_optimization_bypass_reports_reason_without_changing_gate() {
        let h = Harness::new();
        h.options
            .borrow_mut()
            .set_option("ShowCacheTimestamp", "on", ps_config::scope::Scope::Server)
            .unwrap();
        let request = h.get_request();
        let mut render = h.session();
        assert!(render
            .lookup(&h.ok_origin(), &request, &h.purge, None, START_MS)
            .is_none());
        render.write(b"<html>original URLs</html>");
        h.driver.note_detached_rewrite();
        let result = render.finish(&request, START_MS + 1);
        let comment = String::from_utf8(result.show_comment.unwrap()).unwrap();
        assert!(comment.contains("PageSpeed Cache: BYPASS"));
        assert!(comment.contains("PageSpeed Cache Detail: resource-optimization-pending"));
        assert_eq!(0, h.counter("html_cache_inserts"));
    }

    #[test]
    fn identity_splits_user_agents_and_purges() {
        let harness = Harness::new();
        harness
            .options
            .borrow_mut()
            .set_option(
                "HtmlCachePublicBrowserGrouping",
                "off",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = harness.get_request();
        let origin = harness.ok_origin();
        let mut first = harness.session();
        assert!(first
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        first.write(b"<html/>");
        first.finish(&request, START_MS + 1000);
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
        let finish = first.finish(&request, START_MS + 1000);
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
        finder::update_candidates(&mut evidence, &[hash.clone()].into_iter().collect(), false);
        let report = finder::BeaconReport {
            html_hashes: Some([hash].into_iter().collect()),
            lcp_kind: finder::LCP_KIND_IMG.to_string(),
            lcp_image_url: String::new(),
            nonce,
            font_families: Vec::new(),
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
        assert!(!finder::beaconing_active(&Harness::new().driver.options()));
        let harness = Harness::new();
        harness
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::PrioritizeLcpImages);
        assert!(finder::beaconing_active(&harness.driver.options()));
        let request = harness.get_request();
        let origin = harness.ok_origin();
        // Pre-learning variants are eligible: miss, store under the
        // unlearned marker, and hit — first evidence rekeys them.
        let mut cold = harness.session();
        assert!(cold
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(0, harness.counter("html_cache_bypasses"));
        assert_eq!(1, harness.counter("html_cache_misses"));
        assert_eq!(1, harness.counter(finder::NOT_FOUND_COUNT));
        cold.write(b"<html/>");
        settle_driver_identity(&harness);
        let cold_finish = cold.finish(&request, START_MS + 500);
        assert!(cold_finish.detail_line.is_some());
        let mut cold_hit = harness.session();
        assert!(cold_hit
            .lookup(&origin, &request, &harness.purge, None, START_MS + 600)
            .is_some());
        assert_eq!(1, harness.counter("html_cache_hits"));
        // A learned, settled variant misses, stores, and hits.
        seed_learned_beacon(&harness);
        let mut first = harness.session();
        assert!(first
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(2, harness.counter("html_cache_misses"));
        assert_eq!(1, harness.counter(finder::VALID_COUNT));
        first.write(b"<html/>");
        settle_driver_identity(&harness);
        let finish = first.finish(&request, START_MS + 1000);
        assert!(finish.detail_line.is_some());
        assert_eq!(2, harness.counter("html_cache_inserts"));
        let mut second = harness.session();
        assert!(second
            .lookup(&origin, &request, &harness.purge, None, START_MS + 2000)
            .is_some());
        assert_eq!(2, harness.counter("html_cache_hits"));
        // A new accepted beacon invalidates the variant (miss).
        seed_learned_beacon(&harness);
        let mut third = harness.session();
        assert!(third
            .lookup(&origin, &request, &harness.purge, None, START_MS + 3000)
            .is_none());
        assert_eq!(3, harness.counter("html_cache_misses"));
    }

    #[test]
    fn sampling_due_and_unsettled_renders_bypass() {
        // Sampling-due variants bypass even with learned evidence
        // (beacon-capable client, so the sample is productive).
        let harness = Harness::new();
        harness.driver.set_user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
        );
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
        crate::beacon_finder::BeaconFinder::new(server.metadata_cache())
            .store(PAGE_URL, &oh, "@Desktop", &evidence);
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
        let request = settled.get_request();
        let mut render = settled.session();
        assert!(render
            .lookup(
                &settled.ok_origin(),
                &request,
                &settled.purge,
                None,
                START_MS
            )
            .is_none());
        render.write(b"<html/>");
        // No driver snapshot (or a divergent one): skip the store.
        let finish = render.finish(&request, START_MS + 1000);
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

    /// Request with the given `Cookie` header value added.
    fn cookie_request(harness: &Harness, cookies: &str) -> RequestHeaders {
        let mut request = harness.get_request();
        request.headers_mut().add("Cookie", cookies);
        request
    }

    #[test]
    fn allowlisted_cookies_miss_store_and_hit() {
        let harness = Harness::new();
        let origin = harness.ok_origin();
        // Analytics-only requests cache like cookie-free ones.
        let request = cookie_request(&harness, "_ga=GA1.1.1; _gid=2; __utma=3");
        let mut first = harness.session();
        assert!(first
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(1, harness.counter("html_cache_misses"));
        assert_eq!(1, harness.counter("html_cache_cookie_allowlisted"));
        first.write(b"<html/>");
        let finish = first.finish(&request, START_MS + 1000);
        assert!(finish.detail_line.is_some());
        assert!(finish.bypass_line.is_none());
        assert_eq!(1, harness.counter("html_cache_inserts"));
        // New per-client values hit the same key (values ignored).
        let rotated = cookie_request(&harness, "_ga=GA1.9.9; _gid=8; __utma=7");
        let mut second = harness.session();
        assert!(second
            .lookup(&origin, &rotated, &harness.purge, None, START_MS + 2000)
            .is_some());
        assert_eq!(1, harness.counter("html_cache_hits"));
        assert_eq!(2, harness.counter("html_cache_cookie_allowlisted"));
        // And the cookie-free variant shares the key too.
        let mut third = harness.session();
        assert!(third
            .lookup(
                &origin,
                &harness.get_request(),
                &harness.purge,
                None,
                START_MS + 3000
            )
            .is_some());
        assert_eq!(2, harness.counter("html_cache_hits"));
        assert_eq!(0, harness.counter("html_cache_bypasses"));
    }

    #[test]
    fn unknown_and_mixed_cookies_bypass_with_named_reason() {
        let harness = Harness::new();
        let origin = harness.ok_origin();
        for (cookies, expected) in [
            ("sessionid=abc", "sessionid"),
            ("_ga=1; cart=full; _gid=2", "cart"),
            ("PHPSESSID=x; _fbp=y", "PHPSESSID"),
        ] {
            let request = cookie_request(&harness, cookies);
            let mut session = harness.session();
            assert!(session
                .lookup(&origin, &request, &harness.purge, None, START_MS)
                .is_none());
            assert_eq!(
                session.bypass_reason(),
                Some(&HtmlCacheBypassReason::Cookie(expected.to_string())),
                "{cookies}"
            );
            let finish = session.finish(&request, START_MS + 1000);
            assert!(finish.detail_line.is_none());
            let line = finish.bypass_line.expect("bypass line");
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields[0], "PS_HTML_BYPASS", "{line}");
            // The reason names the cookie; values never leak in
            // (the URL field decodes back exactly).
            assert_eq!(4, fields.len(), "{line}");
            assert_eq!(fields[2], format!("cookie:{expected}"), "{line}");
            let url = ps_cache::base64::web64_decode(fields[3]).expect("url b64");
            assert_eq!(PAGE_URL.as_bytes(), url.as_slice());
        }
        assert_eq!(3, harness.counter("html_cache_bypasses"));
        assert_eq!(0, harness.counter("html_cache_cookie_allowlisted"));
    }

    #[test]
    fn malformed_and_cookie2_bypass_with_lines() {
        let harness = Harness::new();
        let origin = harness.ok_origin();
        // Malformed headers fail closed with a valueless line.
        let request = cookie_request(&harness, "_ga=1; oops");
        let mut session = harness.session();
        assert!(session
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(
            session.bypass_reason(),
            Some(&HtmlCacheBypassReason::MalformedCookie)
        );
        let finish = session.finish(&request, START_MS + 1000);
        let line = finish.bypass_line.expect("bypass line");
        assert!(line.contains("malformed-cookie"), "{line}");
        // Obsolete Cookie2 bypasses with a header line.
        let mut cookie2 = harness.get_request();
        cookie2.headers_mut().add("Cookie2", "$Version=1");
        let mut second = harness.session();
        assert!(second
            .lookup(&origin, &cookie2, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(
            second.bypass_reason(),
            Some(&HtmlCacheBypassReason::ForbiddenHeader("Cookie2"))
        );
        let finish = second.finish(&cookie2, START_MS + 1000);
        let line = finish.bypass_line.expect("bypass line");
        assert!(line.contains("header:Cookie2"), "{line}");
        assert_eq!(2, harness.counter("html_cache_bypasses"));
    }

    #[test]
    fn auth_and_generic_bypasses_stay_quiet() {
        let harness = Harness::new();
        let origin = harness.ok_origin();
        // Authorization still bypasses (existing protection), with
        // no new history noise.
        let mut authed = harness.get_request();
        authed.headers_mut().add("Authorization", "Bearer x");
        let mut session = harness.session();
        assert!(session
            .lookup(&origin, &authed, &harness.purge, None, START_MS)
            .is_none());
        assert_eq!(
            session.bypass_reason(),
            Some(&HtmlCacheBypassReason::ForbiddenHeader("Authorization"))
        );
        let finish = session.finish(&authed, START_MS + 1000);
        assert!(finish.bypass_line.is_none());
        // Non-cookie gates attribute `Other`, also without lines
        // (named Vary is eligible; only `Vary: *` bypasses).
        let mut varied = harness.ok_origin();
        varied.headers_mut().add("Vary", "*");
        varied.compute_caching();
        let mut second = harness.session();
        assert!(second
            .lookup(
                &varied,
                &harness.get_request(),
                &harness.purge,
                None,
                START_MS
            )
            .is_none());
        assert_eq!(second.bypass_reason(), Some(&HtmlCacheBypassReason::Other));
        let finish = second.finish(&harness.get_request(), START_MS + 1000);
        assert!(finish.bypass_line.is_none());
    }

    #[test]
    fn cookie_configuration_overrides() {
        // `off` restores strict bypassing for built-in names.
        let strict = Harness::new();
        strict
            .options
            .borrow_mut()
            .set_option(
                "HtmlCacheIgnoreCommonCookies",
                "off",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = cookie_request(&strict, "_ga=1");
        let mut session = strict.session();
        assert!(session
            .lookup(&strict.ok_origin(), &request, &strict.purge, None, START_MS)
            .is_none());
        assert_eq!(
            session.bypass_reason(),
            Some(&HtmlCacheBypassReason::Cookie("_ga".to_string()))
        );
        // Custom ignores extend the built-in list …
        let custom = Harness::new();
        custom
            .options
            .borrow_mut()
            .parse_and_set_1(
                "HtmlCacheIgnoreCookie",
                "tracker",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = cookie_request(&custom, "_ga=1; tracker=2");
        let mut session = custom.session();
        assert!(session
            .lookup(&custom.ok_origin(), &request, &custom.purge, None, START_MS)
            .is_none());
        assert!(session.bypass_reason().is_none());
        // … even with the common list disabled …
        custom
            .options
            .borrow_mut()
            .set_option(
                "HtmlCacheIgnoreCommonCookies",
                "off",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = cookie_request(&custom, "tracker=1");
        let mut session = custom.session();
        assert!(session
            .lookup(&custom.ok_origin(), &request, &custom.purge, None, START_MS)
            .is_none());
        assert!(session.bypass_reason().is_none());
        // … while bypass rules win over both allowlists.
        custom
            .options
            .borrow_mut()
            .set_option(
                "HtmlCacheIgnoreCommonCookies",
                "on",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        custom
            .options
            .borrow_mut()
            .parse_and_set_1(
                "HtmlCacheBypassCookie",
                "_ga",
                ps_config::scope::Scope::Server,
            )
            .unwrap();
        let request = cookie_request(&custom, "_ga=1");
        let mut session = custom.session();
        assert!(session
            .lookup(&custom.ok_origin(), &request, &custom.purge, None, START_MS)
            .is_none());
        assert_eq!(
            session.bypass_reason(),
            Some(&HtmlCacheBypassReason::Cookie("_ga".to_string()))
        );
    }

    #[test]
    fn publication_rechecks_request_cookies() {
        let harness = Harness::new();
        let origin = harness.ok_origin();
        let request = cookie_request(&harness, "_ga=1");
        let mut session = harness.session();
        assert!(session
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        session.write(b"<html/>");
        // Finishing against a poisoned request skips the store.
        let poisoned = cookie_request(&harness, "_ga=1; sessionid=x");
        let finish = session.finish(&poisoned, START_MS + 1000);
        assert!(finish.detail_line.is_none());
        assert_eq!(0, harness.counter("html_cache_inserts"));
    }

    #[test]
    fn bypass_reasons_sanitize_and_truncate() {
        // Control characters and separators cannot break the TSV line.
        let line = bypass_history_line(
            &HtmlCacheBypassReason::Cookie("a\tb\nc d".to_string()),
            PAGE_URL,
            START_MS,
        )
        .expect("line");
        assert_eq!(4, line.split('\t').count(), "{line}");
        assert!(line.contains("cookie:a?b?c?d"), "{line}");
        // Overlong URLs suppress the line like detail lines.
        let long_url = format!("http://x/{}", "y".repeat(4096));
        assert!(bypass_history_line(
            &HtmlCacheBypassReason::Cookie("a".to_string()),
            &long_url,
            START_MS
        )
        .is_none());
        // Non-cookie reasons stay quiet.
        for reason in [
            HtmlCacheBypassReason::Other,
            HtmlCacheBypassReason::ForbiddenHeader("Authorization"),
            HtmlCacheBypassReason::ForbiddenHeader("Vary"),
        ] {
            assert!(bypass_history_line(&reason, PAGE_URL, START_MS).is_none());
        }
    }
}

//! In-place recording (`InPlaceResourceRecorder` core).
//!
//! Ports the recording decisions plus value buffering as a
//! backend-driven core: content-coding gating, the size shortcut,
//! content-type gating, origin caching-header capture and
//! application, lifetime/TTL clamping, empty-body and truncation
//! rules, and the saved-`Cache-Control` lifecycle. Cache writes,
//! failure records, statistics, and log lines execute through the
//! injected backend (Slice 6 provides the live cache-backed one);
//! message texts and decision order are verbatim.

use crate::caching::{read_lifetime_directive, LifetimeName};
use ps_http::content_type::ContentType;
use ps_http::inflate::{GzipInflater, InflateType};
use ps_http::names::header;
use ps_http::response::{RequestProperties, ResponseHeaders, ValidatorOption, VaryOption};

/// Not-in-cache marker status
/// (`CacheUrlAsyncFetcher::kNotInCacheStatus`, 501).
pub const NOT_IN_CACHE_STATUS: i32 = 501;

/// Cache-poisoning failure kinds (`FetchResponseStatus` subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingFailure {
    /// Other error.
    OtherError,
    /// 4xx error.
    Error4xx,
    /// Uncacheable 200.
    Uncacheable200,
    /// Uncacheable error.
    UncacheableError,
    /// Empty body.
    Empty,
}

/// Recorder statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderStat {
    /// Recordings started.
    Resources,
    /// Inserted into cache.
    InsertedIntoCache,
    /// Not cacheable.
    NotCacheable,
    /// Failed.
    Failed,
    /// Dropped due to load.
    DroppedDueToLoad,
    /// Dropped due to size.
    DroppedDueToSize,
}

/// Cache, statistics, and logging backend (Slice 6 provides the
/// live cache-backed one; tests use fakes).
pub trait RecordingBackend: Send {
    /// Records a cache-poisoning failure.
    fn remember_failure(&mut self, url: &str, fragment: &str, kind: RecordingFailure);
    /// Inserts the recorded value.
    fn put(
        &mut self,
        url: &str,
        fragment: &str,
        properties: RequestProperties,
        headers: &ResponseHeaders,
        body: &[u8],
    );
    /// Bumps a recorder statistic.
    fn note_stat(&mut self, stat: RecorderStat);
    /// Logs an informational message.
    fn log_info(&mut self, text: String);
}

/// Header completeness (`HeadersKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadersKind {
    /// Preliminary headers (coding check only).
    Preliminary,
    /// Full final headers.
    Full,
}

/// Live recordings in this process (`active_recordings_`).
static ACTIVE_RECORDINGS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Current live-recording count (for tests).
#[cfg(test)]
fn active_recordings_count() -> i32 {
    ACTIVE_RECORDINGS.load(std::sync::atomic::Ordering::SeqCst)
}

/// In-place resource recorder (`InPlaceResourceRecorder` core).
pub struct InPlaceRecorder {
    url: String,
    fragment: String,
    request_properties: RequestProperties,
    respect_vary: bool,
    max_response_bytes: i64,
    max_concurrent_recordings: i32,
    value: Vec<u8>,
    inflater: Option<GzipInflater>,
    status_code: i32,
    failure: bool,
    full_considered: bool,
    consider_called: bool,
    cache_control: Option<String>,
    origin_caching_headers: Option<ResponseHeaders>,
    backend: Option<Box<dyn RecordingBackend + Send>>,
}

impl Drop for InPlaceRecorder {
    fn drop(&mut self) {
        if self.limit_active_recordings() {
            ACTIVE_RECORDINGS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

impl InPlaceRecorder {
    /// Starts a recording (ctor): counts the resource, refuses
    /// when too many recordings run concurrently (still holding a
    /// count slot until destruction), and resolves the size cap
    /// against `cache_max_content_length` (-1 for unlimited).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        url: &str,
        fragment: &str,
        request_properties: RequestProperties,
        respect_vary: bool,
        max_response_bytes: i64,
        max_concurrent_recordings: i32,
        cache_max_content_length: i64,
        mut backend: Option<Box<dyn RecordingBackend + Send>>,
    ) -> Self {
        if let Some(backend) = backend.as_mut() {
            backend.note_stat(RecorderStat::Resources);
        }
        let mut failure = false;
        if max_concurrent_recordings != 0 {
            let active = ACTIVE_RECORDINGS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if active > max_concurrent_recordings {
                if let Some(backend) = backend.as_mut() {
                    backend.note_stat(RecorderStat::DroppedDueToLoad);
                }
                failure = true;
            }
        }
        let mut max_response_bytes = max_response_bytes;
        if cache_max_content_length != -1 {
            if max_response_bytes <= 0 {
                max_response_bytes = cache_max_content_length;
            } else {
                max_response_bytes = max_response_bytes.min(cache_max_content_length);
            }
        }
        InPlaceRecorder {
            url: url.to_string(),
            fragment: fragment.to_string(),
            request_properties,
            respect_vary,
            max_response_bytes,
            max_concurrent_recordings,
            value: Vec::new(),
            inflater: None,
            status_code: -1,
            failure,
            full_considered: false,
            consider_called: false,
            cache_control: None,
            origin_caching_headers: None,
            backend,
        }
    }

    /// Whether recordings are concurrency-limited.
    #[must_use]
    pub fn limit_active_recordings(&self) -> bool {
        self.max_concurrent_recordings != 0
    }

    /// Whether recording failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failure
    }

    /// Marks failure (`Fail`).
    pub fn fail(&mut self) {
        self.failure = true;
    }

    /// Recorded body bytes so far.
    #[must_use]
    pub fn contents(&self) -> &[u8] {
        &self.value
    }

    /// Whether the named coding inflates (`CanDecodeContentEncoding`
    /// over raw header values): no coding at all, or exactly one
    /// gzip/deflate (empty and `identity` values ignored).
    #[must_use]
    pub fn can_decode_content_encoding(headers: &ResponseHeaders) -> bool {
        let mut num_codings = 0;
        for (name, value) in headers.headers().iter() {
            if !name.eq_ignore_ascii_case(header::CONTENT_ENCODING) {
                continue;
            }
            if value.is_empty() || value.eq_ignore_ascii_case("identity") {
                continue;
            }
            if !value.eq_ignore_ascii_case("gzip") && !value.eq_ignore_ascii_case("deflate") {
                return false;
            }
            num_codings += 1;
        }
        num_codings <= 1
    }

    /// Considers response headers (`ConsiderResponseHeaders`).
    pub fn consider_response_headers(
        &mut self,
        kind: HeadersKind,
        headers: &mut ResponseHeaders,
    ) {
        debug_assert!(
            !self.full_considered,
            "full headers considered twice"
        );
        if !self.consider_called {
            self.consider_called = true;
            // A body in a coding the inflater cannot undo must not
            // be recorded at all (the stored bytes would drop their
            // Content-Encoding and lie about being identity). Not
            // remembered: the coding is negotiated per request.
            if !Self::can_decode_content_encoding(headers) {
                self.log_info(format!(
                    "IPRO: not recording {}: unsupported Content-Encoding",
                    self.url
                ));
                self.failure = true;
                return;
            }
            self.arm_inflater(headers);
        }

        self.status_code = headers.status_code();

        if self.origin_caching_headers.is_none() {
            Self::set_origin_caching_headers(headers, &mut self.origin_caching_headers);
        }

        // Shortcut for bailing out early when the response will be
        // too large. NOTE the inverted gate, verbatim from C++: it
        // fires when the cap is UNLIMITED (`<= 0`) and a length is
        // stated, dropping exactly the responses an unlimited cap
        // should keep (suspected upstream bug: `<=` for `>`).
        if self.max_response_bytes <= 0 {
            if let Some(content_length) = headers.find_content_length() {
                if content_length > self.max_response_bytes {
                    self.dropped_due_to_size();
                    return;
                }
            }
        }

        if headers.headers().has(header::CONTENT_TYPE) || kind == HeadersKind::Full {
            let recordable = headers
                .determine_content_type()
                .is_some_and(|content_type: ContentType| {
                    content_type.is_image() || content_type.is_css() || content_type.is_js_like()
                });
            if !recordable {
                if kind == HeadersKind::Full {
                    self.dropped_as_uncacheable();
                } else {
                    self.failure = true;
                }
                return;
            }
        }

        if kind != HeadersKind::Full {
            return;
        }
        self.full_considered = true;

        if (400..=599).contains(&headers.status_code()) {
            let failure_kind = if headers.status_code() < 500 {
                RecordingFailure::Error4xx
            } else {
                RecordingFailure::OtherError
            };
            self.remember_failure(failure_kind);
            self.failure = true;
            return;
        }

        if self.status_code != 200 {
            self.failure = true;
            return;
        }

        headers.compute_caching();
        let vary = if self.respect_vary {
            VaryOption::RespectVaryOnResources
        } else {
            VaryOption::IgnoreVaryOnResources
        };
        let server_allows_storing = self.origin_caching_headers.is_none()
            || headers.is_proxy_cacheable(self.request_properties, vary, ValidatorOption::NoValidator);
        Self::apply_origin_caching_headers(
            self.origin_caching_headers.as_ref(),
            self.cache_control.clone(),
            headers,
        );
        let is_cacheable = server_allows_storing
            && headers.is_proxy_cacheable(self.request_properties, vary, ValidatorOption::NoValidator);
        if !is_cacheable {
            self.dropped_as_uncacheable();
            self.note_stat(RecorderStat::NotCacheable);
        }
    }

    /// Arms the decoder from preliminary headers (single gzip or
    /// deflate coding).
    fn arm_inflater(&mut self, headers: &ResponseHeaders) {
        for (name, value) in headers.headers().iter() {
            if !name.eq_ignore_ascii_case(header::CONTENT_ENCODING) {
                continue;
            }
            if value.is_empty() || value.eq_ignore_ascii_case("identity") {
                continue;
            }
            let inflate_type = if value.eq_ignore_ascii_case("gzip") {
                InflateType::Gzip
            } else {
                debug_assert!(value.eq_ignore_ascii_case("deflate"));
                InflateType::Deflate
            };
            let mut inflater = GzipInflater::new(inflate_type);
            if inflater.init() {
                self.inflater = Some(inflater);
            }
            return;
        }
    }

    /// Writes body bytes (`Write`): inflates when armed, drops past
    /// the cap.
    pub fn write(&mut self, contents: &[u8]) -> bool {
        debug_assert!(self.consider_called);
        if self.failure {
            return false;
        }
        if let Some(inflater) = self.inflater.as_mut() {
            if inflater.error() {
                self.failure = true;
                return false;
            }
            if !inflater.set_input(contents) {
                self.failure = true;
                return false;
            }
            let mut buf = [0u8; crate::send::STACK_BUFFER_SIZE];
            while inflater.has_unconsumed_input() {
                let n = inflater.inflate_bytes(&mut buf);
                if inflater.error() || n < 0 {
                    self.failure = true;
                    return false;
                }
                if n > 0 {
                    self.value.extend_from_slice(&buf[..n as usize]);
                }
            }
        } else {
            self.value.extend_from_slice(contents);
        }
        if self.max_response_bytes <= 0
            || (self.value.len() as i64) < self.max_response_bytes
        {
            return !self.failure;
        }
        self.dropped_due_to_size();
        false
    }

    /// Flushes (`Flush`): a no-op (the whole value buffers before
    /// the cache write).
    pub fn flush(&mut self) -> bool {
        true
    }

    /// Saves the pre-smaxage `Cache-Control` (`SaveCacheControl`):
    /// `None` (absent) vs `Some("")` (present-but-empty) stay
    /// distinct.
    pub fn save_cache_control(&mut self, cache_control: Option<&str>) {
        self.cache_control = cache_control.map(str::to_string);
    }

    /// Finishes recording (`DoneAndSetHeaders`): truncated bodies
    /// fail (unless already refused), full headers are considered,
    /// empty 200s poisoned, and survivors fixed up (encoding sniff,
    /// length drop, saved `Cache-Control`, origin application)
    /// before the cache insert. The caller drops the recorder
    /// afterwards (C++ deletes itself here).
    pub fn done_and_set_headers(
        &mut self,
        response_headers: &mut ResponseHeaders,
        entire_response_received: bool,
    ) {
        if !entire_response_received && !self.failure {
            self.fail();
        }
        if !self.failure && !self.full_considered {
            self.consider_response_headers(HeadersKind::Full, response_headers);
        }
        if self.status_code == 200 && self.value.is_empty() {
            if !self.failure {
                self.remember_failure(RecordingFailure::Empty);
            }
            self.failure = true;
        }
        if self.failure {
            self.note_stat(RecorderStat::Failed);
            return;
        }
        if !has_gzip_magic_bytes(&self.value) {
            response_headers.headers_mut().remove_all(header::CONTENT_ENCODING);
        }
        response_headers.headers_mut().remove_all(header::CONTENT_LENGTH);
        if let Some(cache_control) = self.cache_control.clone() {
            response_headers.headers_mut().remove_all(header::CACHE_CONTROL);
            if !cache_control.is_empty() {
                response_headers.headers_mut().add(header::CACHE_CONTROL, &cache_control);
            }
        }
        Self::apply_origin_caching_headers(
            self.origin_caching_headers.as_ref(),
            self.cache_control.clone(),
            response_headers,
        );
        if let Some(backend) = self.backend.as_mut() {
            backend.put(
                &self.url,
                &self.fragment,
                self.request_properties,
                response_headers,
                &self.value,
            );
            backend.note_stat(RecorderStat::InsertedIntoCache);
        }
    }

    /// Drops for size, poisoning the cache entry.
    fn dropped_due_to_size(&mut self) {
        self.note_stat(RecorderStat::DroppedDueToSize);
        self.dropped_as_uncacheable();
    }

    /// Drops as uncacheable, poisoning once.
    fn dropped_as_uncacheable(&mut self) {
        if !self.failure {
            let kind = if self.status_code == 200 {
                RecordingFailure::Uncacheable200
            } else {
                RecordingFailure::UncacheableError
            };
            self.remember_failure(kind);
            self.failure = true;
        }
    }

    /// Captures origin caching headers (`SetOriginCachingHeaders`):
    /// kept only when some line states caching at all.
    fn set_origin_caching_headers(
        origin: &ResponseHeaders,
        slot: &mut Option<ResponseHeaders>,
    ) {
        let mut copy = ResponseHeaders::new(origin.http_options());
        let mut states_caching = false;
        for (name, value) in origin.headers().iter() {
            if is_origin_caching_header(name) {
                copy.headers_mut().add(name, value);
                states_caching = states_caching
                    || name.eq_ignore_ascii_case(header::CACHE_CONTROL)
                    || name.eq_ignore_ascii_case(header::EXPIRES)
                    || name.eq_ignore_ascii_case(header::PRAGMA);
            }
        }
        if states_caching {
            *slot = Some(copy);
        }
    }

    /// Reapplies origin caching headers
    /// (`ApplyOriginCachingHeaders`): restores the origin lines
    /// (unioning `Vary` tokens), then clamps a shared-cache entry
    /// to the server TTL via `s-maxage`.
    fn apply_origin_caching_headers(
        origin: Option<&ResponseHeaders>,
        saved_cache_control: Option<String>,
        headers: &mut ResponseHeaders,
    ) -> bool {
        let Some(origin) = origin else {
            return false;
        };
        let mut server_ttl_ms = -1;
        let mut server = headers.clone();
        if let Some(saved) = saved_cache_control {
            server.headers_mut().remove_all(header::CACHE_CONTROL);
            if !saved.is_empty() {
                server.headers_mut().add(header::CACHE_CONTROL, &saved);
            }
        }
        if states_cache_lifetime(&server) {
            server.compute_caching();
            server_ttl_ms = server.cache_ttl_ms();
        }
        let final_vary = vary_tokens(headers);
        for name in [
            header::CACHE_CONTROL,
            header::EXPIRES,
            header::PRAGMA,
            header::SET_COOKIE,
            header::SET_COOKIE2,
            header::VARY,
        ] {
            if !name.eq_ignore_ascii_case(header::VARY) {
                headers.headers_mut().remove_all(name);
            }
        }
        let origin_vary = vary_tokens(origin);
        for (name, value) in origin.headers().iter() {
            if is_origin_caching_header(name) && !name.eq_ignore_ascii_case(header::VARY) {
                headers.headers_mut().add(name, value);
            }
        }
        for token in origin_vary {
            if !final_vary.contains(&token) {
                headers.headers_mut().add(header::VARY, &token);
            }
        }
        headers.compute_caching();
        if server_ttl_ms >= 0 && server_ttl_ms < headers.cache_ttl_ms() {
            headers.set_s_max_age(server_ttl_ms / 1000);
            headers.compute_caching();
        }
        true
    }

    /// Records a failure through the backend, if any.
    fn remember_failure(&mut self, kind: RecordingFailure) {
        if let Some(backend) = self.backend.as_mut() {
            backend.remember_failure(&self.url, &self.fragment, kind);
        }
    }

    /// Bumps a statistic through the backend, if any.
    fn note_stat(&mut self, stat: RecorderStat) {
        if let Some(backend) = self.backend.as_mut() {
            backend.note_stat(stat);
        }
    }

    /// Logs through the backend, if any.
    fn log_info(&mut self, text: String) {
        if let Some(backend) = self.backend.as_mut() {
            backend.log_info(text);
        }
    }
}

/// Whether a name is an origin caching header (case-insensitive).
fn is_origin_caching_header(name: &str) -> bool {
    name.eq_ignore_ascii_case(header::CACHE_CONTROL)
        || name.eq_ignore_ascii_case(header::EXPIRES)
        || name.eq_ignore_ascii_case(header::PRAGMA)
        || name.eq_ignore_ascii_case(header::SET_COOKIE)
        || name.eq_ignore_ascii_case(header::SET_COOKIE2)
        || name.eq_ignore_ascii_case(header::VARY)
}

/// Lowercased `Vary` tokens (`VaryTokens`, sorted like the C++
/// `StringSet` so re-emitted order is deterministic).
fn vary_tokens(headers: &ResponseHeaders) -> std::collections::BTreeSet<String> {
    let mut tokens = std::collections::BTreeSet::new();
    for (name, value) in headers.headers().iter() {
        if !name.eq_ignore_ascii_case(header::VARY) {
            continue;
        }
        for part in value.split(',').filter(|piece| !piece.is_empty()) {
            let token = part.trim().to_ascii_lowercase();
            if !token.is_empty() {
                tokens.insert(token);
            }
        }
    }
    tokens
}

/// Whether headers state a cache lifetime (`StatesCacheLifetime`):
/// any `Expires`, or any lifetime directive (malformed ones count).
fn states_cache_lifetime(headers: &ResponseHeaders) -> bool {
    if headers.headers().has(header::EXPIRES) {
        return true;
    }
    // Raw values (no comma pre-split): each line splits here.
    for (name, value) in headers.headers().iter() {
        if !name.eq_ignore_ascii_case(header::CACHE_CONTROL) {
            continue;
        }
        for directive in value.split(',').filter(|piece| !piece.is_empty()) {
            if read_lifetime_directive(directive.trim()).name != LifetimeName::None {
                return true;
            }
        }
    }
    false
}

/// Whether bytes start with the gzip magic (`HasGzipMagicBytes`).
fn has_gzip_magic_bytes(contents: &[u8]) -> bool {
    contents.len() >= 2 && contents[0] == 0x1f && contents[1] == 0x8b
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_http::response::HttpOptions;

    /// Backend transcript, shared with the moved-in double.
    #[derive(Default)]
    struct Transcript {
        failures: Vec<(String, String, RecordingFailure)>,
        puts: Vec<(String, Vec<u8>)>,
        stats: Vec<RecorderStat>,
        logs: Vec<String>,
    }

    /// Recording backend double.
    #[derive(Clone, Default)]
    struct FakeBackend {
        transcript: std::sync::Arc<std::sync::Mutex<Transcript>>,
    }

    impl RecordingBackend for FakeBackend {
        fn remember_failure(&mut self, url: &str, fragment: &str, kind: RecordingFailure) {
            self.transcript.lock().unwrap().failures.push((url.to_string(), fragment.to_string(), kind));
        }

        fn put(
            &mut self,
            url: &str,
            _fragment: &str,
            _properties: RequestProperties,
            _headers: &ResponseHeaders,
            body: &[u8],
        ) {
            self.transcript.lock().unwrap().puts.push((url.to_string(), body.to_vec()));
        }

        fn note_stat(&mut self, stat: RecorderStat) {
            self.transcript.lock().unwrap().stats.push(stat);
        }

        fn log_info(&mut self, text: String) {
            self.transcript.lock().unwrap().logs.push(text);
        }
    }

    fn properties() -> RequestProperties {
        RequestProperties { has_cookie: false, has_cookie2: false, has_authorization: false }
    }

    fn responses() -> ResponseHeaders {
        ResponseHeaders::new(HttpOptions::default_for_tests())
    }

    /// Cacheable CSS headers (nginx always sets `Date`).
    fn cacheable_css() -> ResponseHeaders {
        let mut headers = responses();
        headers.set_status_code(200);
        headers.headers_mut().add(header::CONTENT_TYPE, "text/css");
        headers.headers_mut().add(header::CACHE_CONTROL, "max-age=60");
        headers.headers_mut().add(header::DATE, "Thu, 01 Jan 1970 00:00:00 GMT");
        headers
    }

    fn recorder_with(
        max_bytes: i64,
        max_concurrent: i32,
        backend: Option<FakeBackend>,
    ) -> InPlaceRecorder {
        InPlaceRecorder::new(
            "http://h/a.css",
            "",
            properties(),
            false,
            max_bytes,
            max_concurrent,
            -1,
            backend.map(|backend| Box::new(backend) as Box<dyn RecordingBackend + Send>),
        )
    }

    #[test]
    fn construction_limits() {
        assert_eq!(0, active_recordings_count());
        let backend = FakeBackend::default();
        let transcript = backend.transcript.clone();
        {
            let _first = recorder_with(1024, 1, Some(backend));
            assert_eq!(1, active_recordings_count());
            // A second concurrent recording refuses (but still counts
            // until destroyed).
            let backend = FakeBackend::default();
            let second = recorder_with(1024, 1, Some(backend));
            assert!(second.failed());
            assert_eq!(2, active_recordings_count());
        }
        assert_eq!(0, active_recordings_count());
        assert!(transcript.lock().unwrap().stats.contains(&RecorderStat::Resources));
        // Unlimited concurrency never counts.
        let _open = recorder_with(1024, 0, None);
        assert_eq!(0, active_recordings_count());
    }

    #[test]
    fn preliminary_gating() {
        // Unsupported codings fail with a log, unremembered.
        let backend = FakeBackend::default();
        let transcript = backend.transcript.clone();
        let mut recorder = recorder_with(1024, 0, Some(backend));
        let mut headers = cacheable_css();
        headers.headers_mut().add(header::CONTENT_ENCODING, "br");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.failed());
        assert_eq!(1, transcript.lock().unwrap().logs.len());
        assert!(transcript.lock().unwrap().failures.is_empty());
        // Stacked codings fail the same way.
        let mut recorder = recorder_with(1024, 0, None);
        let mut headers = cacheable_css();
        headers.headers_mut().add(header::CONTENT_ENCODING, "gzip, br");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.failed());
        // Uninteresting content types fail quietly (no poisoning).
        let mut recorder = recorder_with(1024, 0, None);
        let mut headers = cacheable_css();
        headers.headers_mut().remove_all(header::CONTENT_TYPE);
        headers.headers_mut().add(header::CONTENT_TYPE, "text/html");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.failed());
        // Clean CSS proceeds.
        let mut recorder = recorder_with(1024, 0, None);
        let mut headers = cacheable_css();
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(!recorder.failed());
    }

    #[test]
    fn full_gating() {
        // Error statuses poison by class.
        for (status, kind) in [(404, RecordingFailure::Error4xx), (500, RecordingFailure::OtherError)] {
            let backend = FakeBackend::default();
            let transcript = backend.transcript.clone();
            let mut recorder = recorder_with(1024, 0, Some(backend));
            let mut headers = cacheable_css();
            headers.set_status_code(status);
            recorder.consider_response_headers(HeadersKind::Full, &mut headers);
            assert!(recorder.failed());
            assert_eq!(vec![("http://h/a.css".to_string(), "".to_string(), kind)], transcript.lock().unwrap().failures);
        }
        // Non-200 non-errors fail without poisoning.
        let mut recorder = recorder_with(1024, 0, None);
        let mut headers = cacheable_css();
        headers.set_status_code(206);
        recorder.consider_response_headers(HeadersKind::Full, &mut headers);
        assert!(recorder.failed());
        // Uncacheable 200s poison as such.
        let backend = FakeBackend::default();
        let transcript = backend.transcript.clone();
        let mut recorder = recorder_with(1024, 0, Some(backend));
        let mut headers = cacheable_css();
        headers.headers_mut().remove_all(header::CACHE_CONTROL);
        headers.headers_mut().add(header::CACHE_CONTROL, "no-store");
        recorder.consider_response_headers(HeadersKind::Full, &mut headers);
        assert!(recorder.failed());
        assert_eq!(
            vec![("http://h/a.css".to_string(), "".to_string(), RecordingFailure::Uncacheable200)],
            transcript.lock().unwrap().failures
        );
        assert!(transcript.lock().unwrap().stats.contains(&RecorderStat::NotCacheable));
        // Cacheable 200s proceed.
        let mut recorder = recorder_with(1024, 0, None);
        let mut headers = cacheable_css();
        recorder.consider_response_headers(HeadersKind::Full, &mut headers);
        assert!(!recorder.failed());
    }

    #[test]
    fn size_rules() {
        // Writes past the cap drop and poison.
        let backend = FakeBackend::default();
        let transcript = backend.transcript.clone();
        let mut recorder = recorder_with(4, 0, Some(backend));
        let mut headers = cacheable_css();
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.write(b"hey"));
        assert!(!recorder.write(b"toolong"));
        assert!(recorder.failed());
        assert!(transcript.lock().unwrap().stats.contains(&RecorderStat::DroppedDueToSize));
        assert_eq!(RecordingFailure::Uncacheable200, transcript.lock().unwrap().failures[0].2);
        // NOTE the inverted shortcut, verbatim from C++: an
        // UNLIMITED cap plus a stated length drops (suspected
        // upstream bug: `<=` for `>`).
        let mut recorder = recorder_with(0, 0, None);
        let mut headers = cacheable_css();
        headers.headers_mut().add(header::CONTENT_LENGTH, "50");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.failed());
        // A limited cap below the length proceeds here (writes enforce).
        let mut recorder = recorder_with(100, 0, None);
        let mut headers = cacheable_css();
        headers.headers_mut().add(header::CONTENT_LENGTH, "50");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(!recorder.failed());
    }

    #[test]
    fn done_flow() {
        // Truncated bodies fail without poisoning.
        let mut recorder = recorder_with(1024, 0, None);
        let mut headers = cacheable_css();
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.write(b"data"));
        recorder.done_and_set_headers(&mut headers, false);
        assert!(recorder.failed());
        // Empty 200s poison as empty.
        let backend = FakeBackend::default();
        let transcript = backend.transcript.clone();
        let mut recorder = recorder_with(1024, 0, Some(backend));
        let mut headers = cacheable_css();
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        recorder.done_and_set_headers(&mut headers, true);
        assert!(recorder.failed());
        assert_eq!(RecordingFailure::Empty, transcript.lock().unwrap().failures[0].2);
        // Complete cacheable bodies insert with fixed-up headers.
        let backend = FakeBackend::default();
        let transcript = backend.transcript.clone();
        let mut recorder = recorder_with(1024, 0, Some(backend));
        let mut headers = cacheable_css();
        headers.headers_mut().add(header::CONTENT_LENGTH, "4");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.write(b"data"));
        recorder.done_and_set_headers(&mut headers, true);
        assert!(!recorder.failed());
        assert_eq!(1, transcript.lock().unwrap().puts.len());
        assert_eq!(b"data".to_vec(), transcript.lock().unwrap().puts[0].1);
        assert!(transcript.lock().unwrap().stats.contains(&RecorderStat::InsertedIntoCache));
        assert!(!headers.headers().has(header::CONTENT_LENGTH));
    }

    #[test]
    fn gunzips_when_armed() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"data").unwrap();
        let compressed = encoder.finish().unwrap();
        let mut recorder = recorder_with(1024, 0, None);
        let mut headers = cacheable_css();
        headers.headers_mut().add(header::CONTENT_ENCODING, "gzip");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.write(&compressed));
        assert_eq!(b"data", recorder.contents());
        // Gunzipped contents drop their encoding header at insert
        // (the stored bytes are identity).
        recorder.done_and_set_headers(&mut headers, true);
        assert!(!recorder.failed());
        assert!(!headers.headers().has(header::CONTENT_ENCODING));
    }

    #[test]
    fn saved_cache_control_lifecycle() {
        // The origin snapshot wins over the saved value at insert;
        // the saved value only steers the server-TTL clamp (here it
        // clamps the shared entry to 7s alongside the origin line).
        let mut recorder = recorder_with(1024, 0, None);
        let mut headers = cacheable_css();
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
        assert!(recorder.write(b"data"));
        recorder.save_cache_control(Some("max-age=7"));
        recorder.done_and_set_headers(&mut headers, true);
        assert!(!recorder.failed());
        let cache_control = headers.headers().lookup_joined(header::CACHE_CONTROL);
        assert!(cache_control.contains("max-age=60"), "{cache_control}");
        assert!(cache_control.contains("s-maxage=7"), "{cache_control}");
    }

    #[test]
    fn server_ttl_clamps_shared_entry() {
        // Origin allows 100s, the served headers allow 1000s, the
        // pre-smaxage original allowed 50s: the stored entry keeps
        // the origin lines plus a clamped s-maxage.
        let mut recorder = recorder_with(1024, 0, None);
        let mut prelim = cacheable_css();
        prelim.headers_mut().remove_all(header::CACHE_CONTROL);
        prelim.headers_mut().add(header::CACHE_CONTROL, "max-age=100");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut prelim);
        assert!(recorder.write(b"data"));
        let mut full = cacheable_css();
        full.headers_mut().remove_all(header::CACHE_CONTROL);
        full.headers_mut().add(header::CACHE_CONTROL, "max-age=1000");
        recorder.save_cache_control(Some("max-age=50"));
        recorder.done_and_set_headers(&mut full, true);
        assert!(!recorder.failed());
        let cache_control = full.headers().lookup_joined(header::CACHE_CONTROL);
        assert!(cache_control.contains("max-age=100"), "{cache_control}");
        assert!(cache_control.contains("s-maxage=50"), "{cache_control}");
        // Without the saved value the server TTL exceeds the final
        // one and no clamp applies.
        let mut recorder = recorder_with(1024, 0, None);
        let mut prelim = cacheable_css();
        prelim.headers_mut().remove_all(header::CACHE_CONTROL);
        prelim.headers_mut().add(header::CACHE_CONTROL, "max-age=100");
        recorder.consider_response_headers(HeadersKind::Preliminary, &mut prelim);
        assert!(recorder.write(b"data"));
        let mut full = cacheable_css();
        full.headers_mut().remove_all(header::CACHE_CONTROL);
        full.headers_mut().add(header::CACHE_CONTROL, "max-age=1000");
        recorder.done_and_set_headers(&mut full, true);
        assert!(!recorder.failed());
        let cache_control = full.headers().lookup_joined(header::CACHE_CONTROL);
        assert!(cache_control.contains("max-age=100"), "{cache_control}");
        assert!(!cache_control.contains("s-maxage"), "{cache_control}");
    }
}

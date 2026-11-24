//! Body delivery to PageSpeed (`ps_send_to_pagespeed`).
//!
//! Ports the buffer walk: input bytes flow to the fetch (inflated
//! when the response was encoded), flush flags mirror when
//! configured, `last_buf` completes the fetch, and every consumed
//! buffer is marked sent. The live sink rewrites HTML through the
//! sync driver into a shared base fetch; the FFI marks nginx
//! buffers sent from the outcome.

use crate::fetch::SharedBaseFetch;
use ps_http::inflate::GzipInflater;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// Stack output buffer (`kStackBufferSize`).
pub const STACK_BUFFER_SIZE: usize = 10000;

/// Fetch sink (`ProxyFetch` write side).
pub trait BodySink {
    /// Accepts body bytes (`Write`).
    fn write(&mut self, bytes: &[u8]);
    /// Flushes pending output (`Flush`).
    fn flush(&mut self);
    /// Completes the fetch (`Done(true)`).
    fn done(&mut self);
}

/// Shared output buffer for the driver writer.
#[derive(Debug, Default, Clone)]
struct VecSink {
    output: Rc<RefCell<Vec<u8>>>,
}

impl ps_html::writer::ByteSink for VecSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> bool {
        self.output.borrow_mut().extend_from_slice(bytes);
        true
    }
}

/// HTML rewrite sink: streams response bytes through the sync
/// driver into a shared base fetch (the live `ProxyFetch` write
/// side, minus origin fetching).
///
/// The rewrite driver (filter chain + parser state) builds lazily on
/// the first body byte, *after* the HTML-cache lookup: cache HITs
/// serve stored bytes and swallow the origin body without ever
/// constructing filters or parsing.
pub struct HtmlSink<'a> {
    #[cfg(feature = "pipeline-profile")]
    timings: [u128; 4],
    driver: Option<ps_html::driver::Driver>,
    /// Deferred driver inputs (consumed by the first write).
    deferred: Option<DeferredDriver>,
    output: Rc<RefCell<Vec<u8>>>,
    fetch: Arc<SharedBaseFetch>,
    slot: &'a Mutex<Option<crate::pipe::PipeChannel>>,
    resource_driver: Option<Rc<ps_html::server_context::ResourceDriver>>,
    html_cache: Option<ps_html::html_cache::HtmlCacheSession>,
    html_cache_line: Option<String>,
    html_cache_request: Option<ps_http::request::RequestHeaders>,
}

/// Owned inputs for a lazily-built rewrite driver.
struct DeferredDriver {
    options: Option<ps_config::options::RewriteOptions>,
    url: String,
    content_is_xml_like: bool,
}

impl<'a> HtmlSink<'a> {
    /// Builds a sink over request options (`ProxyFetch` ctor
    /// shape): beacon results on (system default), property
    /// cache on (metadata-backed beacon cohort).
    /// `resource_driver` instantiates the driver-bound CSS/JS
    /// filters (`None` keeps the minify subsets).
    ///
    /// The filter chain builds on the first body byte (see
    /// [`HtmlSink::html_cache_lookup`]): constructing eagerly here
    /// would charge every HTML-cache HIT for parsing and ~27 filters
    /// it never uses.
    #[must_use]
    pub fn new(
        options: &ps_config::options::RewriteOptions,
        url: &str,
        content_is_xml_like: bool,
        fetch: &Arc<SharedBaseFetch>,
        slot: &'a Mutex<Option<crate::pipe::PipeChannel>>,
        resource_driver: Option<Rc<ps_html::server_context::ResourceDriver>>,
    ) -> Self {
        Self::new_inner(
            Some(options.clone()),
            url,
            content_is_xml_like,
            fetch,
            slot,
            resource_driver,
        )
    }

    /// Uses the request driver's existing options snapshot. Cache HITs never
    /// need a second configuration copy; MISSes borrow it while building the
    /// parser. Callers needing independent options retain `new` semantics.
    #[must_use]
    pub fn new_for_resource_driver(
        url: &str,
        content_is_xml_like: bool,
        fetch: &Arc<SharedBaseFetch>,
        slot: &'a Mutex<Option<crate::pipe::PipeChannel>>,
        resource_driver: Rc<ps_html::server_context::ResourceDriver>,
    ) -> Self {
        Self::new_inner(
            None,
            url,
            content_is_xml_like,
            fetch,
            slot,
            Some(resource_driver),
        )
    }

    fn new_inner(
        options: Option<ps_config::options::RewriteOptions>,
        url: &str,
        content_is_xml_like: bool,
        fetch: &Arc<SharedBaseFetch>,
        slot: &'a Mutex<Option<crate::pipe::PipeChannel>>,
        resource_driver: Option<Rc<ps_html::server_context::ResourceDriver>>,
    ) -> Self {
        let output = Rc::new(RefCell::new(Vec::new()));
        let html_cache = resource_driver
            .clone()
            .map(|driver| ps_html::html_cache::HtmlCacheSession::new(driver, url));
        HtmlSink {
            #[cfg(feature = "pipeline-profile")]
            timings: [0; 4],
            driver: None,
            deferred: Some(DeferredDriver {
                options,
                url: url.to_string(),
                content_is_xml_like,
            }),
            output,
            fetch: Arc::clone(fetch),
            slot,
            resource_driver,
            html_cache,
            html_cache_line: None,
            html_cache_request: None,
        }
    }

    /// Builds the rewrite driver on first body byte (idempotent).
    fn ensure_driver(&mut self) {
        if self.driver.is_some() {
            return;
        }
        let Some(deferred) = self.deferred.take() else {
            return;
        };
        let context = ps_html::chain::ChainContext::new(true, true);
        #[cfg(feature = "pipeline-profile")]
        let profile_start = std::time::Instant::now();
        let build = |options: &ps_config::options::RewriteOptions| {
            ps_html::driver::Driver::new(
                options,
                &context,
                VecSink {
                    output: Rc::clone(&self.output),
                },
                self.resource_driver.clone(),
            )
        };
        let mut driver = if let Some(options) = deferred.options.as_ref() {
            build(options)
        } else {
            let resource_driver = self
                .resource_driver
                .as_ref()
                .expect("driver-backed options");
            build(&resource_driver.options())
        };
        driver.start_parse(deferred.url.as_bytes(), deferred.content_is_xml_like);
        #[cfg(feature = "pipeline-profile")]
        {
            self.timings[0] += profile_start.elapsed().as_nanos();
        }
        self.driver = Some(driver);
    }

    /// Whether the rewrite driver was built (false on pure HITs).
    #[must_use]
    #[cfg(test)]
    pub fn driver_built(&self) -> bool {
        self.driver.is_some()
    }

    /// Moves rewritten bytes into the base fetch.
    fn pump_output(&mut self) {
        let bytes = std::mem::take(&mut *self.output.borrow_mut());
        if !bytes.is_empty() {
            if let Some(session) = self.html_cache.as_mut() {
                session.write(&bytes);
            }
            self.fetch.handle_write(&bytes, self.slot);
        }
    }

    /// Takes accumulated image rewrite records for the admin images
    /// page (drains incrementally; empty without a resource driver).
    #[must_use]
    pub fn take_image_records(&self) -> Vec<ps_html::server_context::ImageRewriteRecord> {
        self.resource_driver
            .as_ref()
            .map(|driver| driver.take_image_records())
            .unwrap_or_default()
    }

    /// Looks up a cached rewrite for origin headers (hit entries
    /// serve instead of the origin body). `file_mtime` carries the
    /// origin file's modification time when the URL maps to one.
    /// The request headers are snapshotted for the publication
    /// re-check at `done`.
    pub fn html_cache_lookup(
        &mut self,
        origin: &ps_http::response::ResponseHeaders,
        request: &ps_http::request::RequestHeaders,
        purge: &ps_cache::purge::PurgeSet,
        file_mtime: Option<(u64, u32)>,
        now_ms: i64,
    ) -> Option<ps_cache::html::HtmlCacheEntry> {
        #[cfg(feature = "pipeline-profile")]
        let profile_start = std::time::Instant::now();
        let out = self
            .html_cache
            .as_mut()
            .and_then(|session| session.lookup(origin, request, purge, file_mtime, now_ms));
        // Only rewriting/publication uses this snapshot. HIT completion does
        // not publish, so do not copy every request header on that path.
        self.html_cache_request = out.is_none().then(|| request.clone());
        #[cfg(feature = "pipeline-profile")]
        {
            self.timings[1] += profile_start.elapsed().as_nanos();
        }
        out
    }

    /// Serves a cache hit through the base fetch, completing it.
    pub fn serve_html_cache_hit(&self, entry: &ps_cache::html::HtmlCacheEntry, now_ms: i64) {
        if let Some(session) = self.html_cache.as_ref() {
            self.fetch.handle_write(&entry.body, self.slot);
            if let Some(comment) = session.hit_comment(entry, now_ms) {
                self.fetch.handle_write(&comment, self.slot);
            }
            let _deleted = self.fetch.handle_done(self.slot);
        }
    }

    /// Takes the pending HTML-cache history line, if any
    /// (publication detail or cookie-bypass line, mutually
    /// exclusive).
    #[must_use]
    pub fn take_html_cache_line(&mut self) -> Option<String> {
        self.html_cache_line.take()
    }

    /// Current time in ms since the epoch.
    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0)
    }
}

#[cfg(feature = "pipeline-profile")]
impl Drop for HtmlSink<'_> {
    fn drop(&mut self) {
        eprintln!(
            "PS_PIPELINE_NS init={} lookup={} processing={} store={} hit={}",
            self.timings[0],
            self.timings[1],
            self.timings[2],
            self.timings[3],
            self.html_cache
                .as_ref()
                .is_some_and(|session| session.is_hit())
        );
    }
}

impl BodySink for HtmlSink<'_> {
    fn write(&mut self, bytes: &[u8]) {
        self.ensure_driver();
        if let Some(driver) = self.driver.as_mut() {
            #[cfg(feature = "pipeline-profile")]
            let profile_start = std::time::Instant::now();
            driver.write(bytes);
            #[cfg(feature = "pipeline-profile")]
            {
                self.timings[2] += profile_start.elapsed().as_nanos();
            }
        }
        self.pump_output();
    }

    fn flush(&mut self) {
        self.ensure_driver();
        if let Some(driver) = self.driver.as_mut() {
            #[cfg(feature = "pipeline-profile")]
            let profile_start = std::time::Instant::now();
            driver.flush();
            #[cfg(feature = "pipeline-profile")]
            {
                self.timings[2] += profile_start.elapsed().as_nanos();
            }
        }
        // Rearm the per-flush fetch-wait budget (C++ rearms the
        // rewrite deadline every flush window).
        if let Some(driver) = self.resource_driver.as_ref() {
            driver.reset_fetch_budget();
        }
        self.pump_output();
        self.fetch.handle_flush(self.slot);
    }

    fn done(&mut self) {
        // Pure HITs never built a driver (already served): finishing
        // one here would construct filters just to discard them.
        let served_hit = self
            .html_cache
            .as_ref()
            .is_some_and(|session| session.is_hit());
        if !served_hit {
            self.ensure_driver();
        }
        if let Some(driver) = self.driver.as_mut() {
            #[cfg(feature = "pipeline-profile")]
            let profile_start = std::time::Instant::now();
            driver.finish();
            #[cfg(feature = "pipeline-profile")]
            {
                self.timings[2] += profile_start.elapsed().as_nanos();
            }
        }
        self.pump_output();
        if let Some(session) = self.html_cache.as_mut() {
            let empty;
            let request = match self.html_cache_request.as_ref() {
                Some(request) => request,
                None => {
                    empty = ps_http::request::RequestHeaders::new();
                    &empty
                }
            };
            #[cfg(feature = "pipeline-profile")]
            let profile_start = std::time::Instant::now();
            let finish = session.finish(request, Self::now_ms());
            #[cfg(feature = "pipeline-profile")]
            {
                self.timings[3] += profile_start.elapsed().as_nanos();
            }
            // Stores and cookie bypasses never coincide.
            self.html_cache_line = finish.detail_line.or(finish.bypass_line);
            if let Some(comment) = finish.show_comment {
                self.fetch.handle_write(&comment, self.slot);
            }
        }
        self.fetch.handle_done(self.slot);
    }
}

/// One input buffer's extracted view.
#[derive(Debug, Clone, Copy)]
pub struct SendBuffer<'a> {
    /// Buffer bytes (`pos..last`).
    pub data: &'a [u8],
    /// Flush flag (`buf->flush`).
    pub flush: bool,
    /// Last-buffer flag (`buf->last_buf`, pre-clear).
    pub last_buf: bool,
}

/// Inflation failure kind (the caller logs limit vs corrupt distinctly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateError {
    /// Output outgrew the cap (`limit_exceeded()`).
    LimitExceeded,
    /// Corrupt stream.
    Corrupt,
}

/// Outcome of a send call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SendOutcome {
    /// Buffers marked sent (always all of them).
    pub buffers_sent: usize,
    /// The fetch completed (`last_buf` seen — even after an
    /// inflation failure, whose body simply stops short).
    pub fetch_done: bool,
    /// First inflation failure, if any.
    pub inflate_error: Option<InflateError>,
}

/// Sends buffers (`ps_send_to_pagespeed` past extraction).
pub fn send_to_pagespeed(
    buffers: &[SendBuffer],
    mut inflater: Option<&mut GzipInflater>,
    follow_flushes: bool,
    sink: &mut dyn BodySink,
) -> SendOutcome {
    let mut outcome = SendOutcome::default();
    let mut failed = false;
    let mut buf = [0u8; STACK_BUFFER_SIZE];
    for buffer in buffers {
        let last_buf = buffer.last_buf;
        if !failed {
            match inflater.as_mut() {
                None => sink.write(buffer.data),
                Some(inflater) => {
                    // SetInput's verdict is ignored like C++: empty
                    // buffers simply have nothing unconsumed.
                    let _ = inflater.set_input(buffer.data);
                    while inflater.has_unconsumed_input() {
                        let n = inflater.inflate_bytes(&mut buf);
                        if n < 0 {
                            failed = true;
                            outcome.inflate_error = Some(if inflater.limit_exceeded() {
                                InflateError::LimitExceeded
                            } else {
                                InflateError::Corrupt
                            });
                            break;
                        }
                        if n > 0 {
                            sink.write(&buf[..n as usize]);
                        }
                    }
                }
            }
        }
        if buffer.flush && follow_flushes {
            sink.flush();
        }
        outcome.buffers_sent += 1;
        if last_buf {
            sink.done();
            outcome.fetch_done = true;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use ps_http::inflate::{InflateType, MAX_INFLATED_HTML_BYTES};
    use std::io::Write;

    /// Recording sink.
    #[derive(Debug, Default)]
    struct FakeSink {
        written: Vec<u8>,
        flushes: usize,
        dones: usize,
    }

    impl BodySink for FakeSink {
        fn write(&mut self, bytes: &[u8]) {
            self.written.extend_from_slice(bytes);
        }

        fn flush(&mut self) {
            self.flushes += 1;
        }

        fn done(&mut self) {
            self.dones += 1;
        }
    }

    fn plain(data: &[u8]) -> SendBuffer<'_> {
        SendBuffer {
            data,
            flush: false,
            last_buf: false,
        }
    }

    #[test]
    fn pass_through_writes_and_completes() {
        let buffers = [
            plain(b"he"),
            SendBuffer {
                data: b"llo",
                flush: true,
                last_buf: true,
            },
        ];
        // Flushes mirror only when configured.
        for follow in [false, true] {
            let mut sink = FakeSink::default();
            let outcome = send_to_pagespeed(&buffers, None, follow, &mut sink);
            assert_eq!(b"hello".to_vec(), sink.written);
            assert_eq!(usize::from(follow), sink.flushes);
            assert_eq!(1, sink.dones);
            assert_eq!(2, outcome.buffers_sent);
            assert!(outcome.fetch_done);
            assert_eq!(None, outcome.inflate_error);
        }
        // Empty buffers still flow (and complete).
        let buffers = [SendBuffer {
            data: b"",
            flush: false,
            last_buf: true,
        }];
        let mut sink = FakeSink::default();
        let outcome = send_to_pagespeed(&buffers, None, false, &mut sink);
        assert!(sink.written.is_empty());
        assert_eq!(1, sink.dones);
        assert_eq!(1, outcome.buffers_sent);
        assert!(outcome.fetch_done);
    }

    fn gzipped(body: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn inflated_delivery() {
        let body = b"<html>hi hi hi</html>";
        let compressed = gzipped(body);
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        inflater.set_max_inflated_bytes(MAX_INFLATED_HTML_BYTES);
        // Split mid-stream: both halves inflate through one decoder.
        let cut = compressed.len() / 2;
        let buffers = [
            SendBuffer {
                data: &compressed[..cut],
                flush: false,
                last_buf: false,
            },
            SendBuffer {
                data: &compressed[cut..],
                flush: false,
                last_buf: true,
            },
        ];
        let mut sink = FakeSink::default();
        let outcome = send_to_pagespeed(&buffers, Some(&mut inflater), false, &mut sink);
        assert_eq!(body.to_vec(), sink.written);
        assert_eq!(1, sink.dones);
        assert_eq!(2, outcome.buffers_sent);
        assert!(outcome.fetch_done);
        assert_eq!(None, outcome.inflate_error);
    }

    #[test]
    fn inflation_failure_drops_the_rest() {
        let body = b"<html>hi hi hi</html>";
        let mut compressed = gzipped(body);
        let cut = compressed.len() / 2;
        compressed[cut] ^= 0xff;
        compressed[cut + 1] ^= 0xff;
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        inflater.set_max_inflated_bytes(MAX_INFLATED_HTML_BYTES);
        let buffers = [
            SendBuffer {
                data: &compressed[..cut],
                flush: false,
                last_buf: false,
            },
            SendBuffer {
                data: &compressed[cut..],
                flush: false,
                last_buf: true,
            },
        ];
        let mut sink = FakeSink::default();
        let outcome = send_to_pagespeed(&buffers, Some(&mut inflater), false, &mut sink);
        assert_eq!(Some(InflateError::Corrupt), outcome.inflate_error);
        // Every buffer still marks sent, and the fetch still completes.
        assert_eq!(2, outcome.buffers_sent);
        assert!(outcome.fetch_done);
        assert_eq!(1, sink.dones);
    }

    #[test]
    fn limit_reports_distinctly() {
        let body = b"<html>hi hi hi</html>";
        let compressed = gzipped(body);
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        inflater.set_max_inflated_bytes(4);
        let buffers = [SendBuffer {
            data: &compressed,
            flush: false,
            last_buf: true,
        }];
        let mut sink = FakeSink::default();
        let outcome = send_to_pagespeed(&buffers, Some(&mut inflater), false, &mut sink);
        assert_eq!(Some(InflateError::LimitExceeded), outcome.inflate_error);
        assert!(sink.written.len() <= 4);
        assert!(outcome.fetch_done);
    }

    #[test]
    fn html_sink_drains_driver_image_records() {
        use crate::fetch::{BaseFetchType, SharedBaseFetch};
        use crate::pipe;
        use ps_config::options::RewriteOptions;
        use ps_http::response::HttpOptions;
        let slot: Mutex<Option<pipe::PipeChannel>> = Mutex::new(None);
        let (ps_side, _ngx_side) = SharedBaseFetch::new_pair(
            "http://h/p",
            std::ptr::null_mut(),
            BaseFetchType::HtmlTransform,
            crate::caching::PreserveCachingHeaders::DontPreserve,
            HttpOptions::default(),
            false,
        );
        let options = RewriteOptions::new();
        // Without a resource driver there is nothing to drain.
        let bare = HtmlSink::new(&options, "http://h/p", false, &ps_side, &slot, None);
        assert!(bare.take_image_records().is_empty());
        // With one, driver records flow through and drain.
        let backend: std::sync::Arc<dyn ps_cache::cache::Cache> =
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new());
        let html = crate::rewrite::html_context(
            backend,
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new()),
            std::sync::Arc::new(ps_stats::core::Statistics::new()),
            0,
            std::sync::Arc::new(crate::rewrite::UnavailableFetcher),
        );
        let driver = std::rc::Rc::new(ps_html::server_context::ResourceDriver::new(
            html,
            std::rc::Rc::new(std::cell::RefCell::new(options.clone())),
            "http://h/p",
        ));
        driver.record_image_rewrite(ps_html::server_context::ImageRewriteRecord {
            timestamp_ms: 1,
            original_bytes: 10,
            optimized_bytes: 4,
            output_mime_type: "image/webp".to_string(),
            original_url: "http://h/a.jpg".to_string(),
            optimized_url: "http://h/a.jpg.pagespeed.ic.h.webp".to_string(),
        });
        let sink = HtmlSink::new(&options, "http://h/p", false, &ps_side, &slot, Some(driver));
        let records = sink.take_image_records();
        assert_eq!(1, records.len());
        assert!(records[0].message_line().starts_with("PS_IMAGE_DETAIL\t"));
        assert!(sink.take_image_records().is_empty());
    }

    #[test]
    fn html_sink_caches_miss_then_serves_hit() {
        use crate::fetch::{BaseFetchType, SharedBaseFetch};
        use crate::pipe;
        use ps_config::options::RewriteOptions;
        use ps_http::request::Method;
        use ps_http::response::HttpOptions;
        // One metadata cache shared by both sinks (like one server).
        let metadata: std::sync::Arc<dyn ps_cache::cache::Cache> =
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new());
        let mut make_sink = |metadata: std::sync::Arc<dyn ps_cache::cache::Cache>| {
            // The slot outlives the sink via a leaked box (test-only).
            let slot: &'static Mutex<Option<pipe::PipeChannel>> =
                Box::leak(Box::new(Mutex::new(None)));
            let (ps_side, _ngx_side) = SharedBaseFetch::new_pair(
                "http://h/p",
                std::ptr::null_mut(),
                BaseFetchType::HtmlTransform,
                crate::caching::PreserveCachingHeaders::DontPreserve,
                HttpOptions::default(),
                false,
            );
            let html = crate::rewrite::html_context(
                std::sync::Arc::new(ps_cache::cache::MemoryCache::new()),
                metadata,
                std::sync::Arc::new(ps_stats::core::Statistics::new()),
                0,
                std::sync::Arc::new(crate::rewrite::UnavailableFetcher),
            );
            let mut options = RewriteOptions::new();
            options
                .set_option("HtmlCache", "on", ps_config::scope::Scope::Server)
                .unwrap();
            options
                .set_option("HtmlCacheTtlSec", "60", ps_config::scope::Scope::Server)
                .unwrap();
            let driver = std::rc::Rc::new(ps_html::server_context::ResourceDriver::new(
                html,
                std::rc::Rc::new(std::cell::RefCell::new(options.clone())),
                "http://h/p",
            ));
            // The sink clones what it needs; locals drop freely.
            let sink =
                HtmlSink::new_for_resource_driver("http://h/p", false, &ps_side, slot, driver);
            (sink, ps_side)
        };
        fn get_request() -> ps_http::request::RequestHeaders {
            let mut request = ps_http::request::RequestHeaders::new();
            request.set_method(Method::Get);
            request.set_version(1, 1);
            request.headers_mut().add("Host", "h");
            request
        }
        fn ok_origin(date_ms: i64) -> ps_http::response::ResponseHeaders {
            let mut origin =
                ps_http::response::ResponseHeaders::new(HttpOptions::default_for_tests());
            origin.set_status_and_reason(200);
            origin.headers_mut().add("Content-Type", "text/html");
            origin
                .headers_mut()
                .add("Date", &ps_http::time::format_http_date(date_ms));
            origin
                .headers_mut()
                .add("Cache-Control", "public, max-age=300");
            origin.compute_caching();
            origin
        }
        // The sink stamps with wall time; keep fixtures coherent.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0);
        let purge = ps_cache::purge::PurgeSet::new(25_000);
        let request = get_request();
        let origin = ok_origin(now_ms);
        // First sink misses, buffers the rewrite, and stores on done.
        let (mut first, _fetch) = make_sink(std::sync::Arc::clone(&metadata));
        assert!(first
            .html_cache_lookup(&origin, &request, &purge, None, now_ms)
            .is_none());
        first.write(b"<html>hi</html>");
        BodySink::done(&mut first);
        let line = first.take_html_cache_line().expect("stored line");
        assert!(line.starts_with("PS_HTML_DETAIL\t"), "{line}");
        // A fresh sink over the same metadata hits and serves.
        // (Later wall time: entries created after `now_ms` are
        // future-dated by construction.)
        let later_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(now_ms)
            .max(now_ms);
        let (mut second, _fetch) = make_sink(std::sync::Arc::clone(&metadata));
        let entry = second
            .html_cache_lookup(&origin, &request, &purge, None, later_ms)
            .expect("hit");
        assert!(!entry.body.is_empty());
        second.serve_html_cache_hit(&entry, later_ms);
    }

    #[test]
    fn html_sink_builds_filters_only_past_lookup() {
        use crate::fetch::{BaseFetchType, SharedBaseFetch};
        use crate::pipe;
        use ps_config::options::RewriteOptions;
        use ps_http::request::Method;
        use ps_http::response::HttpOptions;
        // Regression: HTML-cache HITs must bypass filter construction
        // and parsing entirely; MISSes build the driver on first byte.
        let metadata: std::sync::Arc<dyn ps_cache::cache::Cache> =
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new());
        let make_sink = |metadata: std::sync::Arc<dyn ps_cache::cache::Cache>| {
            let slot: &'static Mutex<Option<pipe::PipeChannel>> =
                Box::leak(Box::new(Mutex::new(None)));
            let (ps_side, _ngx_side) = SharedBaseFetch::new_pair(
                "http://h/p",
                std::ptr::null_mut(),
                BaseFetchType::HtmlTransform,
                crate::caching::PreserveCachingHeaders::DontPreserve,
                HttpOptions::default(),
                false,
            );
            let html = crate::rewrite::html_context(
                std::sync::Arc::new(ps_cache::cache::MemoryCache::new()),
                metadata,
                std::sync::Arc::new(ps_stats::core::Statistics::new()),
                0,
                std::sync::Arc::new(crate::rewrite::UnavailableFetcher),
            );
            let mut options = RewriteOptions::new();
            options.enable_filter(ps_config::filter::Filter::AddInstrumentation);
            options
                .set_option("HtmlCache", "on", ps_config::scope::Scope::Server)
                .unwrap();
            options
                .set_option("HtmlCacheTtlSec", "60", ps_config::scope::Scope::Server)
                .unwrap();
            let driver = std::rc::Rc::new(ps_html::server_context::ResourceDriver::new(
                html,
                std::rc::Rc::new(std::cell::RefCell::new(options.clone())),
                "http://h/p",
            ));
            driver.set_user_agent("Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36");
            HtmlSink::new_for_resource_driver("http://h/p", false, &ps_side, slot, driver)
        };
        let mut request = ps_http::request::RequestHeaders::new();
        request.set_method(Method::Get);
        request.set_version(1, 1);
        request.headers_mut().add("Host", "h");
        let mut origin = ps_http::response::ResponseHeaders::new(HttpOptions::default_for_tests());
        origin.set_status_and_reason(200);
        origin.headers_mut().add("Content-Type", "text/html");
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0);
        origin
            .headers_mut()
            .add("Date", &ps_http::time::format_http_date(now_ms));
        origin
            .headers_mut()
            .add("Cache-Control", "public, max-age=300");
        origin.compute_caching();
        let purge = ps_cache::purge::PurgeSet::new(25_000);
        // Construction alone builds no filters.
        let mut first = make_sink(std::sync::Arc::clone(&metadata));
        assert!(!first.driver_built());
        // Lookup MISS still builds nothing; the first byte does.
        assert!(first
            .html_cache_lookup(&origin, &request, &purge, None, now_ms)
            .is_none());
        assert!(!first.driver_built());
        first.write(b"<html><head></head><body>hi</body></html>");
        assert!(first.driver_built());
        BodySink::done(&mut first);
        assert!(first.take_html_cache_line().is_some());
        // A HIT serve (plus a defensive done) never builds filters.
        // (Later wall time: the entry was created after `now_ms`.)
        let later_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(now_ms)
            .max(now_ms);
        let mut second = make_sink(std::sync::Arc::clone(&metadata));
        let entry = second
            .html_cache_lookup(&origin, &request, &purge, None, later_ms)
            .expect("hit");
        let body = String::from_utf8_lossy(&entry.body);
        assert!(
            body.contains("window.mod_pagespeed_start = Number(new Date())"),
            "{body}"
        );
        assert!(body.contains("pagespeed.addInstrumentationInit("), "{body}");
        assert!(body.contains("/ngx_pagespeed_beacon"), "{body}");
        assert!(!second.driver_built());
        assert!(second.html_cache_request.is_none());
        assert!(second.deferred.as_ref().unwrap().options.is_none());
        second.serve_html_cache_hit(&entry, later_ms);
        // Served: the origin body is swallowed downstream, so no
        // driver, parse, or filter ever runs for this request.
        assert!(!second.driver_built());
    }

    #[test]
    fn html_sink_caches_analytics_cookies_and_names_bypassers() {
        use crate::fetch::{BaseFetchType, SharedBaseFetch};
        use crate::pipe;
        use ps_config::options::RewriteOptions;
        use ps_http::request::Method;
        use ps_http::response::HttpOptions;
        let metadata: std::sync::Arc<dyn ps_cache::cache::Cache> =
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new());
        let make_sink = |metadata: std::sync::Arc<dyn ps_cache::cache::Cache>| {
            let slot: &'static Mutex<Option<pipe::PipeChannel>> =
                Box::leak(Box::new(Mutex::new(None)));
            let (ps_side, _ngx_side) = SharedBaseFetch::new_pair(
                "http://h/c",
                std::ptr::null_mut(),
                BaseFetchType::HtmlTransform,
                crate::caching::PreserveCachingHeaders::DontPreserve,
                HttpOptions::default(),
                false,
            );
            let html = crate::rewrite::html_context(
                std::sync::Arc::new(ps_cache::cache::MemoryCache::new()),
                metadata,
                std::sync::Arc::new(ps_stats::core::Statistics::new()),
                0,
                std::sync::Arc::new(crate::rewrite::UnavailableFetcher),
            );
            let mut options = RewriteOptions::new();
            options
                .set_option("HtmlCache", "on", ps_config::scope::Scope::Server)
                .unwrap();
            options
                .set_option("HtmlCacheTtlSec", "60", ps_config::scope::Scope::Server)
                .unwrap();
            let driver = std::rc::Rc::new(ps_html::server_context::ResourceDriver::new(
                html,
                std::rc::Rc::new(std::cell::RefCell::new(options.clone())),
                "http://h/c",
            ));
            HtmlSink::new_for_resource_driver("http://h/c", false, &ps_side, slot, driver)
        };
        fn cookie_request(cookies: &str) -> ps_http::request::RequestHeaders {
            let mut request = ps_http::request::RequestHeaders::new();
            request.set_method(Method::Get);
            request.set_version(1, 1);
            request.headers_mut().add("Host", "h");
            request.headers_mut().add("Cookie", cookies);
            request
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0);
        let mut origin = ps_http::response::ResponseHeaders::new(HttpOptions::default_for_tests());
        origin.set_status_and_reason(200);
        origin.headers_mut().add("Content-Type", "text/html");
        origin
            .headers_mut()
            .add("Date", &ps_http::time::format_http_date(now_ms));
        origin
            .headers_mut()
            .add("Cache-Control", "public, max-age=300");
        origin.compute_caching();
        let purge = ps_cache::purge::PurgeSet::new(25_000);
        // Analytics-only requests store …
        let request = cookie_request("_ga=GA1.1; _gid=2");
        let mut first = make_sink(std::sync::Arc::clone(&metadata));
        assert!(first
            .html_cache_lookup(&origin, &request, &purge, None, now_ms)
            .is_none());
        first.write(b"<html>hi</html>");
        BodySink::done(&mut first);
        let line = first.take_html_cache_line().expect("stored line");
        assert!(line.starts_with("PS_HTML_DETAIL\t"), "{line}");
        // … and rotated analytics values hit the same entry
        // (fresh wall time: entries created after `now_ms` are
        // future-dated by construction).
        let later_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(now_ms)
            .max(now_ms);
        let rotated = cookie_request("_ga=GA9.9; _gid=8");
        let mut second = make_sink(std::sync::Arc::clone(&metadata));
        let entry = second
            .html_cache_lookup(&origin, &rotated, &purge, None, later_ms)
            .expect("hit");
        assert!(!entry.body.is_empty());
        // Unknown cookies bypass with a named history line.
        let personal = cookie_request("_ga=1; sessionid=secret-value");
        let mut third = make_sink(std::sync::Arc::clone(&metadata));
        assert!(third
            .html_cache_lookup(&origin, &personal, &purge, None, later_ms)
            .is_none());
        third.write(b"<html>hi</html>");
        BodySink::done(&mut third);
        let line = third.take_html_cache_line().expect("bypass line");
        assert!(line.starts_with("PS_HTML_BYPASS\t"), "{line}");
        assert!(line.contains("cookie:sessionid"), "{line}");
        assert!(!line.contains("secret-value"), "{line}");
    }

    #[test]
    fn html_sink_rewrites_into_fetch() {
        use crate::fetch::{BaseFetchType, SharedBaseFetch};
        use crate::pipe;
        use ps_config::options::RewriteOptions;
        use ps_http::response::HttpOptions;
        let slot: Mutex<Option<pipe::PipeChannel>> = Mutex::new(None);
        let fds = pipe::open_pipe().expect("pipe");
        assert!(pipe::make_nonblocking(fds.read_fd));
        assert!(pipe::make_nonblocking(fds.write_fd));
        pipe::install_channel_into(&slot, pipe::PipeChannel::new(fds));
        let (ps_side, ngx_side) = SharedBaseFetch::new_pair(
            "http://h/p",
            std::ptr::null_mut(),
            BaseFetchType::HtmlTransform,
            crate::caching::PreserveCachingHeaders::DontPreserve,
            HttpOptions::default(),
            false,
        );
        let options = RewriteOptions::new();
        let mut sink = HtmlSink::new(&options, "http://h/p", false, &ps_side, &slot, None);
        sink.write(b"<html><head><title>t</title></head><body><p>hi</p></body></html>");
        sink.done();
        // Rewritten bytes landed in the fetch with Done latched.
        let buffered = ps_side.with_state(|fetch| (fetch.buffered_len() > 0, fetch.done_called()));
        assert_eq!(buffered, (true, true));
        // A Done event waits in the pipe.
        let channel = pipe::take_channel_from(&slot).expect("installed");
        let mut kinds = Vec::new();
        let outcome = channel.drain(&mut |event| {
            kinds.push(event.event_type);
            // SAFETY: paired by the posts above; drop after counting.
            drop(unsafe { crate::fetch::reclaim_event_sender(event) });
        });
        assert!(matches!(outcome, pipe::DrainOutcome::Dispatched(_)));
        assert!(kinds.contains(&crate::fetch::event_byte(crate::fetch::FetchEvent::Done)));
        drop(ps_side);
        drop(ngx_side);
        channel.shutdown_write();
        pipe::close_fd(fds.read_fd);
    }
}

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
pub struct HtmlSink<'a> {
    driver: ps_html::driver::Driver,
    output: Rc<RefCell<Vec<u8>>>,
    fetch: Arc<SharedBaseFetch>,
    slot: &'a Mutex<Option<crate::pipe::PipeChannel>>,
}

impl<'a> HtmlSink<'a> {
    /// Builds a sink over request options (`ProxyFetch` ctor
    /// shape): beacon results on (system default), property
    /// cache off (unported). `resource_driver` instantiates the
    /// driver-bound CSS/JS filters (`None` keeps the minify
    /// subsets).
    #[must_use]
    pub fn new(
        options: &ps_config::options::RewriteOptions,
        url: &str,
        content_is_xml_like: bool,
        fetch: &Arc<SharedBaseFetch>,
        slot: &'a Mutex<Option<crate::pipe::PipeChannel>>,
        resource_driver: Option<Rc<ps_html::server_context::ResourceDriver>>,
    ) -> Self {
        let output = Rc::new(RefCell::new(Vec::new()));
        let context = ps_html::chain::ChainContext::new(true, false);
        let mut driver = ps_html::driver::Driver::new(
            options,
            &context,
            VecSink {
                output: Rc::clone(&output),
            },
            resource_driver,
        );
        driver.start_parse(url.as_bytes(), content_is_xml_like);
        HtmlSink {
            driver,
            output,
            fetch: Arc::clone(fetch),
            slot,
        }
    }

    /// Moves rewritten bytes into the base fetch.
    fn pump_output(&self) {
        let bytes = std::mem::take(&mut *self.output.borrow_mut());
        if !bytes.is_empty() {
            self.fetch.handle_write(&bytes, self.slot);
        }
    }
}

impl BodySink for HtmlSink<'_> {
    fn write(&mut self, bytes: &[u8]) {
        self.driver.write(bytes);
        self.pump_output();
    }

    fn flush(&mut self) {
        self.driver.flush();
        self.pump_output();
        self.fetch.handle_flush(self.slot);
    }

    fn done(&mut self) {
        self.driver.finish();
        self.pump_output();
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

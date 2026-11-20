//! Body delivery to PageSpeed (`ps_send_to_pagespeed`).
//!
//! Ports the buffer walk: input bytes flow to the fetch (inflated
//! when the response was encoded), flush flags mirror when
//! configured, `last_buf` completes the fetch, and every consumed
//! buffer is marked sent. The fetch sink (proxy fetch) arrives in
//! Slice 6; the core runs against an injected sink, and the FFI
//! marks nginx buffers sent from the outcome.

use ps_http::inflate::GzipInflater;

/// Stack output buffer (`kStackBufferSize`).
pub const STACK_BUFFER_SIZE: usize = 10000;

/// Fetch sink (`ProxyFetch` write side; Slice 6 provides the live one).
pub trait BodySink {
    /// Accepts body bytes (`Write`).
    fn write(&mut self, bytes: &[u8]);
    /// Flushes pending output (`Flush`).
    fn flush(&mut self);
    /// Completes the fetch (`Done(true)`).
    fn done(&mut self);
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
        SendBuffer { data, flush: false, last_buf: false }
    }

    #[test]
    fn pass_through_writes_and_completes() {
        let buffers =
            [plain(b"he"), SendBuffer { data: b"llo", flush: true, last_buf: true }];
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
        let buffers = [SendBuffer { data: b"", flush: false, last_buf: true }];
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
            SendBuffer { data: &compressed[..cut], flush: false, last_buf: false },
            SendBuffer { data: &compressed[cut..], flush: false, last_buf: true },
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
            SendBuffer { data: &compressed[..cut], flush: false, last_buf: false },
            SendBuffer { data: &compressed[cut..], flush: false, last_buf: true },
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
        let buffers = [SendBuffer { data: &compressed, flush: false, last_buf: true }];
        let mut sink = FakeSink::default();
        let outcome = send_to_pagespeed(&buffers, Some(&mut inflater), false, &mut sink);
        assert_eq!(Some(InflateError::LimitExceeded), outcome.inflate_error);
        assert!(sink.written.len() <= 4);
        assert!(outcome.fetch_done);
    }
}

//! Streaming decompression (`GzipInflater`).
//!
//! Ports the incremental inflate interface over miniz_oxide's raw
//! and zlib inflate plus manual gzip framing (header, CRC32,
//! trailer): gzip and zlib streams, the deflate-to-raw fallback,
//! output limiting, and finished/error accounting, with the same
//! call protocol (`SetInput` past consumption, `InflateBytes`
//! until drained, `-1` on error). The one-shot statics
//! (`Deflate`/`Inflate`/`InflateWithLimit`/`HasGzipMagicBytes`)
//! stay unported: the nginx path never calls them, and the fetch
//! layer already covers one-shot gzip in `fetch.rs`.

use miniz_oxide::inflate::stream::{inflate, InflateState};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

/// Most bytes a compressed HTML response may inflate to
/// (`kMaxInflatedHtmlBytes`, 64 MiB).
pub const MAX_INFLATED_HTML_BYTES: i64 = 64 << 20;

/// Input framing (`InflateType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateType {
    /// gzip framing (RFC 1952).
    Gzip,
    /// zlib framing (RFC 1950), raw deflate on fallback.
    Deflate,
}

/// Working stream format (adds the raw fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamFormat {
    /// gzip framing.
    Gzip,
    /// zlib framing.
    ZlibStream,
    /// Raw deflate (post-fallback).
    RawInflate,
}

/// Gzip header parse outcome.
enum GzipHeader {
    /// Need more input bytes.
    NeedMore,
    /// Header complete.
    Done,
    /// Not a gzip stream.
    Corrupt,
}

/// Streaming inflater (`GzipInflater`).
pub struct GzipInflater {
    format: StreamFormat,
    state: Option<Box<InflateState>>,
    input: Vec<u8>,
    input_offset: usize,
    total_in: u64,
    inflated_bytes: i64,
    max_inflated_bytes: i64,
    limit_exceeded: bool,
    finished: bool,
    error: bool,
    header: Vec<u8>,
    header_done: bool,
    crc: crc32fast::Hasher,
    trailer: Vec<u8>,
    deflate_ended: bool,
}

impl GzipInflater {
    /// Creates an inflater (`GzipInflater` ctor; call `init` next).
    #[must_use]
    pub fn new(format: InflateType) -> Self {
        GzipInflater {
            format: match format {
                InflateType::Gzip => StreamFormat::Gzip,
                InflateType::Deflate => StreamFormat::ZlibStream,
            },
            state: None,
            input: Vec::new(),
            input_offset: 0,
            total_in: 0,
            inflated_bytes: 0,
            max_inflated_bytes: -1,
            limit_exceeded: false,
            finished: false,
            error: false,
            header: Vec::new(),
            header_done: false,
            crc: crc32fast::Hasher::new(),
            trailer: Vec::new(),
            deflate_ended: false,
        }
    }

    /// Initializes the decoder (`Init`); false when already live.
    pub fn init(&mut self) -> bool {
        if self.state.is_some() {
            return false;
        }
        let data_format = match self.format {
            StreamFormat::Gzip | StreamFormat::RawInflate => DataFormat::Raw,
            StreamFormat::ZlibStream => DataFormat::Zlib,
        };
        self.state = Some(InflateState::new_boxed(data_format));
        true
    }

    /// Releases the decoder (`ShutDown`).
    pub fn shutdown(&mut self) {
        self.state = None;
    }

    /// Whether input remains unconsumed (`HasUnconsumedInput`).
    #[must_use]
    pub fn has_unconsumed_input(&self) -> bool {
        if self.state.is_none() || self.finished || self.error {
            return false;
        }
        self.input_offset < self.input.len()
    }

    /// Hands the inflater its next input buffer (`SetInput`):
    /// false unless initialized, drained, unfinished, unerrored,
    /// and non-empty. A zlib stream whose first byte cannot start
    /// one switches to raw deflate up front.
    pub fn set_input(&mut self, input: &[u8]) -> bool {
        if self.state.is_none()
            || self.has_unconsumed_input()
            || self.finished
            || self.error
            || input.is_empty()
        {
            return false;
        }
        if self.format == StreamFormat::ZlibStream
            && self.total_in == 0
            && !is_valid_zlib_stream_header_byte(input[0])
        {
            self.switch_to_raw_deflate();
        }
        self.input = input.to_vec();
        self.input_offset = 0;
        true
    }

    /// Whether inflation completed (`finished()`).
    #[must_use]
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Whether inflation failed (`error()`).
    #[must_use]
    pub fn error(&self) -> bool {
        self.error
    }

    /// Caps lifetime output (`set_max_inflated_bytes`; negative
    /// disables).
    pub fn set_max_inflated_bytes(&mut self, max_inflated_bytes: i64) {
        self.max_inflated_bytes = max_inflated_bytes;
    }

    /// Whether the input outgrew the cap (`limit_exceeded()`).
    #[must_use]
    pub fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }

    /// Inflates into `out` (`InflateBytes`): bytes produced, or -1
    /// on error (corrupt input, exhausted limit, misuse). Call
    /// until `has_unconsumed_input` clears.
    pub fn inflate_bytes(&mut self, out: &mut [u8]) -> i64 {
        if self.state.is_none()
            || !self.has_unconsumed_input()
            || self.finished
            || self.error
            || out.is_empty()
        {
            return -1;
        }
        // With a limit, ask for no more than one byte past it.
        let mut out_size = out.len();
        if self.max_inflated_bytes >= 0 {
            let remaining = (self.max_inflated_bytes - self.inflated_bytes) as u64;
            if remaining + 1 < out_size as u64 {
                out_size = (remaining + 1) as usize;
            }
        }
        let first_inflate = self.total_in == 0;

        // Gzip header phase: consume input until the header parses.
        if self.format == StreamFormat::Gzip && !self.header_done {
            match self.consume_gzip_header() {
                GzipHeader::NeedMore => return 0,
                GzipHeader::Done => {}
                GzipHeader::Corrupt => {
                    self.error = true;
                    return -1;
                }
            }
        }

        // Gzip trailer phase (deflate already ended on an earlier call).
        if self.format == StreamFormat::Gzip && self.deflate_ended && !self.finished {
            self.consume_gzip_trailer();
            return if self.error { -1 } else { 0 };
        }

        let snapshot_offset = self.input_offset;
        let snapshot_total = self.total_in;
        let output = self.inflate_once(&mut out[..out_size]);
        let mut status = output.1;
        let mut produced = output.0;

        // A zlib stream that fails on its first inflate is retried as
        // raw deflate ("deflate" is served both ways in the wild).
        if self.format == StreamFormat::ZlibStream
            && first_inflate
            && status == InflateOutcome::DataError
        {
            self.switch_to_raw_deflate();
            self.input_offset = snapshot_offset;
            self.total_in = snapshot_total;
            let retry = self.inflate_once(&mut out[..out_size]);
            produced = retry.0;
            status = retry.1;
        }

        if self.max_inflated_bytes >= 0
            && self.inflated_bytes + produced as i64 > self.max_inflated_bytes
        {
            self.error = true;
            self.limit_exceeded = true;
            return -1;
        }
        self.inflated_bytes += produced as i64;
        match status {
            InflateOutcome::StreamEnd => {
                if self.format == StreamFormat::Gzip {
                    // The trailer follows in this same buffer (or a
                    // later one); verify it before finishing.
                    self.deflate_ended = true;
                    self.consume_gzip_trailer();
                    if self.error {
                        return -1;
                    }
                } else {
                    self.finished = true;
                }
            }
            InflateOutcome::Ok => {}
            InflateOutcome::NeedMore => {}
            InflateOutcome::DataError => {
                self.error = true;
                return -1;
            }
        }
        produced as i64
    }

    /// Runs one miniz inflate over the pending input.
    fn inflate_once(&mut self, out: &mut [u8]) -> (usize, InflateOutcome) {
        let state = self.state.as_mut().expect("guarded by caller");
        let input = &self.input[self.input_offset..];
        let result = inflate(state, input, out, MZFlush::None);
        self.input_offset += result.bytes_consumed;
        self.total_in += result.bytes_consumed as u64;
        if self.format == StreamFormat::Gzip {
            self.crc.update(&out[..result.bytes_written]);
        }
        let status = match result.status {
            Ok(MZStatus::StreamEnd) => InflateOutcome::StreamEnd,
            Ok(_) => InflateOutcome::Ok,
            Err(MZError::Buf) => InflateOutcome::NeedMore,
            Err(_) => InflateOutcome::DataError,
        };
        (result.bytes_written, status)
    }

    /// Switches a zlib stream to raw deflate mid-flight.
    fn switch_to_raw_deflate(&mut self) {
        self.format = StreamFormat::RawInflate;
        if let Some(state) = self.state.as_mut() {
            state.reset(DataFormat::Raw);
        }
    }

    /// Consumes input into the gzip header buffer until it parses.
    fn consume_gzip_header(&mut self) -> GzipHeader {
        while self.input_offset < self.input.len() {
            self.header.push(self.input[self.input_offset]);
            self.input_offset += 1;
            self.total_in += 1;
            match parse_gzip_header(&self.header) {
                GzipHeader::Done => {
                    self.header_done = true;
                    return GzipHeader::Done;
                }
                GzipHeader::NeedMore => {}
                GzipHeader::Corrupt => return GzipHeader::Corrupt,
            }
        }
        GzipHeader::NeedMore
    }

    /// Consumes and verifies the gzip trailer.
    fn consume_gzip_trailer(&mut self) {
        while self.input_offset < self.input.len() && self.trailer.len() < 8 {
            self.trailer.push(self.input[self.input_offset]);
            self.input_offset += 1;
            self.total_in += 1;
        }
        if self.trailer.len() < 8 {
            return;
        }
        let crc = u32::from_le_bytes(self.trailer[0..4].try_into().expect("len 8"));
        let isize = u32::from_le_bytes(self.trailer[4..8].try_into().expect("len 8"));
        if crc != self.crc.clone().finalize() || isize != self.inflated_bytes as u32 {
            self.error = true;
            return;
        }
        self.finished = true;
    }
}

/// Miniz outcome, mapped to zlib terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InflateOutcome {
    /// Progress (`Z_OK`).
    Ok,
    /// Stream ended (`Z_STREAM_END`).
    StreamEnd,
    /// Stalled for input (`Z_BUF_ERROR`, non-terminal).
    NeedMore,
    /// Corrupt input (`Z_DATA_ERROR` and friends).
    DataError,
}

/// Whether a byte can start a zlib stream
/// (`IsValidZlibStreamHeaderByte`): method 8, info ≤ 7.
fn is_valid_zlib_stream_header_byte(first_byte: u8) -> bool {
    first_byte & 0xf == 8 && first_byte >> 4 <= 7
}

/// Parses a buffered gzip header (`Done` with its length once the
/// bytes suffice).
fn parse_gzip_header(header: &[u8]) -> GzipHeader {
    if header.len() < 10 {
        return GzipHeader::NeedMore;
    }
    if header[0] != 0x1f || header[1] != 0x8b || header[2] != 8 || header[3] & 0xe0 != 0 {
        return GzipHeader::Corrupt;
    }
    let flags = header[3];
    let mut pos = 10;
    if flags & 0x04 != 0 {
        if header.len() < pos + 2 {
            return GzipHeader::NeedMore;
        }
        let xlen = u16::from_le_bytes([header[pos], header[pos + 1]]) as usize;
        pos += 2;
        if header.len() < pos + xlen {
            return GzipHeader::NeedMore;
        }
        pos += xlen;
    }
    for mask in [0x08, 0x10] {
        if flags & mask != 0 {
            match header[pos..].iter().position(|b| *b == 0) {
                Some(end) => pos += end + 1,
                None => return GzipHeader::NeedMore,
            }
        }
    }
    if flags & 0x02 != 0 {
        if header.len() < pos + 2 {
            return GzipHeader::NeedMore;
        }
        let mut check = crc32fast::Hasher::new();
        check.update(&header[..pos]);
        let expected = u16::from_le_bytes([header[pos], header[pos + 1]]);
        if check.finalize() as u16 != expected {
            return GzipHeader::Corrupt;
        }
    }
    GzipHeader::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use flate2::Compression;
    use std::io::Write;

    const BODY: &[u8] =
        b"<html><head><title>hello hello hello</title></head><body>hi</body></html>";

    fn gzip_bytes() -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(BODY).unwrap();
        encoder.finish().unwrap()
    }

    fn zlib_bytes() -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(BODY).unwrap();
        encoder.finish().unwrap()
    }

    fn raw_bytes() -> Vec<u8> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(BODY).unwrap();
        encoder.finish().unwrap()
    }

    /// Drains one input buffer through fresh `InflateBytes` calls.
    fn drain(inflater: &mut GzipInflater, out_size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; out_size];
        while inflater.has_unconsumed_input() {
            let n = inflater.inflate_bytes(&mut buf);
            assert!(n >= 0, "unexpected inflate error");
            out.extend_from_slice(&buf[..n as usize]);
            if n == 0 {
                break;
            }
        }
        out
    }

    #[test]
    fn gzip_round_trip() {
        let compressed = gzip_bytes();
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        assert!(!inflater.init());
        assert!(inflater.set_input(&compressed));
        assert!(inflater.has_unconsumed_input());
        let out = drain(&mut inflater, 10000);
        assert_eq!(BODY.to_vec(), out);
        assert!(inflater.finished());
        assert!(!inflater.error());
        // Terminal states refuse further work.
        assert!(!inflater.set_input(&compressed));
        assert_eq!(-1, inflater.inflate_bytes(&mut [0u8; 8]));
    }

    #[test]
    fn gzip_split_at_every_boundary() {
        let compressed = gzip_bytes();
        for split in 0..compressed.len() {
            let mut inflater = GzipInflater::new(InflateType::Gzip);
            assert!(inflater.init());
            let mut out = Vec::new();
            if split > 0 {
                assert!(inflater.set_input(&compressed[..split]), "split {split}");
                out.extend(drain(&mut inflater, 16));
            }
            assert!(inflater.set_input(&compressed[split..]), "split {split}");
            out.extend(drain(&mut inflater, 16));
            assert_eq!(BODY.to_vec(), out, "split {split}");
            assert!(inflater.finished(), "split {split}");
        }
    }

    #[test]
    fn zlib_and_raw_deflate() {
        for (format, compressed) in [
            (InflateType::Deflate, zlib_bytes()),
            (InflateType::Deflate, raw_bytes()),
        ] {
            let mut inflater = GzipInflater::new(format);
            assert!(inflater.init());
            assert!(inflater.set_input(&compressed));
            let out = drain(&mut inflater, 10000);
            assert_eq!(BODY.to_vec(), out);
            assert!(inflater.finished());
            assert!(!inflater.error());
        }
    }

    #[test]
    fn tiny_output_buffers() {
        let compressed = gzip_bytes();
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        assert!(inflater.set_input(&compressed));
        let out = drain(&mut inflater, 7);
        assert_eq!(BODY.to_vec(), out);
        assert!(inflater.finished());
    }

    #[test]
    fn limit_fails_closed() {
        let compressed = gzip_bytes();
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        inflater.set_max_inflated_bytes(10);
        assert!(inflater.set_input(&compressed));
        let mut buf = [0u8; 10000];
        let mut handed_out = 0usize;
        let mut saw_error = false;
        while inflater.has_unconsumed_input() {
            let n = inflater.inflate_bytes(&mut buf);
            if n < 0 {
                saw_error = true;
                break;
            }
            handed_out += n as usize;
        }
        assert!(saw_error);
        assert!(inflater.error());
        assert!(inflater.limit_exceeded());
        assert!(handed_out <= 10, "handed out {handed_out}");
        // Further calls keep failing.
        assert_eq!(-1, inflater.inflate_bytes(&mut buf));
    }

    #[test]
    fn corruption_fails() {
        let mut compressed = gzip_bytes();
        // Flip bits in the deflate payload (past the 10-byte header).
        compressed[20] ^= 0xff;
        compressed[21] ^= 0xff;
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        assert!(inflater.set_input(&compressed));
        let mut buf = [0u8; 10000];
        let mut saw_error = false;
        while inflater.has_unconsumed_input() {
            if inflater.inflate_bytes(&mut buf) < 0 {
                saw_error = true;
                break;
            }
        }
        assert!(saw_error || inflater.error());
        // Bad magic fails on the header.
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        assert!(inflater.set_input(b"not gzip at all!!!!"));
        assert_eq!(-1, inflater.inflate_bytes(&mut buf));
        assert!(inflater.error());
        assert!(!inflater.limit_exceeded());
    }

    #[test]
    fn bad_trailer_fails() {
        let mut compressed = gzip_bytes();
        let last = compressed.len() - 1;
        compressed[last] ^= 0xff;
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        assert!(inflater.set_input(&compressed));
        let mut buf = [0u8; 10000];
        let mut saw_error = false;
        while inflater.has_unconsumed_input() {
            if inflater.inflate_bytes(&mut buf) < 0 {
                saw_error = true;
                break;
            }
        }
        assert!(saw_error);
        assert!(inflater.error());
        assert!(!inflater.finished());
    }

    #[test]
    fn truncated_waits_for_more() {
        let compressed = gzip_bytes();
        let cut = compressed.len() / 2;
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        assert!(inflater.init());
        assert!(inflater.set_input(&compressed[..cut]));
        let out = drain(&mut inflater, 10000);
        assert!(!inflater.finished());
        assert!(!inflater.error());
        assert!(!inflater.has_unconsumed_input());
        // The rest completes the stream.
        assert!(inflater.set_input(&compressed[cut..]));
        let mut rest = drain(&mut inflater, 10000);
        let mut full = out;
        full.append(&mut rest);
        assert_eq!(BODY.to_vec(), full);
        assert!(inflater.finished());
    }

    #[test]
    fn misuse_is_rejected() {
        let compressed = gzip_bytes();
        let mut inflater = GzipInflater::new(InflateType::Gzip);
        // Before init.
        assert!(!inflater.set_input(&compressed));
        assert_eq!(-1, inflater.inflate_bytes(&mut [0u8; 8]));
        assert!(inflater.init());
        // Empty input and output.
        assert!(!inflater.set_input(&[]));
        assert!(inflater.set_input(&compressed));
        assert_eq!(-1, inflater.inflate_bytes(&mut []));
        // No re-input before draining.
        assert!(!inflater.set_input(&compressed));
        inflater.shutdown();
        assert!(!inflater.has_unconsumed_input());
        assert_eq!(-1, inflater.inflate_bytes(&mut [0u8; 8]));
    }

    #[test]
    fn preset_dictionary_fails() {
        // A zlib stream naming a preset dictionary (FDICT) can never
        // inflate without one: hard error, no raw retry can save it.
        let mut fdict = vec![0x78, 0x20, 0x00, 0x00, 0x00, 0x00];
        fdict.extend_from_slice(&raw_bytes());
        let mut inflater = GzipInflater::new(InflateType::Deflate);
        assert!(inflater.init());
        assert!(inflater.set_input(&fdict));
        let mut buf = [0u8; 10000];
        let mut saw_error = false;
        while inflater.has_unconsumed_input() {
            if inflater.inflate_bytes(&mut buf) < 0 {
                saw_error = true;
                break;
            }
        }
        assert!(saw_error);
        assert!(inflater.error());
    }

    #[test]
    fn constants_match_cxx() {
        assert_eq!(64 << 20, MAX_INFLATED_HTML_BYTES);
        assert!(is_valid_zlib_stream_header_byte(0x78));
        assert!(!is_valid_zlib_stream_header_byte(0x1f));
        assert!(!is_valid_zlib_stream_header_byte(0x88));
    }
}

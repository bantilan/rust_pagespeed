//! Zstandard output encoding (`compress_zstd` filter core).
//!
//! Pure negotiation + encode helpers shared by the serve paths (the
//! fetch path for HTML, the complete-body paths for resources):
//! client Accept-Encoding parsing, response eligibility, one-shot
//! and streaming compression, and header application. No nginx
//! calls; everything here is unit-testable.
//!
//! Coexistence with gzip is structural: nginx's gzip filter passes
//! through responses that already carry a Content-Encoding, so
//! zstd-encoded responses flow untouched while identity responses
//! keep getting gzip exactly as before.

use crate::names::header;
use crate::response::ResponseHeaders;
use std::io::Write;

/// Content-Encoding value this filter emits.
pub const ZSTD_CONTENT_ENCODING: &str = "zstd";

/// Compression level: the zstd default (fast; within ~10% of top ratio on text).
pub const DEFAULT_ZSTD_LEVEL: i32 = zstd::DEFAULT_COMPRESSION_LEVEL;

/// MIME types worth compressing, besides any `text/*`: the script/data
/// set, minus already-compressed formats (PDF, PostScript). `+xml` and
/// `+json` structured suffixes are covered separately below.
const ZSTD_MIME_EXACT: &[&str] = &[
    "application/ecmascript",
    "application/javascript",
    "application/x-javascript",
    "application/json",
    "application/xml",
    "application/xhtml+xml",
    "image/svg+xml",
];

/// Strips parameters (`; charset=...`) and lowercases a Content-Type value.
fn mime_essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Whether a MIME type is worth zstd-encoding: any `text/*`, the
/// script/data list, or a `+xml`/`+json` structured syntax suffix.
#[must_use]
pub fn compressible_content_type(content_type: &str) -> bool {
    let mime = mime_essence(content_type);
    mime.starts_with("text/")
        || ZSTD_MIME_EXACT.contains(&mime.as_str())
        || mime.ends_with("+xml")
        || mime.ends_with("+json")
}

/// Quality of one Accept-Encoding element (default 1.0). Garbage
/// qualities refuse: misreading negotiation must fall back to
/// identity, never to an encoding the client rejected.
fn element_quality(element: &str) -> f32 {
    let mut parts = element.split(';');
    parts.next();
    let mut quality = 1.0f32;
    for param in parts {
        let param = param.trim();
        let value = param
            .strip_prefix('q')
            .or_else(|| param.strip_prefix('Q'))
            .and_then(|rest| rest.strip_prefix('='));
        if let Some(value) = value {
            quality = value.trim().parse::<f32>().unwrap_or(f32::NAN);
        }
    }
    if quality.is_nan() {
        0.0
    } else {
        quality.max(0.0)
    }
}

/// Whether a client Accept-Encoding header value accepts zstd: any
/// `zstd` token with quality > 0, or a `*` wildcard with quality > 0
/// when zstd itself is unlisted (an explicit `zstd;q=0` always wins).
/// Token match is case-insensitive.
#[must_use]
pub fn client_accepts_zstd(accept_encoding: Option<&str>) -> bool {
    let Some(value) = accept_encoding else {
        return false;
    };
    let mut wildcard_quality = 0.0f32;
    let mut zstd_listed = false;
    for element in value.split(',') {
        let token = element.split(';').next().map(str::trim).unwrap_or("");
        if token.eq_ignore_ascii_case("zstd") {
            zstd_listed = true;
            if element_quality(element) > 0.0 {
                return true;
            }
        } else if token == "*" {
            wildcard_quality = wildcard_quality.max(element_quality(element));
        }
    }
    !zstd_listed && wildcard_quality > 0.0
}

/// Whether a response is eligible for zstd encoding: no
/// Content-Encoding already set (never stack on gzip/br; an empty or
/// `identity` value counts as unset), and no Content-Type or a
/// compressible one (the HTML path passes `None`: its content is HTML
/// by construction).
#[must_use]
pub fn response_eligible_for_zstd(
    content_type: Option<&str>,
    content_encoding: Option<&str>,
) -> bool {
    if let Some(encoding) = content_encoding {
        let encoding = encoding.trim();
        if !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity") {
            return false;
        }
    }
    content_type.is_none_or(compressible_content_type)
}

/// Compresses one complete body at the default level.
pub fn encode_zstd(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    zstd::bulk::compress(bytes, DEFAULT_ZSTD_LEVEL)
}

/// Streaming zstd encoder for the fetch path: chunks in, compressed
/// bytes out, one frame across the whole response.
pub struct ZstdStreamEncoder {
    encoder: Option<zstd::stream::Encoder<'static, Vec<u8>>>,
}

impl ZstdStreamEncoder {
    /// Creates an encoder at the default level.
    pub fn new() -> std::io::Result<Self> {
        Ok(ZstdStreamEncoder {
            encoder: Some(zstd::stream::Encoder::new(Vec::new(), DEFAULT_ZSTD_LEVEL)?),
        })
    }

    /// Compresses one chunk, returning bytes ready to ship (may be
    /// empty until the window fills; `finish` emits the tail).
    ///
    /// # Panics
    ///
    /// When called after [`ZstdStreamEncoder::finish`] (the fetch
    /// completes exactly once, like `handle_done`).
    pub fn write_chunk(&mut self, bytes: &[u8]) -> std::io::Result<Vec<u8>> {
        let encoder = self.encoder.as_mut().expect("encode after finish");
        encoder.write_all(bytes)?;
        Ok(std::mem::take(encoder.get_mut()))
    }

    /// Flushes a block boundary (preserves flush_early streaming),
    /// returning bytes ready to ship.
    ///
    /// # Panics
    ///
    /// When called after [`ZstdStreamEncoder::finish`].
    pub fn flush_chunk(&mut self) -> std::io::Result<Vec<u8>> {
        let encoder = self.encoder.as_mut().expect("encode after finish");
        encoder.flush()?;
        Ok(std::mem::take(encoder.get_mut()))
    }

    /// Finishes the frame, returning trailing bytes.
    ///
    /// # Panics
    ///
    /// When called twice (the fetch completes exactly once).
    pub fn finish(mut self) -> std::io::Result<Vec<u8>> {
        let encoder = self.encoder.take().expect("double finish");
        encoder.finish()
    }
}

/// Whether a Vary value already names Accept-Encoding (`*` covers everything).
#[must_use]
pub fn vary_names_accept_encoding(value: &str) -> bool {
    value.split(',').any(|token| {
        let token = token.trim();
        token.eq_ignore_ascii_case("accept-encoding") || token == "*"
    })
}

/// Appends `Vary: Accept-Encoding` unless already covered (a second
/// Vary line combines with the first; nothing is collapsed).
pub fn ensure_vary_accept_encoding(headers: &mut crate::headers::Headers) {
    let covered = headers
        .lookup(header::VARY)
        .is_some_and(|values| values.iter().any(|v| vary_names_accept_encoding(v)));
    if !covered {
        headers.add(header::VARY, "Accept-Encoding");
    }
}

/// Applies zstd encoding headers for the fetch path: sets
/// Content-Encoding, ensures Vary names Accept-Encoding, drops
/// Content-Length (unknown until the stream finishes) and
/// Accept-Ranges (encoded bytes are not range-addressable).
pub fn apply_zstd_response_headers(headers: &mut ResponseHeaders) {
    headers
        .headers_mut()
        .replace(header::CONTENT_ENCODING, ZSTD_CONTENT_ENCODING);
    ensure_vary_accept_encoding(headers.headers_mut());
    headers.headers_mut().remove_all(header::CONTENT_LENGTH);
    headers.headers_mut().remove_all(header::ACCEPT_RANGES);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8], capacity: usize) -> Vec<u8> {
        zstd::bulk::decompress(bytes, capacity).expect("valid zstd frame")
    }

    #[test]
    fn accepts_zstd_token_forms() {
        assert!(client_accepts_zstd(Some("zstd")));
        assert!(client_accepts_zstd(Some("ZSTD")));
        assert!(client_accepts_zstd(Some("gzip, deflate, br, zstd")));
        assert!(client_accepts_zstd(Some(
            "gzip, deflate, br, zstd, *;q=0.1"
        )));
        assert!(client_accepts_zstd(Some("  zstd  ")));
        assert!(client_accepts_zstd(Some("gzip, zstd;q=0.5")));
        assert!(!client_accepts_zstd(None));
        assert!(!client_accepts_zstd(Some("")));
        assert!(!client_accepts_zstd(Some("   ")));
        assert!(!client_accepts_zstd(Some("gzip")));
        assert!(!client_accepts_zstd(Some("gzip, deflate, br")));
        assert!(!client_accepts_zstd(Some("zsdt")));
        assert!(!client_accepts_zstd(Some("zstandard")));
    }

    #[test]
    fn accepts_zstd_quality_rules() {
        assert!(!client_accepts_zstd(Some("zstd;q=0")));
        assert!(!client_accepts_zstd(Some("zstd;q=0.0")));
        assert!(!client_accepts_zstd(Some("zstd; q=0.000")));
        assert!(client_accepts_zstd(Some("zstd;q=0.1")));
        assert!(client_accepts_zstd(Some("zstd;q=1")));
        assert!(client_accepts_zstd(Some("zstd;Q=0.5")));
        // Garbage quality refuses (safe direction: identity).
        assert!(!client_accepts_zstd(Some("zstd;q=bogus")));
        assert!(!client_accepts_zstd(Some("zstd;q=")));
        // Wildcard covers unlisted zstd, but an explicit q=0 wins.
        assert!(client_accepts_zstd(Some("*;q=0.5")));
        assert!(client_accepts_zstd(Some("gzip, *")));
        assert!(!client_accepts_zstd(Some("*;q=0")));
        assert!(!client_accepts_zstd(Some("zstd;q=0, *;q=1")));
        assert!(!client_accepts_zstd(Some("gzip;q=0, *;q=0")));
    }

    #[test]
    fn compressible_types() {
        for mime in [
            "text/html",
            "text/html; charset=utf-8",
            "TEXT/CSS",
            "text/plain",
            "text/xml",
            "text/csv",
            "application/javascript",
            "application/x-javascript",
            "application/ecmascript",
            "application/json",
            "application/xml",
            "application/xhtml+xml",
            "application/atom+xml",
            "application/hal+json",
            "image/svg+xml",
        ] {
            assert!(compressible_content_type(mime), "{mime}");
        }
        for mime in [
            "image/jpeg",
            "image/png",
            "image/gif",
            "image/webp",
            "image/avif",
            "application/pdf",
            "application/octet-stream",
            "application/wasm",
            "video/mp4",
            "audio/mpeg",
            "font/woff2",
            "",
        ] {
            assert!(!compressible_content_type(mime), "{mime}");
        }
    }

    #[test]
    fn eligibility_rules() {
        assert!(response_eligible_for_zstd(None, None));
        assert!(response_eligible_for_zstd(Some("text/html"), None));
        assert!(response_eligible_for_zstd(
            Some("text/html"),
            Some("identity")
        ));
        assert!(response_eligible_for_zstd(Some("text/html"), Some("")));
        assert!(!response_eligible_for_zstd(Some("text/html"), Some("gzip")));
        assert!(!response_eligible_for_zstd(Some("text/html"), Some("br")));
        assert!(!response_eligible_for_zstd(
            Some("text/html"),
            Some("gzip, zstd")
        ));
        assert!(!response_eligible_for_zstd(Some("image/jpeg"), None));
        assert!(!response_eligible_for_zstd(None, Some("zstd")));
    }

    #[test]
    fn one_shot_round_trips_real_frame() {
        let input = "<html><body>".repeat(500);
        let encoded = encode_zstd(input.as_bytes()).expect("encode");
        // zstd frame magic (little-endian 0xFD2FB528): any client decodes this.
        assert_eq!(&encoded[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
        assert!(encoded.len() < input.len());
        assert_eq!(decode(&encoded, input.len()), input.as_bytes());
    }

    #[test]
    fn streaming_chunks_decode_to_input() {
        let chunks: &[&[u8]] = &[b"<html>", b"<body>hello", b"</body></html>"];
        let mut encoder = ZstdStreamEncoder::new().expect("encoder");
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(encoder.write_chunk(chunk).expect("chunk"));
        }
        out.extend(encoder.flush_chunk().expect("flush"));
        out.extend(encoder.finish().expect("finish"));
        let joined: Vec<u8> = chunks.concat();
        assert_eq!(decode(&out, joined.len()), joined);
    }

    #[test]
    fn streaming_empty_input_finishes_valid_frame() {
        let mut encoder = ZstdStreamEncoder::new().expect("encoder");
        let mut out = encoder.write_chunk(b"").expect("chunk");
        out.extend(encoder.finish().expect("finish"));
        assert!(decode(&out, 0).is_empty());
    }

    #[test]
    fn header_application_sets_encoding_and_vary() {
        use crate::response::HttpOptions;
        let mut headers = ResponseHeaders::new(HttpOptions::default());
        headers.headers_mut().add(header::CONTENT_TYPE, "text/html");
        headers.headers_mut().add(header::CONTENT_LENGTH, "15398");
        headers.headers_mut().add(header::ACCEPT_RANGES, "bytes");
        apply_zstd_response_headers(&mut headers);
        assert_eq!(
            headers.headers().lookup1(header::CONTENT_ENCODING),
            Some(ZSTD_CONTENT_ENCODING.to_string())
        );
        assert!(headers
            .headers()
            .lookup(header::VARY)
            .unwrap_or_default()
            .iter()
            .any(|v| v == "Accept-Encoding"));
        assert!(!headers.headers().has(header::CONTENT_LENGTH));
        assert!(!headers.headers().has(header::ACCEPT_RANGES));
        // Content-Type untouched.
        assert_eq!(
            headers.headers().lookup1(header::CONTENT_TYPE),
            Some("text/html".to_string())
        );
    }

    #[test]
    fn header_application_keeps_existing_vary_coverage() {
        use crate::response::HttpOptions;
        for existing in ["Accept-Encoding", "accept-encoding", "*"] {
            let mut headers = ResponseHeaders::new(HttpOptions::default());
            headers.headers_mut().add(header::VARY, existing);
            apply_zstd_response_headers(&mut headers);
            assert_eq!(
                headers.headers().lookup(header::VARY),
                Some(vec![existing.to_string()]),
                "Vary {existing} must not gain a duplicate"
            );
        }
        // An unrelated Vary gains a second line (legal; combines).
        let mut headers = ResponseHeaders::new(HttpOptions::default());
        headers.headers_mut().add(header::VARY, "Cookie");
        apply_zstd_response_headers(&mut headers);
        assert_eq!(
            headers.headers().lookup(header::VARY),
            Some(vec!["Cookie".to_string(), "Accept-Encoding".to_string()])
        );
    }
}

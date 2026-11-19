//! Resource-name segment codecs (`UrlSegmentEncoder` family).
//!
//! Encoders translate a filter's input URLs into the `name` segment of a
//! `.pagespeed.` leaf and back. The default codec wraps one URL; the
//! multipart codec `+`-joins several with `=` escapes; the CSS codec adds
//! a legacy capability letter. Decoding never touches caller state (the
//! C++ `ResourceContext` out-param is discarded by every decode caller),
//! and UA-derived context filling (`SetInliningImages`) waits for the
//! request-properties phase.
//!
//! Segments must be UTF-8 here (failing closed otherwise) where the C++
//! works byte-wise; rewritten leaves are ASCII in practice.

use ps_cache::namer::{decode_url_segment_bytes, encode_url_segment_bytes};

/// Segment codec (`UrlSegmentEncoder`).
pub trait UrlSegmentEncoder {
    /// Encodes input URLs into one segment (`Encode`).
    fn encode(&self, urls: &[Vec<u8>]) -> Vec<u8>;

    /// Decodes a segment back into URLs (`Decode`); `None` rejects.
    fn decode(&self, segment: &[u8]) -> Option<Vec<Vec<u8>>>;
}

/// Single-URL codec (base `UrlSegmentEncoder`).
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultEncoder;

impl UrlSegmentEncoder for DefaultEncoder {
    fn encode(&self, urls: &[Vec<u8>]) -> Vec<u8> {
        debug_assert_eq!(1, urls.len());
        encode_url_segment_bytes(&urls[0]).into_bytes()
    }

    fn decode(&self, segment: &[u8]) -> Option<Vec<Vec<u8>>> {
        decode_url_segment_bytes(std::str::from_utf8(segment).ok()?).map(|url| vec![url])
    }
}

/// Multi-URL codec (`UrlMultipartEncoder`): `+`-joined with `=` escapes
/// (`=` → `==`, `+` → `=+`), then segment-escaped as a whole.
#[derive(Debug, Clone, Copy, Default)]
pub struct MultipartEncoder;

impl UrlSegmentEncoder for MultipartEncoder {
    fn encode(&self, urls: &[Vec<u8>]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (index, url) in urls.iter().enumerate() {
            if index != 0 {
                buf.push(b'+');
            }
            for byte in url {
                match byte {
                    b'=' => buf.extend_from_slice(b"=="),
                    b'+' => buf.extend_from_slice(b"=+"),
                    _ => buf.push(*byte),
                }
            }
        }
        encode_url_segment_bytes(&buf).into_bytes()
    }

    fn decode(&self, segment: &[u8]) -> Option<Vec<Vec<u8>>> {
        let text = std::str::from_utf8(segment).ok()?;
        let buf = decode_url_segment_bytes(text)?;
        let mut urls = Vec::new();
        let mut url = Vec::new();
        let mut append_last = false;
        let mut index = 0;
        while index < buf.len() {
            let byte = buf[index];
            if byte == b'+' {
                urls.push(std::mem::take(&mut url));
                append_last = true;
            } else {
                let mut byte = byte;
                if byte == b'=' {
                    index += 1;
                    if index >= buf.len() {
                        return None;
                    }
                    byte = buf[index];
                    if byte != b'=' && byte != b'+' {
                        return None;
                    }
                }
                url.push(byte);
            }
            index += 1;
        }
        // A trailing `+` yields one final empty URL.
        if append_last || !url.is_empty() {
            urls.push(url);
        }
        Some(urls)
    }
}

/// CSS codec (`CssUrlEncoder`): an `A.`-prefixed escaped URL on encode;
/// any leading letter accepted (but ignored) on decode.
#[derive(Debug, Clone, Copy, Default)]
pub struct CssUrlEncoder;

impl UrlSegmentEncoder for CssUrlEncoder {
    fn encode(&self, urls: &[Vec<u8>]) -> Vec<u8> {
        debug_assert_eq!(1, urls.len());
        let mut out = b"A.".to_vec();
        out.extend_from_slice(encode_url_segment_bytes(&urls[0]).as_bytes());
        out
    }

    fn decode(&self, segment: &[u8]) -> Option<Vec<Vec<u8>>> {
        if segment.len() < 2 || segment[1] != b'.' {
            return None;
        }
        let text = std::str::from_utf8(&segment[2..]).ok()?;
        decode_url_segment_bytes(text).map(|url| vec![url])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(urls: &[&str]) -> Vec<Vec<u8>> {
        urls.iter().map(|url| url.as_bytes().to_vec()).collect()
    }

    #[test]
    fn multipart_escape_round_trip() {
        let encoder = MultipartEncoder;
        let encoding = encoder.encode(&strings(&["abc", "def", "a=b+c"]));
        assert_eq!(
            encoder.decode(&encoding),
            Some(strings(&["abc", "def", "a=b+c"]))
        );
    }

    #[test]
    fn multipart_edge_cases() {
        let encoder = MultipartEncoder;
        // Empty input decodes to no URLs.
        assert_eq!(encoder.decode(b""), Some(Vec::new()));
        // A trailing separator yields a final empty URL.
        assert_eq!(encoder.decode(b"a+b+"), Some(strings(&["a", "b", ""])));
        assert_eq!(encoder.decode(b"a"), Some(strings(&["a"])));
        // Browsers may re-encode `+` as `%20`.
        assert_eq!(encoder.decode(b"a%20b%20"), Some(strings(&["a", "b", ""])));
        // Dangling escapes and bad followers reject.
        assert_eq!(encoder.decode(b"a="), None);
        assert_eq!(encoder.decode(b"a=x"), None);
    }

    #[test]
    fn css_encode_decode_round_trip() {
        let encoder = CssUrlEncoder;
        let encoding = encoder.encode(&strings(&["a.css"]));
        assert_eq!(encoding, b"A.a.css");
        assert_eq!(encoder.decode(&encoding), Some(strings(&["a.css"])));
    }

    #[test]
    fn css_legacy_letters_decode() {
        let encoder = CssUrlEncoder;
        // Any leading letter decodes; the letter itself is meaningless.
        for encoding in ["V.a.css", "W.a.css", "I.a.css", "A.a.css"] {
            assert_eq!(
                encoder.decode(encoding.as_bytes()),
                Some(strings(&["a.css"])),
                "{encoding}"
            );
        }
        // Missing dot rejects.
        assert_eq!(encoder.decode(b"A"), None);
        assert_eq!(encoder.decode(b"Ax"), None);
    }

    #[test]
    fn default_single_url() {
        let encoder = DefaultEncoder;
        let encoding = encoder.encode(&strings(&["a/b?c"]));
        assert_eq!(encoder.decode(&encoding), Some(strings(&["a/b?c"])));
        assert_eq!(encoder.decode(b"%zz"), None);
    }
}

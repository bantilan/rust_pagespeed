//! `data:` URL building, predicates, and dismantling (`data_url.h`).

use super::content_type::{mime_to_content_type, ContentType};

/// Payload encoding (`Encoding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Unparseable parameters (parse output only).
    Unknown,
    /// `;base64` payload.
    Base64,
    /// Raw payload.
    Plain,
}

/// Builds a `data:` URL (`DataUrl`).
#[must_use]
pub fn make_data_url(content_type: ContentType, encoding: Encoding, content: &[u8]) -> Vec<u8> {
    let mut url = b"data:".to_vec();
    url.extend_from_slice(content_type.mime_type().as_bytes());
    match encoding {
        Encoding::Base64 => {
            url.extend_from_slice(b";base64,");
            url.extend_from_slice(ps_cache::base64::mime_encode(content).as_bytes());
        }
        // Unknown or plain: no encoding, and never fail.
        Encoding::Unknown | Encoding::Plain => {
            url.extend_from_slice(b",");
            url.extend_from_slice(content);
        }
    }
    url
}

/// Whether the URL is a `data:` URL (case-sensitive `data:` prefix).
#[must_use]
pub fn is_data_url(url: &[u8]) -> bool {
    url.starts_with(b"data:")
}

/// Whether the URL is an inlined image (`data:image/` prefix).
#[must_use]
pub fn is_data_image_url(url: &[u8]) -> bool {
    url.starts_with(b"data:image/")
}

/// Dismantles a `data:` URL into content type, encoding, and the
/// still-encoded payload (`ParseDataUrl`, borrowing the input).
pub fn parse_data_url(url: &[u8]) -> Option<(Option<ContentType>, Encoding, &[u8])> {
    let header_end = url.iter().position(|byte| *byte == b',')?;
    if !is_data_url(url) {
        return None;
    }
    let header = &url[..header_end];
    let encoding = match header.iter().position(|byte| *byte == b';') {
        None => Encoding::Plain,
        Some(_) if header.len() >= b";base64".len() && header.ends_with(b";base64") => {
            Encoding::Base64
        }
        Some(semi) if header_end >= semi + b";base64".len() => Encoding::Plain,
        // A `;` too close to the comma leaves the encoding unknown,
        // yet the URL still parses (a C++ quirk, kept).
        Some(_) => Encoding::Unknown,
    };
    let mime_end = header
        .iter()
        .position(|byte| *byte == b';')
        .unwrap_or(header.len());
    let mime = std::str::from_utf8(&url["data:".len()..mime_end]).ok()?;
    let content_type = mime_to_content_type(mime);
    Some((content_type, encoding, &url[header_end + 1..]))
}

/// Decodes a dismantled payload (`DecodeDataUrlContent`).
#[must_use]
pub fn decode_data_url_content(encoding: Encoding, encoded: &[u8]) -> Option<Vec<u8>> {
    match encoding {
        Encoding::Plain => Some(encoded.to_vec()),
        Encoding::Base64 => std::str::from_utf8(encoded)
            .ok()
            .and_then(ps_cache::base64::mime_decode),
        Encoding::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_type::mime_to_content_type;

    const ASCII_DATA: &[u8] =
        b"A_Rather=Long,But-conventional?looking_string#with;some:odd,characters.";
    const ASCII_DATA_BASE64: &[u8] = b"QV9SYXRoZXI9TG9uZyxCdXQtY29udmVudGlvbmFsP2xvb2tpbmdfc3RyaW5nI3dpdGg7c29tZTpvZGQsY2hhcmFjdGVycy4=";
    const MIXED_DATA: &[u8] = b"This string\ncontains\0lots of\tunusual\xe3~characters\xd7\xa5";
    const MIXED_DATA_BASE64: &[u8] =
        b"VGhpcyBzdHJpbmcKY29udGFpbnMAbG90cyBvZgl1bnVzdWFs435jaGFyYWN0ZXJz16U=";
    const PLAIN_PREFIX: &[u8] = b"data:text/plain,";
    const BASE64_PREFIX: &[u8] = b"data:text/plain;base64,";
    const GIF_PLAIN_PREFIX: &[u8] = b"data:image/gif,";
    const GIF_BASE64_PREFIX: &[u8] = b"data:image/gif;base64,";

    fn text_type() -> ContentType {
        mime_to_content_type("text/plain").unwrap()
    }

    fn gif_type() -> ContentType {
        mime_to_content_type("image/gif").unwrap()
    }

    #[test]
    fn data_prefixes() {
        assert!(is_data_url(b"data:abc"));
        assert!(is_data_url(b"data:image/png;base64,AAAA"));
        assert!(is_data_image_url(b"data:image/png;base64,AAAA"));
        assert!(!is_data_image_url(b"data:text/plain,hi"));
        assert!(!is_data_url(b"http://h/data:x"));
        assert!(!is_data_url(b"DATA:abc"));
        assert!(!is_data_url(b""));
    }

    #[test]
    fn make_plain() {
        let url = make_data_url(text_type(), Encoding::Plain, ASCII_DATA);
        assert_eq!([PLAIN_PREFIX, ASCII_DATA].concat(), url);
    }

    #[test]
    fn make_base64() {
        let url = make_data_url(text_type(), Encoding::Base64, ASCII_DATA);
        assert_eq!([BASE64_PREFIX, ASCII_DATA_BASE64].concat(), url);
    }

    #[test]
    fn make_mixed_plain() {
        let url = make_data_url(gif_type(), Encoding::Plain, MIXED_DATA);
        assert_eq!([GIF_PLAIN_PREFIX, MIXED_DATA].concat(), url);
    }

    #[test]
    fn make_mixed_base64() {
        let url = make_data_url(gif_type(), Encoding::Base64, MIXED_DATA);
        assert_eq!([GIF_BASE64_PREFIX, MIXED_DATA_BASE64].concat(), url);
    }

    /// Mirrors `TestDecoding` (which decodes with the *expected*
    /// encoding, quirks included).
    fn check_decoding(
        can_parse: bool,
        can_decode: bool,
        prefix: &[u8],
        encoded: &[u8],
        mime: Option<&str>,
        encoding: Encoding,
        decoded: &[u8],
    ) {
        let url = [prefix, encoded].concat();
        let parsed = parse_data_url(&url);
        assert_eq!(can_parse, parsed.is_some(), "parse {}", url.escape_ascii());
        if can_parse {
            let (parsed_type, parsed_encoding, parsed_encoded) = parsed.unwrap();
            assert_eq!(encoding, parsed_encoding);
            assert_eq!(mime, parsed_type.map(|kind| kind.mime_type()));
            assert_eq!(encoded, parsed_encoded);
        }
        let roundtrip = decode_data_url_content(encoding, encoded);
        assert_eq!(can_decode, roundtrip.is_some());
        assert_eq!(decoded, roundtrip.unwrap_or_default());
    }

    #[test]
    fn parse_plain() {
        check_decoding(
            true,
            true,
            PLAIN_PREFIX,
            ASCII_DATA,
            Some("text/plain"),
            Encoding::Plain,
            ASCII_DATA,
        );
    }

    #[test]
    fn parse_base64() {
        check_decoding(
            true,
            true,
            BASE64_PREFIX,
            ASCII_DATA_BASE64,
            Some("text/plain"),
            Encoding::Base64,
            ASCII_DATA,
        );
    }

    #[test]
    fn parse_mixed_plain() {
        check_decoding(
            true,
            true,
            PLAIN_PREFIX,
            MIXED_DATA,
            Some("text/plain"),
            Encoding::Plain,
            MIXED_DATA,
        );
    }

    #[test]
    fn parse_mixed_base64() {
        check_decoding(
            true,
            true,
            BASE64_PREFIX,
            MIXED_DATA_BASE64,
            Some("text/plain"),
            Encoding::Base64,
            MIXED_DATA,
        );
    }

    #[test]
    fn parse_bad_protocol() {
        check_decoding(
            false,
            false,
            b"http://www.google.com/",
            b"",
            None,
            Encoding::Unknown,
            b"",
        );
    }

    #[test]
    fn parse_no_comma() {
        let prefix = [b"data:text/plain;base64;".as_slice(), MIXED_DATA_BASE64].concat();
        check_decoding(false, false, &prefix, b"", None, Encoding::Unknown, b"");
    }

    #[test]
    fn parse_no_mime() {
        check_decoding(
            true,
            true,
            b"data:;base64,",
            MIXED_DATA_BASE64,
            None,
            Encoding::Base64,
            MIXED_DATA,
        );
    }

    #[test]
    fn parse_corrupt_mime() {
        check_decoding(
            true,
            true,
            b"data:#$!;base64,",
            MIXED_DATA_BASE64,
            None,
            Encoding::Base64,
            MIXED_DATA,
        );
    }

    #[test]
    fn parse_bad_encoding_is_plain() {
        check_decoding(
            true,
            true,
            b"data:text/plain;mumbledypeg,",
            MIXED_DATA,
            Some("text/plain"),
            Encoding::Plain,
            MIXED_DATA,
        );
    }

    #[test]
    fn parse_bad_base64() {
        check_decoding(
            true,
            false,
            BASE64_PREFIX,
            b"@%#$%@#$%^@%%^%*%^&*",
            Some("text/plain"),
            Encoding::Base64,
            b"",
        );
    }
}

//! Source maps, version 3 (`source_map.{h,cc}`).
//!
//! Delta VLQ mappings (4-field, no names) plus the XSSI-guarded JSON
//! envelope. Names are unsupported (`"*UNIMPLEMENTED*"` stderr marker
//! in C++); mappings carrying any are still encoded structurally.
#![forbid(unsafe_code)]

/// One mapping step: generated position plus source position.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mapping {
    /// Generated line (0-based).
    pub gen_line: i32,
    /// Generated column (0-based).
    pub gen_col: i32,
    /// Source file index.
    pub src_file: i32,
    /// Source line (0-based).
    pub src_line: i32,
    /// Source column (0-based).
    pub src_col: i32,
}

impl Mapping {
    /// Builds a mapping step.
    #[must_use]
    pub const fn new(
        gen_line: i32,
        gen_col: i32,
        src_file: i32,
        src_line: i32,
        src_col: i32,
    ) -> Mapping {
        Mapping {
            gen_line,
            gen_col,
            src_file,
            src_line,
            src_col,
        }
    }
}

/// Base64 alphabet for VLQs (`kBase64Chars`).
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes one signed value (` VLQ`).
fn encode_vlq(value: i32, out: &mut Vec<u8>) {
    let mut digit = if value < 0 {
        ((-value) << 1) + 1
    } else {
        value << 1
    };
    loop {
        let mut sextet = (digit & 31) as u8;
        digit >>= 5;
        if digit > 0 {
            sextet |= 32;
        }
        out.push(BASE64[sextet as usize]);
        if digit == 0 {
            break;
        }
    }
}

/// Encodes mappings (`EncodeMappings`).
#[must_use]
pub fn encode_mappings(mappings: &[Mapping]) -> String {
    let mut out = Vec::new();
    let mut gen_line = 0;
    let mut gen_col = 0;
    let mut src_file = 0;
    let mut src_line = 0;
    let mut src_col = 0;
    for mapping in mappings {
        while gen_line < mapping.gen_line {
            out.push(b';');
            gen_line += 1;
            gen_col = 0;
        }
        if !out.is_empty() && !out.ends_with(b";") {
            out.push(b',');
        }
        encode_vlq(mapping.gen_col - gen_col, &mut out);
        encode_vlq(mapping.src_file - src_file, &mut out);
        encode_vlq(mapping.src_line - src_line, &mut out);
        encode_vlq(mapping.src_col - src_col, &mut out);
        gen_col = mapping.gen_col;
        src_file = mapping.src_file;
        src_line = mapping.src_line;
        src_col = mapping.src_col;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// JSON-escapes a string the way jsoncpp `FastWriter` does.
fn json_escape(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// Percent-encodes `<>` in URLs (`PercentEncode`).
fn percent_encode(url: &str, out: &mut String) {
    let mut buf = Vec::with_capacity(url.len());
    for byte in url.bytes() {
        match byte {
            b'<' => buf.extend_from_slice(b"%3C"),
            b'>' => buf.extend_from_slice(b"%3E"),
            _ => buf.push(byte),
        }
    }
    out.push_str(&String::from_utf8_lossy(&buf));
}

/// Encodes the full map (`Encode`): XSSI guard plus the JSON envelope
/// with alphabetically-ordered keys (jsoncpp `std::map` order). The
/// `file` key drops when the generated URL is empty.
#[must_use]
pub fn encode(mappings: &[Mapping], sources: &[&str], output_file: &str) -> String {
    let mut out = String::from(")]}'\n{");
    if !output_file.is_empty() {
        out.push_str("\"file\":");
        let mut encoded = String::new();
        percent_encode(output_file, &mut encoded);
        json_escape(&encoded, &mut out);
        out.push(',');
    }
    out.push_str("\"mappings\":\"");
    // Mappings embed raw (base64-range only, never needing escapes).
    out.push_str(&encode_mappings(mappings));
    out.push_str("\",\"names\":[],\"sources\":[");
    for (i, source) in sources.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let mut encoded = String::new();
        percent_encode(source, &mut encoded);
        json_escape(&encoded, &mut out);
    }
    out.push_str("],\"version\":3}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_vectors() {
        // Spec Appendix C examples.
        let mut out = Vec::new();
        encode_vlq(0, &mut out);
        assert_eq!(out, b"A");
        out.clear();
        encode_vlq(1, &mut out);
        assert_eq!(out, b"C");
        out.clear();
        encode_vlq(-1, &mut out);
        assert_eq!(out, b"D");
        out.clear();
        encode_vlq(123, &mut out);
        assert_eq!(out, b"2H");
        out.clear();
        encode_vlq(123456789, &mut out);
        assert_eq!(out, b"qxmvrH");
    }

    #[test]
    fn mappings_and_envelope() {
        // Two steps on one line plus one on the next.
        let mappings = vec![
            Mapping::new(0, 0, 0, 0, 0),
            Mapping::new(0, 5, 0, 0, 7),
            Mapping::new(1, 0, 0, 3, 0),
        ];
        assert_eq!(encode_mappings(&mappings), "AAAA,KAAO;AAGP");
        let json = encode(&mappings, &["a.js"], "a.min.js");
        assert_eq!(
            json,
            ")]}'\n{\"file\":\"a.min.js\",\"mappings\":\"AAAA,KAAO;AAGP\",\
             \"names\":[],\"sources\":[\"a.js\"],\"version\":3}\n"
        );
        // Empty mappings still produce a valid envelope.
        assert!(encode(&[], &["a.js"], "a.min.js").contains("\"mappings\":\"\""));
    }
}

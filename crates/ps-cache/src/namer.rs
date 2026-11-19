//! Resource naming (`net/instaweb/rewriter/resource_namer.{h,cc}`,
//! `pagespeed/kernel/util/url_escaper.cc`).
//!
//! Encoded form: `name.pagespeed[.experiment|.options].id.hash[signature].ext`
//! where `options` is URL-segment-escaped (`,` escapes, `%XX` hex, plus
//! legacy `,x` codes). Decoding anchors at the first `pagespeed` segment;
//! single lowercase letters between it and the id are experiments, longer
//! runs are options. Unknown shapes fall back to legacy 4-segment decode.
#![forbid(unsafe_code)]

use crate::hash::Hasher;

/// System id marker (`kSystemId`).
pub const SYSTEM_ID: &str = "pagespeed";
/// Overhead added by separators + system id (`kOverhead` = 4 + 9).
pub const OVERHEAD: usize = 13;

/// Encodes options for a URL segment (`EncodeToUrlSegment`).
///
/// Byte-oriented like C++: alphanumerics and `._=+-` pass through;
/// `http://` becomes `,h`, `.pagespeed.` becomes `,M`, and anything else
/// becomes `,%02X` (uppercase hex). `^ % / \ , ? &` have dedicated short
/// codes. Output is always pure ASCII.
#[must_use]
pub fn encode_url_segment(input: &str) -> String {
    encode_url_segment_bytes(input.as_bytes())
}

/// Byte-oriented segment encoder (C++ operates on bytes, not chars).
#[must_use]
pub fn encode_url_segment_bytes(input: &[u8]) -> String {
    let bytes = input;
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        match byte {
            b'^' => out.push_str(",u"),
            b'%' => out.push_str(",P"),
            b'/' => out.push_str(",_"),
            b'\\' => out.push_str(",-"),
            b',' => out.push_str(",,"),
            b'?' => out.push_str(",q"),
            b'&' => out.push_str(",a"),
            b'h' => {
                if bytes[i..].starts_with(b"http://") {
                    out.push_str(",h");
                    i += "http://".len();
                    continue;
                }
                out.push('h');
            }
            b'.' => {
                if bytes[i..].starts_with(b".pagespeed.") {
                    out.push_str(",M");
                    i += ".pagespeed.".len();
                    continue;
                }
                out.push('.');
            }
            _ if byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'=' | b'+' | b'-') =>
            {
                out.push(byte as char);
            }
            _ => {
                out.push(',');
                out.push(hex_digit(byte >> 4));
                out.push(hex_digit(byte & 0xf));
            }
        }
        i += 1;
    }
    out
}

fn hex_digit(nibble: u8) -> char {
    (if nibble < 10 {
        b'0' + nibble
    } else {
        b'A' + nibble - 10
    }) as char
}

fn is_passthrough(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'=' | b'+' | b'-')
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decodes a URL segment to bytes (`DecodeFromUrlSegment`).
///
/// Accepts `,%XX`, the short codes, `%XX` escapes (with the `+`-for-space
/// corner), and the legacy November-2010 `,x` codes. Returns `None` on
/// malformed input.
#[must_use]
pub fn decode_url_segment_bytes(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if is_passthrough(byte) {
            out.push(byte);
            i += 1;
            continue;
        }
        // Expect ',' or '%' followed by an encoding.
        i += 1;
        if i >= bytes.len() {
            return None;
        }
        if byte != b',' {
            if byte != b'%' {
                return None;
            }
            if i + 1 >= bytes.len() {
                return None;
            }
            let (Some(hi), Some(lo)) = (hex_value(bytes[i]), hex_value(bytes[i + 1])) else {
                return None;
            };
            i += 2;
            let mut decoded = (hi << 4) | lo;
            if decoded == b' ' {
                decoded = b'+'; // %-encoded space means '+'.
            }
            if decoded != b',' {
                out.push(decoded);
                continue;
            }
            if i >= bytes.len() {
                return None; // trailing %-encoded ','.
            }
            // Fall through to ','-decoding at bytes[i].
        }
        match bytes[i] {
            b'_' => out.push(b'/'),
            b'-' => out.push(b'\\'),
            b',' => out.push(b','),
            b'a' => out.push(b'&'),
            b'M' => out.extend_from_slice(b".pagespeed."),
            b'P' => out.push(b'%'),
            b'q' => out.push(b'?'),
            b'u' => out.push(b'^'),
            // Legacy November-2010 codes (decoded, never encoded now).
            b'c' => out.extend_from_slice(b".com"),
            b'e' => out.extend_from_slice(b".edu"),
            b'g' => out.extend_from_slice(b".gif"),
            b'h' => out.extend_from_slice(b"http://"),
            b'j' => out.extend_from_slice(b".jpg"),
            b'k' => out.extend_from_slice(b".jpeg"),
            b'l' => out.extend_from_slice(b".js"),
            b'n' => out.extend_from_slice(b".net"),
            b'o' => out.push(b'.'),
            b'p' => out.extend_from_slice(b".png"),
            b's' => out.extend_from_slice(b".css"),
            b't' => out.extend_from_slice(b".html"),
            b'w' => out.extend_from_slice(b"www."),
            other => {
                if i + 1 >= bytes.len() {
                    return None;
                }
                let (Some(hi), Some(lo)) = (hex_value(other), hex_value(bytes[i + 1])) else {
                    return None;
                };
                i += 1;
                out.push((hi << 4) | lo);
            }
        }
        i += 1;
    }
    Some(out)
}

/// Decodes a URL segment to a string.
///
/// This is [`decode_url_segment_bytes`] plus UTF-8 validation: C++
/// permits arbitrary bytes in options, which have no `String`
/// representation, so non-UTF8 results report decode failure here (the
/// bytes form stays available for such blobs).
#[must_use]
pub fn decode_url_segment(input: &str) -> Option<String> {
    String::from_utf8(decode_url_segment_bytes(input)?).ok()
}

/// Known file extensions for legacy decode
/// (`NameExtensionToContentType` table, extension part only). This mirrors
/// `ps-http`'s content-type table entry-for-entry (kept local: `ps-http`
/// depends on this crate, so it cannot be shared); keep the two in sync.
pub const KNOWN_EXTENSIONS: &[&str] = &[
    ".3gp", ".asf", ".avif", ".bin", ".css", ".flv", ".gif", ".htm", ".html", ".ico", ".jpeg",
    ".jpg", ".js", ".json", ".map", ".mov", ".mp3", ".mp4", ".mpg", ".ogg", ".pdf", ".png", ".svg",
    ".swf", ".txt", ".wav", ".webm", ".webp", ".wmv", ".xhtml", ".xml",
];

/// Whether a filename has a known extension (case-insensitive).
#[must_use]
pub fn has_known_extension(name: &str) -> bool {
    match name.rfind('.') {
        Some(pos) => KNOWN_EXTENSIONS
            .iter()
            .any(|ext| ext.eq_ignore_ascii_case(&name[pos..])),
        None => false,
    }
}

/// A decoded resource name (`ResourceNamer`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceNamer {
    name: String,
    id: String,
    hash: String,
    ext: String,
    options: String,
    experiment: String,
    signature: String,
}

impl ResourceNamer {
    /// Creates an empty namer.
    #[must_use]
    pub fn new() -> ResourceNamer {
        ResourceNamer::default()
    }

    /// Original resource name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Filter id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Content hash.
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }
    /// File extension.
    #[must_use]
    pub fn ext(&self) -> &str {
        &self.ext
    }
    /// Encoded options (unescaped form).
    #[must_use]
    pub fn options(&self) -> &str {
        &self.options
    }
    /// Experiment letter, if any.
    #[must_use]
    pub fn experiment(&self) -> &str {
        &self.experiment
    }
    /// URL signature, if any.
    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// Whether an experiment is set.
    #[must_use]
    pub fn has_experiment(&self) -> bool {
        !self.experiment.is_empty()
    }

    /// Whether options are set.
    #[must_use]
    pub fn has_options(&self) -> bool {
        !self.options.is_empty()
    }

    /// Sets the name.
    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_string();
    }
    /// Sets the filter id.
    pub fn set_id(&mut self, id: &str) {
        self.id = id.to_string();
    }
    /// Sets the hash.
    pub fn set_hash(&mut self, hash: &str) {
        self.hash = hash.to_string();
    }

    /// Clears the hash (`ClearHash`).
    pub fn clear_hash(&mut self) {
        self.hash.clear();
    }
    /// Sets the extension (undotted, enforced like the C++ `CHECK`).
    pub fn set_ext(&mut self, ext: &str) {
        assert!(
            ext.is_empty() || !ext.starts_with('.'),
            "extension must not start with a dot (C++ CHECK)"
        );
        self.ext = ext.to_string();
    }
    /// Sets the options.
    pub fn set_options(&mut self, options: &str) {
        self.options = options.to_string();
    }
    /// Sets the experiment letter.
    pub fn set_experiment(&mut self, experiment: &str) {
        self.experiment = experiment.to_string();
    }
    /// Sets the signature.
    pub fn set_signature(&mut self, signature: &str) {
        self.signature = signature.to_string();
    }

    /// Decodes, ignoring hash/signature lengths
    /// (`DecodeIgnoreHashAndSignature`).
    pub fn decode_ignore_hash_and_signature(&mut self, text: &str) -> bool {
        self.decode(text, -1, -1)
    }

    /// Decodes `name.pagespeed[.experiment|.options].id.hash[signature].ext`
    /// (`Decode`).
    ///
    /// When `signature_length > 0` and the hash segment is exactly
    /// `hash_length + signature_length` chars, it splits into hash and
    /// signature; otherwise the whole segment is the hash. Single
    /// lowercase-letter middles are experiments; anything else must
    /// URL-segment-decode as options. Shapes that miss the requirements
    /// fall back to [`ResourceNamer::legacy_decode`].
    pub fn decode(&mut self, text: &str, hash_length: i32, signature_length: i32) -> bool {
        // Split keeping empties (C++ passes omit_empty=false).
        let segments: Vec<&str> = text.split('.').collect();
        let mut system_index: Option<usize> = None;
        for (i, segment) in segments.iter().enumerate() {
            if *segment == SYSTEM_ID {
                system_index = Some(i);
                break;
            }
        }
        self.experiment.clear();
        self.options.clear();
        let Some(system) = system_index else {
            return self.legacy_decode(text);
        };
        let mut end = segments.len();
        if system < 1 || end - system < 4 {
            return self.legacy_decode(text);
        }
        self.name = segments[..system].join(".");
        let is_signed = signature_length > 0
            && segments[end - 2].len() as i32 == hash_length + signature_length;
        end -= 1;
        self.ext = segments[end].to_string();
        end -= 1;
        if is_signed {
            let segment = segments[end];
            let at = hash_length as usize;
            self.hash = segment[..at].to_string();
            self.signature = segment[at..].to_string();
        } else {
            // Unsigned: the whole segment is the hash and a stale
            // signature survives, exactly like the C++ (which only
            // assigns `signature_` on the signed path).
            self.hash = segments[end].to_string();
        }
        end -= 1;
        self.id = segments[end].to_string();
        let middle_start = system + 1;
        if middle_start < end {
            let middle = segments[middle_start..end].join(".");
            if middle.len() == 1 {
                let byte = middle.as_bytes()[0];
                if byte.is_ascii_lowercase() {
                    self.experiment = middle;
                } else {
                    return false; // invalid experiment
                }
            } else if middle.is_empty() {
                return false;
            } else {
                match decode_url_segment(&middle) {
                    Some(options) => self.options = options,
                    None => return false,
                }
            }
        }
        true
    }

    /// Legacy 4-segment decode (`LegacyDecode`): `id.hash.name.ext` with a
    /// 32-char hex hash and a known extension.
    pub fn legacy_decode(&mut self, text: &str) -> bool {
        if !has_known_extension(text) {
            return false;
        }
        // Legacy split omits empty segments.
        let segments: Vec<&str> = text.split('.').filter(|s| !s.is_empty()).collect();
        if segments.len() != 4 {
            return false;
        }
        let hash = segments[1];
        if hash.len() != 32 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        self.hash = hash.to_string();
        self.id = segments[0].to_string();
        self.name = segments[2].to_string();
        self.ext = segments[3].to_string();
        self.options.clear();
        self.experiment.clear();
        // A stale signature survives legacy decode, like the C++ (which
        // never assigns `signature_` on this path).
        true
    }

    /// Encodes (`Encode`); panics in debug on dotted components, like the
    /// C++ `DCHECK`s. Experiment and options are mutually exclusive.
    /// NOTE: the C++ `DCHECK(!hash_.empty())` is deliberately not
    /// mirrored: output sizing reads hashless URLs in real flows
    /// (matching C++ release behavior).
    #[must_use]
    pub fn encode(&self) -> String {
        debug_assert!(!self.id.contains('.'));
        debug_assert!(!self.hash.contains('.'));
        debug_assert!(!self.ext.contains('.'));
        debug_assert!(!self.experiment.contains('.'));
        debug_assert!(!self.signature.contains('.'));
        debug_assert!(!(self.has_experiment() && self.has_options()));
        let mut parts = vec![self.name.clone(), SYSTEM_ID.to_string()];
        if self.has_experiment() {
            parts.push(self.experiment.clone());
        } else if self.has_options() {
            parts.push(encode_url_segment(&self.options));
        }
        parts.push(self.id.clone());
        parts.push(format!("{}{}", self.hash, self.signature));
        parts.push(self.ext.clone());
        parts.join(".")
    }

    /// `id.name` encoding (`EncodeIdName`).
    #[must_use]
    pub fn encode_id_name(&self) -> String {
        assert!(
            !self.id.contains('.'),
            "id must not contain dots (C++ CHECK)"
        );
        format!("{}.{}", self.id, self.name)
    }

    /// Predicts the encoded length (`EventualSize`).
    #[must_use]
    pub fn eventual_size(&self, hasher: &dyn Hasher, signature_length: usize) -> usize {
        let mut size = self.name.len()
            + self.id.len()
            + self.ext.len()
            + OVERHEAD
            + hasher.hash_size_in_chars()
            + signature_length;
        if self.has_experiment() {
            size += 2;
        } else if self.has_options() {
            size += 1 + encode_url_segment(&self.options).len();
        }
        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Md5Hasher;

    #[test]
    fn golden_plain_url() {
        // Audited example: dotted name survives the round trip.
        let mut namer = ResourceNamer::new();
        assert!(namer.decode_ignore_hash_and_signature(
            "name.ext.as.many.as.I.like.pagespeed.jm.0123456789abcdef.js"
        ));
        assert_eq!(namer.name(), "name.ext.as.many.as.I.like");
        assert_eq!(namer.id(), "jm");
        assert_eq!(namer.hash(), "0123456789abcdef");
        assert_eq!(namer.ext(), "js");
        assert!(!namer.has_experiment());
        assert!(!namer.has_options());
        assert_eq!(
            namer.encode(),
            "name.ext.as.many.as.I.like.pagespeed.jm.0123456789abcdef.js"
        );
        assert_eq!(namer.encode_id_name(), "jm.name.ext.as.many.as.I.like");
    }

    #[test]
    fn golden_experiment_url() {
        let mut namer = ResourceNamer::new();
        assert!(namer.decode_ignore_hash_and_signature("photo.jpg.pagespeed.q.ic.ABCDEF1234.jpg"));
        assert_eq!(namer.experiment(), "q");
        assert_eq!(namer.id(), "ic");
        assert_eq!(namer.encode(), "photo.jpg.pagespeed.q.ic.ABCDEF1234.jpg");
        // Non-lowercase single letters are invalid experiments.
        let mut namer = ResourceNamer::new();
        assert!(!namer.decode_ignore_hash_and_signature("photo.jpg.pagespeed.Q.ic.ABCDEF1234.jpg"));
    }

    #[test]
    fn golden_options_url() {
        let mut namer = ResourceNamer::new();
        assert!(namer.decode_ignore_hash_and_signature("a.css.pagespeed.a=b.cf.HASH1234.css"));
        assert_eq!(namer.options(), "a=b");
        assert_eq!(namer.encode(), "a.css.pagespeed.a=b.cf.HASH1234.css");
        // Slash-escaped options from the audit.
        let mut namer = ResourceNamer::new();
        assert!(namer.decode_ignore_hash_and_signature(
            "n.css.pagespeed.options,_with,_slashes.cf.HASH.css"
        ));
        assert_eq!(namer.options(), "options/with/slashes");
    }

    #[test]
    fn golden_cache_extender_shape() {
        let mut namer = ResourceNamer::new();
        assert!(namer.decode_ignore_hash_and_signature("x.html.pagespeed.ce.HASHVALUE1.html"));
        assert_eq!(namer.id(), "ce");
        assert_eq!(namer.ext(), "html");
    }

    #[test]
    fn signature_splitting() {
        let mut namer = ResourceNamer::new();
        assert!(namer.decode("a.js.pagespeed.jm.HASH1234SIG.us", 8, 3));
        assert_eq!(namer.hash(), "HASH1234");
        assert_eq!(namer.signature(), "SIG");
        // Wrong combined length: whole segment stays the hash.
        let mut namer = ResourceNamer::new();
        assert!(namer.decode("a.js.pagespeed.jm.HASH1234SIG.us", 8, 5));
        assert_eq!(namer.hash(), "HASH1234SIG");
        assert_eq!(namer.signature(), "");
    }

    #[test]
    fn stale_signature_survives_unsigned_and_legacy_decode() {
        // The C++ assigns `signature_` only on the signed path; reused
        // namers keep a stale signature through every other decode.
        let mut namer = ResourceNamer::new();
        namer.set_signature("STALE");
        assert!(namer.decode("a.js.pagespeed.jm.HASH.us", 4, 0));
        assert_eq!(namer.signature(), "STALE");
        let hash = "0123456789abcdef0123456789ABCDEF";
        assert!(namer.decode(&format!("cc.{hash}.style.css"), 10, 0));
        assert_eq!(namer.signature(), "STALE");
    }

    #[test]
    fn legacy_decode() {
        let mut namer = ResourceNamer::new();
        let hash = "0123456789abcdef0123456789ABCDEF";
        assert!(namer.decode(&format!("cc.{hash}.style.css"), 10, 0));
        assert_eq!(namer.id(), "cc");
        assert_eq!(namer.hash(), hash);
        assert_eq!(namer.name(), "style");
        assert_eq!(namer.ext(), "css");
        // Short/test hashes rejected (issue 688 log-spam guard).
        let mut namer = ResourceNamer::new();
        assert!(!namer.decode("cc.SHORT.style.css", 10, 0));
        // Unknown extension rejected.
        let mut namer = ResourceNamer::new();
        assert!(!namer.decode(&format!("cc.{hash}.style.bogus"), 10, 0));
        // Wrong segment count rejected.
        let mut namer = ResourceNamer::new();
        assert!(!namer.decode(&format!("cc.{hash}.a.style.css"), 10, 0));
    }

    #[test]
    fn escaper_goldens() {
        assert_eq!(encode_url_segment("a=b"), "a=b");
        assert_eq!(encode_url_segment("a/b"), "a,_b");
        assert_eq!(encode_url_segment("http://x/y"), ",hx,_y");
        assert_eq!(encode_url_segment("a.pagespeed.b"), "a,Mb");
        assert_eq!(encode_url_segment("a?b&c"), "a,qb,ac");
        assert_eq!(encode_url_segment("100%"), "100,P");
        assert_eq!(encode_url_segment("^"), ",u");
        assert_eq!(encode_url_segment("\\"), ",-");
        assert_eq!(decode_url_segment("a,_b"), Some("a/b".to_string()));
        assert_eq!(decode_url_segment(",hx"), Some("http://x".to_string()));
        assert_eq!(
            decode_url_segment("a,Mb"),
            Some("a.pagespeed.b".to_string())
        );
        // Legacy November-2010 codes still decode.
        assert_eq!(decode_url_segment(",w"), Some("www.".to_string()));
        assert_eq!(decode_url_segment("a,px"), Some("a.pngx".to_string()));
        assert_eq!(decode_url_segment(",t"), Some(".html".to_string()));
        // %XX escapes, with the space-means-plus corner.
        assert_eq!(decode_url_segment("%41"), Some("A".to_string()));
        assert_eq!(decode_url_segment("%20"), Some("+".to_string()));
        // Malformed inputs rejected.
        assert_eq!(decode_url_segment("a,"), None);
        assert_eq!(decode_url_segment(",z"), None);
        assert_eq!(decode_url_segment("%4"), None);
        assert_eq!(decode_url_segment("a b"), None);
    }

    #[test]
    fn eventual_size_matches_encode() {
        let hasher = Md5Hasher::new();
        let mut namer = ResourceNamer::new();
        namer.set_name("style.css");
        namer.set_id("cf");
        namer.set_hash(&hasher.hash(b"body"));
        namer.set_ext("css");
        assert_eq!(namer.encode().len(), namer.eventual_size(&hasher, 0));
        namer.set_experiment("b");
        namer.set_hash(&hasher.hash(b"body"));
        assert_eq!(namer.encode().len(), namer.eventual_size(&hasher, 0));
        namer.set_experiment("");
        namer.set_options("a/b?c");
        assert_eq!(namer.encode().len(), namer.eventual_size(&hasher, 0));
        // Signature length counted verbatim.
        namer.set_signature("SIG");
        assert_eq!(namer.encode().len(), namer.eventual_size(&hasher, 3));
    }

    /// Deterministic xorshift64 for dependency-free round-trip fuzzing.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn text(&mut self, len: usize, alphabet: &[u8]) -> String {
            (0..len)
                .map(|_| alphabet[self.next() as usize % alphabet.len()] as char)
                .collect()
        }
    }

    #[test]
    fn round_trip_property() {
        let mut rng = Rng(0x1234_5678_9abc_def0);
        for _ in 0..2000 {
            let name_len = 1 + rng.next() as usize % 24;
            let name = rng.text(name_len, b"abZ019.-_");
            let id_len = 1 + rng.next() as usize % 4;
            let id = rng.text(id_len, b"abcdefghij");
            let hash = rng.text(10, b"ABCDEF0123456789");
            let exts = ["css", "js", "jpg", "png", "html"];
            let ext = exts[rng.next() as usize % exts.len()];
            let mut namer = ResourceNamer::new();
            namer.set_name(&name);
            namer.set_id(&id);
            namer.set_hash(&hash);
            namer.set_ext(ext);
            let which = rng.next() % 3;
            // Single-char middles decode as experiments by design, so
            // fuzz options at length >= 2 only.
            if which == 0 {
                namer.set_experiment("q");
            } else if which == 1 {
                let options_len = 2 + rng.next() as usize % 30;
                let options = rng.text(options_len, b"ab/=?&%.,-_:");
                namer.set_options(&options);
            }
            let encoded = namer.encode();
            let mut back = ResourceNamer::new();
            assert!(
                back.decode_ignore_hash_and_signature(&encoded),
                "decode failed: {encoded}"
            );
            assert_eq!(back.name(), namer.name());
            assert_eq!(back.id(), namer.id());
            assert_eq!(back.hash(), namer.hash());
            assert_eq!(back.ext(), namer.ext());
            assert_eq!(back.options(), namer.options());
            assert_eq!(back.experiment(), namer.experiment());
        }
        // Escaper round-trips over hostile bytes, ASCII and binary.
        for _ in 0..2000 {
            let len = rng.next() as usize % 32;
            let bytes: Vec<u8> = (0..len).map(|_| (rng.next() % 255 + 1) as u8).collect();
            let encoded = encode_url_segment_bytes(&bytes);
            assert!(encoded.is_ascii());
            assert_eq!(
                decode_url_segment_bytes(&encoded).as_deref(),
                Some(bytes.as_slice())
            );
        }
        // Non-UTF8 blobs fail the string form but keep the bytes form.
        assert_eq!(decode_url_segment(",FF"), None);
        assert_eq!(
            decode_url_segment_bytes(",FF").as_deref(),
            Some([0xff].as_slice())
        );
    }
}

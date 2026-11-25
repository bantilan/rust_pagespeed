//! Font preload evidence store (metadata-cache codec).
//!
//! Versioned blobs shared by the filter (reads decisions, writes
//! discoveries) and the beacon merge (writes confirmations):
//! per-page preload decisions keyed by page URL plus options
//! signature, and per-page beacon-confirmed font sets. Corrupt or
//! foreign-version blobs fail closed to recompute.
//!
//! Key layout mirrors the beacon finder (`page_url`, options hash,
//! device suffix); values are line-based `name/1` payloads with
//! strict caps, like the sizing codec.

use std::collections::HashSet;

use crate::font_discovery::{FontFace, FontPreload, FontSource, FontStyle, FontUsage};

/// Decision blob version tag.
const DECISION_CODEC: &str = "font-preloads/3";
/// Confirmed-set blob version tag.
const CONFIRMED_CODEC: &str = "font-confirmed/1";
/// Max cached preloads per page (matches the selection cap with
/// headroom for in-flight beacon updates).
const MAX_CACHED_PRELOADS: usize = 8;
/// Max confirmed triples per page.
const MAX_CONFIRMED: usize = 64;
/// Max blob bytes accepted (generous: URLs dominate).
const MAX_BLOB_BYTES: usize = 16 * 1024;
/// Max single URL bytes in a blob.
const MAX_URL_BYTES: usize = 2048;

/// Cache key namespace for page font decisions.
pub fn decision_key(page_url: &str, options_hash: &str, device_suffix: &str) -> String {
    format!("font-decision-v1:{options_hash}:{device_suffix}:{page_url}")
}

/// Cache key namespace for parsed stylesheet faces.
pub fn css_faces_key(css_url: &str) -> String {
    format!("font-css-v1:{css_url}")
}

/// Stylesheet faces freshness (origin revalidation happens on the
/// fetch path; this bounds how long parsed faces serve outright).
pub const CSS_FACES_TTL_MS: i64 = 3_600_000;

/// Cache key namespace for beacon-confirmed font sets.
pub fn confirmed_key(page_url: &str, options_hash: &str, device_suffix: &str) -> String {
    format!("font-confirmed-v1:{options_hash}:{device_suffix}:{page_url}")
}

/// Cache key namespace for outstanding font beacon nonces.
pub fn nonces_key(page_url: &str, options_hash: &str, device_suffix: &str) -> String {
    format!("font-nonces-v1:{options_hash}:{device_suffix}:{page_url}")
}

/// Pending font nonces: bounded (nonce, issued-ms) pairs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontNonces {
    /// Outstanding nonces in issue order.
    pub pending: Vec<(String, i64)>,
}

/// Nonce blob version tag.
const NONCES_CODEC: &str = "font-nonces/1";
/// Pending-nonce capacity (oldest evicted past it).
const MAX_PENDING_FONT_NONCES: usize = 16;
/// Nonce lifetime in ms (mirrors the image beacon timeout).
pub const FONT_NONCE_TIMEOUT_MS: i64 = 60_000;

/// Encodes pending nonces.
#[must_use]
pub fn encode_nonces(nonces: &FontNonces) -> String {
    let mut out = format!("{NONCES_CODEC}\n{}\n", nonces.pending.len());
    for (nonce, issued_ms) in &nonces.pending {
        out.push_str(&nonce.replace(['\n', '\r'], ""));
        out.push('\n');
        out.push_str(&issued_ms.to_string());
        out.push('\n');
    }
    out
}

/// Decodes pending nonces; corrupt payloads fail closed to empty
/// (unknown nonces simply never validate).
#[must_use]
pub fn decode_nonces(input: &str) -> FontNonces {
    let mut out = FontNonces::default();
    if input.len() > MAX_BLOB_BYTES || !input.ends_with('\n') {
        return out;
    }
    let mut lines = input.lines();
    if lines.next() != Some(NONCES_CODEC) {
        return out;
    }
    let count: usize = lines.next().and_then(|line| line.parse().ok()).unwrap_or(0);
    if count > MAX_PENDING_FONT_NONCES {
        return out;
    }
    for _ in 0..count {
        let (Some(nonce), Some(issued)) = (lines.next(), lines.next()) else {
            return FontNonces::default();
        };
        if nonce.is_empty() || nonce.len() > 64 {
            return FontNonces::default();
        }
        let Ok(issued_ms) = issued.parse::<i64>() else {
            return FontNonces::default();
        };
        out.pending.push((nonce.to_string(), issued_ms));
    }
    if lines.next().is_some() {
        return FontNonces::default();
    }
    out
}

/// Issues a nonce: drops expired entries, evicting the oldest past
/// capacity (whose beacon is least likely to still arrive).
pub fn issue_nonce(nonces: &mut FontNonces, nonce: String, now_ms: i64) {
    nonces
        .pending
        .retain(|(_, issued_ms)| now_ms - issued_ms <= FONT_NONCE_TIMEOUT_MS);
    if nonces.pending.len() >= MAX_PENDING_FONT_NONCES {
        nonces.pending.remove(0);
    }
    nonces.pending.push((nonce, now_ms));
}

/// Validates and consumes a nonce (constant-time compare).
#[must_use]
pub fn consume_nonce(nonces: &mut FontNonces, nonce: &str, now_ms: i64) -> bool {
    if nonce.is_empty() || nonce.len() > 64 {
        return false;
    }
    nonces
        .pending
        .retain(|(_, issued_ms)| now_ms - issued_ms <= FONT_NONCE_TIMEOUT_MS);
    if let Some(slot) = nonces.pending.iter().position(|(pending, _)| {
        pending.len() == nonce.len()
            && pending
                .bytes()
                .zip(nonce.bytes())
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                == 0
    }) {
        nonces.pending.remove(slot);
        return true;
    }
    false
}

/// One cached decision: what to preload, under which inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontDecision {
    /// Fingerprint of the CSS inputs (inline text + external URLs).
    pub css_fingerprint: u64,
    /// Beacon generation the decision was computed under.
    pub beacon_generation: u64,
    /// Decided preloads in emission order.
    pub preloads: Vec<FontPreload>,
    /// Whether some matched usage was still unconfirmed: a cache
    /// hit must keep shipping the learning beacon, or a lost
    /// first-visit beacon would stall learning forever.
    pub needs_beacon: bool,
    /// Latest `now_ms` this decision may be replayed. Decisions
    /// stored while external sheets were still loading expire
    /// quickly so later visits pick the sheets up; complete
    /// decisions carry `i64::MAX`.
    pub revalidate_after_ms: i64,
}

/// FNV-1a 64-bit fingerprint (stable across processes).
#[must_use]
pub fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Encodes a decision (`font-preloads/1`, strict fields).
#[must_use]
pub fn encode_decision(decision: &FontDecision) -> String {
    let mut out = format!(
        "{DECISION_CODEC}\n{:016x}\n{}\n{}\n{}\n",
        decision.css_fingerprint,
        decision.beacon_generation,
        i64::from(decision.needs_beacon),
        decision.revalidate_after_ms
    );
    for preload in decision.preloads.iter().take(MAX_CACHED_PRELOADS) {
        out.push_str(&preload.url.replace(['\n', '\r'], ""));
        out.push('\n');
        out.push_str(&preload.mime);
        out.push('\n');
    }
    out
}

/// Decodes a decision; corrupt/oversized/foreign payloads fail closed.
#[must_use]
pub fn decode_decision(input: &str) -> Option<FontDecision> {
    if input.len() > MAX_BLOB_BYTES || !input.ends_with('\n') {
        return None;
    }
    let mut lines = input.lines();
    if lines.next()? != DECISION_CODEC {
        return None;
    }
    let css_fingerprint = u64::from_str_radix(lines.next()?, 16).ok()?;
    let beacon_generation: u64 = lines.next()?.parse().ok()?;
    let needs_beacon = match lines.next()? {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    let revalidate_after_ms: i64 = lines.next()?.parse().ok()?;
    let mut preloads = Vec::new();
    loop {
        let Some(url) = lines.next() else { break };
        let Some(mime) = lines.next() else {
            return None;
        };
        if url.is_empty() || url.len() > MAX_URL_BYTES || mime.is_empty() || mime.len() > 64 {
            return None;
        }
        if preloads.len() == MAX_CACHED_PRELOADS {
            return None;
        }
        preloads.push(FontPreload {
            url: url.to_string(),
            mime: mime.to_string(),
        });
    }
    Some(FontDecision {
        css_fingerprint,
        beacon_generation,
        preloads,
        needs_beacon,
        revalidate_after_ms,
    })
}

/// Encodes a confirmed (family, weight, style) set.
#[must_use]
pub fn encode_confirmed(confirmed: &HashSet<(String, u32, FontStyle)>) -> String {
    let mut triples: Vec<(&str, u32, FontStyle)> = confirmed
        .iter()
        .map(|(family, weight, style)| (family.as_str(), *weight, *style))
        .collect();
    triples.sort();
    let mut out = format!("{CONFIRMED_CODEC}\n{}\n", triples.len().min(MAX_CONFIRMED));
    for (family, weight, style) in triples.into_iter().take(MAX_CONFIRMED) {
        let style_byte = match style {
            FontStyle::Normal => "n",
            FontStyle::Italic => "i",
            FontStyle::Oblique => "o",
        };
        out.push_str(&family.replace(['\n', '\r', '\t'], ""));
        out.push('\t');
        out.push_str(&weight.to_string());
        out.push('\t');
        out.push_str(style_byte);
        out.push('\n');
    }
    out
}

/// Decodes a confirmed set; corrupt payloads fail closed to empty
/// (unknown confirmations must never enable preloads).
#[must_use]
pub fn decode_confirmed(input: &str) -> HashSet<(String, u32, FontStyle)> {
    let mut out = HashSet::new();
    if input.len() > MAX_BLOB_BYTES || !input.ends_with('\n') {
        return out;
    }
    let mut lines = input.lines();
    if lines.next() != Some(CONFIRMED_CODEC) {
        return out;
    }
    let count: usize = lines.next().and_then(|line| line.parse().ok()).unwrap_or(0);
    if count > MAX_CONFIRMED {
        return out;
    }
    for _ in 0..count {
        let Some(line) = lines.next() else {
            return HashSet::new();
        };
        let mut parts = line.split('\t');
        let (Some(family), Some(weight), Some(style)) = (parts.next(), parts.next(), parts.next())
        else {
            return HashSet::new();
        };
        if parts.next().is_some() || family.is_empty() || family.len() > 256 {
            return HashSet::new();
        }
        let weight: u32 = match weight.parse() {
            Ok(weight) if (1..=1000).contains(&weight) => weight,
            _ => return HashSet::new(),
        };
        let style = match style {
            "n" => FontStyle::Normal,
            "i" => FontStyle::Italic,
            "o" => FontStyle::Oblique,
            _ => return HashSet::new(),
        };
        out.insert((family.to_string(), weight, style));
    }
    if lines.next().is_some() {
        return HashSet::new();
    }
    out
}

/// One stylesheet's parsed faces plus fetch time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedCssFaces {
    /// Fetch time in ms (freshness against [`CSS_FACES_TTL_MS`]).
    pub fetched_ms: i64,
    /// Faces in document order (URLs as authored).
    pub faces: Vec<FontFace>,
    /// Raw `@import` URLs for nested resolution.
    pub imports: Vec<String>,
    /// Font usages in document order (deduplicated per sheet).
    pub usages: Vec<FontUsage>,
}

/// Stylesheet faces blob version tag.
const CSS_FACES_CODEC: &str = "font-css-faces/1";
/// Max faces per cached stylesheet.
const MAX_CACHED_FACES: usize = 64;
/// Max usages per cached stylesheet.
const MAX_CACHED_USAGES: usize = 128;

/// Encodes cached stylesheet faces.
#[must_use]
pub fn encode_css_faces(cached: &CachedCssFaces) -> String {
    let mut out = format!("{CSS_FACES_CODEC}\n{}\n", cached.fetched_ms);
    out.push_str(&format!("{}\n", cached.faces.len().min(MAX_CACHED_FACES)));
    for face in cached.faces.iter().take(MAX_CACHED_FACES) {
        let style_byte = match face.style {
            FontStyle::Normal => "n",
            FontStyle::Italic => "i",
            FontStyle::Oblique => "o",
        };
        out.push_str(&face.family.replace(['\n', '\r', '\t'], ""));
        out.push('\n');
        out.push_str(&format!("{} {}\n", face.weight_min, face.weight_max));
        out.push_str(style_byte);
        out.push('\n');
        out.push_str(&format!("{}\n", face.sources.len()));
        for source in &face.sources {
            out.push_str(&source.url.replace(['\n', '\r'], ""));
            out.push('\n');
            out.push_str(source.format.as_deref().unwrap_or(""));
            out.push('\n');
        }
        out.push_str(&format!("{}\n", face.ranges.len()));
        for (lo, hi) in &face.ranges {
            out.push_str(&format!("{lo} {hi}\n"));
        }
    }
    out.push_str(&format!(
        "{}\n",
        cached
            .imports
            .len()
            .min(crate::font_discovery::MAX_IMPORTS_PER_STYLESHEET)
    ));
    for import in cached
        .imports
        .iter()
        .take(crate::font_discovery::MAX_IMPORTS_PER_STYLESHEET)
    {
        out.push_str(&import.replace(['\n', '\r'], ""));
        out.push('\n');
    }
    out.push_str(&format!("{}\n", cached.usages.len().min(MAX_CACHED_USAGES)));
    for usage in cached.usages.iter().take(MAX_CACHED_USAGES) {
        let style_byte = match usage.style {
            FontStyle::Normal => "n",
            FontStyle::Italic => "i",
            FontStyle::Oblique => "o",
        };
        out.push_str(&usage.family.replace(['\n', '\r', '\t'], ""));
        out.push('\t');
        out.push_str(&usage.weight.to_string());
        out.push('\t');
        out.push_str(style_byte);
        out.push('\n');
    }
    out
}

/// Decodes cached stylesheet faces; corrupt payloads fail closed.
#[must_use]
pub fn decode_css_faces(input: &str) -> Option<CachedCssFaces> {
    if input.len() > MAX_BLOB_BYTES || !input.ends_with('\n') {
        return None;
    }
    let mut lines = input.lines();
    if lines.next()? != CSS_FACES_CODEC {
        return None;
    }
    let fetched_ms: i64 = lines.next()?.parse().ok()?;
    let nfaces: usize = lines.next()?.parse().ok()?;
    if nfaces > MAX_CACHED_FACES {
        return None;
    }
    let mut faces = Vec::with_capacity(nfaces);
    for _ in 0..nfaces {
        let family = lines.next()?;
        if family.is_empty() || family.len() > 256 {
            return None;
        }
        let weights = lines.next()?;
        let mut weights = weights.split(' ');
        let (Some(wmin), Some(wmax), None) = (weights.next(), weights.next(), weights.next())
        else {
            return None;
        };
        let (wmin, wmax): (u32, u32) = (wmin.parse().ok()?, wmax.parse().ok()?);
        if wmin == 0 || wmax == 0 || wmin > 1000 || wmax > 1000 || wmin > wmax {
            return None;
        }
        let style = match lines.next()? {
            "n" => FontStyle::Normal,
            "i" => FontStyle::Italic,
            "o" => FontStyle::Oblique,
            _ => return None,
        };
        let nsources: usize = lines.next()?.parse().ok()?;
        if nsources == 0 || nsources > 16 {
            return None;
        }
        let mut sources = Vec::with_capacity(nsources);
        for _ in 0..nsources {
            let url = lines.next()?;
            let format = lines.next()?;
            if url.is_empty() || url.len() > MAX_URL_BYTES || format.len() > 32 {
                return None;
            }
            sources.push(FontSource {
                url: url.to_string(),
                format: if format.is_empty() {
                    None
                } else {
                    Some(format.to_string())
                },
            });
        }
        let nranges: usize = lines.next()?.parse().ok()?;
        if nranges > 64 {
            return None;
        }
        let mut ranges = Vec::with_capacity(nranges);
        for _ in 0..nranges {
            let line = lines.next()?;
            let mut parts = line.split(' ');
            let (Some(lo), Some(hi), None) = (parts.next(), parts.next(), parts.next()) else {
                return None;
            };
            let (lo, hi): (u32, u32) = (lo.parse().ok()?, hi.parse().ok()?);
            if lo > hi || hi > 0x10FFFF {
                return None;
            }
            ranges.push((lo, hi));
        }
        faces.push(FontFace {
            family: family.to_string(),
            weight_min: wmin,
            weight_max: wmax,
            style,
            sources,
            ranges,
        });
    }
    let nimports: usize = lines.next()?.parse().ok()?;
    if nimports > crate::font_discovery::MAX_IMPORTS_PER_STYLESHEET {
        return None;
    }
    let mut imports = Vec::with_capacity(nimports);
    for _ in 0..nimports {
        let import = lines.next()?;
        if import.is_empty() || import.len() > MAX_URL_BYTES {
            return None;
        }
        imports.push(import.to_string());
    }
    let nusages: usize = lines.next()?.parse().ok()?;
    if nusages > MAX_CACHED_USAGES {
        return None;
    }
    let mut usages = Vec::with_capacity(nusages);
    for _ in 0..nusages {
        let line = lines.next()?;
        let mut parts = line.split('\t');
        let (Some(family), Some(weight), Some(style), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        if family.is_empty() || family.len() > 256 {
            return None;
        }
        let weight: u32 = match weight.parse() {
            Ok(weight) if (1..=1000).contains(&weight) => weight,
            _ => return None,
        };
        let style = match style {
            "n" => FontStyle::Normal,
            "i" => FontStyle::Italic,
            "o" => FontStyle::Oblique,
            _ => return None,
        };
        usages.push(FontUsage {
            family: family.to_string(),
            weight,
            style,
        });
    }
    if lines.next().is_some() {
        return None;
    }
    Some(CachedCssFaces {
        fetched_ms,
        faces,
        imports,
        usages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision() -> FontDecision {
        FontDecision {
            css_fingerprint: 0x1234abcd,
            beacon_generation: 7,
            needs_beacon: true,
            revalidate_after_ms: i64::MAX,
            preloads: vec![
                FontPreload {
                    url: "https://x.test/a.woff2".to_string(),
                    mime: "font/woff2".to_string(),
                },
                FontPreload {
                    url: "https://x.test/b.woff2".to_string(),
                    mime: "font/woff2".to_string(),
                },
            ],
        }
    }

    #[test]
    fn decision_round_trips() {
        let decoded = decode_decision(&encode_decision(&decision())).expect("decode");
        assert_eq!(decoded, decision());
    }

    #[test]
    fn decision_rejects_corrupt() {
        assert!(decode_decision("").is_none());
        assert!(decode_decision("font-preloads/3\n0\n0\n").is_none());
        assert!(decode_decision("font-preloads/3\nZZZ\n0\n1\n0\n").is_none());
        assert!(decode_decision("font-preloads/3\n00\nnot-a-number\n1\n0\n").is_none());
        assert!(decode_decision("font-preloads/3\n00\n0\nmaybe\n0\n").is_none());
        assert!(decode_decision("font-preloads/3\n00\n0\n1\nsoon\n").is_none());
        // Stale v1/v2 blobs fail closed (recomputed on next miss).
        assert!(decode_decision("font-preloads/1\n00\n0\n").is_none());
        assert!(decode_decision("font-preloads/2\n00\n0\n1\n").is_none());
        // Odd line count (dangling URL).
        assert!(decode_decision("font-preloads/3\n00\n0\n1\n0\nhttps://x.test/a\n").is_none());
        // Oversized URL.
        let big = format!(
            "font-preloads/1\n00\n0\nhttps://x.test/{}\nfont/woff2\n",
            "a".repeat(3000)
        );
        assert!(decode_decision(&big).is_none());
        assert!(decode_decision(&"x".repeat(MAX_BLOB_BYTES + 1)).is_none());
    }

    #[test]
    fn confirmed_round_trips() {
        let mut set = HashSet::new();
        set.insert(("manrope".to_string(), 400, FontStyle::Normal));
        set.insert(("icons".to_string(), 700, FontStyle::Italic));
        assert_eq!(decode_confirmed(&encode_confirmed(&set)), set);
    }

    #[test]
    fn confirmed_fails_closed_to_empty() {
        assert!(decode_confirmed("").is_empty());
        assert!(decode_confirmed("font-confirmed/2\n0\n").is_empty());
        assert!(decode_confirmed("font-confirmed/1\n99\n").is_empty());
        assert!(decode_confirmed("font-confirmed/1\n1\nmanrope\t400\n").is_empty());
        assert!(decode_confirmed("font-confirmed/1\n1\nmanrope\t0\tn\n").is_empty());
        assert!(decode_confirmed("font-confirmed/1\n1\nmanrope\t400\tx\n").is_empty());
        assert!(decode_confirmed("font-confirmed/1\n1\nmanrope\t400\tn\nextra\n").is_empty());
    }

    fn cached_faces() -> CachedCssFaces {
        CachedCssFaces {
            fetched_ms: 1_700_000_000_000,
            imports: vec!["https://x.test/nested.css".to_string()],
            usages: vec![FontUsage {
                family: "manrope".to_string(),
                weight: 700,
                style: FontStyle::Normal,
            }],
            faces: vec![
                crate::font_discovery::FontFace {
                    family: "manrope".to_string(),
                    weight_min: 400,
                    weight_max: 700,
                    style: crate::font_discovery::FontStyle::Normal,
                    sources: vec![
                        crate::font_discovery::FontSource {
                            url: "https://x.test/a.woff2".to_string(),
                            format: Some("woff2".to_string()),
                        },
                        crate::font_discovery::FontSource {
                            url: "/fonts/a.woff".to_string(),
                            format: None,
                        },
                    ],
                    ranges: vec![(0, 255)],
                },
                crate::font_discovery::FontFace {
                    family: "icons".to_string(),
                    weight_min: 400,
                    weight_max: 400,
                    style: crate::font_discovery::FontStyle::Normal,
                    sources: vec![crate::font_discovery::FontSource {
                        url: "/fonts/i.woff2".to_string(),
                        format: Some("woff2".to_string()),
                    }],
                    ranges: vec![(0xE000, 0xF8FF)],
                },
            ],
        }
    }

    #[test]
    fn css_faces_round_trips() {
        let decoded = decode_css_faces(&encode_css_faces(&cached_faces())).expect("decode");
        assert_eq!(decoded, cached_faces());
    }

    #[test]
    fn css_faces_rejects_corrupt() {
        assert!(decode_css_faces("").is_none());
        assert!(decode_css_faces("font-css-faces/2\n0\n").is_none());
        assert!(decode_css_faces("font-css-faces/1\nnot-a-number\n").is_none());
        assert!(decode_css_faces("font-css-faces/1\n0\nfamily-only\n").is_none());
        assert!(decode_css_faces("font-css-faces/1\n0\nfam\n400\nn\n1\n").is_none());
    }

    #[test]
    fn nonces_round_trip_issue_consume_expire() {
        let mut nonces = FontNonces::default();
        issue_nonce(&mut nonces, "abc123".to_string(), 1000);
        issue_nonce(&mut nonces, "def456".to_string(), 2000);
        let decoded = decode_nonces(&encode_nonces(&nonces));
        assert_eq!(decoded, nonces);
        assert!(consume_nonce(&mut nonces, "abc123", 3000));
        assert!(!consume_nonce(&mut nonces, "abc123", 3000));
        assert!(!consume_nonce(&mut nonces, "nope", 3000));
        assert!(!consume_nonce(&mut nonces, "", 3000));
        // Expiry (60s timeout).
        assert!(!consume_nonce(
            &mut nonces,
            "def456",
            2000 + FONT_NONCE_TIMEOUT_MS + 1
        ));
        // Corrupt blobs fail closed.
        assert!(decode_nonces("").pending.is_empty());
        assert!(decode_nonces("font-nonces/2\n0\n").pending.is_empty());
        assert!(decode_nonces("font-nonces/1\n99\n").pending.is_empty());
    }

    #[test]
    fn nonce_issue_evicts_oldest_past_capacity() {
        let mut nonces = FontNonces::default();
        for index in 0..(MAX_PENDING_FONT_NONCES + 3) {
            issue_nonce(&mut nonces, format!("n{index:03}"), 1000);
        }
        assert_eq!(nonces.pending.len(), MAX_PENDING_FONT_NONCES);
        assert_eq!(nonces.pending[0].0, "n003");
    }

    #[test]
    fn keys_namespace_page_options_device() {
        let key = decision_key("https://h/page", "abc", "mob");
        assert!(key.contains("https://h/page") && key.contains("abc") && key.contains("mob"));
        assert_ne!(
            decision_key("https://h/a", "abc", "mob"),
            decision_key("https://h/b", "abc", "mob")
        );
        assert_ne!(decision_key("u", "o", "d"), confirmed_key("u", "o", "d"));
    }

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(fingerprint_bytes(b"abc"), fingerprint_bytes(b"abc"));
        assert_ne!(fingerprint_bytes(b"abc"), fingerprint_bytes(b"abd"));
    }
}

//! Bounded measured viewport evidence for future automatic eager/LCP sizing.
//! No I/O or HTML mutation. Callers must authenticate reports, scope identities,
//! enforce page-wide caps/TTL, and commit through the shared metadata cache.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Measurement-only browser collector. Producer injection is intentionally
/// separate so registration/cache gates must succeed before activation.
pub const COLLECTOR_JS: &str = include_str!("../assets/automatic_sizes_collector.js");

/// Page-wide limits for automatic measurement registration and reports.
pub const MAX_PLACEMENTS: usize = 64;
pub const MAX_REPORT_BYTES: usize = 8192;

/// Maximum original HTML bytes fingerprinted for learned sizing. Oversize
/// documents fall back; this cap does not restrict the ordinary HTML rewriter.
pub const MAX_LAYOUT_BYTES: usize = 8 * 1024 * 1024;

/// Incremental original-document fingerprint. No retained HTML buffer; chunk
/// boundaries do not affect identity. Caller feeds original input, never the
/// optimized output or injected nonce/script markup.
pub struct LayoutFingerprint {
    digest: Option<Sha256>,
    bytes: usize,
}

impl Default for LayoutFingerprint {
    fn default() -> Self {
        let mut digest = Sha256::new();
        digest.update(b"pagespeed-automatic-sizes-document/1\0");
        Self {
            digest: Some(digest),
            bytes: 0,
        }
    }
}

impl LayoutFingerprint {
    /// False permanently disables this document's learned sizing on overflow.
    pub fn update(&mut self, original: &[u8]) -> bool {
        if original.len() > MAX_LAYOUT_BYTES.saturating_sub(self.bytes) {
            self.digest = None;
            return false;
        }
        let Some(digest) = self.digest.as_mut() else {
            return false;
        };
        digest.update(original);
        self.bytes += original.len();
        true
    }

    /// Only complete original documents may provide identities. A streaming
    /// flush before completion must not consume an intermediate digest.
    #[must_use]
    pub fn finish(self) -> Option<[u8; 32]> {
        if self.bytes == 0 {
            return None;
        }
        Some(self.digest?.finalize().into())
    }
}

/// Server-supplied scope from original content, before rewrite transforms.
/// Digests/epoch must come from trusted document/style state. This structure
/// does not fetch stylesheets or claim that external CSS changes were checked.
pub struct PlacementScope<'a> {
    pub page_url: &'a str,
    pub options_signature: &'a str,
    pub document_digest: [u8; 32],
    pub stylesheet_epoch: [u8; 32],
}

/// Derives one bounded identity. A digest is not an authorization token: the
/// receiver must match the nonce's issued manifest before accepting any data.
/// Ordinal is the original document-order eligible-image position (1..=64),
/// not a browser-provided index or the image URL hash.
#[must_use]
pub fn placement_identity(
    scope: &PlacementScope<'_>,
    ordinal: u32,
    original_src: &[u8],
    original_srcset: &[u8],
    original_sizes: &[u8],
) -> Option<String> {
    if scope.page_url.is_empty()
        || scope.page_url.len() > 8192
        || scope.options_signature.is_empty()
        || scope.options_signature.len() > 16384
        || !(1..=MAX_PLACEMENTS as u32).contains(&ordinal)
        || original_src.is_empty()
        || original_src.len() > 2048
        || original_sizes.len() > 1024
        || !crate::srcset::supports_width_sizes(original_srcset)
    {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"pagespeed-automatic-sizes-placement/1\0");
    for field in [
        scope.page_url.as_bytes(),
        scope.options_signature.as_bytes(),
        scope.document_digest.as_slice(),
        scope.stylesheet_epoch.as_slice(),
        ordinal.to_be_bytes().as_slice(),
        original_src,
        original_srcset,
        original_sizes,
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    Some(
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// Parsed measurements, still untrusted until nonce and manifest validation.
/// One batch is for one viewport and at most64 distinct server-issued slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasurementBatch {
    pub viewport: u32,
    pub widths_64: BTreeMap<String, u32>,
}

impl MeasurementBatch {
    /// Decoded wire format: `automatic-sizes/1 viewport\n`, then `id width64\n`.
    /// Bound the raw POST/URL-encoded input separately before query decoding.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        if input.len() > MAX_REPORT_BYTES || !input.ends_with('\n') {
            return None;
        }
        let mut lines = input.lines();
        let header = lines.next()?.strip_prefix("automatic-sizes/1 ")?;
        let viewport: u32 = number(header)?;
        if viewport == 0 || viewport > MAX_CSS_WIDTH {
            return None;
        }
        let mut widths_64 = BTreeMap::new();
        for line in lines {
            if widths_64.len() == MAX_PLACEMENTS {
                return None;
            }
            let (id, width) = line.split_once(' ')?;
            let width: u32 = number(width)?;
            if !valid_identity(id)
                || width == 0
                || width > MAX_CSS_WIDTH * SCALE
                || widths_64.contains_key(id)
            {
                return None;
            }
            widths_64.insert(id.into(), width);
        }
        if widths_64.is_empty() {
            return None;
        }
        Some(Self {
            viewport,
            widths_64,
        })
    }

    /// Fail closed if any identity was not issued for this nonce's document.
    /// This checks membership only, not nonce authenticity, expiry or replay.
    #[must_use]
    pub fn matches_manifest(&self, issued: &BTreeSet<String>) -> bool {
        (1..=MAX_CSS_WIDTH).contains(&self.viewport)
            && issued.len() <= MAX_PLACEMENTS
            && !self.widths_64.is_empty()
            && self.widths_64.len() <= MAX_PLACEMENTS
            && self.widths_64.iter().all(|(id, width)| {
                valid_identity(id)
                    && (1..=MAX_CSS_WIDTH * SCALE).contains(width)
                    && issued.contains(id)
            })
    }
}

fn valid_identity(identity: &str) -> bool {
    identity.len() == 64
        && identity
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// At most this many distinct measured viewports per image placement.
pub const MAX_VIEWPORTS: usize = 16;
const MAX_CSS_WIDTH: u32 = 16384;
const SCALE: u32 = 64;
const REQUIRED_SAMPLES: u8 = 3;
const MAX_CODEC_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    min: u32,
    max: u32,
    count: u8,
}

/// Evidence for one exact document/layout/candidate/element-placement identity.
/// The identity is a server-derived 256-bit digest in lowercase hex, not a URL
/// supplied by the browser. Page/options separation is part of that digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasuredSizes {
    identity: String,
    widths: BTreeMap<u32, Observation>,
}

impl MeasuredSizes {
    /// Creates an empty record. Hash derivation/verification belongs to caller.
    #[must_use]
    pub fn new(identity: &str) -> Option<Self> {
        if !valid_identity(identity) {
            return None;
        }

        Some(Self {
            identity: identity.into(),
            widths: BTreeMap::new(),
        })
    }

    /// Exact registered identity for strict cache-codec validation.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Adds an authenticated observation. Width is positive CSS pixels ×64,
    /// rounded up by the collector. Rejects invalid input before mutation.
    /// Repeated calls are NOT authentication: receiver must reject nonce replay.
    pub fn observe(&mut self, viewport: u32, width_64: u32) -> bool {
        if viewport == 0
            || viewport > MAX_CSS_WIDTH
            || width_64 == 0
            || width_64 > MAX_CSS_WIDTH * SCALE
        {
            return false;
        }
        if let Some(value) = self.widths.get_mut(&viewport) {
            value.min = value.min.min(width_64);
            value.max = value.max.max(width_64);
            value.count = value.count.saturating_add(1).min(REQUIRED_SAMPLES);
        } else {
            if self.widths.len() == MAX_VIEWPORTS {
                return false;
            }
            self.widths.insert(
                viewport,
                Observation {
                    min: width_64,
                    max: width_64,
                    count: 1,
                },
            );
        }
        true
    }

    /// Exact observed-viewport clauses only; no assumptions about unobserved
    /// breakpoints. Unstable measurements are excluded. Unknown viewports reach
    /// the unchanged author fallback. None means preserve the original markup.
    #[must_use]
    pub fn sizes(&self, identity: &str, fallback: &str) -> Option<String> {
        if identity != self.identity || fallback.len() > 1024 {
            return None;
        }
        let clauses: Vec<_> = self
            .widths
            .iter()
            .filter(|(_, o)| o.count == REQUIRED_SAMPLES && o.max - o.min <= SCALE)
            .map(|(viewport, o)| format!("(width: {viewport}px) {}px", o.max.div_ceil(SCALE)))
            .collect();
        if clauses.is_empty() {
            return None;
        }
        let mut result = clauses.join(", ");
        result.push_str(", ");
        result.push_str(if fallback.trim().is_empty() {
            "100vw"
        } else {
            fallback
        });
        Some(result)
    }

    /// Deterministic, bounded, versioned cache payload. No external input stored.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut output = format!("measured-sizes/1\n{}\n", self.identity);
        for (viewport, value) in &self.widths {
            output.push_str(&format!(
                "{viewport} {} {} {}\n",
                value.min, value.max, value.count
            ));
        }
        output
    }

    /// Strict cache decode: corrupt/oversized/other-version records fail closed.
    #[must_use]
    pub fn decode(input: &str) -> Option<Self> {
        if input.len() > MAX_CODEC_BYTES || !input.ends_with('\n') {
            return None;
        }
        let mut lines = input.lines();
        if lines.next()? != "measured-sizes/1" {
            return None;
        }
        let mut result = Self::new(lines.next()?)?;
        for line in lines {
            if result.widths.len() == MAX_VIEWPORTS {
                return None;
            }
            let mut parts = line.split(' ');
            let viewport: u32 = number(parts.next()?)?;
            let min: u32 = number(parts.next()?)?;
            let max: u32 = number(parts.next()?)?;
            let count: u8 = number(parts.next()?)?;
            if parts.next().is_some()
                || viewport == 0
                || viewport > MAX_CSS_WIDTH
                || min == 0
                || max < min
                || max > MAX_CSS_WIDTH * SCALE
                || (count == 1 && min != max)
                || !(1..=REQUIRED_SAMPLES).contains(&count)
                || result.widths.contains_key(&viewport)
            {
                return None;
            }
            result
                .widths
                .insert(viewport, Observation { min, max, count });
        }
        Some(result)
    }
}

fn number<T: std::str::FromStr>(text: &str) -> Option<T> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        None
    } else {
        text.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn layout_fingerprint_is_chunk_independent_and_overflow_fails_closed() {
        let mut whole = LayoutFingerprint::default();
        whole.update(b"<html>original</html>");
        let mut chunks = LayoutFingerprint::default();
        for part in [b"<html>".as_slice(), b"", b"original", b"</html>"] {
            assert!(chunks.update(part));
        }
        let original = whole.finish().unwrap();
        assert_eq!(Some(original), chunks.finish());
        let mut changed = LayoutFingerprint::default();
        changed.update(b"<html>other</html>");
        assert_ne!(Some(original), changed.finish());
        assert_eq!(LayoutFingerprint::default().finish(), None);
        let mut capped = LayoutFingerprint::default();
        let chunk = [b'x'; 1024];
        for _ in 0..MAX_LAYOUT_BYTES / chunk.len() {
            assert!(capped.update(&chunk));
        }
        assert!(!capped.update(b"x"));
        assert!(!capped.update(b""));
        assert_eq!(capped.finish(), None);
    }

    #[test]
    fn placement_scope_separates_page_layout_candidates_and_duplicate_images() {
        let scope = PlacementScope {
            page_url: "https://site.test/page",
            options_signature: "options-a",
            document_digest: [1; 32],
            stylesheet_epoch: [2; 32],
        };
        let derive = |s: &PlacementScope<'_>, n, src: &[u8], candidates: &[u8], sizes: &[u8]| {
            placement_identity(s, n, src, candidates, sizes).unwrap()
        };
        let original = derive(
            &scope,
            1,
            b"hero.jpg",
            b"small.jpg 300w, hero.jpg 800w",
            b"100vw",
        );
        assert!(valid_identity(&original));
        assert_eq!(
            original,
            derive(
                &scope,
                1,
                b"hero.jpg",
                b"small.jpg 300w, hero.jpg 800w",
                b"100vw"
            )
        );
        let mut variants = BTreeSet::new();
        variants.insert(original.clone());
        for alternate in [
            PlacementScope {
                page_url: "https://site.test/other",
                ..scope
            },
            PlacementScope {
                options_signature: "options-b",
                ..scope
            },
            PlacementScope {
                document_digest: [3; 32],
                ..scope
            },
            PlacementScope {
                stylesheet_epoch: [4; 32],
                ..scope
            },
        ] {
            assert!(variants.insert(derive(
                &alternate,
                1,
                b"hero.jpg",
                b"small.jpg 300w, hero.jpg 800w",
                b"100vw"
            )));
        }
        for (n, src, set, fallback) in [
            (
                2,
                b"hero.jpg".as_slice(),
                b"small.jpg 300w, hero.jpg 800w".as_slice(),
                b"100vw".as_slice(),
            ),
            (1, b"other.jpg", b"small.jpg 300w, hero.jpg 800w", b"100vw"),
            (1, b"hero.jpg", b"small.jpg 300w, hero.jpg 1024w", b"100vw"),
            (1, b"hero.jpg", b"small.jpg 300w, hero.jpg 800w", b"50vw"),
        ] {
            assert!(variants.insert(derive(&scope, n, src, set, fallback)));
        }
        // Length framing separates adjacent variable-length fields.
        assert_ne!(
            derive(&scope, 1, b"a", b"b.jpg 300w", b"c"),
            derive(&scope, 1, b"ab", b".jpg 300w", b"c")
        );
        for n in [0, 65] {
            assert!(placement_identity(&scope, n, b"a.jpg", b"a.jpg 300w", b"").is_none());
        }
        assert!(placement_identity(&scope, 1, b"a.jpg", b"a.jpg 1x", b"").is_none());
        assert!(placement_identity(&scope, 1, &vec![b'x'; 2049], b"a.jpg 300w", b"").is_none());
        assert!(
            placement_identity(&scope, 1, b"a.jpg", b"a.jpg 300w", &vec![b'x'; 1025]).is_none()
        );
    }

    #[test]
    fn measurement_wire_is_bounded_and_rejects_unissued_or_duplicate_slots() {
        let wire = format!("automatic-sizes/1 412\n{ID} 22528\n");
        let batch = MeasurementBatch::parse(&wire).unwrap();
        assert_eq!(batch.viewport, 412);
        assert_eq!(batch.widths_64[ID], 352 * 64);
        assert!(batch.matches_manifest(&[ID.into()].into_iter().collect()));
        assert!(!batch.matches_manifest(&BTreeSet::new()));
        for malformed in [
            wire.replace("/1", "/2"),
            wire.replace("412", "0"),
            wire.replace("412", "16385"),
            wire.replace("412", "412.0"),
            wire.replace("22528", "0"),
            wire.replace("22528", "1048577"),
            wire.replace("22528", "+22528"),
            wire.replace("22528", "22528 extra"),
            wire.replace(ID, "unknown"),
            format!("{wire}{ID} 22528\n"),
            "automatic-sizes/1 412\n".into(),
            wire.trim_end().into(),
            "x".repeat(MAX_REPORT_BYTES + 1),
        ] {
            assert!(
                MeasurementBatch::parse(&malformed).is_none(),
                "{malformed:?}"
            );
        }
        let mut full = "automatic-sizes/1 412\n".to_string();
        for i in 0..MAX_PLACEMENTS {
            full.push_str(&format!("{i:064x} 22528\n"));
        }
        let max_batch = MeasurementBatch::parse(&full).unwrap();
        assert_eq!(max_batch.widths_64.len(), MAX_PLACEMENTS);
        full.push_str(&format!("{:064x} 22528\n", MAX_PLACEMENTS));
        assert!(MeasurementBatch::parse(&full).is_none());
        let mut forged = batch;
        forged.widths_64.insert("a".repeat(64), 100);
        assert!(!forged.matches_manifest(&[ID.into()].into_iter().collect()));
    }

    #[test]
    fn stable_viewports_require_evidence_and_preserve_unknown_fallback() {
        let mut sizes = MeasuredSizes::new(ID).unwrap();
        for _ in 0..2 {
            assert!(sizes.observe(412, 352 * 64));
        }
        assert_eq!(sizes.sizes(ID, "100vw"), None);
        sizes.observe(412, 352 * 64 + 1);
        for _ in 0..3 {
            sizes.observe(1024, 645 * 64);
        }
        assert_eq!(
            sizes.sizes(ID, "(max-width: 800px) 100vw, 800px").unwrap(),
            "(width: 412px) 353px, (width: 1024px) 645px, (max-width: 800px) 100vw, 800px"
        );
        assert!(sizes.sizes(&"a".repeat(64), "100vw").is_none());
        assert!(sizes.sizes(ID, &"x".repeat(1025)).is_none());
        assert!(sizes.sizes(ID, "").unwrap().ends_with(", 100vw"));
        // Conflicting layout suppresses only that viewport, never relearns it
        // from repeated narrow samples under the same layout identity.
        sizes.observe(412, 600 * 64);
        for _ in 0..10 {
            sizes.observe(412, 352 * 64);
        }
        assert_eq!(
            sizes.sizes(ID, "100vw").unwrap(),
            "(width: 1024px) 645px, 100vw"
        );
    }

    #[test]
    fn bounds_reject_before_mutation_and_codec_round_trips() {
        let mut sizes = MeasuredSizes::new(ID).unwrap();
        for width in 1..=MAX_VIEWPORTS {
            assert!(sizes.observe(width as u32, width as u32 * 64));
        }
        let snapshot = sizes.encode();
        for (v, w) in [(17, 64), (0, 64), (16385, 64), (1, 0), (1, 16384 * 64 + 1)] {
            assert!(!sizes.observe(v, w));
            assert_eq!(sizes.encode(), snapshot);
        }
        assert!(snapshot.len() < MAX_CODEC_BYTES);
        assert_eq!(MeasuredSizes::decode(&snapshot), Some(sizes));
    }

    #[test]
    fn codec_rejects_malformed_and_duplicate_observations() {
        let prefix = format!("measured-sizes/1\n{ID}\n");
        for tail in [
            "412 64 64 0\n",
            "412 65 64 3\n",
            "412 64 65 1\n",
            "412 0 64 3\n",
            "+412 64 64 3\n",
            "412 64 64 4\n",
            "412 64 64 3 extra\n",
            "412  64 64 3\n",
            "412 64 64 3",
            "\n",
            "412 64 64 3\n412 64 64 3\n",
            "0 64 64 3\n",
            "412 64 1048577 3\n",
        ] {
            assert!(
                MeasuredSizes::decode(&(prefix.clone() + tail)).is_none(),
                "{tail:?}"
            );
        }
        assert!(MeasuredSizes::decode(&"x".repeat(MAX_CODEC_BYTES + 1)).is_none());
        assert!(MeasuredSizes::decode(&prefix.replace("/1", "/2")).is_none());
        assert!(MeasuredSizes::new(&"z".repeat(64)).is_none());
    }
}

//! Bounded measured viewport evidence for future automatic eager/LCP sizing.
//! No I/O or HTML mutation. Callers must authenticate reports, scope identities,
//! enforce page-wide caps/TTL, and commit through the shared metadata cache.
use std::collections::BTreeMap;

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
        if identity.len() != 64
            || !identity
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        Some(Self {
            identity: identity.into(),
            widths: BTreeMap::new(),
        })
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

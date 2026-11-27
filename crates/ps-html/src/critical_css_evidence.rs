//! Bounded selector inputs and decisions for the shared critical/LCP report.
//! This module does not fetch CSS or issue independent beacon nonces.
use std::collections::BTreeSet;

pub const MAX_SELECTORS: usize = 2048;
pub const MAX_SELECTOR_BYTES: usize = 1024;
pub const MAX_TOTAL_BYTES: usize = 32 * 1024;
pub const MAX_ENCODED_BYTES: usize = 192 * 1024;
pub const MAX_WIRE_BYTES: usize = 128 * 1024;
const VERSION: &str = "critical-css-evidence/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssEvidence {
    candidates: BTreeSet<String>,
    matched: Option<BTreeSet<String>>,
}

fn bounded(values: &BTreeSet<String>) -> bool {
    values.len() <= MAX_SELECTORS
        && values.iter().all(|s| {
            !s.is_empty() && s.len() <= MAX_SELECTOR_BYTES && !s.chars().any(char::is_control)
        })
        && values.iter().map(String::len).sum::<usize>() <= MAX_TOTAL_BYTES
}

impl CssEvidence {
    /// All inputs must be complete; callers must never truncate discovery.
    pub fn new(candidates: BTreeSet<String>) -> Option<Self> {
        bounded(&candidates).then_some(Self {
            candidates,
            matched: None,
        })
    }
    pub fn candidates(&self) -> &BTreeSet<String> {
        &self.candidates
    }
    /// None is unlearned; Some(empty) is an accepted empty decision.
    pub fn matched(&self) -> Option<&BTreeSet<String>> {
        self.matched.as_ref()
    }
    pub fn accepts(&self, reported: &BTreeSet<String>) -> bool {
        bounded(reported) && reported.is_subset(&self.candidates)
    }
    pub(crate) fn record(&mut self, reported: BTreeSet<String>) {
        debug_assert!(self.accepts(&reported));
        self.matched = Some(reported);
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("{VERSION}\n{}\n", self.candidates.len());
        for selector in &self.candidates {
            out.push_str(&ps_cache::base64::web64_encode(selector.as_bytes()));
            out.push('\n');
        }
        match &self.matched {
            None => out.push_str("-1\n"),
            Some(matched) => {
                out.push_str(&format!("{}\n", matched.len()));
                for selector in matched {
                    out.push_str(&ps_cache::base64::web64_encode(selector.as_bytes()));
                    out.push('\n');
                }
            }
        }
        out.into_bytes()
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_ENCODED_BYTES || !bytes.ends_with(b"\n") {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.lines();
        if lines.next()? != VERSION {
            return None;
        }
        fn read<'a>(
            lines: &mut impl Iterator<Item = &'a str>,
            count: usize,
        ) -> Option<BTreeSet<String>> {
            if count > MAX_SELECTORS {
                return None;
            }
            let mut set = BTreeSet::new();
            let mut total = 0;
            for _ in 0..count {
                let line = lines.next()?;
                if line.len() > 2 * MAX_SELECTOR_BYTES {
                    return None;
                }
                let decoded = ps_cache::base64::web64_decode(line)?;
                total += decoded.len();
                if total > MAX_TOTAL_BYTES {
                    return None;
                }
                if !set.insert(String::from_utf8(decoded).ok()?) {
                    return None;
                }
            }
            bounded(&set).then_some(set)
        }
        let count = lines.next()?.parse().ok()?;
        let mut evidence = Self::new(read(&mut lines, count)?)?;
        let matched: i64 = lines.next()?.parse().ok()?;
        if matched >= 0 {
            let reported = read(&mut lines, usize::try_from(matched).ok()?)?;
            if !evidence.accepts(&reported) {
                return None;
            }
            evidence.record(reported);
        } else if matched != -1 {
            return None;
        }
        if lines.next().is_some() {
            return None;
        }
        Some(evidence)
    }
}

/// Original PageSpeed wire format: split encoded components before decoding,
/// so a comma inside a functional selector is not a selector-list delimiter.
/// Invalid/over-limit data rejects the whole report instead of truncating it.
pub fn parse_report(escaped: &str) -> Option<BTreeSet<String>> {
    if escaped.len() > MAX_WIRE_BYTES {
        return None;
    }
    if escaped.is_empty() {
        return Some(BTreeSet::new());
    }
    let mut set = BTreeSet::new();
    let mut total = 0;
    for (index, item) in escaped.split(',').enumerate() {
        if index >= MAX_SELECTORS || item.len() > 3 * MAX_SELECTOR_BYTES {
            return None;
        }
        let selector = ps_http::query_params::unescape_query_value(item)?;
        total += selector.len();
        if total > MAX_TOTAL_BYTES {
            return None;
        }
        set.insert(selector);
    }
    bounded(&set).then_some(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_codec_distinguishes_pending_and_empty_decisions() {
        let mut evidence = CssEvidence::new(["div".into(), ":is(.a,.b)".into()].into()).unwrap();
        assert_eq!(
            CssEvidence::decode(&evidence.encode()),
            Some(evidence.clone())
        );
        assert!(evidence.matched().is_none());
        evidence.record(BTreeSet::new());
        assert!(CssEvidence::decode(&evidence.encode())
            .unwrap()
            .matched()
            .unwrap()
            .is_empty());
        assert!(!evidence.accepts(&["unknown".into()].into()));
        assert!(CssEvidence::new(["a".repeat(MAX_SELECTOR_BYTES + 1)].into()).is_none());
        assert!(CssEvidence::new(["div\nspan".into()].into()).is_none());
        assert!(CssEvidence::decode(b"critical-css-evidence/1\n2049\n").is_none());
        assert!(CssEvidence::decode(&[evidence.encode(), b"junk\n".to_vec()].concat()).is_none());
    }
    #[test]
    fn wire_preserves_encoded_commas_and_rejects_bad_or_partial_reports() {
        assert_eq!(
            parse_report("div,%3Ais%28.a%2C.b%29").unwrap(),
            ["div".into(), ":is(.a,.b)".into()].into()
        );
        assert_eq!(parse_report(""), Some(BTreeSet::new()));
        for bad in ["div,", "%zz", "%FF", "%0A", "a,,b"] {
            assert!(parse_report(bad).is_none(), "{bad}");
        }
        assert!(parse_report(&"a".repeat(MAX_SELECTOR_BYTES + 1)).is_none());
        assert!(parse_report(&vec!["a"; MAX_SELECTORS + 1].join(",")).is_none());
        let many: BTreeSet<String> = (0..MAX_SELECTORS)
            .map(|n| format!(".selector_{n}_{}", "x".repeat(20)))
            .collect();
        assert!(CssEvidence::new(many).is_none());
    }
}

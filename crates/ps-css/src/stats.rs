//! CSS stat names and owned counters (per-filter `InitStats`).
//!
//! Counters are plain owned integers, mirrored into `ps-stats` by the
//! driver layer (same pattern as cache backends).
#![forbid(unsafe_code)]

/// `css_filter_blocks_rewritten` (Variable).
pub const BLOCKS_REWRITTEN: &str = "css_filter_blocks_rewritten";
/// `css_filter_parse_failures` (Variable).
pub const PARSE_FAILURES: &str = "css_filter_parse_failures";
/// `css_filter_parse_max_bytes_exceeded` (Variable).
pub const PARSE_MAX_BYTES_EXCEEDED: &str = "css_filter_parse_max_bytes_exceeded";
/// `css_filter_fallback_rewrites` (Variable).
pub const FALLBACK_REWRITES: &str = "css_filter_fallback_rewrites";
/// `css_filter_fallback_failures` (Variable).
pub const FALLBACK_FAILURES: &str = "css_filter_fallback_failures";
/// `css_filter_rewrites_dropped` (Variable).
pub const REWRITES_DROPPED: &str = "css_filter_rewrites_dropped";
/// `css_filter_total_bytes_saved` (UpDownCounter).
pub const TOTAL_BYTES_SAVED: &str = "css_filter_total_bytes_saved";
/// `css_filter_total_original_bytes` (Variable).
pub const TOTAL_ORIGINAL_BYTES: &str = "css_filter_total_original_bytes";
/// `css_filter_uses` (Variable).
pub const USES: &str = "css_filter_uses";
/// `flatten_imports_charset_mismatch` (Variable).
pub const CHARSET_MISMATCH: &str = "flatten_imports_charset_mismatch";
/// `flatten_imports_invalid_url` (Variable).
pub const INVALID_URL: &str = "flatten_imports_invalid_url";
/// `flatten_imports_limit_exceeded` (Variable).
pub const LIMIT_EXCEEDED: &str = "flatten_imports_limit_exceeded";
/// `flatten_imports_minify_failed` (Variable).
pub const MINIFY_FAILED: &str = "flatten_imports_minify_failed";
/// `flatten_imports_recursion` (Variable).
pub const RECURSION: &str = "flatten_imports_recursion";
/// `flatten_imports_complex_queries` (Variable).
pub const COMPLEX_QUERIES: &str = "flatten_imports_complex_queries";
/// `num_css_inlined` (Variable; exact name below).
pub const NUM_CSS_INLINED: &str = "num_css_inlined";
/// `css_elements_moved` (Variable).
pub const ELEMENTS_MOVED: &str = "css_elements_moved";
/// `css_combine_opportunities` (Variable).
pub const COMBINE_OPPORTUNITIES: &str = "css_combine_opportunities";
/// `css_file_count_reduction` (Variable).
pub const FILE_COUNT_REDUCTION: &str = "css_file_count_reduction";

/// Owned CSS counters.
#[derive(Debug, Default)]
pub struct CssStats {
    /// Per-name counters (all names above).
    pub counters: std::collections::HashMap<&'static str, i64>,
}

impl CssStats {
    /// Empty counters.
    #[must_use]
    pub fn new() -> CssStats {
        CssStats::default()
    }

    /// Adds to a counter.
    pub fn add(&mut self, name: &'static str, delta: i64) {
        *self.counters.entry(name).or_insert(0) += delta;
    }

    /// Reads a counter (0 if unset).
    #[must_use]
    pub fn get(&self, name: &'static str) -> i64 {
        self.counters.get(name).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_basics() {
        let mut stats = CssStats::new();
        assert_eq!(stats.get(PARSE_FAILURES), 0);
        stats.add(PARSE_FAILURES, 1);
        stats.add(TOTAL_BYTES_SAVED, -5);
        assert_eq!(stats.get(PARSE_FAILURES), 1);
        assert_eq!(stats.get(TOTAL_BYTES_SAVED), -5);
    }
}

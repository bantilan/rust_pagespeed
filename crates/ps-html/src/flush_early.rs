//! Flush-early property state (`FlushEarlyInfo`, without the proto).
//!
//! The driver decodes one of these per page from the metadata cache
//! (the port's stand-in for the DOM cohort of the property cache),
//! hands it to the filters that read and rotate it (today just
//! `InsertDnsPrefetchFilter`), and serializes it back at end of
//! parse. Like C++ (`-1`, no TTL checking), entries are used
//! whenever they decode, regardless of age; the backend's eviction
//! is the only expiry, and missing, expired, or corrupt entries
//! safely behave like a first view.

/// DNS-prefetch domain info for one page (`FlushEarlyInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlushEarlyInfo {
    /// Domains eligible for prefetch tags, capped at insert time.
    dns_prefetch_domains: Vec<String>,
    /// Eligible-domain count from the current rewrite.
    total_dns_prefetch_domains: i32,
    /// Eligible-domain count from the previous rewrite.
    total_dns_prefetch_domains_previous: i32,
}

impl FlushEarlyInfo {
    /// Reads the stored domains (`dns_prefetch_domains`).
    #[must_use]
    pub fn dns_prefetch_domains(&self) -> &[String] {
        &self.dns_prefetch_domains
    }

    /// Reads the current-rewrite total (`total_dns_prefetch_domains`).
    #[must_use]
    pub fn total_dns_prefetch_domains(&self) -> i32 {
        self.total_dns_prefetch_domains
    }

    /// Reads the previous-rewrite total
    /// (`total_dns_prefetch_domains_previous`).
    #[must_use]
    pub fn total_dns_prefetch_domains_previous(&self) -> i32 {
        self.total_dns_prefetch_domains_previous
    }

    /// Rotates the totals and stores up to `cap` domains (the
    /// `InsertDnsPrefetchFilter::EndDocument` write).
    pub fn store_dns_prefetch_domains(&mut self, domains: &[String], cap: usize) {
        self.total_dns_prefetch_domains_previous = self.total_dns_prefetch_domains;
        self.total_dns_prefetch_domains = domains.len() as i32;
        self.dns_prefetch_domains.clear();
        self.dns_prefetch_domains
            .extend(domains.iter().take(cap).cloned());
    }

    /// Whether the eligible-domain count is stable across the last two
    /// rewrites (`IsDomainListStable`, tolerance `max_diff`).
    #[must_use]
    pub fn is_domain_list_stable(&self, max_diff: i32) -> bool {
        (self.total_dns_prefetch_domains - self.total_dns_prefetch_domains_previous).abs()
            <= max_diff
    }
}

/// Metadata-cache key prefix (page-variant flush-early state).
pub const FLUSH_EARLY_KEY_PREFIX: &str = "flush-early-v1:";
/// HMAC signing key (backend-safe keys, like the beacon cohort).
const FLUSH_EARLY_SIGNING_KEY: &str = "pagespeed-flush-early-v1";
/// Codec version line.
const CODEC_VERSION: &str = "flush-early-v1";
/// Largest accepted entry (bounds corrupt-entry allocation).
const MAX_ENTRY_BYTES: usize = 64 * 1024;
/// Largest accepted domain list (bounds corrupt-entry allocation).
const MAX_STORED_DOMAINS: usize = 1024;

/// Builds the page-variant key (page URL, options hash, and device
/// suffix, like the beacon cohort's property page).
#[must_use]
pub fn flush_early_key(url: &str, options_hash: &str, device_suffix: &str) -> String {
    let identity = format!("{url}\n{options_hash}\n{device_suffix}");
    format!(
        "{FLUSH_EARLY_KEY_PREFIX}{}",
        ps_cache::html::sign_url(FLUSH_EARLY_SIGNING_KEY, &identity)
    )
}

/// Encodes state (`SerializeToString`):
/// `v1\n<total>\n<previous>\n<count>\n<domain>\n…`.
#[must_use]
pub fn encode_flush_early(info: &FlushEarlyInfo) -> Vec<u8> {
    let mut out = format!(
        "{CODEC_VERSION}\n{}\n{}\n{}\n",
        info.total_dns_prefetch_domains,
        info.total_dns_prefetch_domains_previous,
        info.dns_prefetch_domains.len()
    );
    for domain in &info.dns_prefetch_domains {
        out.push_str(domain);
        out.push('\n');
    }
    out.into_bytes()
}

/// Decodes state, rejecting version, shape, bound, and content
/// violations (`None` behaves like a first view).
#[must_use]
pub fn decode_flush_early(bytes: &[u8]) -> Option<FlushEarlyInfo> {
    if bytes.len() > MAX_ENTRY_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.split('\n');
    if lines.next()? != CODEC_VERSION {
        return None;
    }
    let total: i32 = lines.next()?.parse().ok()?;
    let previous: i32 = lines.next()?.parse().ok()?;
    let count: usize = lines.next()?.parse().ok()?;
    if count > MAX_STORED_DOMAINS {
        return None;
    }
    let mut domains = Vec::with_capacity(count.min(8));
    for _ in 0..count {
        let domain = lines.next()?;
        if domain.is_empty() {
            return None;
        }
        domains.push(domain.to_string());
    }
    // Exactly one trailing newline, nothing after it.
    if lines.next() != Some("") || lines.next().is_some() {
        return None;
    }
    if total < 0 || previous < 0 {
        return None;
    }
    Some(FlushEarlyInfo {
        dns_prefetch_domains: domains,
        total_dns_prefetch_domains: total,
        total_dns_prefetch_domains_previous: previous,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rejections() {
        let mut info = FlushEarlyInfo::default();
        info.store_dns_prefetch_domains(&["a.com".to_string(), "b.com".to_string()], 8);
        let bytes = encode_flush_early(&info);
        assert_eq!(decode_flush_early(&bytes), Some(info));
        // Empty state round-trips too.
        let empty = FlushEarlyInfo::default();
        assert_eq!(
            decode_flush_early(&encode_flush_early(&empty)),
            Some(empty)
        );
        // Every corruption shape fails closed.
        for bad in [
            b"bogus".as_slice(),
            b"flush-early-v0\n0\n0\n0\n",
            b"flush-early-v1\nx\n0\n0\n",
            b"flush-early-v1\n0\n0\n2\na.com\n",
            b"flush-early-v1\n0\n0\n1\na.com\nb.com\n",
            b"flush-early-v1\n0\n0\n1\n\n",
            b"flush-early-v1\n-1\n0\n0\n",
            b"flush-early-v1\n0\n0\n0",
            "flush-early-v1\n0\n0\n1025\n".as_bytes(),
        ] {
            assert_eq!(decode_flush_early(bad), None, "{bad:?}");
        }
        assert_eq!(decode_flush_early(&vec![b'x'; MAX_ENTRY_BYTES + 1]), None);
    }

    #[test]
    fn keys_isolate_pages_and_variants() {
        let key = flush_early_key("http://x/page", "oh", "@Desktop");
        assert!(key.starts_with(FLUSH_EARLY_KEY_PREFIX));
        assert_eq!(key, flush_early_key("http://x/page", "oh", "@Desktop"));
        assert_ne!(key, flush_early_key("http://x/other", "oh", "@Desktop"));
        assert_ne!(key, flush_early_key("http://x/page", "oh2", "@Desktop"));
        assert_ne!(key, flush_early_key("http://x/page", "oh", "@Mobile"));
    }
}

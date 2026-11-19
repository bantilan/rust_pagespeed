//! Flush-early property state (`FlushEarlyInfo`, without the proto).
//!
//! The driver decodes one of these per page from the DOM cohort of the
//! property cache, hands it to the filters that read and rotate it
//! (today just `InsertDnsPrefetchFilter`), and serializes it back at
//! end of parse. The decode/encode round trip is driver business that
//! has not landed yet; filters share one instance per page exactly
//! like the C++ sharing `driver()->flush_early_info()`.

/// DNS-prefetch domain info for one page (`FlushEarlyInfo`).
#[derive(Debug, Clone, Default)]
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

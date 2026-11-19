//! Cache purging (`AdminSite::Purge` + `PurgeContext`).
//!
//! A purge names a URL or the whole cache (`*`-suffixed, which
//! only flushes everything). Per-vhost consoles may not evict
//! other sites' records.

use ps_cache::purge::PurgeSet;

/// Refusal when a per-vhost purge cannot fit (`kPurgeSetFull`).
pub const PURGE_SET_FULL: &str = "purge set is full: purge this URL from the global admin console";

/// Purge sink (`PurgeContext`).
pub trait PurgeBackend {
    /// Purges a URL, or everything when it ends in `*`.
    /// `allow_eviction` false forbids evicting other sites'
    /// records while making room. `Err` carries the refusal
    /// reason the console renders (`PurgeCallback` reason).
    fn purge(&mut self, url: &str, now_ms: i64, allow_eviction: bool) -> Result<(), String>;
}

/// `PurgeSet`-backed sink.
#[derive(Debug)]
pub struct PurgeSetBackend {
    /// Live purge set.
    pub purge_set: PurgeSet,
}

impl PurgeSetBackend {
    /// Builds a sink over a purge set.
    #[must_use]
    pub fn new(purge_set: PurgeSet) -> Self {
        PurgeSetBackend { purge_set }
    }
}

impl PurgeBackend for PurgeSetBackend {
    fn purge(&mut self, url: &str, now_ms: i64, allow_eviction: bool) -> Result<(), String> {
        if url.ends_with('*') {
            // If the url is "*" we'll just purge everything. Note
            // that we will ignore any sub-paths in the expression.
            // We can only purge the entire cache, or specific
            // URLs, not general wildcards.
            self.purge_set.update_global_timestamp(now_ms);
            return Ok(());
        }
        let admitted = if allow_eviction {
            self.purge_set.put(url, now_ms)
        } else {
            self.purge_set.put_without_eviction(url, now_ms)
        };
        if admitted {
            Ok(())
        } else {
            Err(PURGE_SET_FULL.to_string())
        }
    }
}

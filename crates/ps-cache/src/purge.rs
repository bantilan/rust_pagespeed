//! Cache purge and invalidation (`purge_set.{h,cc}`,
//! `purge_context.{h,cc}`, `system_cache_path.cc`, `IsUrlCacheValid`).
//!
//! - [`PurgeSet`]: bounded URL→timestamp LRU plus a global invalidation
//!   timestamp, with clock-skew sanitizing.
//! - [`PurgeFile`]: the `cache.purge` file format (global timestamp line,
//!   then `TIMESTAMP URL` lines) with future-timestamp validation.
//! - [`UrlInvalidationList`]: legacy timestamp-ordered wildcard entries.
//! - [`is_url_cache_valid`]: the combined validity check.
//!
//! Interprocess locking and async callbacks around file updates land with
//! the server integration; the data format and set semantics here are
//! exact.
#![forbid(unsafe_code)]

use crate::timer::MINUTE_MS;
use ps_config::wildcard::Wildcard;
use std::collections::HashMap;

/// Initial global timestamp: nothing invalidated (`kInitialTimestampMs`).
pub const INITIAL_TIMESTAMP_MS: i64 = -1;
/// Accepted clock skew before a timestamp is rejected
/// (`kClockSkewAllowanceMs` = 10 min).
pub const CLOCK_SKEW_ALLOWANCE_MS: i64 = 10 * MINUTE_MS;
/// Purge-set byte budget (`RewriteOptions::kCachePurgeBytes`).
pub const CACHE_PURGE_BYTES: usize = 25_000;
/// Refusal message when a non-evicting purge does not fit (`kPurgeSetFull`).
pub const PURGE_SET_FULL: &str = "purge set is full: purge this URL from the global admin console";
/// Purge-file poll interval (`kCheckCacheIntervalMs` = 5 s).
pub const CHECK_CACHE_INTERVAL_MS: i64 = 5_000;
/// Flush filename with purging enabled (`cache.purge`).
pub const PURGE_FILENAME: &str = "cache.purge";
/// Flush filename with purging disabled (`cache.flush`, mtime-driven).
pub const FLUSH_FILENAME: &str = "cache.flush";

/// Bounded invalidation set (`PurgeSet`).
///
/// Records are `url -> timestamp_ms`, evicted least-recently-inserted
/// first; each eviction raises the global timestamp to the evicted
/// record's stamp. `IsValid` requires the entry timestamp to postdate
/// both the global stamp and any per-URL record.
#[derive(Debug, Clone, Default)]
pub struct PurgeSet {
    /// Insertion-ordered records (LRU order; re-put moves to back).
    records: Vec<(String, i64)>,
    max_bytes: usize,
    used_bytes: usize,
    global_timestamp_ms: i64,
    last_timestamp_ms: i64,
}

impl PurgeSet {
    /// Creates a set with a byte budget (`PurgeSet(max_size)`).
    #[must_use]
    pub fn new(max_bytes: usize) -> PurgeSet {
        PurgeSet {
            max_bytes,
            global_timestamp_ms: INITIAL_TIMESTAMP_MS,
            ..PurgeSet::default()
        }
    }

    /// Current global invalidation timestamp.
    #[must_use]
    pub fn global_timestamp_ms(&self) -> i64 {
        self.global_timestamp_ms
    }

    /// Whether a global timestamp was ever set.
    #[must_use]
    pub fn has_global_timestamp(&self) -> bool {
        self.global_timestamp_ms != INITIAL_TIMESTAMP_MS
    }

    /// Number of per-URL records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether no per-URL records are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Iterates records oldest-first (`Begin`/`End` order).
    pub fn iter(&self) -> impl Iterator<Item = (&str, i64)> + '_ {
        self.records
            .iter()
            .map(|(key, stamp)| (key.as_str(), *stamp))
    }

    /// Renders records for consoles (`ToString`): `Global@…`
    /// plus one `url@time` line each. Times render as HTTP
    /// dates, falling back to integers when out of range.
    #[must_use]
    pub fn to_display_string(&self, mut format_time: impl FnMut(i64) -> Option<String>) -> String {
        let mut out = format!(
            "Global@{}",
            format_time(self.global_timestamp_ms)
                .unwrap_or_else(|| self.global_timestamp_ms.to_string())
        );
        for (key, stamp) in self.iter() {
            out.push_str(&format!(
                "\n{key}@{}",
                format_time(stamp).unwrap_or_else(|| stamp.to_string())
            ));
        }
        out
    }

    /// Clears records and resets the global timestamp (`Clear`).
    pub fn clear(&mut self) {
        self.records.clear();
        self.used_bytes = 0;
        self.global_timestamp_ms = INITIAL_TIMESTAMP_MS;
    }

    /// Entry size accounting: key bytes + 8-byte timestamp.
    fn entry_bytes(key: &str) -> usize {
        key.len() + 8
    }

    /// Sanitizes a timestamp against clock skew (`SanitizeTimestamp`).
    ///
    /// Forward time advances the high-water mark; small backward steps
    /// clamp to it; larger jumps reject the update.
    fn sanitize(&mut self, timestamp_ms: &mut i64) -> bool {
        let behind = self.last_timestamp_ms - *timestamp_ms;
        if behind <= 0 {
            self.last_timestamp_ms = *timestamp_ms;
        } else if behind <= CLOCK_SKEW_ALLOWANCE_MS {
            *timestamp_ms = self.last_timestamp_ms;
        } else {
            return false;
        }
        true
    }

    /// Raises the global timestamp (`UpdateGlobalInvalidationTimestampMs`).
    pub fn update_global_timestamp(&mut self, mut timestamp_ms: i64) -> bool {
        if !self.sanitize(&mut timestamp_ms) {
            return false;
        }
        self.global_timestamp_ms = self.global_timestamp_ms.max(timestamp_ms);
        true
    }

    /// Evicts oldest records until `needed` bytes fit, raising the global
    /// stamp to each evicted record (`EvictNotify`).
    fn evict_for(&mut self, needed: usize) {
        while self.used_bytes + needed > self.max_bytes {
            let Some((key, stamp)) = self.records.first().cloned() else {
                break;
            };
            self.records.remove(0);
            self.used_bytes -= Self::entry_bytes(&key);
            self.global_timestamp_ms = self.global_timestamp_ms.max(stamp);
        }
    }

    /// Records a purge, evicting as needed (`Put`).
    ///
    /// Sanitizing failures still return true (the record is dropped but the
    /// call reports success, as in C++); records predating the global stamp
    /// are ignored; records that cannot fit even an empty set are dropped
    /// (`EvictIfNecessary` returns false in C++).
    pub fn put(&mut self, key: &str, mut timestamp_ms: i64) -> bool {
        if !self.sanitize(&mut timestamp_ms) {
            return true;
        }
        if timestamp_ms <= self.global_timestamp_ms {
            return true;
        }
        // Refresh position for existing keys (LRU re-put moves to back).
        if let Some(pos) = self.records.iter().position(|(key2, _)| key2 == key) {
            self.records.remove(pos);
            self.used_bytes -= Self::entry_bytes(key);
        }
        let needed = Self::entry_bytes(key);
        if needed >= self.max_bytes {
            return true; // too big to ever fit: dropped.
        }
        self.evict_for(needed);
        self.records.push((key.to_string(), timestamp_ms));
        self.used_bytes += needed;
        true
    }

    /// Records a purge only when it fits without eviction
    /// (`PutWithoutEviction`). Replacing an existing key always fits.
    pub fn put_without_eviction(&mut self, key: &str, timestamp_ms: i64) -> bool {
        if !self.records.iter().any(|(key2, _)| key2 == key) {
            let needed = Self::entry_bytes(key);
            if needed >= self.max_bytes || self.used_bytes + needed > self.max_bytes {
                return false;
            }
        }
        self.put(key, timestamp_ms)
    }

    /// Validates an entry timestamp (`IsValid`).
    #[must_use]
    pub fn is_valid(&self, key: &str, timestamp_ms: i64) -> bool {
        if timestamp_ms <= self.global_timestamp_ms {
            return false;
        }
        match self.records.iter().find(|(key2, _)| key2 == key) {
            None => true,
            Some((_, purge_stamp)) => timestamp_ms > *purge_stamp,
        }
    }

    /// Merges another set (`Merge`): global stamp takes the max, records
    /// merge by key keeping the newest stamp, in timestamp order.
    pub fn merge(&mut self, other: &PurgeSet) {
        self.global_timestamp_ms = self.global_timestamp_ms.max(other.global_timestamp_ms);
        // Collect newest stamp per surviving key, oldest-first.
        let mut merged: HashMap<&str, i64> = HashMap::new();
        for (key, stamp) in self.records.iter().chain(other.records.iter()) {
            if *stamp > self.global_timestamp_ms {
                let entry = merged.entry(key.as_str()).or_insert(*stamp);
                *entry = (*entry).max(*stamp);
            }
        }
        let mut ordered: Vec<(String, i64)> = merged
            .into_iter()
            .map(|(key, stamp)| (key.to_string(), stamp))
            .collect();
        ordered.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        self.records.clear();
        self.used_bytes = 0;
        // C++ resets the skew baseline to the global stamp so the
        // survivors (all newer than it) re-insert undistorted.
        self.last_timestamp_ms = self.global_timestamp_ms;
        for (key, stamp) in ordered {
            // Re-insertion may evict; C++ accepts that here.
            self.put(&key, stamp);
        }
    }

    /// Structural equality (`Equals`): global stamp plus record set.
    #[must_use]
    pub fn equals(&self, other: &PurgeSet) -> bool {
        if self.global_timestamp_ms != other.global_timestamp_ms {
            return false;
        }
        let mut mine: Vec<(&str, i64)> = self.iter().collect();
        let mut theirs: Vec<(&str, i64)> = other.iter().collect();
        mine.sort_unstable();
        theirs.sort_unstable();
        mine == theirs
    }
}

/// Parsed `cache.purge` file (`PurgeContext` file format).
///
/// Line 1 holds the global timestamp; later lines hold
/// `TIMESTAMP URL` records (URLs unrestricted, ending at newline).
/// Timestamps must parse as int64 and may not exceed now + skew
/// allowance (except the -1 initial value). Malformed lines count as
/// parse failures but do not abort the read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeFile {
    /// Parsed global timestamp (`None` when the file is empty).
    pub global_timestamp_ms: Option<i64>,
    /// Parsed `(timestamp, url)` records.
    pub records: Vec<(i64, String)>,
    /// Number of malformed lines skipped.
    pub parse_failures: u64,
}

impl PurgeFile {
    /// Parses file bytes (`ReadPurgeFile`).
    #[must_use]
    pub fn parse(text: &str, now_ms: i64) -> PurgeFile {
        let mut file = PurgeFile::default();
        // Split keeping line order; C++ omits empty segments.
        let lines: Vec<&str> = text.split('\n').filter(|line| !line.is_empty()).collect();
        if lines.is_empty() {
            return file; // empty file leaves everything unchanged.
        }
        let Some(global) = valid_timestamp(lines[0], now_ms) else {
            file.parse_failures += 1;
            return file;
        };
        file.global_timestamp_ms = Some(global);
        for line in &lines[1..] {
            if line.is_empty() {
                continue;
            }
            let Some(space) = line.find(' ') else {
                file.parse_failures += 1;
                continue;
            };
            let Some(stamp) = valid_timestamp(&line[..space], now_ms) else {
                file.parse_failures += 1;
                continue;
            };
            file.records.push((stamp, line[space + 1..].to_string()));
        }
        file
    }

    /// Serializes a set (`ModifyPurgeSet` buffer collection).
    #[must_use]
    pub fn serialize(set: &PurgeSet) -> String {
        let mut out = format!("{}\n", set.global_timestamp_ms());
        for (key, stamp) in set.iter() {
            out.push_str(&format!("{stamp} {key}\n"));
        }
        out
    }

    /// Applies a parsed file to a set (global stamp, then records).
    pub fn apply_to(&self, set: &mut PurgeSet) {
        if let Some(global) = self.global_timestamp_ms {
            set.update_global_timestamp(global);
        }
        for (stamp, url) in &self.records {
            set.put(url, *stamp);
        }
    }
}

/// Validates one timestamp field (`ParseAndValidateTimestamp`).
fn valid_timestamp(text: &str, now_ms: i64) -> Option<i64> {
    let stamp: i64 = text.parse().ok()?;
    if stamp != INITIAL_TIMESTAMP_MS && (stamp < 0 || stamp > now_ms + CLOCK_SKEW_ALLOWANCE_MS) {
        return None;
    }
    Some(stamp)
}

/// Legacy timestamp-ordered wildcard invalidation entries
/// (`url_cache_invalidation_entries_`, ascending by timestamp).
#[derive(Debug, Clone, Default)]
pub struct UrlInvalidationList {
    entries: Vec<(i64, Wildcard)>,
}

impl UrlInvalidationList {
    /// Creates an empty list.
    #[must_use]
    pub fn new() -> UrlInvalidationList {
        UrlInvalidationList::default()
    }

    /// Inserts keeping ascending timestamp order (C++ `inplace_merge`).
    pub fn insert(&mut self, timestamp_ms: i64, pattern: &str) {
        self.entries.push((timestamp_ms, Wildcard::new(pattern)));
        self.entries.sort_by_key(|entry| entry.0);
    }

    /// Merges another list preserving order.
    pub fn merge(&mut self, other: &UrlInvalidationList) {
        self.entries.extend(other.entries.iter().cloned());
        self.entries.sort_by_key(|entry| entry.0);
    }

    /// Whether any entry at/after `time_ms` matches `url`
    /// (entries older than the entry time cannot invalidate it).
    #[must_use]
    pub fn matches(&self, url: &str, time_ms: i64) -> bool {
        // Skip entries older than the entry (linear scan, as in C++).
        let mut i = 0;
        while i < self.entries.len() && time_ms > self.entries[i].0 {
            i += 1;
        }
        self.entries[i..]
            .iter()
            .any(|(_, pattern)| pattern.matches(url))
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no entries are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Combined validity check (`RewriteOptions::IsUrlCacheValid`).
///
/// The purge set always applies; legacy wildcard entries apply only when
/// `search_wildcards` is set (metadata checks pass false).
#[must_use]
pub fn is_url_cache_valid(
    purge_set: &PurgeSet,
    invalidations: &UrlInvalidationList,
    url: &str,
    time_ms: i64,
    search_wildcards: bool,
) -> bool {
    if !purge_set.is_valid(url, time_ms) {
        return false;
    }
    if !search_wildcards {
        return true;
    }
    !invalidations.matches(url, time_ms)
}

/// Resolves the flush filename: explicit config wins, else `cache.purge`
/// with purging enabled or `cache.flush` without; relative names join the
/// file-cache path (`SystemCachePath` constructor).
#[must_use]
pub fn flush_filename(configured: &str, file_cache_path: &str, enable_purge: bool) -> String {
    if !configured.is_empty() {
        return configured.to_string();
    }
    let name = if enable_purge {
        PURGE_FILENAME
    } else {
        FLUSH_FILENAME
    };
    if file_cache_path.is_empty() {
        return name.to_string();
    }
    let separator = if file_cache_path.ends_with('/') {
        ""
    } else {
        "/"
    };
    format!("{file_cache_path}{separator}{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_valid_and_global() {
        let mut set = PurgeSet::new(CACHE_PURGE_BYTES);
        assert!(!set.has_global_timestamp());
        assert!(set.is_valid("http://x/a", 1000));
        assert!(set.put("http://x/a", 2000));
        assert!(!set.is_valid("http://x/a", 1000));
        assert!(!set.is_valid("http://x/a", 2000)); // not strictly after
        assert!(set.is_valid("http://x/a", 2001));
        assert!(set.is_valid("http://x/other", 1000));
        assert!(set.update_global_timestamp(3000));
        assert!(set.has_global_timestamp());
        assert!(!set.is_valid("http://x/other", 3000));
        assert!(set.is_valid("http://x/other", 3001));
        // Records predating the global stamp are ignored.
        assert!(set.put("http://x/old", 1000));
        assert_eq!(set.len(), 1); // "old" never stored
    }

    #[test]
    fn eviction_raises_global() {
        // One 10-byte record ("u1" + stamp) fits in 19 bytes; a second
        // forces eviction (10 + 10 > 19).
        let mut set = PurgeSet::new(19);
        assert!(set.put("u1", 1000));
        assert_eq!(set.len(), 1);
        assert!(set.put("u2", 2000)); // evicts u1, global -> 1000
        assert_eq!(set.len(), 1);
        assert_eq!(set.global_timestamp_ms(), 1000);
        assert!(!set.is_valid("anything", 1000));
        assert!(set.is_valid("anything", 1001));
        // Oversized records never fit, even empty.
        let mut set = PurgeSet::new(19);
        assert!(set.put("this-key-is-far-too-long-for-budget", 1000));
        assert!(set.is_empty());
        // Non-evicting put refuses instead of evicting.
        let mut set = PurgeSet::new(19);
        assert!(set.put("u1", 1000));
        assert!(!set.put_without_eviction("u2", 2000));
        assert_eq!(set.global_timestamp_ms(), INITIAL_TIMESTAMP_MS);
        // ...but replacing an existing key always fits.
        assert!(set.put_without_eviction("u1", 1500));
        assert!(!set.is_valid("u1", 1500));
    }

    #[test]
    fn clock_skew_sanitize() {
        let mut set = PurgeSet::new(CACHE_PURGE_BYTES);
        assert!(set.put("a", 10_000_000));
        // Small backward step clamps to the high-water mark.
        assert!(set.put("b", 10_000_000 - 60_000));
        assert!(!set.is_valid("b", 10_000_000));
        assert!(set.is_valid("b", 10_000_001));
        // Large backward jump rejected (global update reports false).
        assert!(!set.update_global_timestamp(1000));
        assert!(!set.has_global_timestamp());
    }

    #[test]
    fn merge_and_equals() {
        let mut left = PurgeSet::new(CACHE_PURGE_BYTES);
        left.put("a", 1000);
        left.put("b", 2000);
        let mut right = PurgeSet::new(CACHE_PURGE_BYTES);
        right.put("b", 2500);
        right.put("c", 1500);
        left.merge(&right);
        // Newest stamp per key wins, undistorted (a stays @1000, not
        // clamped to the merge high-water mark).
        assert!(!left.is_valid("b", 2500));
        assert!(left.is_valid("b", 2501));
        assert!(!left.is_valid("a", 1000));
        assert!(left.is_valid("a", 1001));
        assert!(!left.is_valid("c", 1500));
        let mut copy = PurgeSet::new(CACHE_PURGE_BYTES);
        copy.merge(&left);
        assert!(copy.equals(&left));
        copy.clear();
        assert!(!copy.equals(&left));
    }

    #[test]
    fn purge_file_round_trip() {
        let mut set = PurgeSet::new(CACHE_PURGE_BYTES);
        set.update_global_timestamp(5000);
        set.put("http://x/a b", 6000); // URLs unrestricted in format
        set.put("http://x/c", 7000);
        let text = PurgeFile::serialize(&set);
        assert_eq!(text, "5000\n6000 http://x/a b\n7000 http://x/c\n");
        let parsed = PurgeFile::parse(&text, 10_000);
        assert_eq!(parsed.global_timestamp_ms, Some(5000));
        assert_eq!(parsed.parse_failures, 0);
        let mut back = PurgeSet::new(CACHE_PURGE_BYTES);
        parsed.apply_to(&mut back);
        assert!(back.equals(&set));
    }

    #[test]
    fn purge_file_validation() {
        // Empty file: nothing parsed, no failure.
        let parsed = PurgeFile::parse("", 10_000);
        assert_eq!(parsed.global_timestamp_ms, None);
        assert_eq!(parsed.parse_failures, 0);
        // Bad global line: whole read fails fast with one failure.
        let parsed = PurgeFile::parse("bogus\n100 http://x\n", 10_000);
        assert_eq!(parsed.global_timestamp_ms, None);
        assert_eq!(parsed.parse_failures, 1);
        assert!(parsed.records.is_empty());
        // Future timestamps rejected; bad record lines counted, rest kept.
        let parsed = PurgeFile::parse(
            "100\n99999999999999 http://x/future\nno-space-here\n200 http://x/ok\n",
            10_000,
        );
        assert_eq!(parsed.global_timestamp_ms, Some(100));
        assert_eq!(parsed.parse_failures, 2);
        assert_eq!(parsed.records, vec![(200, "http://x/ok".to_string())]);
        // -1 global (initial value) explicitly allowed.
        let parsed = PurgeFile::parse("-1\n", 10_000);
        assert_eq!(parsed.global_timestamp_ms, Some(-1));
    }

    #[test]
    fn invalidation_list_and_combined_check() {
        let mut list = UrlInvalidationList::new();
        assert!(list.is_empty());
        list.insert(5000, "http://x/private/*");
        list.insert(1000, "http://old/*");
        assert_eq!(list.len(), 2);
        // Older than the entry: cannot invalidate it.
        assert!(!list.matches("http://old/a", 2000));
        assert!(list.matches("http://old/a", 1000));
        assert!(list.matches("http://x/private/a", 5000));
        assert!(!list.matches("http://x/public/a", 5000));

        let empty = PurgeSet::new(CACHE_PURGE_BYTES);
        assert!(is_url_cache_valid(&empty, &list, "http://x/a", 9999, true));
        assert!(!is_url_cache_valid(
            &empty,
            &list,
            "http://old/a",
            500,
            true
        ));
        // Wildcard search disabled (metadata path): list ignored.
        assert!(is_url_cache_valid(
            &empty,
            &list,
            "http://old/a",
            500,
            false
        ));
        // Purge-set records apply regardless of the wildcard flag.
        let mut set = PurgeSet::new(CACHE_PURGE_BYTES);
        set.put("http://x/a", 9000);
        assert!(!is_url_cache_valid(&set, &list, "http://x/a", 8000, false));
        assert!(is_url_cache_valid(&set, &list, "http://x/a", 9001, true));
    }

    #[test]
    fn flush_filenames() {
        assert_eq!(
            flush_filename("", "/tmp/cache", true),
            "/tmp/cache/cache.purge"
        );
        assert_eq!(
            flush_filename("", "/tmp/cache/", false),
            "/tmp/cache/cache.flush"
        );
        assert_eq!(flush_filename("", "", true), "cache.purge");
        assert_eq!(
            flush_filename("/abs/custom", "/tmp/cache", true),
            "/abs/custom"
        );
    }
}

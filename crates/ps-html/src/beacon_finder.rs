//! Beacon evidence store and critical-images finder
//! (`BeaconCriticalImagesFinder` data half).
//!
//! Browsers instrumented with the critical-images beacon report the
//! LCP image (stamped hash, kind, or loaded URL) plus a nonce. This
//! module stores that evidence per page variant in the metadata
//! cache (our property-cache cohort), validates nonces, merges
//! support counts with decay, and answers the lookups filters need:
//! availability, critical sets, LCP kind/URL, and the sampling
//! schedule. The learned evidence generation also feeds the HTML
//! cache identity, so each accepted beacon invalidates the variant.
//!
//! Divergences from C++: rendered dimensions (`rd`) and critical CSS
//! selectors (`cs`) are ignored (no port consumer reads them); CSS
//! critical sets are never populated (C++ never shipped CSS beacon
//! detection either); the downstream-cache rebeaconing special case
//! is omitted (the port has no downstream integration); evidence
//! records use a line codec instead of protobufs.
#![forbid(unsafe_code)]

use ps_cache::base64::web64_encode;
use ps_cache::cache::{Cache as CacheBackend, KeyState};
use ps_cache::hash::hash_string_case_preserve;
use ps_cache::html::sign_url;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

/* Wire parameters (`server_context.cc kBeacon*`). */

/// Options-hash parameter (required for the evidence half).
pub const PARAM_OPTIONS_HASH: &str = "oh";
/// Nonce parameter.
pub const PARAM_NONCE: &str = "n";
/// Stamped LCP image hash parameter.
pub const PARAM_LCP_IMAGE: &str = "lcp";
/// LCP kind parameter (`img`, `bg`, or `text`).
pub const PARAM_LCP_KIND: &str = "lcpk";
/// LCP loaded-URL parameter (unstamped only).
pub const PARAM_LCP_URL: &str = "lcpu";
/// Maximum accepted LCP URL length (`kBeaconLcpUrlMaxLength`).
pub const LCP_URL_MAX_LENGTH: usize = 2048;

/// LCP kind: `<img>` element (`kLcpKindImg`).
pub const LCP_KIND_IMG: &str = "img";
/// LCP kind: image on a non-img element (`kLcpKindBackground`).
pub const LCP_KIND_BACKGROUND: &str = "bg";
/// LCP kind: text (`kLcpKindText`).
pub const LCP_KIND_TEXT: &str = "text";

/// Whether a reported LCP kind is one of the three known values.
#[must_use]
pub fn valid_lcp_kind(kind: &str) -> bool {
    kind == LCP_KIND_IMG || kind == LCP_KIND_BACKGROUND || kind == LCP_KIND_TEXT
}

/* Support, sampling, and nonce constants. */

/// Beacon support interval (`kBeaconImageSupportInterval`).
pub const IMAGE_SUPPORT_INTERVAL: i64 = 10;
/// Percent-seen threshold for criticality
/// (`kDefaultPercentSeenForCritical`).
pub const PERCENT_SEEN_FOR_CRITICAL: i64 = 100;
/// Nonce length in web64 characters (`kNonceSize`).
pub const NONCE_CHARS: usize = 11;
/// Pending-nonce capacity (`kMaxPendingNonces`).
pub const MAX_PENDING_NONCES: usize = 128;
/// Nonce lifetime in ms (`kBeaconTimeoutIntervalMs`, one minute).
pub const NONCE_TIMEOUT_MS: i64 = 60_000;
/// Stale-candidate retention cap (`kMaxStaleCandidateKeys`).
pub const MAX_STALE_CANDIDATE_KEYS: usize = 100;
/// Valid beacons before low-frequency sampling
/// (`kHighFreqBeaconCount`).
pub const HIGH_FREQ_BEACON_COUNT: u64 = 3;
/// Low-frequency interval multiplier (`kLowFreqBeaconMult`).
pub const LOW_FREQ_BEACON_MULT: i64 = 100;
/// Expired nonces before low-frequency sampling
/// (`kNonceExpirationLimit`).
pub const NONCE_EXPIRATION_LIMIT: u64 = 5;

/// Evidence namespace and codec version.
pub const EVIDENCE_KEY_PREFIX: &str = "beacon-evidence-v1:";
/// Evidence codec version line.
const CODEC_VERSION: &str = "beacon-evidence/1";
/// Evidence key signing key (namespace separator).
const EVIDENCE_SIGNING_KEY: &str = "BeaconEvidenceKey";

/* Statistics (`CriticalImagesFinder::InitStats` + beacon filter). */

/// Valid-evidence lookups (`kCriticalImagesValidCount`).
pub const VALID_COUNT: &str = "critical_images_valid_count";
/// Expired-evidence lookups (`kCriticalImagesExpiredCount`).
pub const EXPIRED_COUNT: &str = "critical_images_expired_count";
/// Missing-evidence lookups (`kCriticalImagesNotFoundCount`).
pub const NOT_FOUND_COUNT: &str = "critical_images_not_found_count";
/// Beacon insertions (`kCriticalImagesBeaconAddedCount`).
pub const BEACON_ADDED_COUNT: &str = "critical_images_beacon_filter_script_added_count";

/// Hashes an image URL to its beacon key (`GetKeyForUrl`): decimal
/// ×131 over the absolute spec.
#[must_use]
pub fn url_hash(url: &str) -> String {
    hash_string_case_preserve(url.as_bytes()).to_string()
}

/// Builds the evidence key for a page variant (URL, options hash,
/// and device suffix, like the beacon cohort's property page).
#[must_use]
pub fn evidence_key(url: &str, options_hash: &str, device_suffix: &str) -> String {
    let identity = format!("{url}\n{options_hash}\n{device_suffix}");
    format!(
        "{EVIDENCE_KEY_PREFIX}{}",
        sign_url(EVIDENCE_SIGNING_KEY, &identity)
    )
}

/// Beacon evidence for one page variant (the port's
/// `CriticalImages` proto for the beacon cohort).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeaconEvidence {
    /// Key-hash to support count (decayed per accepted beacon).
    pub support: BTreeMap<String, i64>,
    /// Maximum possible support (decay baseline).
    pub max_support: i64,
    /// Accepted beacons merged (`valid_beacons_received`).
    pub valid_beacons: u64,
    /// Nonces expired since the last accepted beacon.
    pub nonces_expired: u64,
    /// Outstanding (nonce, issued-ms) pairs.
    pub pending_nonces: Vec<(String, i64)>,
    /// Next sampling-due timestamp in ms (0 = due now).
    pub next_beacon_ms: i64,
    /// Last reported LCP kind (`""` when unknown).
    pub lcp_kind: String,
    /// Last reported LCP loaded URL (`""` when stamped, text, or
    /// unknown).
    pub lcp_image_url: String,
    /// Accepted-beacon generation (HTML identity input).
    pub generation: u64,
}

/// Finder availability (`CriticalImagesFinder::Availability`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Beaconing disabled; data will never arrive.
    Disabled,
    /// Beaconing on but no valid beacon merged yet.
    NoDataYet,
    /// Merged evidence answers lookups.
    Available,
}

/// Encodes evidence to cache bytes (versioned line codec; support
/// entries sort by key for stable output).
#[must_use]
pub fn encode_evidence(evidence: &BeaconEvidence) -> Vec<u8> {
    let mut out = format!(
        "{CODEC_VERSION}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        evidence.max_support,
        evidence.valid_beacons,
        evidence.nonces_expired,
        evidence.next_beacon_ms,
        evidence.generation,
        web64_encode(evidence.lcp_kind.as_bytes()),
        web64_encode(evidence.lcp_image_url.as_bytes()),
    );
    out.push_str(&format!("{}\n", evidence.pending_nonces.len()));
    for (nonce, issued_ms) in &evidence.pending_nonces {
        out.push_str(&format!(
            "{}\t{issued_ms}\n",
            web64_encode(nonce.as_bytes())
        ));
    }
    out.push_str(&format!("{}\n", evidence.support.len()));
    for (key, support) in &evidence.support {
        out.push_str(&format!("{}\t{support}\n", web64_encode(key.as_bytes())));
    }
    out.into_bytes()
}

/// Decodes evidence cache bytes (`None` on version mismatch or any
/// malformed field).
#[must_use]
pub fn decode_evidence(bytes: &[u8]) -> Option<BeaconEvidence> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.split('\n');
    if lines.next()? != CODEC_VERSION {
        return None;
    }
    let number = |line: Option<&str>| line?.parse::<i64>().ok();
    let max_support = number(lines.next())?;
    let valid_beacons = number(lines.next())? as u64;
    let nonces_expired = number(lines.next())? as u64;
    let next_beacon_ms = number(lines.next())?;
    let generation = number(lines.next())? as u64;
    let lcp_kind = String::from_utf8(web64_decode(lines.next()?)?).ok()?;
    let lcp_image_url = String::from_utf8(web64_decode(lines.next()?)?).ok()?;
    let pending_count = number(lines.next())?;
    if pending_count < 0 || pending_count > MAX_PENDING_NONCES as i64 {
        return None;
    }
    let mut pending_nonces = Vec::new();
    for _ in 0..pending_count {
        let line = lines.next()?;
        let (nonce, issued) = line.split_once('\t')?;
        let nonce = String::from_utf8(web64_decode(nonce)?).ok()?;
        pending_nonces.push((nonce, issued.parse::<i64>().ok()?));
    }
    let support_count = number(lines.next())?;
    if support_count < 0 {
        return None;
    }
    let mut support = BTreeMap::new();
    for _ in 0..support_count {
        let line = lines.next()?;
        let (key, value) = line.split_once('\t')?;
        let key = String::from_utf8(web64_decode(key)?).ok()?;
        support.insert(key, value.parse::<i64>().ok()?);
    }
    Some(BeaconEvidence {
        support,
        max_support,
        valid_beacons,
        nonces_expired,
        pending_nonces,
        next_beacon_ms,
        lcp_kind,
        lcp_image_url,
        generation,
    })
}

/// Decodes web64 (`None` on invalid input).
fn web64_decode(text: &str) -> Option<Vec<u8>> {
    ps_cache::base64::web64_decode(text)
}

/// Constant-time string comparison (`ConstantTimeCompare`).
#[must_use]
pub fn constant_time_compare(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Classifies evidence for a rewrite (`Available`): disabled
/// unless beaconing is on; available once at least one valid
/// beacon merged (`IsBeaconDataAvailable`).
#[must_use]
pub fn availability(beacon_enabled: bool, evidence: Option<&BeaconEvidence>) -> Availability {
    if !beacon_enabled {
        return Availability::Disabled;
    }
    match evidence {
        Some(record) if record.valid_beacons > 0 => Availability::Available,
        _ => Availability::NoDataYet,
    }
}

/// Collects the critical key hashes (`GetCriticalKeysFromProto`
/// at 100%: support at or above the decayed maximum; empty keys
/// excluded).
#[must_use]
pub fn html_critical_images(evidence: &BeaconEvidence) -> HashSet<String> {
    let threshold = PERCENT_SEEN_FOR_CRITICAL * evidence.max_support;
    evidence
        .support
        .iter()
        .filter(|(key, support)| !key.is_empty() && **support * 100 >= threshold)
        .map(|(key, _)| key.clone())
        .collect()
}

/// Whether an image URL is critical (`IsHtmlCriticalImage`).
#[must_use]
pub fn is_html_critical_image(image_url: &str, evidence: &BeaconEvidence) -> bool {
    html_critical_images(evidence).contains(&url_hash(image_url))
}

/// Downstream-cache beacon gate (`ShouldBeacon` rebeaconing-key
/// path): with downstream integration plus a configured key, only
/// matching `PS-ShouldBeacon` request headers rebeacon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownstreamBeacon {
    /// No downstream gate applies.
    Inactive,
    /// Gate applies; whether the request header matched.
    MustMatch(bool),
}

/// Builds the downstream gate from options plus the request's
/// `PS-ShouldBeacon` header value, if any.
#[must_use]
pub fn downstream_beacon_gate(
    options: &ps_config::options::RewriteOptions,
    header: Option<&str>,
) -> DownstreamBeacon {
    if !options.is_downstream_cache_integration_enabled() {
        return DownstreamBeacon::Inactive;
    }
    if options.downstream_cache_rebeaconing_key().is_empty() {
        return DownstreamBeacon::Inactive;
    }
    let matched =
        header.is_some_and(|value| options.matches_downstream_cache_rebeaconing_key(value));
    DownstreamBeacon::MustMatch(matched)
}

/// Whether beacon learning drives rewrites
/// (`is_critical_images_beacon_enabled`, minus the factory and
/// property-cache conjuncts, which always hold here: the cohort
/// is the metadata cache).
#[must_use]
pub fn beaconing_active(options: &ps_config::options::RewriteOptions) -> bool {
    use ps_config::filter::Filter;
    let filters = options.filters();
    (filters.enabled(Filter::LazyloadImages)
        || filters.enabled(Filter::InlineImages)
        || filters.enabled(Filter::DelayImages)
        || filters.enabled(Filter::PrioritizeLcpImages)
        || filters.enabled(Filter::ResizeToRenderedImageDimensions))
        && options.critical_images_beacon_enabled()
}

/// Whether a rewrite is sampling-due (`ShouldBeacon`): the
/// downstream key decides when gated, else the first visit past
/// the reinstrumentation timestamp beacons.
#[must_use]
pub fn should_beacon(
    now_ms: i64,
    evidence: Option<&BeaconEvidence>,
    downstream: DownstreamBeacon,
) -> bool {
    if let DownstreamBeacon::MustMatch(matched) = downstream {
        return matched;
    }
    evidence.is_none_or(|record| now_ms >= record.next_beacon_ms)
}

/// Decays one support value by k/(k+1), rounding down (`Decay`).
fn decay(support_interval: i64, support: i64) -> i64 {
    support * support_interval / (support_interval + 1)
}

/// Saturating int64 addition (capped like the int32 proto math).
fn saturating_add(current: i64, addend: i64) -> i64 {
    current.saturating_add(addend)
}

/// Merges one beacon result into the support map
/// (`UpdateCriticalKeys`): decay everything, refresh the maximum,
/// credit the reported set (candidate members only, against
/// cache-fill DoS), and count the beacon.
fn merge_support(evidence: &mut BeaconEvidence, reported: &HashSet<String>) {
    // Decay every entry, dropping keys whose support falls to
    // zero; never-supported candidates (support 0) survive.
    evidence.support.retain(|_, support| {
        let old = *support;
        let decayed = decay(IMAGE_SUPPORT_INTERVAL, old);
        *support = decayed;
        decayed != 0 || old == 0
    });
    evidence.max_support = decay(IMAGE_SUPPORT_INTERVAL, evidence.max_support);
    evidence.max_support = saturating_add(evidence.max_support, IMAGE_SUPPORT_INTERVAL);
    for key in reported {
        if let Some(support) = evidence.support.get_mut(key) {
            *support = saturating_add(*support, IMAGE_SUPPORT_INTERVAL);
        }
    }
    evidence.valid_beacons += 1;
    evidence.nonces_expired = 0;
    evidence.generation += 1;
}

/// Expires pending nonces past the timeout, counting them
/// (`ClearInvalidNonces`); with none valid outstanding, the list
/// clears.
fn clear_invalid_nonces(now_ms: i64, evidence: &mut BeaconEvidence) {
    evidence.pending_nonces.retain(|(_, issued_ms)| {
        if issued_ms + NONCE_TIMEOUT_MS < now_ms {
            evidence.nonces_expired += 1;
            false
        } else {
            true
        }
    });
}

/// Issues a web64 nonce from a 64-bit draw, truncated to
/// [`NONCE_CHARS`] (`AddNonceToCriticalSelectors` shaping).
#[must_use]
pub fn format_nonce(nonce_value: u64) -> String {
    let mut out = web64_encode(&nonce_value.to_le_bytes());
    out.truncate(NONCE_CHARS);
    out
}

/// Prepares beacon insertion (`PrepareForBeaconInsertionHelper`):
/// `None` when sampling is not due; otherwise schedules the next
/// reinstrumentation (low-frequency after enough valid beacons
/// or expired nonces) and issues a pending nonce. The caller
/// persists the mutated evidence.
pub fn prepare_beacon(
    evidence: &mut BeaconEvidence,
    now_ms: i64,
    reinstrument_sec: i64,
    nonce_value: u64,
) -> Option<String> {
    if now_ms < evidence.next_beacon_ms {
        return None;
    }
    let mut interval_ms = reinstrument_sec.max(0) * 1000;
    if evidence.nonces_expired > NONCE_EXPIRATION_LIMIT
        || evidence.valid_beacons >= HIGH_FREQ_BEACON_COUNT
    {
        interval_ms *= LOW_FREQ_BEACON_MULT;
    }
    evidence.next_beacon_ms = now_ms + interval_ms;
    clear_invalid_nonces(now_ms, evidence);
    let nonce = format_nonce(nonce_value);
    if evidence.pending_nonces.len() >= MAX_PENDING_NONCES {
        // Full of live nonces: replace the oldest, whose beacon is
        // the least likely to still arrive.
        if let Some(oldest) = evidence
            .pending_nonces
            .iter_mut()
            .min_by_key(|(_, issued_ms)| *issued_ms)
        {
            *oldest = (nonce.clone(), now_ms);
        }
        return Some(nonce);
    }
    evidence.pending_nonces.push((nonce.clone(), now_ms));
    Some(nonce)
}

/// Validates and consumes a reported nonce
/// (`ValidateAndExpireNonce`): over-long or unknown nonces fail.
fn validate_nonce(now_ms: i64, nonce: &str, evidence: &mut BeaconEvidence) -> bool {
    if nonce.is_empty() || nonce.len() > NONCE_CHARS {
        return false;
    }
    clear_invalid_nonces(now_ms, evidence);
    if let Some(slot) = evidence
        .pending_nonces
        .iter()
        .position(|(pending, _)| constant_time_compare(nonce, pending))
    {
        evidence.pending_nonces.remove(slot);
        return true;
    }
    false
}

/// Registers page candidate keys (`UpdateCandidateKeys`): unknown
/// keys enter with zero support; changed sets reset the beacon
/// count (high-frequency mode) and optionally force rebeaconing.
/// Returns whether the set changed.
pub fn update_candidates(
    evidence: &mut BeaconEvidence,
    keys: &HashSet<String>,
    clear_rebeacon: bool,
) -> bool {
    let mut changed = false;
    for key in keys {
        if !evidence.support.contains_key(key) {
            evidence.support.insert(key.clone(), 0);
            changed = true;
        }
    }
    if !changed {
        return false;
    }
    // Forget unsupported keys that left the page once they exceed
    // the page's key count or the retention cap.
    let stale: Vec<String> = evidence
        .support
        .iter()
        .filter(|(key, support)| **support <= 0 && !keys.contains(*key))
        .map(|(key, _)| key.clone())
        .collect();
    if stale.len() > MAX_STALE_CANDIDATE_KEYS.max(keys.len()) {
        for key in stale {
            evidence.support.remove(&key);
        }
    }
    evidence.valid_beacons = 0;
    // No generation bump: generation counts accepted beacons
    // only (merge_support). Candidate changes already alter the
    // HTML identity via the reset count and key map above.
    if clear_rebeacon {
        evidence.next_beacon_ms = 0;
    }
    true
}

/// A parsed beacon evidence report (the pcache half's inputs).
#[derive(Debug, Clone, Default)]
pub struct BeaconReport {
    /// Stamped critical-image hashes (`Some`, possibly empty, when
    /// the beacon carries LCP evidence).
    pub html_hashes: Option<HashSet<String>>,
    /// Reported LCP kind (`""` when absent or invalid).
    pub lcp_kind: String,
    /// Reported LCP loaded URL (`""` when absent, over-long,
    /// stamped, or text).
    pub lcp_image_url: String,
    /// Reported nonce (`""` when absent).
    pub nonce: String,
}

/// Outcome of merging a beacon report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptOutcome {
    /// Nonce valid; evidence merged and generation bumped.
    Accepted,
    /// Nonce missing, over-long, unknown, or expired.
    BadNonce,
    /// Nothing reportable carried (no store, no validation).
    Empty,
}

/// Merges a beacon report
/// (`UpdateCriticalImagesCacheEntry`): validates the nonce, stores
/// LCP kind/URL, and merges the hash set. Empty reports store
/// nothing; bad nonces reject. Returns whether the evidence
/// changed (callers persist on change, which also preserves
/// nonce-expiry feedback).
pub fn accept_beacon(
    evidence: &mut BeaconEvidence,
    report: &BeaconReport,
    now_ms: i64,
) -> AcceptOutcome {
    let Some(hashes) = report.html_hashes.as_ref() else {
        return AcceptOutcome::Empty;
    };
    if !validate_nonce(now_ms, &report.nonce, evidence) {
        return AcceptOutcome::BadNonce;
    }
    if !report.lcp_kind.is_empty() {
        evidence.lcp_kind = report.lcp_kind.clone();
        if !report.lcp_image_url.is_empty() {
            evidence.lcp_image_url = report.lcp_image_url.clone();
        } else {
            evidence.lcp_image_url.clear();
        }
    }
    // Beacon results outside the registered candidates are
    // ignored, so a beacon cannot add entries of its own choosing.
    let known: HashSet<String> = hashes
        .iter()
        .filter(|hash| evidence.support.contains_key(*hash))
        .cloned()
        .collect();
    merge_support(evidence, &known);
    AcceptOutcome::Accepted
}

/// Evidence lookup outcome (valid/expired/not-found counting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceLookup {
    /// Decoded record present.
    Found(BeaconEvidence),
    /// Record present but undecodable.
    Corrupt,
    /// No record stored.
    Missing,
}

impl EvidenceLookup {
    /// Borrows the record, if found.
    #[must_use]
    pub fn record(&self) -> Option<&BeaconEvidence> {
        match self {
            EvidenceLookup::Found(record) => Some(record),
            _ => None,
        }
    }

    /// Unwraps the record, defaulting to empty when absent (a
    /// corrupt record restarts empty, like an expired property).
    #[must_use]
    pub fn into_record(self) -> BeaconEvidence {
        match self {
            EvidenceLookup::Found(record) => record,
            _ => BeaconEvidence::default(),
        }
    }
}

/// Metadata-cache-backed evidence store (the port's beacon
/// cohort). Counter bumps stay with callers (they own the
/// statistics handle); the exported `*_COUNT` names keep them
/// consistent.
pub struct BeaconFinder {
    cache: Arc<dyn CacheBackend>,
}

impl BeaconFinder {
    /// Binds a finder to a metadata cache backend.
    #[must_use]
    pub fn new(cache: &Arc<dyn CacheBackend>) -> Self {
        BeaconFinder {
            cache: Arc::clone(cache),
        }
    }

    /// Loads a variant's evidence, distinguishing absent from
    /// corrupt records (callers count valid/expired/not-found).
    #[must_use]
    pub fn load(&self, url: &str, options_hash: &str, device: &str) -> EvidenceLookup {
        let lookup = self.cache.get(&evidence_key(url, options_hash, device));
        match lookup {
            ps_cache::cache::Lookup {
                state: KeyState::Available,
                value: Some(blob),
            } => match decode_evidence(&blob) {
                Some(evidence) => EvidenceLookup::Found(evidence),
                None => EvidenceLookup::Corrupt,
            },
            _ => EvidenceLookup::Missing,
        }
    }

    /// Mutate evidence under the metadata backend's transaction. None means
    /// contention: no nonce/report state was changed, so the caller may retry.
    pub fn update<R>(
        &self,
        url: &str,
        options_hash: &str,
        device: &str,
        mut change: impl FnMut(&mut BeaconEvidence) -> R,
    ) -> Option<R> {
        let mut result = None;
        self.cache
            .update(&evidence_key(url, options_hash, device), &mut |lookup| {
                let mut evidence = lookup
                    .value
                    .as_deref()
                    .and_then(decode_evidence)
                    .unwrap_or_default();
                let before = evidence.clone();
                result = Some(change(&mut evidence));
                (evidence != before).then(|| encode_evidence(&evidence))
            });
        result
    }

    /// Stores a variant's evidence (best-effort, like other
    /// metadata writes).
    pub fn store(&self, url: &str, options_hash: &str, device: &str, evidence: &BeaconEvidence) {
        self.cache.put(
            &evidence_key(url, options_hash, device),
            &encode_evidence(evidence),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn multiprocess_evidence_updates_preserve_nonces_and_reports() {
        use ps_cache::cache::{Cache, FileCache, FileCachePolicy};
        const ENV: &str = "PS_BEACON_TRANSACTION_CHILD";
        let worker = std::env::var("PS_BEACON_TRANSACTION_WORKER").ok();
        let directory = std::env::var_os(ENV)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!(
                    "ps-beacon-transactions-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))
            });
        std::fs::create_dir_all(&directory).unwrap();
        let file: Arc<dyn Cache> = Arc::new(FileCache::new(
            &directory.join("disk"),
            FileCachePolicy {
                clean_interval_ms: -1,
                target_size_bytes: 16_777_216,
                target_inode_count: 0,
                path_length_limit: 8192,
            },
            0,
        ));
        let cache: Arc<dyn Cache> = Arc::new(
            ps_cache::transactional::TransactionalCache::open(
                &directory.join("transactions"),
                file,
            )
            .unwrap(),
        );
        let finder = BeaconFinder::new(&cache);
        if let Some(worker) = worker {
            for iteration in 0..32 {
                let nonce = format_nonce((worker.parse::<u64>().unwrap() << 32) | iteration);
                // Generous: four children hammer one transaction shard
                // while the full workspace suite saturates CPU/disk.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while finder
                    .update("http://h/p", "options", "@Desktop", |evidence| {
                        evidence.pending_nonces.push((nonce.clone(), 1000));
                    })
                    .is_none()
                {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                }
                let report = report_with(Some(HashSet::new()), "img", "", &nonce);
                let accepted = loop {
                    if let Some(outcome) =
                        finder.update("http://h/p", "options", "@Desktop", |evidence| {
                            accept_beacon(evidence, &report, 1001)
                        })
                    {
                        break outcome;
                    }
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                };
                assert_eq!(accepted, AcceptOutcome::Accepted);
                let replay = loop {
                    if let Some(outcome) =
                        finder.update("http://h/p", "options", "@Desktop", |evidence| {
                            accept_beacon(evidence, &report, 1001)
                        })
                    {
                        break outcome;
                    }
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                };
                assert_eq!(replay, AcceptOutcome::BadNonce);
            }
            return;
        }
        let mut children = Vec::new();
        for worker in [1, 2, 3, 4] {
            children.push(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "beacon_finder::tests::multiprocess_evidence_updates_preserve_nonces_and_reports", "--nocapture"])
                .env(ENV, &directory).env("PS_BEACON_TRANSACTION_WORKER", worker.to_string())
                .stdout(std::process::Stdio::null()).spawn().unwrap());
        }
        let statuses: Vec<_> = children
            .iter_mut()
            .map(|child| child.wait().unwrap())
            .collect();
        assert!(statuses.iter().all(|status| status.success()));
        let evidence = finder
            .load("http://h/p", "options", "@Desktop")
            .into_record();
        assert_eq!(evidence.valid_beacons, 128);
        assert_eq!(evidence.generation, 128);
        assert!(evidence.pending_nonces.is_empty());
        drop(finder);
        drop(cache);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn report_with(
        hashes: Option<HashSet<String>>,
        kind: &str,
        url: &str,
        nonce: &str,
    ) -> BeaconReport {
        BeaconReport {
            html_hashes: hashes,
            lcp_kind: kind.to_string(),
            lcp_image_url: url.to_string(),
            nonce: nonce.to_string(),
        }
    }

    fn prepared(now_ms: i64) -> (BeaconEvidence, String) {
        let mut evidence = BeaconEvidence::default();
        let nonce = prepare_beacon(&mut evidence, now_ms, 5, 0x1234_5678_9abc_def0)
            .expect("first visit is sampling-due");
        assert_eq!(NONCE_CHARS, nonce.len());
        (evidence, nonce)
    }

    #[test]
    fn url_hash_matches_reference() {
        // result = result * 131 + byte, decimal output.
        assert_eq!(url_hash(""), "0");
        assert_eq!(url_hash("a"), "97");
        assert_eq!(url_hash("ab"), (97u32 * 131 + 98).to_string());
        assert_eq!(
            url_hash("http://example.com/x.png"),
            hash_string_case_preserve(b"http://example.com/x.png").to_string()
        );
        assert!(evidence_key("http://h/p", "oh", "@Desktop").starts_with(EVIDENCE_KEY_PREFIX));
        assert_ne!(
            evidence_key("http://h/p", "oh", "@Desktop"),
            evidence_key("http://h/p", "oh", "@Mobile")
        );
    }

    #[test]
    fn codec_round_trip_and_rejects() {
        let mut evidence = BeaconEvidence::default();
        evidence.support.insert("1".to_string(), 10);
        evidence.support.insert("2".to_string(), 0);
        evidence.max_support = 10;
        evidence.valid_beacons = 3;
        evidence.nonces_expired = 1;
        evidence.pending_nonces.push(("n1".to_string(), 100));
        evidence.next_beacon_ms = 200;
        evidence.lcp_kind = LCP_KIND_IMG.to_string();
        evidence.lcp_image_url = "http://h/i.png".to_string();
        evidence.generation = 7;
        let bytes = encode_evidence(&evidence);
        assert_eq!(decode_evidence(&bytes).as_ref(), Some(&evidence));
        assert!(decode_evidence(b"bogus").is_none());
        assert!(decode_evidence(b"beacon-evidence/9\n").is_none());
        let mut truncated = bytes.clone();
        truncated.truncate(truncated.len() / 2);
        assert!(decode_evidence(&truncated).is_none());
    }

    #[test]
    fn availability_needs_a_valid_beacon() {
        assert_eq!(availability(false, None), Availability::Disabled);
        assert_eq!(
            availability(false, Some(&BeaconEvidence::default())),
            Availability::Disabled
        );
        assert_eq!(availability(true, None), Availability::NoDataYet);
        assert_eq!(
            availability(true, Some(&BeaconEvidence::default())),
            Availability::NoDataYet
        );
        let mut evidence = BeaconEvidence::default();
        evidence.valid_beacons = 1;
        assert_eq!(availability(true, Some(&evidence)), Availability::Available);
    }

    #[test]
    fn criticality_needs_max_support() {
        let mut evidence = BeaconEvidence::default();
        assert!(html_critical_images(&evidence).is_empty());
        evidence.support.insert(url_hash("http://h/a.png"), 10);
        evidence.support.insert(url_hash("http://h/b.png"), 5);
        evidence.max_support = 10;
        assert!(is_html_critical_image("http://h/a.png", &evidence));
        assert!(!is_html_critical_image("http://h/b.png", &evidence));
        assert!(!is_html_critical_image("http://h/c.png", &evidence));
        assert_eq!(1, html_critical_images(&evidence).len());
    }

    #[test]
    fn sampling_cadence_and_low_frequency() {
        use DownstreamBeacon::Inactive;
        // Unknown variants are due immediately.
        assert!(should_beacon(1000, None, Inactive));
        let (mut evidence, _) = prepared(1000);
        // Just instrumented: next due in 5s (high frequency).
        assert_eq!(6000, evidence.next_beacon_ms);
        assert!(!should_beacon(5999, Some(&evidence), Inactive));
        assert!(should_beacon(6000, Some(&evidence), Inactive));
        // A downstream gate replaces the timestamp check.
        assert!(should_beacon(
            5999,
            Some(&evidence),
            DownstreamBeacon::MustMatch(true)
        ));
        assert!(!should_beacon(
            6000,
            Some(&evidence),
            DownstreamBeacon::MustMatch(false)
        ));
        // Three valid beacons flip to low frequency (×100).
        evidence.valid_beacons = HIGH_FREQ_BEACON_COUNT;
        evidence.next_beacon_ms = 0;
        prepare_beacon(&mut evidence, 10_000, 5, 7).expect("due");
        assert_eq!(10_000 + 500_000, evidence.next_beacon_ms);
        // Expired nonces flip it too.
        let mut fresh = BeaconEvidence::default();
        fresh.nonces_expired = NONCE_EXPIRATION_LIMIT + 1;
        prepare_beacon(&mut fresh, 10_000, 5, 7).expect("due");
        assert_eq!(10_000 + 500_000, fresh.next_beacon_ms);
        // Not-due preparation is a no-op returning None.
        assert!(prepare_beacon(&mut fresh, 10_001, 5, 8).is_none());
    }

    #[test]
    fn nonces_validate_once_and_expire() {
        let (mut evidence, nonce) = prepared(1000);
        // Unknown nonces fail without consuming the pending one.
        let mut probe = evidence.clone();
        assert!(!super::validate_nonce(1000, "aaaaaaaaaaa", &mut probe));
        assert_eq!(1, probe.pending_nonces.len());
        // Over-long nonces fail fast.
        assert!(!super::validate_nonce(1000, "aaaaaaaaaaaa", &mut probe));
        // The issued nonce validates exactly once.
        assert!(super::validate_nonce(1000, &nonce, &mut evidence));
        assert!(evidence.pending_nonces.is_empty());
        assert!(!super::validate_nonce(1000, &nonce, &mut evidence));
        // Stale nonces expire and count.
        let (mut old, stale) = prepared(1000);
        assert!(!super::validate_nonce(
            1000 + NONCE_TIMEOUT_MS + 1,
            &stale,
            &mut old
        ));
        assert_eq!(1, old.nonces_expired);
        assert!(old.pending_nonces.is_empty());
    }

    #[test]
    fn candidates_gate_and_reset() {
        let mut evidence = BeaconEvidence::default();
        let keys: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        // New keys register at zero support and reset beacons.
        evidence.valid_beacons = 5;
        assert!(update_candidates(&mut evidence, &keys, true));
        assert_eq!(0, evidence.valid_beacons);
        assert_eq!(0, evidence.next_beacon_ms);
        assert_eq!(Some(0), evidence.support.get("a").copied());
        // Re-registering changes nothing.
        assert!(!update_candidates(&mut evidence, &keys, true));
        // Added keys reset again without forcing rebeacon.
        evidence.next_beacon_ms = 999;
        evidence.valid_beacons = 2;
        let more: HashSet<String> = ["a".to_string(), "c".to_string()].into_iter().collect();
        assert!(update_candidates(&mut evidence, &more, false));
        assert_eq!(0, evidence.valid_beacons);
        assert_eq!(999, evidence.next_beacon_ms);
        // Candidate churn never bumps the accepted-beacon
        // generation (merge_support owns it).
        assert_eq!(0, evidence.generation);
    }

    #[test]
    fn accept_stamped_beacon_end_to_end() {
        let (mut evidence, nonce) = prepared(1000);
        let hash = url_hash("http://h/hero.png");
        let keys: HashSet<String> = [hash.clone()].into_iter().collect();
        update_candidates(&mut evidence, &keys, false);
        // Empty reports store nothing and skip validation.
        assert_eq!(
            accept_beacon(&mut evidence, &report_with(None, "", "", ""), 1000),
            AcceptOutcome::Empty
        );
        // Bad nonces reject.
        assert_eq!(
            accept_beacon(
                &mut evidence,
                &report_with(Some(keys.clone()), LCP_KIND_IMG, "", "bad"),
                1000
            ),
            AcceptOutcome::BadNonce
        );
        assert_eq!(0, evidence.valid_beacons);
        // The stamped hash merges with img kind.
        assert_eq!(
            accept_beacon(
                &mut evidence,
                &report_with(Some(keys), LCP_KIND_IMG, "", &nonce),
                1000
            ),
            AcceptOutcome::Accepted
        );
        assert_eq!(1, evidence.valid_beacons);
        assert_eq!(1, evidence.generation);
        assert_eq!(LCP_KIND_IMG, evidence.lcp_kind);
        assert!(evidence.lcp_image_url.is_empty());
        assert!(is_html_critical_image("http://h/hero.png", &evidence));
    }

    #[test]
    fn accept_unstamped_and_text_lcp() {
        // Unstamped img LCP: empty set still merges (decay +
        // availability), URL stored.
        let (mut evidence, nonce) = prepared(1000);
        assert_eq!(
            accept_beacon(
                &mut evidence,
                &report_with(
                    Some(HashSet::new()),
                    LCP_KIND_IMG,
                    "http://h/bg.png",
                    &nonce
                ),
                1000
            ),
            AcceptOutcome::Accepted
        );
        assert_eq!(1, evidence.valid_beacons);
        assert_eq!(LCP_KIND_IMG, evidence.lcp_kind);
        assert_eq!("http://h/bg.png", evidence.lcp_image_url);
        assert_eq!(Availability::Available, availability(true, Some(&evidence)));
        // Text LCP clears any stored URL.
        let (mut text, nonce) = prepared(2000);
        text.lcp_image_url = "http://h/old.png".to_string();
        assert_eq!(
            accept_beacon(
                &mut text,
                &report_with(Some(HashSet::new()), LCP_KIND_TEXT, "", &nonce),
                2000
            ),
            AcceptOutcome::Accepted
        );
        assert_eq!(LCP_KIND_TEXT, text.lcp_kind);
        assert!(text.lcp_image_url.is_empty());
        // Foreign hashes never enter the set.
        let (mut foreign, nonce) = prepared(3000);
        update_candidates(
            &mut foreign,
            &["known".to_string()].into_iter().collect(),
            false,
        );
        let evil: HashSet<String> = ["known".to_string(), "evil".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            accept_beacon(
                &mut foreign,
                &report_with(Some(evil), LCP_KIND_IMG, "", &nonce),
                3000
            ),
            AcceptOutcome::Accepted
        );
        assert!(foreign.support.contains_key("known"));
        assert!(!foreign.support.contains_key("evil"));
    }

    #[test]
    fn finder_round_trips_through_cache() {
        let cache: Arc<dyn CacheBackend> = Arc::new(ps_cache::cache::MemoryCache::new());
        let finder = BeaconFinder::new(&cache);
        assert_eq!(
            finder.load("http://h/p", "oh", "@Desktop"),
            EvidenceLookup::Missing
        );
        let mut evidence = BeaconEvidence::default();
        evidence.valid_beacons = 2;
        evidence.lcp_kind = LCP_KIND_BACKGROUND.to_string();
        finder.store("http://h/p", "oh", "@Desktop", &evidence);
        assert_eq!(
            finder.load("http://h/p", "oh", "@Desktop"),
            EvidenceLookup::Found(evidence)
        );
        // Variants segregate by device.
        assert_eq!(
            finder.load("http://h/p", "oh", "@Mobile"),
            EvidenceLookup::Missing
        );
    }
}

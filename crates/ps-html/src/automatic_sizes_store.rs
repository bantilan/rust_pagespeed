//! Transactional nonce-bound measured sizes. Not yet connected to HTTP/JS.
//! Requires a process-safe Cache::update backend (as used by beacon evidence).
//! Measurements and consumed nonces occupy one record; no split-key commits.
use crate::automatic_sizes::{MeasuredSizes, MeasurementBatch, MAX_PLACEMENTS};
use ps_cache::cache::{Cache, KeyState};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const RECORD_TTL_MS: i64 = 300_000;
pub const NONCE_TTL_MS: i64 = 60_000;
pub const MAX_NONCES: usize = 128;
const MAX_RECORD_BYTES: usize = 256 * 1024;
const VERSION: &str = "automatic-sizes-store/1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Nonce {
    issued_ms: i64,
    consumed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    epoch: String,
    expires_ms: i64,
    generation: u64,
    issued: BTreeSet<String>,
    nonces: BTreeMap<String, Nonce>,
    measurements: BTreeMap<String, MeasuredSizes>,
}

/// A successful mutation still relies on the backend's persistence semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterOutcome {
    Registered,
    DuplicateNonce,
    ManifestMismatch,
    Capacity,
    InvalidInput,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOutcome {
    Accepted {
        hints_changed: bool,
        generation: u64,
    },
    BadNonce,
    Expired,
    Unissued,
    Capacity,
    InvalidInput,
}

/// Expiry-checked snapshot, for a later HTML consumer. No nonce data exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasurementSnapshot {
    /// Include epoch AND generation in dependent HTML identities; a TTL reset
    /// must not collide with the previous cohort's generation numbers.
    pub epoch: String,
    pub expires_ms: i64,
    pub generation: u64,
    pub measurements: BTreeMap<String, MeasuredSizes>,
}

pub struct MeasurementStore {
    cache: Arc<dyn Cache>,
}
impl MeasurementStore {
    #[must_use]
    pub fn new(cache: Arc<dyn Cache>) -> Self {
        Self { cache }
    }

    /// `scope` is a server-derived page/options/document/style digest. `nonce`
    /// must be an unpredictable server draw, never supplied by a report.
    /// None means unavailable/contended; caller must not send an unregistered nonce.
    pub fn register(
        &self,
        scope: &str,
        nonce: &str,
        issued: &BTreeSet<String>,
        now_ms: i64,
    ) -> Option<RegisterOutcome> {
        if !valid_id(scope)
            || !valid_nonce(nonce)
            || !valid_time(now_ms)
            || issued.is_empty()
            || issued.len() > MAX_PLACEMENTS
            || !issued.iter().all(|id| valid_id(id))
        {
            return Some(RegisterOutcome::InvalidInput);
        }
        let mut outcome = None;
        let completed = self.cache.update(&key(scope), &mut |lookup| {
            if !matches!(lookup.state, KeyState::Available | KeyState::NotFound) {
                return None;
            }
            let existing = if lookup.state == KeyState::Available {
                lookup.value.as_deref().and_then(decode)
            } else {
                None
            };
            let mut record = match existing {
                Some(r) if now_ms < r.expires_ms => r,
                _ => Record {
                    epoch: nonce.into(),
                    expires_ms: now_ms + RECORD_TTL_MS,
                    generation: 0,
                    issued: issued.clone(),
                    nonces: BTreeMap::new(),
                    measurements: BTreeMap::new(),
                },
            };
            if now_ms < record.expires_ms - RECORD_TTL_MS {
                outcome = Some(RegisterOutcome::InvalidInput);
                return None;
            }
            if record.issued != *issued {
                outcome = Some(RegisterOutcome::ManifestMismatch);
                return None;
            }
            if record.nonces.contains_key(nonce) {
                outcome = Some(RegisterOutcome::DuplicateNonce);
                return None;
            }
            if record.nonces.len() == MAX_NONCES {
                outcome = Some(RegisterOutcome::Capacity);
                return None;
            }
            record.nonces.insert(
                nonce.into(),
                Nonce {
                    issued_ms: now_ms,
                    consumed: false,
                },
            );
            outcome = Some(RegisterOutcome::Registered);
            Some(encode(&record))
        });
        if completed {
            outcome
        } else {
            None
        }
    }

    /// Authenticate, merge and consume under one transaction. Invalid/partial
    /// batches leave both nonce and evidence untouched. None means retry later.
    pub fn accept(
        &self,
        scope: &str,
        nonce: &str,
        batch: &MeasurementBatch,
        now_ms: i64,
    ) -> Option<MergeOutcome> {
        if !valid_id(scope) || !valid_nonce(nonce) || !valid_time(now_ms) {
            return Some(MergeOutcome::InvalidInput);
        }
        let mut outcome = None;
        let completed = self.cache.update(&key(scope), &mut |lookup| {
            if lookup.state != KeyState::Available {
                if lookup.state == KeyState::NotFound {
                    outcome = Some(MergeOutcome::BadNonce);
                }
                return None;
            }
            let Some(mut record) = lookup.value.as_deref().and_then(decode) else {
                outcome = Some(MergeOutcome::BadNonce);
                return None;
            };
            if now_ms >= record.expires_ms {
                outcome = Some(MergeOutcome::Expired);
                return None;
            }
            let Some(pending) = record.nonces.get(nonce) else {
                outcome = Some(MergeOutcome::BadNonce);
                return None;
            };
            if pending.consumed
                || now_ms < pending.issued_ms
                || now_ms - pending.issued_ms >= NONCE_TTL_MS
            {
                outcome = Some(MergeOutcome::BadNonce);
                return None;
            }
            if !batch.matches_manifest(&record.issued) {
                outcome = Some(MergeOutcome::Unissued);
                return None;
            }
            let mut changed = false;
            for (id, width) in &batch.widths_64 {
                let evidence = record
                    .measurements
                    .entry(id.clone())
                    .or_insert_with(|| MeasuredSizes::new(id).expect("validated issued identity"));
                let before = evidence.sizes(id, "");
                if !evidence.observe(batch.viewport, *width) {
                    outcome = Some(MergeOutcome::Capacity);
                    return None;
                }
                changed |= before != evidence.sizes(id, "");
            }
            if changed {
                let Some(next) = record.generation.checked_add(1) else {
                    outcome = Some(MergeOutcome::Capacity);
                    return None;
                };
                record.generation = next;
            }
            record
                .nonces
                .get_mut(nonce)
                .expect("validated nonce")
                .consumed = true;
            outcome = Some(MergeOutcome::Accepted {
                hints_changed: changed,
                generation: record.generation,
            });
            Some(encode(&record))
        });
        if completed {
            outcome
        } else {
            None
        }
    }

    #[must_use]
    pub fn load(&self, scope: &str, now_ms: i64) -> Option<MeasurementSnapshot> {
        if !valid_id(scope) || !valid_time(now_ms) {
            return None;
        }
        let lookup = self.cache.get(&key(scope));
        if lookup.state != KeyState::Available {
            return None;
        }
        let record = decode(lookup.value.as_deref()?)?;
        if now_ms < record.expires_ms - RECORD_TTL_MS || now_ms >= record.expires_ms {
            return None;
        }
        Some(MeasurementSnapshot {
            epoch: record.epoch,
            expires_ms: record.expires_ms,
            generation: record.generation,
            measurements: record.measurements,
        })
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_nonce(n: &str) -> bool {
    n.len() == 11
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_time(now: i64) -> bool {
    (0..=i64::MAX - RECORD_TTL_MS).contains(&now)
}
fn key(scope: &str) -> String {
    format!("automatic-sizes-cohort-v1:{scope}")
}
fn number<T: std::str::FromStr>(s: &str) -> Option<T> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        None
    } else {
        s.parse().ok()
    }
}
fn encode(record: &Record) -> Vec<u8> {
    let mut out = format!(
        "{VERSION}\n{}\n{}\n{}\n{}\n",
        record.epoch,
        record.expires_ms,
        record.generation,
        record.issued.len()
    );
    for id in &record.issued {
        out.push_str(id);
        out.push('\n');
    }
    out.push_str(&format!("{}\n", record.nonces.len()));
    for (nonce, value) in &record.nonces {
        out.push_str(&format!(
            "{nonce} {} {}\n",
            value.issued_ms,
            u8::from(value.consumed)
        ));
    }
    out.push_str(&format!("{}\n", record.measurements.len()));
    for (id, evidence) in &record.measurements {
        out.push_str(&format!(
            "{id} {}\n",
            ps_cache::base64::web64_encode(evidence.encode().as_bytes())
        ));
    }
    debug_assert!(out.len() <= MAX_RECORD_BYTES);
    out.into_bytes()
}
fn decode(bytes: &[u8]) -> Option<Record> {
    if bytes.len() > MAX_RECORD_BYTES || !bytes.ends_with(b"\n") {
        return None;
    }
    let mut lines = std::str::from_utf8(bytes).ok()?.lines();
    if lines.next()? != VERSION {
        return None;
    }
    let epoch = lines.next()?;
    if !valid_nonce(epoch) {
        return None;
    }
    let expires_ms: i64 = number(lines.next()?)?;
    let generation: u64 = number(lines.next()?)?;
    if expires_ms < RECORD_TTL_MS {
        return None;
    }
    let count: usize = number(lines.next()?)?;
    if !(1..=MAX_PLACEMENTS).contains(&count) {
        return None;
    }
    let mut issued = BTreeSet::new();
    for _ in 0..count {
        let id = lines.next()?;
        if !valid_id(id) || !issued.insert(id.into()) {
            return None;
        }
    }
    let count: usize = number(lines.next()?)?;
    if count > MAX_NONCES {
        return None;
    }
    let mut nonces = BTreeMap::new();
    for _ in 0..count {
        let mut parts = lines.next()?.split(' ');
        let nonce = parts.next()?;
        let issued_ms: i64 = number(parts.next()?)?;
        let consumed = match parts.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        if parts.next().is_some()
            || !valid_nonce(nonce)
            || !valid_time(issued_ms)
            || issued_ms < expires_ms - RECORD_TTL_MS
            || issued_ms >= expires_ms
            || nonces
                .insert(
                    nonce.into(),
                    Nonce {
                        issued_ms,
                        consumed,
                    },
                )
                .is_some()
        {
            return None;
        }
    }
    if !nonces.contains_key(epoch) {
        return None;
    }
    let count: usize = number(lines.next()?)?;
    if count > MAX_PLACEMENTS {
        return None;
    }
    let mut measurements = BTreeMap::new();
    for _ in 0..count {
        let (id, text) = lines.next()?.split_once(' ')?;
        if !issued.contains(id) || text.len() > 2732 {
            return None;
        }
        let raw = ps_cache::base64::web64_decode(text)?;
        let evidence = MeasuredSizes::decode(std::str::from_utf8(&raw).ok()?)?;
        if evidence.identity() != id || measurements.insert(id.into(), evidence).is_some() {
            return None;
        }
    }
    if lines.next().is_some() {
        return None;
    }
    Some(Record {
        epoch: epoch.into(),
        expires_ms,
        generation,
        issued,
        nonces,
        measurements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_cache::cache::{Lookup, MemoryCache};
    const SCOPE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    fn nonce(n: u64) -> String {
        crate::beacon_finder::format_nonce(n)
    }
    fn issued() -> BTreeSet<String> {
        [ID.into()].into_iter().collect()
    }
    fn batch(width: u32) -> MeasurementBatch {
        MeasurementBatch {
            viewport: 412,
            widths_64: [(ID.into(), width)].into_iter().collect(),
        }
    }
    fn fixture() -> (Arc<dyn Cache>, MeasurementStore) {
        let cache: Arc<dyn Cache> = Arc::new(MemoryCache::new());
        (cache.clone(), MeasurementStore::new(cache))
    }

    #[test]
    fn registration_replay_confidence_and_conflicts_are_atomic() {
        let (cache, store) = fixture();
        for n in 1..=5 {
            assert_eq!(
                store.register(SCOPE, &nonce(n), &issued(), 1000),
                Some(RegisterOutcome::Registered)
            );
        }
        for n in 1..=3 {
            assert_eq!(
                store.accept(SCOPE, &nonce(n), &batch(352 * 64), 1100),
                Some(MergeOutcome::Accepted {
                    hints_changed: n == 3,
                    generation: u64::from(n == 3)
                })
            );
            let before = cache.get(&key(SCOPE));
            assert_eq!(
                store.accept(SCOPE, &nonce(n), &batch(352 * 64), 1100),
                Some(MergeOutcome::BadNonce)
            );
            assert_eq!(
                store.register(SCOPE, &nonce(n), &issued(), 1100),
                Some(RegisterOutcome::DuplicateNonce)
            );
            assert_eq!(cache.get(&key(SCOPE)), before);
        }
        let learned = store.load(SCOPE, 1100).unwrap();
        assert_eq!(
            learned.measurements[ID].sizes(ID, "100vw").unwrap(),
            "(width: 412px) 352px, 100vw"
        );
        assert_eq!(
            store.accept(SCOPE, &nonce(4), &batch(352 * 64), 1100),
            Some(MergeOutcome::Accepted {
                hints_changed: false,
                generation: 1
            })
        );
        assert_eq!(
            store.accept(SCOPE, &nonce(5), &batch(600 * 64), 1100),
            Some(MergeOutcome::Accepted {
                hints_changed: true,
                generation: 2
            })
        );
        assert!(store.load(SCOPE, 1100).unwrap().measurements[ID]
            .sizes(ID, "100vw")
            .is_none());
    }

    #[test]
    fn failed_batch_never_consumes_nonce_or_commits_partial_measurements() {
        let (cache, store) = fixture();
        store.register(SCOPE, &nonce(1), &issued(), 1000);
        let before = cache.get(&key(SCOPE));
        let mut report = batch(352 * 64);
        report.widths_64.insert("c".repeat(64), 100);
        assert_eq!(
            store.accept(SCOPE, &nonce(1), &report, 1100),
            Some(MergeOutcome::Unissued)
        );
        assert_eq!(cache.get(&key(SCOPE)), before);
        assert_eq!(
            store.register(
                SCOPE,
                &nonce(2),
                &["c".repeat(64)].into_iter().collect(),
                1100
            ),
            Some(RegisterOutcome::ManifestMismatch)
        );
        assert_eq!(cache.get(&key(SCOPE)), before);
        assert!(matches!(
            store.accept(SCOPE, &nonce(1), &batch(352 * 64), 1100),
            Some(MergeOutcome::Accepted { .. })
        ));
    }

    #[test]
    fn expiry_corruption_capacity_and_new_epoch_fail_closed() {
        let (cache, store) = fixture();
        store.register(SCOPE, &nonce(1), &issued(), 1000);
        let old = store.load(SCOPE, 1000).unwrap();
        assert_eq!(
            store.accept(SCOPE, &nonce(1), &batch(64), 1000 + NONCE_TTL_MS),
            Some(MergeOutcome::BadNonce)
        );
        assert_eq!(
            store.accept(SCOPE, &nonce(1), &batch(64), 999),
            Some(MergeOutcome::BadNonce)
        );
        assert!(store.load(SCOPE, 1000 + RECORD_TTL_MS).is_none());
        assert_eq!(
            store.accept(SCOPE, &nonce(1), &batch(64), 1000 + RECORD_TTL_MS),
            Some(MergeOutcome::Expired)
        );
        store.register(SCOPE, &nonce(2), &issued(), 1000 + RECORD_TTL_MS);
        let new = store.load(SCOPE, 1000 + RECORD_TTL_MS).unwrap();
        assert_ne!(old.epoch, new.epoch);
        assert_eq!(new.generation, 0);
        cache.put(&key(SCOPE), b"broken");
        assert_eq!(
            store.accept(SCOPE, &nonce(2), &batch(64), 1200),
            Some(MergeOutcome::BadNonce)
        );
        store.register(SCOPE, &nonce(3), &issued(), 1200);
        for n in 4..=130 {
            assert_eq!(
                store.register(SCOPE, &nonce(n), &issued(), 1200),
                Some(RegisterOutcome::Registered)
            );
        }
        assert_eq!(
            store.register(SCOPE, &nonce(131), &issued(), 1200),
            Some(RegisterOutcome::Capacity)
        );
        let value = cache.get(&key(SCOPE)).value.unwrap();
        assert!(decode(&value).is_some());
        assert!(decode(&vec![b'x'; MAX_RECORD_BYTES + 1]).is_none());
        assert!(decode(&value[..value.len() - 1]).is_none());
        assert!(decode(
            &String::from_utf8(value)
                .unwrap()
                .replace("/1", "/2")
                .into_bytes()
        )
        .is_none());
    }

    #[test]
    fn capacity_failure_rolls_back_earlier_slots_and_preserves_nonce() {
        let (cache, store) = fixture();
        let second = "c".repeat(64);
        let manifest: BTreeSet<String> = [ID.into(), second.clone()].into_iter().collect();
        for viewport in 1..=16 {
            store.register(SCOPE, &nonce(viewport), &manifest, 1000);
            let report = MeasurementBatch {
                viewport: viewport as u32,
                widths_64: [(second.clone(), 64)].into_iter().collect(),
            };
            assert!(matches!(
                store.accept(SCOPE, &nonce(viewport), &report, 1100),
                Some(MergeOutcome::Accepted { .. })
            ));
        }
        store.register(SCOPE, &nonce(17), &manifest, 1000);
        let before = cache.get(&key(SCOPE));
        let report = MeasurementBatch {
            viewport: 17,
            widths_64: [(ID.into(), 64), (second, 64)].into_iter().collect(),
        };
        assert_eq!(
            store.accept(SCOPE, &nonce(17), &report, 1100),
            Some(MergeOutcome::Capacity)
        );
        assert_eq!(cache.get(&key(SCOPE)), before);
        let reduced = MeasurementBatch {
            viewport: 17,
            widths_64: [(ID.into(), 64)].into_iter().collect(),
        };
        assert!(matches!(
            store.accept(SCOPE, &nonce(17), &reduced, 1100),
            Some(MergeOutcome::Accepted { .. })
        ));
    }

    #[derive(Debug)]
    struct Unavailable(bool);
    impl Cache for Unavailable {
        fn get(&self, _: &str) -> Lookup {
            Lookup::miss(KeyState::Overload)
        }
        fn put(&self, _: &str, _: &[u8]) {
            panic!("must not write");
        }
        fn delete(&self, _: &str) {}
        fn name(&self) -> String {
            "Unavailable".into()
        }
        fn update(&self, _: &str, change: &mut dyn FnMut(Lookup) -> Option<Vec<u8>>) -> bool {
            if self.0 {
                assert!(change(self.get("")).is_none());
                true
            } else {
                false
            }
        }
    }
    #[test]
    fn contention_and_error_lookup_are_not_acknowledged() {
        for call in [false, true] {
            let store = MeasurementStore::new(Arc::new(Unavailable(call)));
            assert_eq!(store.register(SCOPE, &nonce(1), &issued(), 1000), None);
            assert_eq!(store.accept(SCOPE, &nonce(1), &batch(64), 1000), None);
            assert!(store.load(SCOPE, 1000).is_none());
        }
    }

    #[test]
    fn concurrent_same_nonce_counts_once() {
        let cache: Arc<dyn Cache> = Arc::new(MemoryCache::new());
        MeasurementStore::new(cache.clone()).register(SCOPE, &nonce(1), &issued(), 1000);
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    MeasurementStore::new(cache).accept(SCOPE, &nonce(1), &batch(64), 1100)
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Some(MergeOutcome::Accepted { .. })))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == Some(MergeOutcome::BadNonce))
                .count(),
            7
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn multiprocess_transaction_preserves_nonce_history_and_measurements() {
        use ps_cache::cache::{FileCache, FileCachePolicy};
        const ENV: &str = "PS_AUTO_SIZE_TRANSACTION_ROOT";
        let child = std::env::var("PS_AUTO_SIZE_TRANSACTION_CHILD").ok();
        let directory = std::env::var_os(ENV)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!(
                    "ps-auto-size-store-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))
            });
        std::fs::create_dir_all(&directory).unwrap();
        let disk: Arc<dyn Cache> = Arc::new(FileCache::new(
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
                disk,
            )
            .unwrap(),
        );
        let store = MeasurementStore::new(cache.clone());
        let retry = |n| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(result) = store.accept(SCOPE, &nonce(n), &batch(352 * 64), 1100) {
                    break result;
                }
                assert!(
                    std::time::Instant::now() < until,
                    "transaction contention timeout"
                );
                std::thread::yield_now();
            }
        };
        if let Some(child) = child {
            let own: u64 = child.parse().unwrap();
            // Every process races the shared nonce, then consumes its own.
            assert!(matches!(
                retry(99),
                MergeOutcome::Accepted { .. } | MergeOutcome::BadNonce
            ));
            assert!(matches!(retry(own), MergeOutcome::Accepted { .. }));
            return;
        }
        for n in [1, 2, 3, 4, 99] {
            assert_eq!(
                store.register(SCOPE, &nonce(n), &issued(), 1000),
                Some(RegisterOutcome::Registered)
            );
        }
        let executable = std::env::current_exe().unwrap();
        let mut children: Vec<_> = (1..=4).map(|n| std::process::Command::new(&executable)
            .args(["--exact", "automatic_sizes_store::tests::multiprocess_transaction_preserves_nonce_history_and_measurements", "--nocapture"])
            .env(ENV, &directory).env("PS_AUTO_SIZE_TRANSACTION_CHILD", n.to_string())
            .spawn().unwrap()).collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let snapshot = store.load(SCOPE, 1200).unwrap();
        assert_eq!(snapshot.generation, 1);
        assert_eq!(
            snapshot.measurements[ID].sizes(ID, "100vw").unwrap(),
            "(width: 412px) 352px, 100vw"
        );
        let record = decode(&cache.get(&key(SCOPE)).value.unwrap()).unwrap();
        assert_eq!(record.nonces.len(), 5);
        assert!(record.nonces.values().all(|n| n.consumed));
        assert_eq!(retry(99), MergeOutcome::BadNonce);
        drop(store);
        drop(cache);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

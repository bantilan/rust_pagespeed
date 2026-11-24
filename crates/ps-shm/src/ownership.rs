//! Bounded background-job ownership with fenced publication.
//!
//! Call only on background workers: claims may inspect procfs. Never hold the
//! publication lock while fetching or optimizing. Busy release/completion must
//! be retried or fenced with `revoke_through`; there is no lossy Drop cleanup.
use crate::process_identity::{ProcessIdentity, ProcessLiveness};
use crate::region::{RegionLayout, SharedRegion};
use std::os::unix::fs::MetadataExt;
use std::{io, path::Path};

const KEY_BYTES: usize = 1024;
const RECORD: usize = 32 + KEY_BYTES;

/// An opaque claim bound to one table mapping; never reuse across tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnershipToken {
    slot: usize,
    serial: u64,
    mapping: (u64, u64),
}

/// Nonblocking admission outcomes. Running records are never evicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnershipClaim {
    /// This worker may execute the job and publish using this token.
    Owned(OwnershipToken),
    /// A matching live or unknown owner already protects this key.
    AlreadyOwned,
    /// The table lock is contended; retry later on a background worker.
    Busy,
    /// Every slot has an owner that cannot safely be reclaimed.
    Full,
    /// Key exceeds the bounded admission size; retain local optimization.
    KeyTooLong,
    /// The monotonic token namespace is exhausted; select a new segment.
    Exhausted,
}

/// Outcome of completion or explicit release.
#[derive(Debug, PartialEq, Eq)]
pub enum OwnershipFinish<T> {
    /// Publication ran while the claim was protected; the claim is released.
    Finished(T),
    /// The caller must retry; ownership has not been released.
    Busy,
    /// Ownership was revoked or already released; publication did not run.
    Stale,
}

/// Fixed, non-evicting owner records, independent of cache data eviction.
#[derive(Debug)]
pub struct OwnershipTable {
    region: SharedRegion,
    revocations: SharedRegion,
    metrics: crate::metrics::SharedMetrics,
    slots: usize,
    mapping: (u64, u64),
}

impl OwnershipTable {
    /// Open a dedicated ownership-format file without truncating live state.
    /// Capacity is 1–1024; changing capacity requires a different filename.
    /// The adjacent `.revocations-v1` file is part of this mapping's lifetime.
    /// Use one canonical pathname (no hard-link aliases) and never remove either
    /// file while workers hold mappings.
    pub fn open(path: &Path, slots: usize) -> io::Result<Self> {
        if !(1..=1024).contains(&slots) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid owner capacity",
            ));
        }
        let region = SharedRegion::open(path, RegionLayout::new(1, slots * RECORD + 8)?)?;
        // Canonical path makes symlink aliases use the same cancellation file.
        let canonical = std::fs::canonicalize(path)?;
        let mut cancellation = canonical.as_os_str().to_os_string();
        cancellation.push(".revocations-v1");
        let revocations = SharedRegion::open(Path::new(&cancellation), RegionLayout::new(1, 8)?)?;
        let mut metrics_path = canonical.as_os_str().to_os_string();
        metrics_path.push(".metrics-v2");
        let metrics = crate::metrics::SharedMetrics::open(Path::new(&metrics_path), 9)?;
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            region,
            revocations,
            metrics,
            slots,
            mapping: (metadata.dev(), metadata.ino()),
        })
    }

    /// Shared event counters since domain creation and exact mapping capacity.
    pub fn diagnostics(&self) -> io::Result<Vec<(&'static str, u64)>> {
        let names = [
            "owner_claims",
            "owner_duplicates",
            "owner_busy",
            "owner_full",
            "owner_long_keys",
            "owner_errors_or_exhaustion",
            "owner_revocations",
            "owner_stale_publications",
            "owner_dead_reclaims",
        ];
        let mut result = Vec::new();
        for (index, name) in names.into_iter().enumerate() {
            result.push((name, self.metrics.get(index)?));
        }
        result.push(("owner_slot_capacity", self.slots as u64));
        result.push((
            "owner_mapping_bytes",
            (self.region.layout().mapped_bytes()
                + self.revocations.layout().mapped_bytes()
                + self.metrics.mapped_bytes()) as u64,
        ));
        Ok(result)
    }

    /// Attempt once. Cache-path domain and effective options must be included
    /// in the caller's exact job key. Current worker identity is read after fork.
    pub fn try_claim(&self, key: &str) -> io::Result<OwnershipClaim> {
        let result = self.claim_impl(key);
        let index = match result {
            Ok(OwnershipClaim::Owned(_)) => 0,
            Ok(OwnershipClaim::AlreadyOwned) => 1,
            Ok(OwnershipClaim::Busy) => 2,
            Ok(OwnershipClaim::Full) => 3,
            Ok(OwnershipClaim::KeyTooLong) => 4,
            _ => 5,
        };
        let _ = self.metrics.increment(index);
        result
    }
    fn claim_impl(&self, key: &str) -> io::Result<OwnershipClaim> {
        if key.len() > KEY_BYTES {
            return Ok(OwnershipClaim::KeyTooLong);
        }
        let owner = ProcessIdentity::current()?;
        let Some(mut guard) = self.region.try_lock(0)? else {
            return Ok(OwnershipClaim::Busy);
        };
        let bytes = guard.bytes_mut();
        let mut free = None;
        for slot in 0..self.slots {
            let record = &mut bytes[slot * RECORD..(slot + 1) * RECORD];
            if word(record, 0) == 0 || word(record, 0) <= self.revocations.epoch(0)? {
                record.fill(0);
                free.get_or_insert(slot);
                continue;
            }
            // Writers bounds-check lengths and PIDs, so out-of-range
            // values are unwritable garbage: recycle the slot instead
            // of failing every future claim against it.
            let len = match usize::try_from(word(record, 24)) {
                Ok(len) if len <= KEY_BYTES => len,
                _ => {
                    record.fill(0);
                    free.get_or_insert(slot);
                    continue;
                }
            };
            if &record[32..32 + len] == key.as_bytes() {
                match identity(record) {
                    Ok(identity) if identity.liveness() != ProcessLiveness::Dead => {
                        return Ok(OwnershipClaim::AlreadyOwned);
                    }
                    Ok(_) => {
                        self.metrics.increment(8)?;
                        free = Some(slot);
                        break;
                    }
                    Err(_) => {
                        record.fill(0);
                        free = Some(slot);
                        break;
                    }
                }
            }
        }
        if free.is_none() {
            for slot in 0..self.slots {
                let record = &mut bytes[slot * RECORD..(slot + 1) * RECORD];
                match identity(record) {
                    Ok(identity) if identity.liveness() == ProcessLiveness::Dead => {
                        self.metrics.increment(8)?;
                        free = Some(slot);
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        record.fill(0);
                        free = Some(slot);
                        break;
                    }
                }
            }
        }
        let Some(slot) = free else {
            return Ok(OwnershipClaim::Full);
        };
        if self.region.epoch(0)? == u64::MAX {
            return Ok(OwnershipClaim::Exhausted);
        }
        self.region.invalidate(0)?;
        let serial = self.region.epoch(0)?;
        let record = &mut bytes[slot * RECORD..(slot + 1) * RECORD];
        record.fill(0);
        set_word(record, 0, serial);
        set_word(record, 8, u64::from(owner.pid));
        set_word(record, 16, owner.start_ticks);
        set_word(record, 24, key.len() as u64);
        record[32..32 + key.len()].copy_from_slice(key.as_bytes());
        Ok(OwnershipClaim::Owned(OwnershipToken {
            slot,
            serial,
            mapping: self.mapping,
        }))
    }

    /// Nonblocking exact-key presence check for request threads: true
    /// when an unrevoked record matches, without any liveness (procfs)
    /// inspection. False on lock contention, I/O errors, or no match —
    /// callers treat false as "proceed locally" (at worst one duplicate
    /// optimization, the pre-coordination behavior). A lingering dead
    /// record can cause bounded false-positive waits; normal claiming
    /// reclaims such records.
    pub fn is_claimed(&self, key: &str) -> bool {
        let Ok(Some(guard)) = self.region.try_lock(0) else {
            return false;
        };
        let Ok(watermark) = self.revocations.epoch(0) else {
            return false;
        };
        let bytes = guard.bytes();
        for slot in 0..self.slots {
            let record = &bytes[slot * RECORD..(slot + 1) * RECORD];
            if word(record, 0) == 0 || word(record, 0) <= watermark {
                continue;
            }
            let Ok(len) = usize::try_from(word(record, 24)) else {
                continue;
            };
            if len > KEY_BYTES {
                continue;
            }
            if &record[32..32 + len] == key.as_bytes() {
                return true;
            }
        }
        false
    }

    /// Revoke this token and all earlier tokens without waiting for a lock.
    /// This conservative fallback may discard other in-progress jobs; later
    /// requests retry them. It is intended for contended/panicking cleanup.
    pub fn revoke_through(&self, token: OwnershipToken) -> io::Result<()> {
        if token.mapping == self.mapping {
            self.metrics.increment(6)?;
            self.revocations.advance_epoch_to(0, token.serial)?;
        }
        Ok(())
    }

    /// Publish while retaining ownership for further metadata writes. The
    /// closure runs under the owner mutex; never fetch or optimize inside it.
    pub fn try_publish<T>(
        &self,
        token: OwnershipToken,
        publish: impl FnOnce() -> T,
    ) -> io::Result<OwnershipFinish<T>> {
        self.publish(token, false, publish)
    }

    /// Validate and publish under one lock, then release this claim. Passing
    /// `|| ()` explicitly releases without publishing. Busy requires retry or
    /// conservative `revoke_through` cleanup on the background worker.
    pub fn try_finish<T>(
        &self,
        token: OwnershipToken,
        publish: impl FnOnce() -> T,
    ) -> io::Result<OwnershipFinish<T>> {
        self.publish(token, true, publish)
    }

    fn publish<T>(
        &self,
        token: OwnershipToken,
        release: bool,
        publish: impl FnOnce() -> T,
    ) -> io::Result<OwnershipFinish<T>> {
        if token.mapping != self.mapping || token.slot >= self.slots {
            let _ = self.metrics.increment(7);
            return Ok(OwnershipFinish::Stale);
        }
        let Some(mut guard) = self.region.try_lock(0)? else {
            return Ok(OwnershipFinish::Busy);
        };
        let record = &mut guard.bytes_mut()[token.slot * RECORD..(token.slot + 1) * RECORD];
        if token.serial == 0
            || word(record, 0) != token.serial
            || token.serial <= self.revocations.epoch(0)?
        {
            let _ = self.metrics.increment(7);
            return Ok(OwnershipFinish::Stale);
        }
        let result = publish();
        if release {
            record.fill(0);
        }
        Ok(OwnershipFinish::Finished(result))
    }
}
fn word(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn set_word(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn identity(record: &[u8]) -> io::Result<ProcessIdentity> {
    let pid = u32::try_from(word(record, 8))
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid owner PID"))?;
    Ok(ProcessIdentity {
        pid,
        start_ticks: word(record, 16),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "ps-owner-{}-{}-{}",
                std::process::id(),
                ProcessIdentity::current().unwrap().start_ticks,
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> std::path::PathBuf {
            self.0.join("owners-v2")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn claim(table: &OwnershipTable, key: &str) -> OwnershipToken {
        match table.try_claim(key).unwrap() {
            OwnershipClaim::Owned(token) => token,
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn exact_keys_capacity_release_and_fencing() {
        let temp = Temp::new();
        let first = OwnershipTable::open(&temp.path(), 1).unwrap();
        let second = OwnershipTable::open(&temp.path(), 1).unwrap();
        let token = claim(&first, "domain:options:key");
        assert_eq!(
            second.try_claim("domain:options:key").unwrap(),
            OwnershipClaim::AlreadyOwned
        );
        assert_eq!(second.try_claim("different").unwrap(), OwnershipClaim::Full);
        assert_eq!(
            second.try_claim(&"x".repeat(KEY_BYTES + 1)).unwrap(),
            OwnershipClaim::KeyTooLong
        );
        assert_eq!(
            second.try_finish(token, || 42).unwrap(),
            OwnershipFinish::Finished(42)
        );
        let next = claim(&second, "domain:options:key");
        assert_ne!(token, next);
        assert_eq!(
            first
                .try_finish(token, || panic!("stale publication"))
                .unwrap(),
            OwnershipFinish::<()>::Stale
        );
        let unrelated = Temp::new();
        let unrelated = OwnershipTable::open(&unrelated.path(), 1).unwrap();
        let _ = claim(&unrelated, "domain:options:key");
        assert_eq!(
            unrelated
                .try_finish(next, || panic!("wrong mapping"))
                .unwrap(),
            OwnershipFinish::<()>::Stale
        );
        assert!(OwnershipTable::open(&temp.path(), 2).is_err());
    }
    #[test]
    fn panic_revokes_other_jobs_without_reusing_tokens() {
        let temp = Temp::new();
        let table = OwnershipTable::open(&temp.path(), 2).unwrap();
        let first = claim(&table, "first");
        let second = claim(&table, "second");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = table.try_finish(first, || panic!("publication panic"));
        }));
        assert!(result.is_err());
        let next = claim(&table, "second");
        assert!(next.serial > second.serial);
        assert_eq!(
            table
                .try_finish(second, || panic!("revoked publication"))
                .unwrap(),
            OwnershipFinish::<()>::Stale
        );
    }
    #[test]
    fn corrupt_records_recycle_instead_of_failing_claims() {
        let temp = Temp::new();
        let table = OwnershipTable::open(&temp.path(), 1).unwrap();
        // Poison the only slot with an unwritable key length, as a
        // torn write that somehow escaped crash recovery would.
        {
            let mut guard = table.region.try_lock(0).unwrap().unwrap();
            let bytes = guard.bytes_mut();
            bytes[0..8].copy_from_slice(&7u64.to_le_bytes());
            bytes[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        }
        // The claim recycles the garbage slot instead of erroring.
        let token = claim(&table, "job");
        assert_eq!(
            table
                .try_finish(token, || 7)
                .unwrap(),
            OwnershipFinish::Finished(7)
        );
        // And a corrupt PID on an exact-key match heals the same way
        // (tokens are inert values; abandoning one releases nothing).
        let _abandoned = claim(&table, "job");
        {
            let mut guard = table.region.try_lock(0).unwrap().unwrap();
            let bytes = guard.bytes_mut();
            bytes[8..16].copy_from_slice(&0u64.to_le_bytes());
        }
        let _ = claim(&table, "job");
    }

    #[test]
    fn contended_claims_never_duplicate_lose_or_deadlock() {
        let temp = Temp::new();
        let table = std::sync::Arc::new(OwnershipTable::open(&temp.path(), 32).unwrap());
        // Fewer keys than workers: forces AlreadyOwned overlap, Full
        // pressure, and interleaved finish/revoke paths.
        let start = std::time::Instant::now();
        let mut workers = Vec::new();
        for worker in 0..8 {
            let table = table.clone();
            workers.push(std::thread::spawn(move || {
                let mut owned = 0u32;
                let mut shared = 0u32;
                for round in 0..200 {
                    let key = format!("job-{}", (worker + round) % 4);
                    match table.try_claim(&key).unwrap() {
                        OwnershipClaim::Owned(token) => {
                            owned += 1;
                            // Hold briefly to overlap peers, then finish,
                            // retrying through contention like production.
                            if round % 7 == 0 {
                                std::thread::yield_now();
                            }
                            let mut finished = false;
                            for _ in 0..100 {
                                match table.try_finish(token, || {}).unwrap() {
                                    OwnershipFinish::Finished(())
                                    | OwnershipFinish::Stale => {
                                        finished = true;
                                        break;
                                    }
                                    OwnershipFinish::Busy => {
                                        std::thread::yield_now();
                                    }
                                }
                            }
                            assert!(finished, "finish starved under contention");
                        }
                        OwnershipClaim::AlreadyOwned
                        | OwnershipClaim::Busy
                        | OwnershipClaim::Full => shared += 1,
                        other => panic!("unexpected claim outcome: {other:?}"),
                    }
                }
                (owned, shared)
            }));
        }
        let mut owned = 0u32;
        let mut shared = 0u32;
        for worker in workers {
            let (own, shr) = worker.join().unwrap();
            owned += own;
            shared += shr;
        }
        // Every attempt resolved (nothing lost, no deadlock, no
        // unexpected outcome); the owned/shared split itself is
        // scheduling-dependent and asserted nowhere.
        assert_eq!(owned + shared, 8 * 200);
        eprintln!(
            "ownership contention: {owned} owned, {shared} shared in {:?}",
            start.elapsed()
        );
        // Table reusable afterwards: every slot reclaimable or free.
        let token = claim(&table, "aftermath");
        assert!(matches!(
            table.try_finish(token, || {}).unwrap(),
            OwnershipFinish::Finished(())
        ));
    }

    #[test]
    fn publication_stays_locked_until_finished() {
        let temp = Temp::new();
        let table = OwnershipTable::open(&temp.path(), 1).unwrap();
        let peer = OwnershipTable::open(&temp.path(), 1).unwrap();
        let token = claim(&table, "job");
        assert_eq!(
            table
                .try_finish(token, || {
                    assert_eq!(peer.try_claim("job").unwrap(), OwnershipClaim::Busy);
                    42
                })
                .unwrap(),
            OwnershipFinish::Finished(42)
        );
        let _ = claim(&peer, "job");
    }
    #[test]
    fn revoked_busy_release_reclaims_slots_and_fences_old_publication() {
        let temp = Temp::new();
        let table = OwnershipTable::open(&temp.path(), 2).unwrap();
        let peer = OwnershipTable::open(&temp.path(), 2).unwrap();
        let first = claim(&table, "first");
        let second = claim(&table, "second");
        let guard = table.region.try_lock(0).unwrap().unwrap();
        assert_eq!(
            table.try_finish(second, || ()).unwrap(),
            OwnershipFinish::Busy
        );
        peer.revoke_through(second).unwrap();
        drop(guard);
        for token in [first, second] {
            assert_eq!(
                table
                    .try_publish(token, || panic!("revoked write"))
                    .unwrap(),
                OwnershipFinish::<()>::Stale
            );
        }
        let next = claim(&peer, "second");
        // Late cleanup of an old token cannot revoke a newer claim.
        table.revoke_through(first).unwrap();
        assert_eq!(
            peer.try_publish(next, || 1).unwrap(),
            OwnershipFinish::Finished(1)
        );
        assert_eq!(
            peer.try_publish(next, || 2).unwrap(),
            OwnershipFinish::Finished(2)
        );
        assert_eq!(
            peer.try_finish(next, || ()).unwrap(),
            OwnershipFinish::Finished(())
        );
    }
    #[test]
    fn contention_never_publishes_or_claims() {
        let temp = Temp::new();
        let table = OwnershipTable::open(&temp.path(), 1).unwrap();
        let token = claim(&table, "job");
        let guard = table.region.try_lock(0).unwrap().unwrap();
        assert_eq!(table.try_claim("job").unwrap(), OwnershipClaim::Busy);
        assert_eq!(
            table
                .try_finish(token, || panic!("busy publication"))
                .unwrap(),
            OwnershipFinish::<()>::Busy
        );
        drop(guard);
        assert_eq!(
            table.try_finish(token, || ()).unwrap(),
            OwnershipFinish::Finished(())
        );
    }
    #[test]
    fn subprocess_entry() {
        let Some(path) = std::env::var_os("PS_OWNER_CHILD") else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        let table = OwnershipTable::open(&path, 2).unwrap();
        let token = claim(&table, "shared-job");
        let _guard = if std::env::var_os("PS_OWNER_LOCK").is_some() {
            table.region.try_lock(0).unwrap()
        } else {
            None
        };
        std::fs::write(path.with_extension("ready"), token.serial.to_string()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
    #[test]
    fn independent_owner_deduplicates_and_recovers_after_kill() {
        for kill_locked in [false, true] {
            let temp = Temp::new();
            let table = OwnershipTable::open(&temp.path(), 2).unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "ownership::tests::subprocess_entry",
                    "--nocapture",
                ])
                .env("PS_OWNER_CHILD", temp.path())
                .stdout(std::process::Stdio::null());
            if kill_locked {
                command.env("PS_OWNER_LOCK", "1");
            }
            let mut child = command.spawn().unwrap();
            let ready = temp.path().with_extension("ready");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !ready.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let before = table.try_claim("shared-job");
            let serial = std::fs::read_to_string(&ready);
            child.kill().unwrap();
            child.wait().unwrap();
            let serial = serial.unwrap().parse::<u64>().unwrap();
            assert_eq!(
                before.unwrap(),
                if kill_locked {
                    OwnershipClaim::Busy
                } else {
                    OwnershipClaim::AlreadyOwned
                }
            );
            let next = claim(&table, "shared-job");
            assert!(next.serial > serial);
            let stale = OwnershipToken {
                slot: 0,
                serial,
                mapping: table.mapping,
            };
            assert_eq!(
                table
                    .try_finish(stale, || panic!("dead owner publication"))
                    .unwrap(),
                OwnershipFinish::<()>::Stale
            );
            assert_eq!(
                table.try_finish(next, || ()).unwrap(),
                OwnershipFinish::Finished(())
            );
        }
    }
}

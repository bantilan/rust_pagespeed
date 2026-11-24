//! Recoverable bounded tracking for disk-cache mutations.
//!
//! A writer holds a robust slot mutex throughout its disk mutation. Active
//! bitmap bits and generations are atomic and disjoint from guarded payloads.
//! This primitive never waits for another writer and never reads procfs.
use crate::region::{RegionLayout, ShardGuard, SharedRegion};
use std::{io, path::Path};

/// Maximum simultaneously tracked disk mutations in one cache domain.
pub const WRITER_SLOTS: usize = 64;
/// Fixed key-hash shards; allocation shape is independent of byte-cache size.
pub const WRITE_SHARDS: usize = 128;

/// Three dedicated, boot-qualified regions forming one lifetime unit. Never
/// unlink/replace any of their files while workers hold mappings.
#[derive(Debug)]
pub struct WriteTracker {
    slots: SharedRegion,
    active: SharedRegion,
    generations: SharedRegion,
}
impl WriteTracker {
    /// Open at configuration time in an application-owned dedicated directory.
    /// Partial/incompatible mappings fail closed; existing files are not reset.
    pub fn open(directory: &Path) -> io::Result<Self> {
        Ok(Self {
            slots: SharedRegion::open(
                &directory.join("writer-slots-v1"),
                RegionLayout::new(WRITER_SLOTS, 8)?,
            )?,
            active: SharedRegion::open(
                &directory.join("writer-active-v1"),
                RegionLayout::new(WRITE_SHARDS, 8)?,
            )?,
            generations: SharedRegion::open(
                &directory.join("writer-generations-v1"),
                RegionLayout::new(WRITE_SHARDS, 8)?,
            )?,
        })
    }

    /// Entire fixed mapping allocation (including alignment and headers).
    pub fn mapped_bytes(&self) -> usize {
        self.slots.layout().mapped_bytes()
            + self.active.layout().mapped_bytes()
            + self.generations.layout().mapped_bytes()
    }

    /// Lock-free stable generation, unavailable during a write or exhaustion.
    /// The caller must validate again after fetching/backfilling memory.
    pub fn snapshot(&self, shard: usize) -> io::Result<Option<u64>> {
        if self.active.epoch(shard)? != 0 {
            return Ok(None);
        }
        let generation = self.generations.epoch(shard)?;
        Ok((generation != u64::MAX && self.active.epoch(shard)? == 0).then_some(generation))
    }

    /// Fail closed for memory reads before an exceptional untracked disk write.
    /// Saturated generations never wrap; choose a fresh lifetime domain to reset.
    pub fn disable_memory(&self, shard: usize) -> io::Result<()> {
        self.generations.advance_epoch_to(shard, u64::MAX)
    }

    /// Try once per slot, without waiting. None means bounded capacity is busy;
    /// the caller must not perform an untracked mutation as if admitted.
    pub fn try_begin(&self, shard: usize) -> io::Result<Option<TrackedWrite<'_>>> {
        // Validate before taking a slot, including on a saturated table.
        self.active.epoch(shard)?;
        for slot in 0..WRITER_SLOTS {
            if let Some(guard) = self.slots.try_lock(slot)? {
                self.repair_slot(slot)?;
                self.active.set_tracking_bit(shard, 1u64 << slot)?;
                return Ok(Some(TrackedWrite {
                    tracker: self,
                    shard,
                    slot,
                    _guard: guard,
                }));
            }
        }
        Ok(None)
    }

    /// Nonblocking recovery of candidate writers for one key shard. Acquiring
    /// a robust slot proves that no live writer holds it; a timeout never does.
    /// Also repairs all other shard references before making that slot reusable.
    pub fn recover(&self, shard: usize) -> io::Result<usize> {
        let mut candidates = self.active.epoch(shard)?;
        let mut recovered = 0;
        while candidates != 0 {
            let slot = candidates.trailing_zeros() as usize;
            candidates &= candidates - 1;
            if let Some(_guard) = self.slots.try_lock(slot)? {
                if self.repair_slot(slot)? {
                    recovered += 1;
                }
            }
        }
        Ok(recovered)
    }

    fn repair_slot(&self, slot: usize) -> io::Result<bool> {
        let bit = 1u64 << slot;
        let mut repaired = false;
        for shard in 0..WRITE_SHARDS {
            if self.active.epoch(shard)? & bit != 0 {
                // Bump first, then expose idle. Death between these steps is
                // harmless: the next repair bumps again and clears the bit.
                self.generations.invalidate(shard)?;
                self.active.clear_tracking_bit(shard, bit)?;
                repaired = true;
            }
        }
        Ok(repaired)
    }
}

/// Admitted mutation. Keep this !Send guard on its acquiring thread until disk
/// publication finishes. Drop invalidates old generations even during panic.
pub struct TrackedWrite<'a> {
    tracker: &'a WriteTracker,
    shard: usize,
    slot: usize,
    _guard: ShardGuard<'a>,
}
impl Drop for TrackedWrite<'_> {
    fn drop(&mut self) {
        // Indices were checked before constructing this guard. These operations
        // touch mapped atomics only and cannot fail for valid indices.
        let _ = self.tracker.generations.invalidate(self.shard);
        let _ = self
            .tracker
            .active
            .clear_tracking_bit(self.shard, 1u64 << self.slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "ps-writes-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn overlapping_writes_keep_memory_unavailable_until_last_completion() {
        let temp = Temp::new();
        let tracker = WriteTracker::open(&temp.0).unwrap();
        let peer = WriteTracker::open(&temp.0).unwrap();
        assert_eq!(tracker.snapshot(3).unwrap(), Some(0));
        let first = tracker.try_begin(3).unwrap().unwrap();
        let second = peer.try_begin(3).unwrap().unwrap();
        assert_eq!(peer.snapshot(3).unwrap(), None);
        assert_eq!(peer.recover(3).unwrap(), 0);
        assert_eq!(peer.snapshot(4).unwrap(), Some(0));
        drop(first);
        assert_eq!(tracker.snapshot(3).unwrap(), None);
        drop(second);
        assert_eq!(tracker.snapshot(3).unwrap(), Some(2));
        assert_eq!(tracker.mapped_bytes(), 41344);
        assert!(tracker.try_begin(WRITE_SHARDS).is_err());
    }
    #[test]
    fn bounded_capacity_and_panic_cleanup() {
        let temp = Temp::new();
        let tracker = WriteTracker::open(&temp.0).unwrap();
        let mut writers = Vec::new();
        for _ in 0..WRITER_SLOTS {
            writers.push(tracker.try_begin(0).unwrap().unwrap());
        }
        assert!(tracker.try_begin(0).unwrap().is_none());
        assert_eq!(tracker.recover(0).unwrap(), 0);
        writers.clear();
        assert_eq!(tracker.snapshot(0).unwrap(), Some(64));
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _write = tracker.try_begin(1).unwrap().unwrap();
            panic!("disk writer panic");
        }))
        .is_err());
        assert_eq!(tracker.snapshot(1).unwrap(), Some(1));
    }
    #[test]
    fn dead_thread_recovery_preserves_a_live_overlapping_writer() {
        let temp = Temp::new();
        let tracker = std::sync::Arc::new(WriteTracker::open(&temp.0).unwrap());
        let live = tracker.try_begin(3).unwrap().unwrap();
        let moved = tracker.clone();
        std::thread::spawn(move || {
            // Model termination that bypasses Rust destructors. The native
            // robust list is repaired by the kernel when this thread exits.
            let interrupted = moved.try_begin(3).unwrap().unwrap();
            std::mem::forget(interrupted);
        })
        .join()
        .unwrap();
        assert_eq!(tracker.recover(3).unwrap(), 1);
        assert_eq!(tracker.snapshot(3).unwrap(), None);
        drop(live);
        assert_eq!(tracker.snapshot(3).unwrap(), Some(2));
    }

    #[test]
    fn slot_reuse_repairs_other_shards_and_exhaustion_never_wraps() {
        let temp = Temp::new();
        let tracker = std::sync::Arc::new(WriteTracker::open(&temp.0).unwrap());
        let moved = tracker.clone();
        std::thread::spawn(move || {
            let interrupted = moved.try_begin(7).unwrap().unwrap();
            std::mem::forget(interrupted);
        })
        .join()
        .unwrap();
        assert_eq!(tracker.snapshot(7).unwrap(), None);
        let next = tracker.try_begin(9).unwrap().unwrap();
        assert_eq!(tracker.snapshot(7).unwrap(), Some(1));
        assert_eq!(tracker.snapshot(9).unwrap(), None);
        drop(next);
        tracker.generations.advance_epoch_to(9, u64::MAX).unwrap();
        let write = tracker.try_begin(9).unwrap().unwrap();
        drop(write);
        assert_eq!(tracker.snapshot(9).unwrap(), None);
        assert_eq!(tracker.generations.epoch(9).unwrap(), u64::MAX);
    }

    #[test]
    fn subprocess_entry() {
        let Some(path) = std::env::var_os("PS_WRITE_TRACKER_CHILD") else {
            return;
        };
        let directory = std::path::PathBuf::from(path);
        let tracker = WriteTracker::open(&directory).unwrap();
        let boundary = std::env::var("PS_WRITE_TRACKER_BOUNDARY").unwrap();
        let _slot = tracker.slots.try_lock(0).unwrap().unwrap();
        if boundary != "before_active" {
            tracker.active.set_tracking_bit(3, 1).unwrap();
        }
        if matches!(
            boundary.as_str(),
            "after_disk" | "after_generation" | "after_clear"
        ) {
            std::fs::write(directory.join("disk"), b"new").unwrap();
        }
        if matches!(boundary.as_str(), "after_generation" | "after_clear") {
            tracker.generations.invalidate(3).unwrap();
        }
        if boundary == "after_clear" {
            tracker.active.clear_tracking_bit(3, 1).unwrap();
        }
        std::fs::write(directory.join("ready"), b"ready").unwrap();
        std::thread::sleep(Duration::from_secs(30));
    }
    #[test]
    fn killed_writers_recover_at_each_publication_boundary() {
        for boundary in [
            "before_active",
            "after_active",
            "after_disk",
            "after_generation",
            "after_clear",
        ] {
            let temp = Temp::new();
            let tracker = WriteTracker::open(&temp.0).unwrap();
            std::fs::write(temp.0.join("disk"), b"old").unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "write_tracker::tests::subprocess_entry",
                    "--nocapture",
                ])
                .env("PS_WRITE_TRACKER_CHILD", &temp.0)
                .env("PS_WRITE_TRACKER_BOUNDARY", boundary)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !temp.0.join("ready").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            let ready = temp.0.join("ready").exists();
            let before = tracker.snapshot(3).unwrap();
            let live_recovery = tracker.recover(3).unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(ready, "{boundary}");
            assert_eq!(live_recovery, 0);
            if matches!(boundary, "after_active" | "after_disk" | "after_generation") {
                assert_eq!(before, None);
            }
            tracker.recover(3).unwrap();
            assert!(
                tracker.snapshot(3).unwrap().is_some(),
                "stranded {boundary}"
            );
            assert_eq!(
                std::fs::read(temp.0.join("disk")).unwrap(),
                if matches!(boundary, "before_active" | "after_active") {
                    b"old"
                } else {
                    b"new"
                }
            );
            let _next = tracker.try_begin(3).unwrap().unwrap();
        }
    }
}

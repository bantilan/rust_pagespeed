//! Bounded background-job ownership with fenced publication.
//!
//! Call only on background workers: claims may inspect procfs. Never hold the
//! publication lock while fetching or optimizing. Busy release/completion must
//! be retried by the caller; this table deliberately has no lossy Drop cleanup.
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
    slots: usize,
    mapping: (u64, u64),
}

impl OwnershipTable {
    /// Open a dedicated ownership-format file without truncating live state.
    /// Capacity is 1–1024; changing capacity requires a different filename.
    pub fn open(path: &Path, slots: usize) -> io::Result<Self> {
        if !(1..=1024).contains(&slots) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid owner capacity",
            ));
        }
        let region = SharedRegion::open(path, RegionLayout::new(1, slots * RECORD)?)?;
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            region,
            slots,
            mapping: (metadata.dev(), metadata.ino()),
        })
    }

    /// Attempt once. Cache-path domain and effective options must be included
    /// in the caller's exact job key. Current worker identity is read after fork.
    pub fn try_claim(&self, key: &str) -> io::Result<OwnershipClaim> {
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
            if word(record, 0) == 0 {
                free.get_or_insert(slot);
                continue;
            }
            let len = usize::try_from(word(record, 24)).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid owner key length")
            })?;
            if len > KEY_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "corrupt owner record",
                ));
            }
            if &record[32..32 + len] == key.as_bytes() {
                if identity(record)?.liveness() != ProcessLiveness::Dead {
                    return Ok(OwnershipClaim::AlreadyOwned);
                }
                free = Some(slot);
                break;
            }
        }
        if free.is_none() {
            for slot in 0..self.slots {
                let record = &bytes[slot * RECORD..(slot + 1) * RECORD];
                if identity(record)?.liveness() == ProcessLiveness::Dead {
                    free = Some(slot);
                    break;
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

    /// Validate the token and publish under the same lock, closing the
    /// validation/publication race. Panic clears the shard, fencing every job.
    /// Pass `|| ()` for explicit release without publication. Never fetch or
    /// optimize in the closure. A Busy outcome requires worker-side retry.
    pub fn try_finish<T>(
        &self,
        token: OwnershipToken,
        publish: impl FnOnce() -> T,
    ) -> io::Result<OwnershipFinish<T>> {
        if token.mapping != self.mapping || token.slot >= self.slots {
            return Ok(OwnershipFinish::Stale);
        }
        let Some(mut guard) = self.region.try_lock(0)? else {
            return Ok(OwnershipFinish::Busy);
        };
        let record = &mut guard.bytes_mut()[token.slot * RECORD..(token.slot + 1) * RECORD];
        if token.serial == 0 || word(record, 0) != token.serial {
            return Ok(OwnershipFinish::Stale);
        }
        let result = publish();
        record.fill(0);
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
            self.0.join("owners-v1")
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

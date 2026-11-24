//! Nonblocking cross-worker metadata read/modify/write transactions.
use crate::cache::{Cache, Lookup};
use ps_shm::region::{RegionLayout, SharedRegion};
use std::{io, path::Path, sync::Arc};

/// Wrap the final metadata codec stack; reads and ordinary immutable puts keep
/// existing behavior. Every read/modify/write participant must use update.
#[derive(Debug)]
pub struct TransactionalCache {
    backend: Arc<dyn Cache>,
    locks: Arc<SharedRegion>,
}
impl TransactionalCache {
    /// Open at configuration time, before handles escape. One domain per
    /// canonical disk path; mapping remains boot-qualified and cleaner-excluded.
    pub fn open(path: &Path, backend: Arc<dyn Cache>) -> io::Result<Self> {
        Ok(Self {
            backend,
            locks: Arc::new(SharedRegion::open(path, RegionLayout::new(128, 8)?)?),
        })
    }
    /// Exact mapped capacity of the shared transaction lock domain.
    pub fn mapped_bytes(&self) -> usize {
        self.locks.layout().mapped_bytes()
    }
    /// Reuse the same mapped transaction domain around another codec stack.
    pub fn with_backend(&self, backend: Arc<dyn Cache>) -> Self {
        Self {
            backend,
            locks: self.locks.clone(),
        }
    }
}
impl Cache for TransactionalCache {
    fn get(&self, key: &str) -> Lookup {
        self.backend.get(key)
    }
    fn put(&self, key: &str, value: &[u8]) {
        self.backend.put(key, value);
    }
    fn delete(&self, key: &str) {
        self.backend.delete(key);
    }
    fn update(&self, key: &str, change: &mut dyn FnMut(Lookup) -> Option<Vec<u8>>) -> bool {
        let shard = key.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        }) as usize
            % 128;
        let Ok(Some(_guard)) = self.locks.try_lock(shard) else {
            return false;
        };
        if let Some(value) = change(self.backend.get(key)) {
            self.backend.put(key, &value);
        }
        true
    }
    fn name(&self) -> String {
        format!("Transactional({})", self.backend.name())
    }
    fn is_blocking(&self) -> bool {
        self.backend.is_blocking()
    }
    fn is_healthy(&self) -> bool {
        self.backend.is_healthy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::MemoryCache;
    #[test]
    fn independent_handles_preserve_updates_and_skip_contended_callbacks() {
        let directory = std::env::temp_dir().join(format!(
            "ps-transactions-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let backend = Arc::new(MemoryCache::new());
        let first =
            Arc::new(TransactionalCache::open(&directory.join("locks"), backend.clone()).unwrap());
        let peer = Arc::new(TransactionalCache::open(&directory.join("locks"), backend).unwrap());
        let mut threads = vec![];
        for cache in [first.clone(), peer.clone()] {
            threads.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    while !cache.update("evidence", &mut |old| {
                        let count = old
                            .value
                            .map(|v| u64::from_le_bytes(v.try_into().unwrap()))
                            .unwrap_or(0);
                        Some((count + 1).to_le_bytes().to_vec())
                    }) {
                        std::thread::yield_now();
                    }
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(first.get("evidence").value.unwrap(), 2000u64.to_le_bytes());
        assert!(first.update("evidence", &mut |_| {
            assert!(!peer.update("evidence", &mut |_| panic!("contended callback")));
            None
        }));
        drop(first);
        drop(peer);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn killed_transaction_holder_recovers_for_next_update() {
        const ENV: &str = "PS_TXN_KILL_CHILD";
        if let Some(path) = std::env::var_os(ENV) {
            let directory = std::path::PathBuf::from(path);
            let backend: Arc<dyn Cache> = Arc::new(crate::cache::FileCache::new(
                &directory.join("disk"),
                crate::cache::FileCachePolicy {
                    clean_interval_ms: -1,
                    target_size_bytes: 16_777_216,
                    target_inode_count: 0,
                    path_length_limit: 8192,
                },
                0,
            ));
            let cache =
                TransactionalCache::open(&directory.join("locks"), backend).unwrap();
            // Hold the transaction lock, then wait to be killed before
            // publishing anything: the file backend stays untouched.
            cache.update("evidence", &mut |_| {
                std::fs::write(directory.join("ready"), b"ready").unwrap();
                std::thread::sleep(std::time::Duration::from_secs(30));
                None
            });
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "ps-transactions-kill-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        // Seed through a file-backed cache so the kill victim and the
        // assertions below share one persistent backend.
        let disk: Arc<dyn Cache> = Arc::new(crate::cache::FileCache::new(
            &directory.join("disk"),
            crate::cache::FileCachePolicy {
                clean_interval_ms: -1,
                target_size_bytes: 16_777_216,
                target_inode_count: 0,
                path_length_limit: 8192,
            },
            0,
        ));
        let shared =
            TransactionalCache::open(&directory.join("locks"), disk.clone()).unwrap();
        shared.update("evidence", &mut |_| Some(b"seeded".to_vec()));
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transactional::tests::killed_transaction_holder_recovers_for_next_update",
                "--nocapture",
            ])
            .env(ENV, &directory)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !directory.join("ready").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(directory.join("ready").exists(), "child never held the lock");
        child.kill().unwrap();
        child.wait().unwrap();
        // The robust lock recovers: the next update runs instead of
        // wedging, and the victim published nothing partial.
        assert!(shared.update("evidence", &mut |old| {
            assert_eq!(old.value.as_deref(), Some(b"seeded".as_slice()));
            Some(b"recovered".to_vec())
        }));
        assert_eq!(
            disk.get("evidence").value.as_deref(),
            Some(b"recovered".as_slice())
        );
        drop((shared, disk));
        std::fs::remove_dir_all(directory).unwrap();
    }
}

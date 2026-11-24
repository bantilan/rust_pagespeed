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
}

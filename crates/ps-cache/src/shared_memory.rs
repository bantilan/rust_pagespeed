//! Safe Cache-trait adapter for the shared byte backend. Not enabled by default.
//! Expiration, purge and disk/L1 coherence remain the caller's adapter concerns.
#![forbid(unsafe_code)]

use crate::cache::{Cache, KeyState, Lookup};
use ps_shm::byte_cache::{ByteCacheOptions, ByteLookup, SharedByteCache};
use std::io;
use std::path::Path;
use std::sync::Arc;

/// Bounded, nonblocking cross-process cache. Failed/oversized mutations
/// invalidate prior values; they never leave an older value readable as a hit.
#[derive(Debug)]
pub struct SharedMemoryCache {
    backend: Arc<SharedByteCache>,
}

impl SharedMemoryCache {
    /// Opens a checked segment. Startup errors can be used to select file-only
    /// caching; no file opens or disk reads occur on normal data operations.
    pub fn open(path: &Path, options: ByteCacheOptions) -> io::Result<Self> {
        Ok(Self {
            backend: Arc::new(SharedByteCache::open(path, options)?),
        })
    }

    /// Fixed mapping allocation, including all unused slot bytes and overhead.
    pub fn mapped_bytes(&self) -> usize {
        self.backend.mapped_bytes()
    }
}

impl Cache for SharedMemoryCache {
    fn get(&self, key: &str) -> Lookup {
        match self.backend.get(key.as_bytes()) {
            Ok(ByteLookup::Hit(value)) => Lookup::hit(value),
            Ok(ByteLookup::Miss) => Lookup::miss(KeyState::NotFound),
            Ok(ByteLookup::Busy) | Err(_) => Lookup::miss(KeyState::Overload),
        }
    }
    fn put(&self, key: &str, value: &[u8]) {
        let _ = self.backend.put(key.as_bytes(), value);
    }
    fn delete(&self, key: &str) {
        let _ = self.backend.delete(key.as_bytes());
    }
    fn diagnostics(&self) -> Vec<(&'static str, u64)> {
        self.backend.diagnostics().unwrap_or_default()
    }
    fn name(&self) -> String {
        "SharedMemoryCache".into()
    }
    fn is_blocking(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coherent_hot_hit_does_not_wait_for_unrelated_disk_write() {
        use crate::cache::{
            CacheCoherence, MemoryCache, WriteThroughCache, WRITE_THROUGH_UNLIMITED,
        };
        use std::sync::{mpsc, Mutex};
        use std::time::Duration;
        #[derive(Debug)]
        struct PerKey;
        impl CacheCoherence for PerKey {
            fn snapshot(&self, _: &str) -> Option<i64> {
                Some(0)
            }
            fn begin_write(&self, _: &str) {}
            fn end_write(&self, _: &str) {}
        }
        #[derive(Debug)]
        struct SlowWrite {
            bytes: MemoryCache,
            gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
        }
        impl Cache for SlowWrite {
            fn get(&self, key: &str) -> Lookup {
                self.bytes.get(key)
            }
            fn put(&self, key: &str, value: &[u8]) {
                if key == "blocked" {
                    if let Some((ready, resume)) = self.gate.lock().unwrap().take() {
                        ready.send(()).unwrap();
                        resume.recv_timeout(Duration::from_secs(5)).unwrap();
                    }
                }
                self.bytes.put(key, value);
            }
            fn delete(&self, key: &str) {
                self.bytes.delete(key);
            }
            fn name(&self) -> String {
                "SlowWrite".into()
            }
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let disk = Arc::new(SlowWrite {
            bytes: MemoryCache::new(),
            gate: Mutex::new(Some((ready_tx, resume_rx))),
        });
        let cache = Arc::new(
            WriteThroughCache::new(Arc::new(MemoryCache::new()), disk, WRITE_THROUGH_UNLIMITED)
                .with_coherence(Arc::new(PerKey)),
        );
        cache.put("warm", b"cached");
        assert!(cache.get("warm").value.is_some());
        let moved = cache.clone();
        let writer = std::thread::spawn(move || moved.put("blocked", b"slow"));
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (response_tx, response_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || response_tx.send(cache.get("warm")).unwrap());
        let observed = response_rx.recv_timeout(Duration::from_secs(1));
        // Always release/join even if the regression returns a timeout.
        resume_tx.send(()).unwrap();
        writer.join().unwrap();
        reader.join().unwrap();
        assert_eq!(
            Some(b"cached".to_vec()),
            observed
                .expect("hot hit waited for unrelated disk I/O")
                .value
        );
    }

    #[test]
    fn concurrent_backfill_never_resurrects_replaced_or_deleted_bytes() {
        use crate::cache::{
            CacheCoherence, MemoryCache, WriteThroughCache, WRITE_THROUGH_UNLIMITED,
        };
        use std::sync::{
            atomic::{AtomicI64, Ordering},
            mpsc, Mutex,
        };
        use std::time::Duration;
        #[derive(Debug, Default)]
        struct Generations {
            epoch: AtomicI64,
            writers: AtomicI64,
        }
        impl CacheCoherence for Generations {
            fn snapshot(&self, _: &str) -> Option<i64> {
                if self.writers.load(Ordering::SeqCst) != 0 {
                    return None;
                }
                let epoch = self.epoch.load(Ordering::SeqCst);
                (self.writers.load(Ordering::SeqCst) == 0).then_some(epoch)
            }
            fn begin_write(&self, _: &str) {
                self.writers.fetch_add(1, Ordering::SeqCst);
            }
            fn end_write(&self, _: &str) {
                self.epoch.fetch_add(1, Ordering::SeqCst);
                self.writers.fetch_sub(1, Ordering::SeqCst);
            }
        }
        #[derive(Debug)]
        struct PausedDisk {
            bytes: MemoryCache,
            gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
        }
        impl Cache for PausedDisk {
            fn get(&self, key: &str) -> Lookup {
                let value = self.bytes.get(key);
                let gate = self.gate.lock().unwrap().take();
                if let Some((ready, resume)) = gate {
                    ready.send(()).unwrap();
                    resume.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                value
            }
            fn put(&self, key: &str, value: &[u8]) {
                self.bytes.put(key, value);
            }
            fn delete(&self, key: &str) {
                self.bytes.delete(key);
            }
            fn name(&self) -> String {
                "PausedDisk".into()
            }
        }
        for delete in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "ps-shared-backfill-{}-{}-{delete}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&directory).unwrap();
            let shared: Arc<dyn Cache> = Arc::new(
                SharedMemoryCache::open(
                    &directory.join("bytes"),
                    ByteCacheOptions {
                        shards: 1,
                        entries_per_shard: 8,
                        bytes_per_entry: 256,
                    },
                )
                .unwrap(),
            );
            let (ready_tx, ready_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let disk = Arc::new(PausedDisk {
                bytes: MemoryCache::new(),
                gate: Mutex::new(Some((ready_tx, resume_rx))),
            });
            disk.put("key", b"old");
            let coherence = Arc::new(Generations::default());
            let worker = || -> Arc<dyn Cache> {
                let inner: Arc<dyn Cache> = Arc::new(
                    WriteThroughCache::new(shared.clone(), disk.clone(), WRITE_THROUGH_UNLIMITED)
                        .with_coherence(coherence.clone()),
                );
                Arc::new(
                    WriteThroughCache::new(
                        Arc::new(MemoryCache::new()),
                        inner,
                        WRITE_THROUGH_UNLIMITED,
                    )
                    .with_coherence(coherence.clone()),
                )
            };
            let reader = worker();
            let writer = worker();
            let moved = reader.clone();
            let thread = std::thread::spawn(move || moved.get("key"));
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if delete {
                writer.delete("key");
            } else {
                writer.put("key", b"new");
            }
            resume_tx.send(()).unwrap();
            let expected = if delete { None } else { Some(b"new".to_vec()) };
            assert_eq!(expected, thread.join().unwrap().value);
            assert_eq!(expected, reader.get("key").value);
            assert!(
                shared.get("0:key").value.is_none(),
                "stale backfill entered shared tier"
            );
            drop(reader);
            drop(writer);
            drop(shared);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn separate_adapters_share_mutations_and_remove_oversized_replacements() {
        let directory = std::env::temp_dir().join(format!(
            "ps-cache-shared-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("cache-v1");
        let options = ByteCacheOptions {
            shards: 2,
            entries_per_shard: 8,
            bytes_per_entry: 128,
        };
        let first = SharedMemoryCache::open(&path, options).unwrap();
        let second = SharedMemoryCache::open(&path, options).unwrap();
        assert!(!first.is_blocking());
        first.put("key", b"one");
        assert_eq!(Lookup::hit(b"one".to_vec()), second.get("key"));
        second.put("key", b"two");
        assert_eq!(Lookup::hit(b"two".to_vec()), first.get("key"));
        second.put("key", &[0; 128]);
        assert_eq!(KeyState::NotFound, first.get("key").state);
        first.put("key", b"three");
        second.delete("key");
        assert_eq!(KeyState::NotFound, first.get("key").state);
        assert_eq!("SharedMemoryCache", first.name());
        assert_eq!(first.mapped_bytes(), second.mapped_bytes());
        drop(first);
        drop(second);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

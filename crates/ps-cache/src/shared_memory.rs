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

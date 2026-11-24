//! Bounded, sharded, fixed-slot LRU byte cache. Freshness lives in higher-level
//! cache codecs. Every key/value copy holds a recoverable SharedRegion guard.
//! Contended mutations invalidate old slots atomically instead of waiting.

use crate::region::{RegionLayout, SharedRegion};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const MAGIC: u64 = 0x3143_4253_5350; // PSSBC1
const HEADER: usize = 64;
const SLOT: usize = 32;

/// Allocation policy. Capacity includes fixed slot headers, synchronization and
/// unused space inside admitted slots. No dynamic allocation/compaction occurs.
#[derive(Clone, Copy, Debug)]
pub struct ByteCacheOptions {
    /// Independently locked shards (1–256).
    pub shards: usize,
    /// Fixed slots per shard (1–1024).
    pub entries_per_shard: usize,
    /// Maximum combined key/value length in one slot (1 byte–1 MiB).
    pub bytes_per_entry: usize,
}

impl ByteCacheOptions {
    fn layout(self) -> io::Result<RegionLayout> {
        if !(1..=1024).contains(&self.entries_per_shard)
            || !(1..=1024 * 1024).contains(&self.bytes_per_entry)
        {
            return Err(invalid("invalid byte cache slot dimensions"));
        }
        let bytes = (SLOT + self.bytes_per_entry)
            .checked_mul(self.entries_per_shard)
            .and_then(|n| n.checked_add(HEADER))
            .ok_or_else(|| invalid("byte cache size overflow"))?;
        RegionLayout::new(self.shards, bytes)
    }
}

/// Nonblocking lookup outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum ByteLookup {
    /// Complete, copied value matching the complete requested key.
    Hit(Vec<u8>),
    /// Absent/invalidated entry.
    Miss,
    /// Shard currently locked elsewhere; caller may fall back.
    Busy,
}

/// Nonblocking mutation/admission outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteStore {
    /// Stored successfully.
    Stored,
    /// Key/value exceeds the slot limit; any old key was removed.
    TooLarge,
    /// Contended or epoch exhausted; old entries were invalidated.
    Busy,
}

/// Shared byte backend. No resource loading, disk I/O, expiration policy or
/// process-local pointers are involved in get/put/delete after attachment.
#[derive(Debug)]
pub struct SharedByteCache {
    region: SharedRegion,
    options: ByteCacheOptions,
    metrics: crate::metrics::SharedMetrics,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn read(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn write(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
fn hash(key: &[u8]) -> u64 {
    key.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

impl SharedByteCache {
    /// Opens a versioned cache file; refuses incompatible dimensions. Busy
    /// startup attachments retry for at most 500 ms. Existing values persist
    /// across opens; callers must keep higher-level freshness validation.
    pub fn open(path: &Path, options: ByteCacheOptions) -> io::Result<Self> {
        let mut metrics_path = path.as_os_str().to_os_string();
        metrics_path.push(".metrics-v1");
        let cache = Self {
            region: SharedRegion::open(path, options.layout()?)?,
            options,
            metrics: crate::metrics::SharedMetrics::open(Path::new(&metrics_path), 8)?,
        };
        let deadline = Instant::now() + Duration::from_millis(500);
        for shard in 0..options.shards {
            loop {
                if let Some(mut guard) = cache.region.try_lock(shard)? {
                    if read(guard.bytes(), 0) == 0 {
                        cache.initialize(guard.bytes_mut());
                    } else if !cache.valid_header(guard.bytes()) {
                        return Err(invalid("byte cache header/dimensions mismatch"));
                    }
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "byte cache attachment busy",
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(cache)
    }

    /// Entire fixed mapping allocation, not logical resident value bytes.
    pub fn mapped_bytes(&self) -> usize {
        self.region.layout().mapped_bytes()
    }

    /// On-demand shared counters and resident usage. Busy shards are omitted
    /// from usage and explicitly counted; no caller should call partial usage exact.
    pub fn diagnostics(&self) -> io::Result<Vec<(&'static str, u64)>> {
        let names = [
            "hits",
            "misses",
            "busy",
            "puts",
            "oversized",
            "evictions",
            "delete_attempts",
            "errors",
        ];
        let mut result = Vec::new();
        for (index, name) in names.into_iter().enumerate() {
            result.push((name, self.metrics.get(index)?));
        }
        let (mut entries, mut bytes, mut busy) = (0, 0, 0);
        for shard in 0..self.options.shards {
            let Some(guard) = self.region.try_lock(shard)? else {
                busy += 1;
                continue;
            };
            let epoch = self.region.epoch(shard)?;
            if epoch == u64::MAX || !self.valid_header(guard.bytes()) {
                continue;
            }
            for index in 0..self.options.entries_per_shard {
                let offset = self.offset(index);
                if read(guard.bytes(), offset) != 0 && read(guard.bytes(), offset + 24) == epoch {
                    let lengths = read(guard.bytes(), offset + 8);
                    let total = (lengths & 0xffff_ffff) + (lengths >> 32);
                    if total <= self.options.bytes_per_entry as u64 {
                        entries += 1;
                        bytes += total;
                    }
                }
            }
        }
        result.extend([
            ("resident_entries", entries),
            ("resident_key_value_bytes", bytes),
            ("usage_busy_shards", busy),
            ("byte_mapping_bytes", self.mapped_bytes() as u64),
            ("metrics_mapping_bytes", self.metrics.mapped_bytes() as u64),
            (
                "slot_capacity",
                (self.options.shards * self.options.entries_per_shard) as u64,
            ),
        ]);
        Ok(result)
    }

    fn initialize(&self, bytes: &mut [u8]) {
        bytes.fill(0);
        write(bytes, 0, MAGIC);
        write(bytes, 8, self.options.entries_per_shard as u64);
        write(bytes, 16, self.options.bytes_per_entry as u64);
    }
    fn valid_header(&self, bytes: &[u8]) -> bool {
        read(bytes, 0) == MAGIC
            && read(bytes, 8) == self.options.entries_per_shard as u64
            && read(bytes, 16) == self.options.bytes_per_entry as u64
    }
    fn prepare(&self, bytes: &mut [u8]) {
        if !self.valid_header(bytes) {
            self.initialize(bytes);
        }
    }
    fn shard(&self, fingerprint: u64) -> usize {
        (fingerprint % self.options.shards as u64) as usize
    }
    fn offset(&self, index: usize) -> usize {
        HEADER + index * (SLOT + self.options.bytes_per_entry)
    }

    fn matches(
        &self,
        bytes: &[u8],
        offset: usize,
        key: &[u8],
        fingerprint: u64,
        epoch: u64,
    ) -> bool {
        if read(bytes, offset) == 0 || read(bytes, offset + 24) != epoch {
            return false;
        }
        let lengths = read(bytes, offset + 8);
        let key_len = (lengths & 0xffff_ffff) as usize;
        let value_len = (lengths >> 32) as usize;
        key_len
            .checked_add(value_len)
            .is_some_and(|n| n <= self.options.bytes_per_entry)
            && key_len == key.len()
            && read(bytes, offset + 16) == fingerprint
            && &bytes[offset + SLOT..offset + SLOT + key_len] == key
    }
    fn touch(&self, bytes: &mut [u8], offset: usize) {
        let tick = read(bytes, 24);
        let next = if tick == u64::MAX {
            // Preserve validity but safely restart eviction ages on exhaustion.
            for i in 0..self.options.entries_per_shard {
                let slot = self.offset(i);
                if read(bytes, slot) != 0 {
                    write(bytes, slot, 1);
                }
            }
            2
        } else {
            tick + 1
        };
        write(bytes, 24, next);
        write(bytes, offset, next);
    }

    /// Looks up an exact key. Busy shards do not block HTML delivery.
    pub fn get(&self, key: &[u8]) -> io::Result<ByteLookup> {
        let result = self.get_impl(key);
        let index = match &result {
            Ok(ByteLookup::Hit(_)) => 0,
            Ok(ByteLookup::Miss) => 1,
            Ok(ByteLookup::Busy) => 2,
            Err(_) => 7,
        };
        let _ = self.metrics.increment(index);
        result
    }
    fn get_impl(&self, key: &[u8]) -> io::Result<ByteLookup> {
        let fingerprint = hash(key);
        let shard = self.shard(fingerprint);
        let Some(mut guard) = self.region.try_lock(shard)? else {
            return Ok(ByteLookup::Busy);
        };
        let epoch = self.region.epoch(shard)?;
        if epoch == u64::MAX {
            return Ok(ByteLookup::Miss);
        }
        let bytes = guard.bytes_mut();
        self.prepare(bytes);
        for i in 0..self.options.entries_per_shard {
            let slot = self.offset(i);
            if self.matches(bytes, slot, key, fingerprint, epoch) {
                let lengths = read(bytes, slot + 8);
                let start = slot + SLOT + (lengths & 0xffff_ffff) as usize;
                let len = (lengths >> 32) as usize;
                let value = bytes[start..start + len].to_vec();
                self.touch(bytes, slot);
                // A rejected concurrent write/delete can invalidate this shard
                // without owning the payload mutex. Never return the old value.
                return Ok(if self.region.epoch(shard)? == epoch {
                    ByteLookup::Hit(value)
                } else {
                    ByteLookup::Miss
                });
            }
        }
        Ok(ByteLookup::Miss)
    }

    /// Inserts/replaces, evicting the least recently used slot when full. A
    /// failed admission invalidates the prior value rather than exposing it.
    pub fn put(&self, key: &[u8], value: &[u8]) -> io::Result<ByteStore> {
        let result = self.put_impl(key, value);
        let index = match &result {
            Ok(ByteStore::Stored) => 3,
            Ok(ByteStore::TooLarge) => 4,
            Ok(ByteStore::Busy) => 2,
            Err(_) => 7,
        };
        let _ = self.metrics.increment(index);
        result
    }
    fn put_impl(&self, key: &[u8], value: &[u8]) -> io::Result<ByteStore> {
        let fingerprint = hash(key);
        let shard = self.shard(fingerprint);
        let Some(mut guard) = self.region.try_lock(shard)? else {
            self.region.invalidate(shard)?;
            return Ok(ByteStore::Busy);
        };
        let epoch = self.region.epoch(shard)?;
        if epoch == u64::MAX {
            return Ok(ByteStore::Busy);
        }
        let bytes = guard.bytes_mut();
        self.prepare(bytes);
        let mut existing = None;
        let mut victim = self.offset(0);
        let mut oldest = u64::MAX;
        for i in 0..self.options.entries_per_shard {
            let slot = self.offset(i);
            if self.matches(bytes, slot, key, fingerprint, epoch) {
                existing = Some(slot);
            }
            let age = if read(bytes, slot + 24) != epoch {
                0
            } else {
                read(bytes, slot)
            };
            if age < oldest {
                oldest = age;
                victim = slot;
            }
        }
        if key
            .len()
            .checked_add(value.len())
            .is_none_or(|n| n > self.options.bytes_per_entry)
        {
            if let Some(slot) = existing {
                write(bytes, slot, 0);
            }
            return Ok(ByteStore::TooLarge);
        }
        if existing.is_none() && oldest != 0 {
            let _ = self.metrics.increment(5);
        }
        let slot = existing.unwrap_or(victim);
        write(bytes, slot, 0);
        write(
            bytes,
            slot + 8,
            (key.len() as u64) | ((value.len() as u64) << 32),
        );
        write(bytes, slot + 16, fingerprint);
        write(bytes, slot + 24, epoch);
        bytes[slot + SLOT..slot + SLOT + key.len()].copy_from_slice(key);
        bytes[slot + SLOT + key.len()..slot + SLOT + key.len() + value.len()]
            .copy_from_slice(value);
        self.touch(bytes, slot);
        // A concurrent invalidation makes this admission a harmless miss.
        Ok(ByteStore::Stored)
    }

    /// Deletes an exact key. If busy, invalidates the shard atomically so an
    /// older value cannot survive a completed delete operation.
    pub fn delete(&self, key: &[u8]) -> io::Result<()> {
        let _ = self.metrics.increment(6);
        let result = self.delete_impl(key);
        if result.is_err() {
            let _ = self.metrics.increment(7);
        }
        result
    }
    fn delete_impl(&self, key: &[u8]) -> io::Result<()> {
        let fingerprint = hash(key);
        let shard = self.shard(fingerprint);
        let Some(mut guard) = self.region.try_lock(shard)? else {
            self.metrics.increment(2)?;
            return self.region.invalidate(shard);
        };
        let epoch = self.region.epoch(shard)?;
        let bytes = guard.bytes_mut();
        self.prepare(bytes);
        for i in 0..self.options.entries_per_shard {
            let slot = self.offset(i);
            if self.matches(bytes, slot, key, fingerprint, epoch) {
                write(bytes, slot, 0);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::Arc;

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ps-bytes-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self) -> std::path::PathBuf {
            self.0.join("bytes-v1")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn options() -> ByteCacheOptions {
        ByteCacheOptions {
            shards: 1,
            entries_per_shard: 4,
            bytes_per_entry: 128,
        }
    }
    fn cache(temp: &Temp) -> SharedByteCache {
        SharedByteCache::open(&temp.file(), options()).unwrap()
    }
    fn expect(cache: &SharedByteCache, key: &[u8], value: &[u8]) {
        assert_eq!(ByteLookup::Hit(value.to_vec()), cache.get(key).unwrap());
    }
    fn child(temp: &Temp, mode: &str) -> Command {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "byte_cache::tests::subprocess_entry",
                "--nocapture",
            ])
            .env("PS_BYTE_TEST_FILE", temp.file())
            .env("PS_BYTE_TEST_MODE", mode)
            .stdout(Stdio::null());
        child
    }

    #[test]
    fn subprocess_entry() {
        let Some(path) = std::env::var_os("PS_BYTE_TEST_FILE") else {
            return;
        };
        let cache = SharedByteCache::open(Path::new(&path), options()).unwrap();
        let mode = std::env::var("PS_BYTE_TEST_MODE").unwrap();
        if mode == "hold" {
            let mut guard = cache.region.try_lock(0).unwrap().unwrap();
            guard.bytes_mut().fill(0xff);
            std::fs::write(Path::new(&path).with_extension("ready"), b"ready").unwrap();
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        } else if mode == "stress" {
            let byte = std::env::var("PS_BYTE_TEST_VALUE")
                .unwrap()
                .parse::<u8>()
                .unwrap();
            for _ in 0..2000 {
                cache.put(b"same", &[byte; 100]).unwrap();
                if let ByteLookup::Hit(value) = cache.get(b"same").unwrap() {
                    assert_eq!(100, value.len());
                    assert!(value.iter().all(|b| *b == value[0]));
                }
            }
        } else {
            assert_eq!(
                ByteStore::Stored,
                cache.put(b"child", b"from another process").unwrap()
            );
        }
    }

    #[test]
    fn exact_keys_empty_values_replace_and_delete() {
        let temp = Temp::new();
        let cache = cache(&temp);
        assert_eq!(ByteLookup::Miss, cache.get(b"missing").unwrap());
        cache.put(b"abc", b"one").unwrap();
        cache.put(b"abcd", b"two").unwrap();
        cache.put(b"", b"").unwrap();
        expect(&cache, b"abc", b"one");
        expect(&cache, b"abcd", b"two");
        expect(&cache, b"", b"");
        cache.put(b"abc", b"replacement").unwrap();
        expect(&cache, b"abc", b"replacement");
        cache.delete(b"abc").unwrap();
        cache.delete(b"unknown").unwrap();
        assert_eq!(ByteLookup::Miss, cache.get(b"abc").unwrap());
        expect(&cache, b"abcd", b"two");
    }

    #[test]
    fn lru_replacement_and_churn_keep_fixed_allocation() {
        let temp = Temp::new();
        let cache = cache(&temp);
        let size = cache.mapped_bytes();
        for key in [b"a", b"b", b"c", b"d"] {
            cache.put(key, key).unwrap();
        }
        expect(&cache, b"a", b"a");
        cache.put(b"e", b"e").unwrap();
        assert_eq!(ByteLookup::Miss, cache.get(b"b").unwrap());
        expect(&cache, b"a", b"a");
        for i in 0u64..10_000 {
            cache.put(&i.to_le_bytes(), &i.to_le_bytes()).unwrap();
        }
        assert_eq!(size as u64, std::fs::metadata(temp.file()).unwrap().len());
        expect(&cache, &9999u64.to_le_bytes(), &9999u64.to_le_bytes());
    }

    #[test]
    fn oversized_replacement_cannot_leave_old_value() {
        let temp = Temp::new();
        let cache = cache(&temp);
        cache.put(b"key", b"old").unwrap();
        cache.put(b"other", b"retained").unwrap();
        assert_eq!(ByteStore::TooLarge, cache.put(b"key", &[0; 128]).unwrap());
        assert_eq!(ByteLookup::Miss, cache.get(b"key").unwrap());
        expect(&cache, b"other", b"retained");
        assert_eq!(ByteStore::TooLarge, cache.put(&[1; 129], b"").unwrap());
    }

    #[test]
    fn contended_mutations_invalidate_old_values_without_waiting() {
        let temp = Temp::new();
        let cache = cache(&temp);
        cache.put(b"key", b"old").unwrap();
        let guard = cache.region.try_lock(0).unwrap().unwrap();
        assert_eq!(ByteLookup::Busy, cache.get(b"key").unwrap());
        assert_eq!(ByteStore::Busy, cache.put(b"key", b"new").unwrap());
        drop(guard);
        assert_eq!(ByteLookup::Miss, cache.get(b"key").unwrap());
        cache.put(b"key", b"again").unwrap();
        let guard = cache.region.try_lock(0).unwrap().unwrap();
        cache.delete(b"key").unwrap();
        drop(guard);
        assert_eq!(ByteLookup::Miss, cache.get(b"key").unwrap());
    }

    #[test]
    fn matching_hash_is_not_sufficient_and_bad_lengths_are_bounded() {
        let temp = Temp::new();
        let cache = cache(&temp);
        cache.put(b"abc", b"value").unwrap();
        {
            let mut guard = cache.region.try_lock(0).unwrap().unwrap();
            write(guard.bytes_mut(), cache.offset(0) + 16, hash(b"xyz"));
        }
        assert_eq!(ByteLookup::Miss, cache.get(b"xyz").unwrap());
        {
            let mut guard = cache.region.try_lock(0).unwrap().unwrap();
            write(guard.bytes_mut(), cache.offset(0) + 8, u64::MAX);
        }
        assert_eq!(ByteLookup::Miss, cache.get(b"abc").unwrap());
    }

    #[test]
    fn diagnostics_count_eviction_admission_and_partial_usage() {
        let temp = Temp::new();
        let cache = cache(&temp);
        for index in 0..5 {
            cache.put(format!("k{index}").as_bytes(), b"value").unwrap();
        }
        assert_eq!(cache.get(b"absent").unwrap(), ByteLookup::Miss);
        cache.put(b"oversized", &[0; 256]).unwrap();
        let metrics: std::collections::HashMap<_, _> =
            cache.diagnostics().unwrap().into_iter().collect();
        assert_eq!(metrics["puts"], 5);
        assert_eq!(metrics["evictions"], 1);
        assert_eq!(metrics["misses"], 1);
        assert_eq!(metrics["oversized"], 1);
        assert_eq!(metrics["resident_entries"], 4);
        assert_eq!(metrics["resident_key_value_bytes"], 28);
        let _guard = cache.region.try_lock(0).unwrap().unwrap();
        let metrics: std::collections::HashMap<_, _> =
            cache.diagnostics().unwrap().into_iter().collect();
        assert_eq!(metrics["usage_busy_shards"], 1);
        assert_eq!(metrics["resident_entries"], 0);
    }

    #[test]
    fn independent_process_and_reopen_share_values() {
        let temp = Temp::new();
        let first = cache(&temp);
        assert!(child(&temp, "write").status().unwrap().success());
        expect(&first, b"child", b"from another process");
        let metrics: std::collections::HashMap<_, _> =
            first.diagnostics().unwrap().into_iter().collect();
        assert_eq!(metrics["puts"], 1);
        assert_eq!(metrics["hits"], 1);
        assert_eq!(metrics["resident_entries"], 1);
        assert_eq!(
            metrics["resident_key_value_bytes"],
            (b"child".len() + b"from another process".len()) as u64
        );
        drop(first);
        expect(&cache(&temp), b"child", b"from another process");
    }

    #[test]
    fn killed_writer_recovers_to_miss_and_accepts_new_data() {
        let temp = Temp::new();
        let cache = cache(&temp);
        cache.put(b"key", b"old").unwrap();
        let mut child = child(&temp, "hold").spawn().unwrap();
        let ready = temp.file().with_extension("ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("child timeout");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(ByteLookup::Miss, cache.get(b"key").unwrap());
        assert_eq!(
            ByteStore::Stored,
            cache.put(b"key", b"after crash").unwrap()
        );
        expect(&cache, b"key", b"after crash");
    }

    #[test]
    fn concurrent_payloads_are_never_torn() {
        let temp = Temp::new();
        let cache = Arc::new(cache(&temp));
        let mut threads = Vec::new();
        for byte in 0u8..8 {
            let cache = cache.clone();
            threads.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    cache.put(b"same", &[byte; 100]).unwrap();
                    if let ByteLookup::Hit(value) = cache.get(b"same").unwrap() {
                        assert_eq!(100, value.len());
                        assert!(value.iter().all(|b| *b == value[0]));
                    }
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn independent_processes_never_publish_torn_payloads() {
        let temp = Temp::new();
        let cache = cache(&temp);
        let mut children = Vec::new();
        for byte in 0..4 {
            children.push(
                child(&temp, "stress")
                    .env("PS_BYTE_TEST_VALUE", byte.to_string())
                    .spawn()
                    .unwrap(),
            );
        }
        for _ in 0..4000 {
            if let ByteLookup::Hit(value) = cache.get(b"same").unwrap() {
                assert_eq!(100, value.len());
                assert!(value.iter().all(|b| *b == value[0]));
            }
        }
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
    }

    #[test]
    fn dimensions_are_checked_without_resizing_existing_files() {
        let temp = Temp::new();
        let first = cache(&temp);
        let invalid_options = ByteCacheOptions {
            entries_per_shard: 0,
            ..options()
        };
        assert!(SharedByteCache::open(&temp.file(), invalid_options).is_err());
        // Same payload size but different slot dimensions must not alias.
        let options = ByteCacheOptions {
            entries_per_shard: 2,
            bytes_per_entry: 288,
            ..options()
        };
        assert!(SharedByteCache::open(&temp.file(), options).is_err());
        assert_eq!(
            first.mapped_bytes() as u64,
            std::fs::metadata(temp.file()).unwrap().len()
        );
    }
}

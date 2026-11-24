//! Linux recoverable disk-writer coherence; no request-path filesystem checks.
use crate::cache::CacheCoherence;
use ps_shm::write_tracker::{WriteTracker, WRITE_SHARDS};
use std::{io, path::Path};

/// All writers sharing a disk path must enroll in the same tracker domain.
#[derive(Debug)]
pub struct TrackedCoherence {
    tracker: WriteTracker,
    namespace: String,
}
impl TrackedCoherence {
    /// Open configuration-time mappings in an existing private directory.
    /// Namespace must separate full-stop incarnations from old memory entries.
    pub fn open(directory: &Path, namespace: String) -> io::Result<Self> {
        Ok(Self {
            tracker: WriteTracker::open(directory)?,
            namespace,
        })
    }
    /// Key shard for the shared generation domain (FNV-1a mod 128).
    /// Public so integrators can construct colliding keys in tests.
    pub fn shard(key: &str) -> usize {
        key.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        }) as usize
            % WRITE_SHARDS
    }
}
impl CacheCoherence for TrackedCoherence {
    fn diagnostics(&self) -> Vec<(&'static str, u64)> {
        self.tracker.diagnostics().unwrap_or_default()
    }
    fn snapshot(&self, key: &str) -> Option<i64> {
        let shard = Self::shard(key);
        let mut generation = self.tracker.snapshot(shard).ok()?;
        if generation.is_none() {
            self.tracker.recover(shard).ok()?;
            generation = self.tracker.snapshot(shard).ok()?;
        }
        generation.map(|value| value as i64)
    }
    fn memory_key(&self, generation: i64, key: &str) -> String {
        format!("{}{generation}:{key}", self.namespace)
    }
    fn mutate(&self, key: &str, mutation: &mut dyn FnMut()) {
        let shard = Self::shard(key);
        match self.tracker.try_begin(shard) {
            Ok(Some(_write)) => mutation(),
            _ => {
                // Preserve disk writes and deletions when bounded tracking is
                // exhausted. Never leave old memory values valid after them.
                self.tracker
                    .disable_memory(shard)
                    .expect("validated tracking shard");
                mutation();
            }
        }
    }
    fn begin_write(&self, _: &str) {
        panic!("tracked coherence requires transaction callback")
    }
    fn end_write(&self, _: &str) {
        panic!("tracked coherence requires transaction callback")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{Cache, MemoryCache, WriteThroughCache, WRITE_THROUGH_UNLIMITED};
    use std::sync::Arc;
    fn file(directory: &Path) -> Arc<dyn Cache> {
        Arc::new(crate::cache::FileCache::new(
            &directory.join("disk"),
            crate::cache::FileCachePolicy {
                clean_interval_ms: -1,
                target_size_bytes: 16_777_216,
                target_inode_count: 0,
                path_length_limit: 8192,
            },
            0,
        ))
    }
    #[test]
    fn killed_disk_writer_restores_nested_memory_hits() {
        const ENV: &str = "PS_TRACKED_DISK_CHILD";
        if let Some(path) = std::env::var_os(ENV) {
            let directory = std::path::PathBuf::from(path);
            let coherence = TrackedCoherence::open(&directory, "process:".into()).unwrap();
            let disk = file(&directory);
            coherence.mutate("key", &mut || {
                if std::env::var_os("PS_TRACKED_DELETE").is_some() {
                    disk.delete("key");
                } else {
                    disk.put("key", b"new");
                }
                std::fs::write(directory.join("ready"), b"ready").unwrap();
                std::thread::sleep(std::time::Duration::from_secs(30));
            });
            return;
        }
        for delete in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "ps-tracked-kill-{}-{}",
                std::process::id(),
                delete
            ));
            std::fs::create_dir_all(&directory).unwrap();
            let coherence =
                Arc::new(TrackedCoherence::open(&directory, "process:".into()).unwrap());
            let disk = file(&directory);
            let middle: Arc<dyn Cache> = Arc::new(
                WriteThroughCache::new(
                    Arc::new(MemoryCache::new()),
                    disk.clone(),
                    WRITE_THROUGH_UNLIMITED,
                )
                .with_coherence(coherence.clone()),
            );
            let cache = WriteThroughCache::new(
                Arc::new(MemoryCache::new()),
                middle,
                WRITE_THROUGH_UNLIMITED,
            )
            .with_coherence(coherence.clone());
            cache.put("key", b"old");
            assert_eq!(cache.get("key").value.unwrap(), b"old");
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "tracked_coherence::tests::killed_disk_writer_restores_nested_memory_hits",
                    "--nocapture",
                ])
                .env(ENV, &directory)
                .stdout(std::process::Stdio::null());
            if delete {
                command.env("PS_TRACKED_DELETE", "1");
            }
            let mut child = command.spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !directory.join("ready").exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let ready = directory.join("ready").exists();
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(ready);
            assert_eq!(
                cache.get("key").value,
                if delete { None } else { Some(b"new".to_vec()) }
            );
            assert!(coherence.snapshot("key").is_some());
            if !delete {
                // Prove memory admissions resumed, not merely safe disk bypass.
                disk.delete("key");
                assert_eq!(cache.get("key").value, Some(b"new".to_vec()));
            }
            drop(cache);
            drop(coherence);
            drop(disk);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn nested_adapters_recover_terminated_writer_and_preserve_saturated_deletes() {
        let directory = std::env::temp_dir().join(format!(
            "ps-tracked-coherence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let coherence = Arc::new(TrackedCoherence::open(&directory, "test:".into()).unwrap());
        let disk = Arc::new(MemoryCache::new());
        let inner: Arc<dyn Cache> = Arc::new(
            WriteThroughCache::new(
                Arc::new(MemoryCache::new()),
                disk.clone(),
                WRITE_THROUGH_UNLIMITED,
            )
            .with_coherence(coherence.clone()),
        );
        let cache =
            WriteThroughCache::new(Arc::new(MemoryCache::new()), inner, WRITE_THROUGH_UNLIMITED)
                .with_coherence(coherence.clone());
        cache.put("key", b"old");
        assert_eq!(cache.get("key").value.unwrap(), b"old");
        let moved = coherence.clone();
        let changed = disk.clone();
        std::thread::spawn(move || {
            let interrupted = moved
                .tracker
                .try_begin(TrackedCoherence::shard("key"))
                .unwrap()
                .unwrap();
            changed.put("key", b"new");
            std::mem::forget(interrupted);
        })
        .join()
        .unwrap();
        assert_eq!(cache.get("key").value.unwrap(), b"new");
        assert!(coherence.snapshot("key").is_some());
        let mut full = Vec::new();
        for _ in 0..ps_shm::write_tracker::WRITER_SLOTS {
            full.push(coherence.tracker.try_begin(0).unwrap().unwrap());
        }
        cache.put("key", b"saturated");
        assert_eq!(cache.get("key").value.unwrap(), b"saturated");
        cache.delete("key");
        assert!(cache.get("key").value.is_none());
        drop(full);
        assert!(coherence.snapshot("key").is_none());
        drop(cache);
        drop(coherence);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn offline_domain_replacement_restores_admission_and_preserves_disk() {
        let directory = std::env::temp_dir().join(format!(
            "ps-tracked-offline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let domain = directory.join("tracker");
        std::fs::create_dir_all(&domain).unwrap();
        let disk = file(&directory);
        let coherence = Arc::new(TrackedCoherence::open(&domain, "before:".into()).unwrap());
        let cache = WriteThroughCache::new(
            Arc::new(MemoryCache::new()),
            disk.clone(),
            WRITE_THROUGH_UNLIMITED,
        )
        .with_coherence(coherence.clone());
        cache.put("keep", b"old");
        cache.put("delete", b"old");
        let full: Vec<_> = (0..ps_shm::write_tracker::WRITER_SLOTS)
            .map(|_| coherence.tracker.try_begin(0).unwrap().unwrap())
            .collect();
        cache.put("keep", b"persisted replacement");
        cache.delete("delete");
        drop(full);
        assert!(coherence.snapshot("keep").is_none());
        assert!(coherence.snapshot("delete").is_none());
        assert_eq!(cache.get("keep").value.unwrap(), b"persisted replacement");
        assert!(cache.get("delete").value.is_none());
        // Simulate a full stop: no tracker/cache handles survive removal.
        drop(cache);
        drop(coherence);
        drop(disk);
        std::fs::remove_dir_all(&domain).unwrap();
        std::fs::create_dir(&domain).unwrap();
        let coherence = Arc::new(TrackedCoherence::open(&domain, "after:".into()).unwrap());
        let memory = Arc::new(MemoryCache::new());
        let cache =
            WriteThroughCache::new(memory.clone(), file(&directory), WRITE_THROUGH_UNLIMITED)
                .with_coherence(coherence.clone());
        assert_eq!(cache.get("keep").value.unwrap(), b"persisted replacement");
        let generation = coherence
            .snapshot("keep")
            .expect("new domain admits memory again");
        assert_eq!(
            memory
                .get(&coherence.memory_key(generation, "keep"))
                .value
                .unwrap(),
            b"persisted replacement"
        );
        assert!(cache.get("delete").value.is_none());
        drop(cache);
        drop(coherence);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

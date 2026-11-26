//! Local read-only cache workload after creating a private temporary fixture.
use ps_cache::cache::{
    Cache, FileCache, FileCachePolicy, KeyState, Lookup, LruCache, WriteThroughCache,
    DISABLE_CLEANING, WRITE_THROUGH_UNLIMITED,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
#[derive(Debug)]
struct Counted {
    disk: FileCache,
    reads: AtomicUsize,
}
impl Cache for Counted {
    fn get(&self, k: &str) -> Lookup {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.disk.get(k)
    }
    fn put(&self, k: &str, v: &[u8]) {
        self.disk.put(k, v)
    }
    fn delete(&self, k: &str) {
        self.disk.delete(k)
    }
    fn name(&self) -> String {
        "counted-file".into()
    }
}
fn main() {
    let path = std::env::temp_dir().join(format!("ps-missing-metadata-{}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    let disk = Arc::new(Counted {
        disk: FileCache::new(
            &path,
            FileCachePolicy {
                clean_interval_ms: DISABLE_CLEANING,
                target_size_bytes: 1048576,
                target_inode_count: 0,
                path_length_limit: 8192,
            },
            0,
        ),
        reads: AtomicUsize::new(0),
    });
    let cache = WriteThroughCache::new(
        Arc::new(LruCache::new(1048576)),
        disk.clone(),
        WRITE_THROUGH_UNLIMITED,
    );
    cache.put("present-font-evidence", b"confirmed");
    println!("state,lookups,disk_reads,total_us,ns_per_lookup");
    for (key, state) in [
        ("missing-font-evidence", KeyState::NotFound),
        ("present-font-evidence", KeyState::Available),
    ] {
        let before = disk.reads.load(Ordering::Relaxed);
        let t = Instant::now();
        for _ in 0..10000 {
            assert_eq!(std::hint::black_box(cache.get(key)).state, state);
        }
        let elapsed = t.elapsed();
        println!(
            "{state:?},10000,{},{},{}",
            disk.reads.load(Ordering::Relaxed) - before,
            elapsed.as_micros(),
            elapsed.as_nanos() / 10000
        );
    }
    std::fs::remove_dir_all(path).unwrap();
}

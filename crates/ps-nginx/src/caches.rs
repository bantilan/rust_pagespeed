//! System caches (`SystemCaches`, `SystemCachePath`).
//!
//! Ports the cache composition: per-path file plus LRU caches
//! with cleaning-policy merge rules, config registration, the
//! RootInit/ChildInit lifecycle, and the per-context cache
//! assembly. Shared bytes are available through an opt-in factory API;
//! the Nginx directive remains unsupported until configuration integration.
//! External (memcached/redis) caches, job lock managers and the property
//! store remain unsupported; configured external caches warn once
//! per path and fall back to file cache, and purge-file polling
//! waits for HTTPCache purge integration (the file format is
//! ported). Cleaning runs synchronously from the backends (C++
//! offloads it to a slow worker); interprocess clean locking is
//! a documented follow-up, as in the backend.

use crate::conf::NgxOptions;
use ps_cache::cache::{
    Cache, CacheCoherence, CompressedCache, FileCache, FileCachePolicy, LruCache,
    WriteThroughCache, DEFAULT_MAX_INFLATED_BYTES, DEFAULT_PATH_LENGTH_LIMIT,
    WRITE_THROUGH_UNLIMITED,
};
use ps_config::options::Value;
use ps_http::http_cache::HttpCache;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// File-cache stats prefix (`SystemCachePath::kFileCache`).
pub const FILE_CACHE_STATS: &str = "file_cache";
/// LRU stats prefix (`SystemCachePath::kLruCache`).
pub const LRU_CACHE_STATS: &str = "lru_cache";

/// External-cache fallback warning (port-authored: C++ builds
/// memcached/redis clients here).
pub const EXTERNAL_CACHE_WARNING: &str =
    "Memcached/Redis caching is not supported in this build; using the file cache.";

/// Path key (`CachePath`): `"<unplugged>"`, or the path plus the
/// purge/flush word plus the configured (not resolved) filename.
#[must_use]
pub fn cache_path_key(unplugged: bool, path: &str, purge: bool, filename: &str) -> String {
    if unplugged {
        return "<unplugged>".to_string();
    }
    format!(
        "{path} {} {filename}",
        if purge { "purge" } else { "flush" }
    )
}

/// Resolves the flush filename (ctor rule): explicit, else
/// `cache.purge`/`cache.flush` by mode, relativized under the
/// path unless absolute (verbatim, including the empty-path
/// leading slash).
#[must_use]
pub fn flush_filename(file_cache_path: &str, configured: &str, purge: bool) -> String {
    let name = if configured.is_empty() {
        if purge {
            "cache.purge"
        } else {
            "cache.flush"
        }
    } else {
        configured
    };
    if name.starts_with('/') {
        return name.to_string();
    }
    let separator = if !file_cache_path.is_empty() && file_cache_path.ends_with('/') {
        ""
    } else {
        "/"
    };
    format!("{file_cache_path}{separator}{name}")
}

/// Merges one cleaning-policy entry (`MergeEntries`): equal values
/// keep quiet; a lone explicit side wins silently; two explicit
/// values warn and keep the larger (sizes) or smaller (interval).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn merge_policy_entry(
    config_value: i64,
    config_set: bool,
    take_larger: bool,
    name: &str,
    policy_value: i64,
    policy_set: bool,
    path: &str,
) -> (i64, bool, Option<String>) {
    if config_value == policy_value {
        return (policy_value, policy_set, None);
    }
    if config_set && !policy_set {
        return (config_value, true, None);
    }
    if !config_set && policy_set {
        return (policy_value, policy_set, None);
    }
    let warning = format!(
        "Conflicting settings {config_value}!={policy_value} for FileCacheClean{name} \
         for file-cache {path}, keeping the {} value",
        if take_larger { "larger" } else { "smaller" }
    );
    let value = if (take_larger && config_value > policy_value)
        || (!take_larger && config_value < policy_value)
    {
        config_value
    } else {
        policy_value
    };
    (value, true, Some(warning))
}

/// Metadata inflation cap (`MaxMetadataInflatedValueSize`):
/// negative (unlimited) passes through, otherwise the response
/// limit only floors the 16 MiB default.
#[must_use]
pub fn max_metadata_inflated_value_size(max_cacheable_response_content_length: i64) -> i64 {
    if max_cacheable_response_content_length < 0 {
        max_cacheable_response_content_length
    } else {
        max_cacheable_response_content_length.max(DEFAULT_MAX_INFLATED_BYTES as i64)
    }
}

/// Root-phase line (`SystemCachePath::RootInit`, logged even
/// without shared memory, like C++).
#[must_use]
pub fn root_init_message(path: &str) -> String {
    format!("Initializing shared memory for path: {path}.")
}

/// Child-phase line (`SystemCachePath::ChildInit`).
#[must_use]
pub fn child_init_message(path: &str) -> String {
    format!("Reusing shared memory for path: {path}.")
}

/// Shared generation and active-writer count, mapped once before worker fork.
#[derive(Debug)]
struct SharedCacheCoherence(ps_shm::SharedCounters, Option<String>);
impl SharedCacheCoherence {
    fn slot(key: &str) -> usize {
        let hash = key.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        (hash as usize % 128) * 2
    }
}
impl CacheCoherence for SharedCacheCoherence {
    fn snapshot(&self, key: &str) -> Option<i64> {
        let slot = Self::slot(key);
        if self.0.coordination_get(slot + 1) != 0 {
            return None;
        }
        let generation = self.0.coordination_get(slot);
        (self.0.coordination_get(slot + 1) == 0).then_some(generation)
    }
    fn memory_key(&self, generation: i64, key: &str) -> String {
        match &self.1 {
            Some(prefix) => format!("{prefix}{generation}:{key}"),
            None => format!("{generation}:{key}"),
        }
    }
    fn begin_write(&self, key: &str) {
        self.0.coordination_add(Self::slot(key) + 1, 1);
    }
    fn end_write(&self, key: &str) {
        let slot = Self::slot(key);
        self.0.coordination_add(slot, 1);
        self.0.coordination_add(slot + 1, -1);
    }
}

fn shared_coherence(path: &str) -> Option<Arc<dyn CacheCoherence>> {
    if path.is_empty() {
        return None;
    }
    // Reserved subtree: the file cleaner explicitly excludes live mappings.
    let directory = Path::new(path).join(".pagespeed-lru");
    std::fs::create_dir_all(&directory).ok()?;
    let names: Vec<_> = (0..128)
        .flat_map(|slot| {
            [
                format!("cache_generation_{slot}"),
                format!("active_writers_{slot}"),
            ]
        })
        .collect();
    let shared = ps_shm::SharedCounters::open(&directory, &names).ok()?;
    Some(Arc::new(SharedCacheCoherence(shared, None)))
}

fn same_disk_path(left: &str, right: &str) -> bool {
    left == right
        || matches!((std::fs::canonicalize(left), std::fs::canonicalize(right)), (Ok(left), Ok(right)) if left == right)
}

/// Per-path caches (`SystemCachePath` without shm locks, purge
/// polling, or the clean worker).
pub struct SystemCachePath {
    /// Cache path.
    pub path: String,
    /// Resolved flush filename.
    pub flush_filename: String,
    /// Path belongs to unplugged contexts only.
    pub unplugged: bool,
    /// Purge (vs flush) mode.
    pub purge_enabled: bool,
    /// File backend (shared by handle; merged before setup).
    pub file_cache: Arc<FileCache>,
    /// Per-process LRU (absent when sized 0).
    pub lru_cache: Option<Arc<LruCache>>,
    coherence: Option<Arc<dyn CacheCoherence>>,
    /// Opt-in shared byte tier; startup must enroll every writer to this path.
    pub shared_cache: Option<Arc<dyn Cache>>,
    memory_namespace: Option<String>,
    rewrite_coordinator: Option<Arc<dyn ps_cache::rewrite_coordinator::RewriteCoordinator>>,
    clean_interval_set: bool,
    clean_size_set: bool,
    clean_inode_set: bool,
}

impl SystemCachePath {
    /// Builds a path's caches from a config (ctor). The purge
    /// flag rides separately: it lives on core options, not the
    /// system subset.
    #[must_use]
    pub fn new(
        path: &str,
        system: &ps_config::options::NginxSystemOptions,
        purge: bool,
        unplugged: bool,
        now_ms: i64,
    ) -> Self {
        let policy = FileCachePolicy {
            clean_interval_ms: system.file_cache_clean_interval_ms,
            target_size_bytes: system.file_cache_clean_size_kb.saturating_mul(1024),
            target_inode_count: system.file_cache_clean_inode_limit,
            path_length_limit: DEFAULT_PATH_LENGTH_LIMIT,
        };
        let file_cache = Arc::new(FileCache::new(Path::new(path), policy, now_ms));
        let coherence = (system.lru_cache_kb_per_process > 0 && !unplugged)
            .then(|| shared_coherence(path))
            .flatten();
        let lru_cache = if system.lru_cache_kb_per_process > 0 && coherence.is_some() {
            // `size_t` conversion, verbatim (negative wraps huge).
            let bytes = system.lru_cache_kb_per_process.wrapping_mul(1024) as usize;
            Some(Arc::new(LruCache::new(bytes)))
        } else {
            None
        };
        SystemCachePath {
            path: path.to_string(),
            flush_filename: flush_filename(path, &system.cache_flush_filename, purge),
            unplugged,
            purge_enabled: purge,
            file_cache,
            lru_cache,
            coherence,
            shared_cache: None,
            memory_namespace: None,
            rewrite_coordinator: None,
            clean_interval_set: system.was_set.clean_interval,
            clean_size_set: system.was_set.clean_size,
            clean_inode_set: system.was_set.clean_inode,
        }
    }

    /// Merges another config's cleaning policy (`MergeConfig`),
    /// returning conflict warnings. Panics when handles already
    /// escaped (merges run at config time, before setup).
    pub fn merge_config(&mut self, system: &ps_config::options::NginxSystemOptions) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut policy = self.file_cache.policy().clone();
        let (value, set, warning) = merge_policy_entry(
            system.file_cache_clean_interval_ms,
            system.was_set.clean_interval,
            false,
            "IntervalMs",
            policy.clean_interval_ms,
            self.clean_interval_set,
            &self.path,
        );
        policy.clean_interval_ms = value;
        self.clean_interval_set = set;
        warnings.extend(warning);
        let (value, set, warning) = merge_policy_entry(
            system.file_cache_clean_size_kb.saturating_mul(1024),
            system.was_set.clean_size,
            true,
            "SizeKb",
            policy.target_size_bytes,
            self.clean_size_set,
            &self.path,
        );
        policy.target_size_bytes = value;
        self.clean_size_set = set;
        warnings.extend(warning);
        let (value, set, warning) = merge_policy_entry(
            system.file_cache_clean_inode_limit,
            system.was_set.clean_inode,
            true,
            "InodeLimit",
            policy.target_inode_count,
            self.clean_inode_set,
            &self.path,
        );
        policy.target_inode_count = value;
        self.clean_inode_set = set;
        warnings.extend(warning);
        Arc::get_mut(&mut self.file_cache)
            .expect("merge before setup")
            .set_policy(policy);
        warnings
    }
}

impl std::fmt::Debug for SystemCachePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemCachePath")
            .field("path", &self.path)
            .field("flush_filename", &self.flush_filename)
            .field("unplugged", &self.unplugged)
            .field("lru", &self.lru_cache.is_some())
            .finish_non_exhaustive()
    }
}

/// Assembled context caches (`SetupCaches` product).
#[derive(Debug)]
pub struct ContextCaches {
    /// HTTP cache (file backend, or LRU writethrough).
    pub http_cache: HttpCache,
    /// HTTP backend handle (shared with HTML contexts).
    pub http_backend: Arc<dyn Cache>,
    /// Metadata cache (LRU writethrough over file, compressed
    /// unless disabled).
    pub metadata_cache: Arc<dyn Cache>,
    /// Optional cross-worker HTML optimization domain (only opt-in shared paths).
    pub rewrite_coordinator: Option<Arc<dyn ps_cache::rewrite_coordinator::RewriteCoordinator>>,
}

/// Cache system with optional coherent shared bytes and persistent file storage.
#[derive(Debug, Default)]
pub struct SystemCaches {
    paths: HashMap<String, SystemCachePath>,
    warned_external: HashSet<String>,
}

impl SystemCaches {
    /// Creates an empty system.
    #[must_use]
    pub fn new() -> Self {
        SystemCaches::default()
    }

    /// Registers a config (`RegisterConfig`): merges or creates
    /// its path; configured external caches warn once per path
    /// and fall back to file cache.
    pub fn register_config(
        &mut self,
        system: &ps_config::options::NginxSystemOptions,
        purge: bool,
        unplugged: bool,
        now_ms: i64,
    ) -> Vec<String> {
        let key = cache_path_key(
            unplugged,
            &system.file_cache_path,
            purge,
            &system.cache_flush_filename,
        );
        let mut warnings = Vec::new();
        if (!system.memcached_servers.is_empty() || system.redis_server.is_some())
            && self.warned_external.insert(key.clone())
        {
            warnings.push(EXTERNAL_CACHE_WARNING.to_string());
        }
        let enrolled = (!unplugged)
            .then(|| {
                self.paths
                    .values()
                    .find(|path| {
                        same_disk_path(&path.path, &system.file_cache_path)
                            && path.shared_cache.is_some()
                    })
                    .map(|path| {
                        (
                            path.coherence.clone(),
                            path.shared_cache.clone(),
                            path.memory_namespace.clone(),
                            path.rewrite_coordinator.clone(),
                        )
                    })
            })
            .flatten();
        match self.paths.get_mut(&key) {
            Some(path) => warnings.extend(path.merge_config(system)),
            None => {
                let mut path =
                    SystemCachePath::new(&system.file_cache_path, system, purge, unplugged, now_ms);
                if let Some((coherence, shared, namespace, coordinator)) = enrolled {
                    path.coherence = coherence;
                    path.shared_cache = shared;
                    path.memory_namespace = namespace;
                    path.rewrite_coordinator = coordinator;
                }
                self.paths.insert(key.clone(), path);
            }
        }
        warnings
    }

    /// Enables shared bytes for every registered context sharing a disk path.
    /// Must run before setup_caches exposes handles. This API is intentionally
    /// opt-in; configuration and mixed-version migration are separate steps.
    #[cfg(target_os = "linux")]
    pub fn enable_shared_memory(
        &mut self,
        disk_path: &str,
        options: ps_shm::byte_cache::ByteCacheOptions,
    ) -> Result<(), String> {
        let paths: Vec<_> = self
            .paths
            .values()
            .filter(|path| same_disk_path(&path.path, disk_path) && !path.unplugged)
            .collect();
        if paths.is_empty() {
            return Err("no registered cache path".into());
        }
        std::fs::create_dir_all(disk_path).map_err(|e| e.to_string())?;
        let paths: Vec<_> = self
            .paths
            .values()
            .filter(|path| same_disk_path(&path.path, disk_path) && !path.unplugged)
            .collect();
        if paths
            .iter()
            .any(|path| Arc::strong_count(&path.file_cache) != 1 || path.shared_cache.is_some())
        {
            return Err("shared cache must be enabled once, before backend handles escape".into());
        }
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|e| e.to_string())?;
        let boot = boot.trim();
        if boot.len() != 36 || !boot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err("invalid Linux boot identity".into());
        }
        let directory = Path::new(disk_path)
            .join(".pagespeed-lru")
            .join("shared-v2")
            .join(boot);
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let names: Vec<_> = (0..128)
            .flat_map(|slot| {
                [
                    format!("cache_generation_{slot}"),
                    format!("active_writers_{slot}"),
                ]
            })
            .collect();
        let counters =
            ps_shm::SharedCounters::open_coherent(&directory, &names).map_err(|e| e.to_string())?;
        let namespace = format!(
            "shm-v2:{:032x}:",
            counters.incarnation().expect("coherent lifetime")
        );
        let coherence: Arc<dyn CacheCoherence> =
            Arc::new(SharedCacheCoherence(counters, Some(namespace.clone())));
        let filename = directory.join(format!(
            "bytes-v2-{}-{}-{}.dat",
            options.shards, options.entries_per_shard, options.bytes_per_entry
        ));
        let shared: Arc<dyn Cache> = Arc::new(
            ps_cache::shared_memory::SharedMemoryCache::open(&filename, options)
                .map_err(|e| e.to_string())?,
        );
        let coordinator: Arc<dyn ps_cache::rewrite_coordinator::RewriteCoordinator> = Arc::new(
            ps_cache::rewrite_coordinator::SharedRewriteCoordinator::open(
                &directory.join("rewrite-owners-v2-256.dat"),
                256,
                std::time::Duration::from_millis(5),
            )
            .map_err(|e| e.to_string())?,
        );
        for path in self
            .paths
            .values_mut()
            .filter(|path| same_disk_path(&path.path, disk_path) && !path.unplugged)
        {
            path.coherence = Some(coherence.clone());
            path.shared_cache = Some(shared.clone());
            path.memory_namespace = Some(namespace.clone());
            path.rewrite_coordinator = Some(coordinator.clone());
        }
        Ok(())
    }

    /// Root-phase messages (`RootInit`): one info line per path.
    #[must_use]
    pub fn root_init_messages(&self) -> Vec<String> {
        let mut paths: Vec<&str> = self.paths.values().map(|path| path.path.as_str()).collect();
        paths.sort_unstable();
        paths.into_iter().map(root_init_message).collect()
    }

    /// Child-phase messages (`ChildInit` minus shm attach, worker
    /// handoff, and purge setup): one info line per live path.
    #[must_use]
    pub fn child_init_messages(&self) -> Vec<String> {
        let mut paths: Vec<&str> = self
            .paths
            .values()
            .filter(|path| !path.unplugged)
            .map(|path| path.path.as_str())
            .collect();
        paths.sort_unstable();
        paths.into_iter().map(child_init_message).collect()
    }

    /// Looks up a path's caches.
    #[must_use]
    pub fn path(&self, key: &str) -> Option<&SystemCachePath> {
        self.paths.get(key)
    }

    /// Assembles a context's caches (`SetupCaches` without shm,
    /// external caches, or the property store): HTTP cache over
    /// file (LRU writethrough when sized), metadata cache as
    /// LRU-over-file compressed unless disabled. Returns `None`
    /// when the path was never registered.
    #[must_use]
    pub fn setup_caches(&self, options: &NgxOptions) -> Option<ContextCaches> {
        let key = cache_path_key(
            options.unplugged(),
            &options.system.file_cache_path,
            options.core.enable_cache_purge(),
            &options.system.cache_flush_filename,
        );
        let path = self.paths.get(&key)?;
        let backend = path_backend(path, options.system.lru_cache_byte_limit as u64);
        let levels =
            1 + i32::from(path.lru_cache.is_some()) + i32::from(path.shared_cache.is_some());
        let mut http_cache = HttpCache::new(Arc::clone(&backend));
        configure_http_cache(&mut http_cache, options, levels);
        // Metadata rides the same file/LRU stack (both L2s are the
        // file cache without external storage).
        let metadata: Arc<dyn Cache> =
            path_backend(path, options.system.lru_cache_byte_limit as u64);
        let metadata: Arc<dyn Cache> = if options.system.compress_metadata_cache {
            let max_body = match options.core.get_by_id("rcl") {
                Some(Value::Int(max)) => max,
                _ => 16_777_216,
            };
            let cap = max_metadata_inflated_value_size(max_body);
            let cap = if cap < 0 { usize::MAX } else { cap as usize };
            Arc::new(CompressedCache::with_limit(metadata, cap))
        } else {
            metadata
        };
        Some(ContextCaches {
            http_cache,
            http_backend: backend,
            metadata_cache: metadata,
            rewrite_coordinator: path.rewrite_coordinator.clone(),
        })
    }
}

/// Builds a path's HTTP backend: file, or LRU writethrough when
/// the path sizes an LRU.
#[must_use]
pub fn path_backend(path: &SystemCachePath, lru_byte_limit: u64) -> Arc<dyn Cache> {
    let file: Arc<dyn Cache> = path.file_cache.clone();
    let backend: Arc<dyn Cache> = match &path.shared_cache {
        Some(shared) => Arc::new(
            WriteThroughCache::new(shared.clone(), file, WRITE_THROUGH_UNLIMITED).with_coherence(
                path.coherence
                    .as_ref()
                    .expect("shared tier requires coherence")
                    .clone(),
            ),
        ),
        None => file,
    };
    match path.lru_cache.as_ref() {
        Some(lru) => Arc::new(
            WriteThroughCache::new(lru.clone(), backend, lru_byte_limit).with_coherence(
                path.coherence
                    .as_ref()
                    .expect("LRU requires coherence")
                    .clone(),
            ),
        ),
        None => backend,
    }
}

/// Applies config to an HTTP cache (`SetupCaches` tail): levels,
/// compression, body cap.
pub fn configure_http_cache(cache: &mut HttpCache, options: &NgxOptions, levels: i32) {
    cache.set_cache_levels(levels);
    // Range-checked at parse, so the narrowing cannot fail.
    if let Some(Value::Int(level)) = options.core.get_by_id("hccl") {
        cache.set_compression_level(level as i32);
    }
    if let Some(Value::Int(max)) = options.core.get_by_id("rcl") {
        cache.set_max_body_bytes(max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conf::NgxOptions;

    #[test]
    fn path_keys_and_filenames() {
        assert_eq!(cache_path_key(true, "/c", false, "f"), "<unplugged>");
        assert_eq!(cache_path_key(false, "/c", false, "f"), "/c flush f");
        assert_eq!(cache_path_key(false, "/c", true, "f"), "/c purge f");
        assert_eq!(flush_filename("/c", "", false), "/c/cache.flush");
        assert_eq!(flush_filename("/c", "", true), "/c/cache.purge");
        assert_eq!(flush_filename("/c/", "", false), "/c/cache.flush");
        assert_eq!(flush_filename("/c", "/abs", false), "/abs");
        assert_eq!(flush_filename("/c", "rel", false), "/c/rel");
        assert_eq!(flush_filename("", "", false), "/cache.flush");
    }

    #[test]
    fn merge_matrix() {
        // Equal values keep quiet.
        assert_eq!(
            merge_policy_entry(10, true, false, "IntervalMs", 10, true, "/c"),
            (10, true, None)
        );
        // Lone explicit side wins silently.
        assert_eq!(
            merge_policy_entry(10, true, false, "IntervalMs", 20, false, "/c"),
            (10, true, None)
        );
        assert_eq!(
            merge_policy_entry(10, false, false, "IntervalMs", 20, true, "/c"),
            (20, true, None)
        );
        // Two explicit values warn and keep smaller (interval).
        let (value, set, warning) =
            merge_policy_entry(10, true, false, "IntervalMs", 20, true, "/c");
        assert_eq!((value, set), (10, true));
        assert_eq!(
            warning.unwrap(),
            "Conflicting settings 10!=20 for FileCacheCleanIntervalMs for file-cache /c, \
             keeping the smaller value"
        );
        // ... or larger (sizes).
        let (value, _, warning) = merge_policy_entry(10, true, true, "SizeKb", 20, true, "/c");
        assert_eq!(value, 20);
        assert!(warning.unwrap().ends_with("keeping the larger value"));
    }

    #[test]
    fn metadata_cap() {
        assert_eq!(max_metadata_inflated_value_size(-1), -1);
        assert_eq!(max_metadata_inflated_value_size(0), 16 << 20);
        assert_eq!(max_metadata_inflated_value_size(1024), 16 << 20);
        assert_eq!(max_metadata_inflated_value_size(32 << 20), 32 << 20);
    }

    fn system_with(path: &str) -> ps_config::options::NginxSystemOptions {
        let mut system = ps_config::options::NginxSystemOptions::default();
        system.file_cache_path = path.to_string();
        system
    }

    #[test]
    fn register_merges_and_warns() {
        let mut caches = SystemCaches::new();
        let system = system_with("/c");
        assert!(caches.register_config(&system, false, false, 0).is_empty());
        // Same path merges cleaning policy (explicit size wins).
        let mut system2 = system_with("/c");
        system2.file_cache_clean_size_kb = 1;
        system2.was_set.clean_size = true;
        assert!(caches.register_config(&system2, false, false, 0).is_empty());
        let path = caches.path("/c flush ").expect("registered");
        assert_eq!(1024, path.file_cache.policy().target_size_bytes);
        // External caches warn once per path and fall back.
        let mut system3 = system_with("/c");
        system3.memcached_servers = vec![ps_config::options::ExternalServerSpec {
            host: "h".to_string(),
            port: 1,
        }];
        let warnings = caches.register_config(&system3, false, false, 0);
        assert_eq!(warnings, vec![EXTERNAL_CACHE_WARNING.to_string()]);
        let warnings = caches.register_config(&system3, false, false, 0);
        assert!(warnings.is_empty());
        // Lifecycle messages, sorted by path.
        assert_eq!(
            caches.root_init_messages(),
            vec!["Initializing shared memory for path: /c.".to_string()]
        );
        assert_eq!(
            caches.child_init_messages(),
            vec!["Reusing shared memory for path: /c.".to_string()]
        );
    }

    #[test]
    fn unplugged_paths_skip_child_messages() {
        let mut caches = SystemCaches::new();
        let system = system_with("");
        caches.register_config(&system, false, true, 0);
        assert!(caches.path("<unplugged>").is_some());
        assert!(caches.child_init_messages().is_empty());
        assert_eq!(caches.root_init_messages().len(), 1);
    }

    #[test]
    fn cross_worker_coherence() {
        const CHILD_PATH: &str = "PS_TEST_COHERENCE_PATH";
        let child = std::env::var(CHILD_PATH).ok();
        let path = child.clone().unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("ps-coherence-{}", std::process::id()))
                .to_string_lossy()
                .into_owned()
        });
        let mut options = NgxOptions::new();
        options.system.file_cache_path = path;
        let mut caches = SystemCaches::new();
        caches.register_config(&options.system, false, false, 0);
        let backend = caches.setup_caches(&options).unwrap().http_backend;
        if child.is_some() {
            backend.put("shared", b"new");
            backend.delete("deleted");
            return;
        }
        backend.put("shared", b"old");
        backend.put("deleted", b"old");
        assert_eq!(
            backend.get("shared").value.as_deref(),
            Some(b"old".as_slice())
        );
        assert_eq!(
            backend.get("shared").value.as_deref(),
            Some(b"old".as_slice())
        );
        assert!(backend.get("deleted").value.is_some());
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "caches::tests::cross_worker_coherence"])
            .env(CHILD_PATH, &options.system.file_cache_path)
            .status()
            .unwrap();
        assert!(result.success());
        assert_eq!(
            backend.get("shared").value.as_deref(),
            Some(b"new".as_slice())
        );
        assert!(backend.get("deleted").value.is_none());
        // Cleanup must leave the shared mapping intact.
        let path = caches
            .path(&cache_path_key(
                false,
                &options.system.file_cache_path,
                false,
                "",
            ))
            .unwrap();
        path.file_cache.clean_if_needed(i64::MAX);
        assert!(Path::new(&options.system.file_cache_path)
            .join(".pagespeed-lru")
            .is_dir());
    }

    #[cfg(target_os = "linux")]
    fn shared_options() -> ps_shm::byte_cache::ByteCacheOptions {
        ps_shm::byte_cache::ByteCacheOptions {
            shards: 4,
            entries_per_shard: 16,
            bytes_per_entry: 1024,
        }
    }

    #[cfg(target_os = "linux")]
    fn shared_fixture(path: &str) -> (SystemCaches, NgxOptions) {
        let mut options = NgxOptions::new();
        options.system.file_cache_path = path.to_string();
        let mut caches = SystemCaches::new();
        caches.register_config(&options.system, false, false, 0);
        caches.enable_shared_memory(path, shared_options()).unwrap();
        (caches, options)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_factory_enrolls_all_path_variants_and_rejects_late_switches() {
        let directory =
            std::env::temp_dir().join(format!("ps-shm-factory-all-{}", std::process::id()));
        let path = directory.to_str().unwrap();
        let mut caches = SystemCaches::new();
        let options = system_with(path);
        caches.register_config(&options, false, false, 0);
        caches.register_config(&options, true, false, 0);
        let mut alias = options.clone();
        alias.file_cache_path = format!("{path}/");
        caches.register_config(&alias, false, false, 0);
        caches.enable_shared_memory(path, shared_options()).unwrap();
        alias.cache_flush_filename = "another.flush".to_string();
        caches.register_config(&alias, false, false, 0);
        assert!(caches
            .paths
            .values()
            .all(|p| p.shared_cache.is_some() && p.rewrite_coordinator.is_some()));
        let mut options = NgxOptions::new();
        options.system.file_cache_path = path.to_string();
        let handles = caches.setup_caches(&options).unwrap();
        assert!(handles.http_backend.name().contains("SharedMemoryCache"));
        assert!(handles.rewrite_coordinator.is_some());
        // All aliases and configurations, including late registrations, share
        // one canonical cache-domain coordinator.
        let coordinator = handles.rewrite_coordinator.unwrap();
        let held = match coordinator.claim("same-options:ua:partition") {
            ps_cache::rewrite_coordinator::RewriteClaim::Owned(lease) => lease,
            _ => panic!("initial ownership refused"),
        };
        for path in caches.paths.values() {
            assert!(matches!(
                path.rewrite_coordinator
                    .as_ref()
                    .unwrap()
                    .claim("same-options:ua:partition"),
                ps_cache::rewrite_coordinator::RewriteClaim::Retry
            ));
        }
        drop(held);
        assert!(caches.enable_shared_memory(path, shared_options()).is_err());
        let late = directory.join("late");
        let mut options = NgxOptions::new();
        options.system.file_cache_path = late.to_str().unwrap().to_string();
        let mut caches = SystemCaches::new();
        caches.register_config(&options.system, false, false, 0);
        let _escaped = caches.setup_caches(&options).unwrap();
        assert!(caches
            .enable_shared_memory(late.to_str().unwrap(), shared_options())
            .is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_factory_cross_process_put_delete_and_oversized_replacement() {
        const ENV: &str = "PS_SHM_FACTORY_CHILD";
        let child = std::env::var(ENV).ok();
        let directory = child.clone().unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("ps-shm-factory-worker-{}", std::process::id()))
                .to_string_lossy()
                .into_owned()
        });
        let (caches, options) = shared_fixture(&directory);
        let backend = caches.setup_caches(&options).unwrap().http_backend;
        if child.is_some() {
            backend.put("shared", b"updated");
            backend.put("oversized", &[7; 2048]);
            backend.delete("deleted");
            return;
        }
        for key in ["shared", "oversized", "deleted"] {
            backend.put(key, b"old");
            assert_eq!(Some(b"old".to_vec()), backend.get(key).value);
        }
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "caches::tests::shared_factory_cross_process_put_delete_and_oversized_replacement",
            ])
            .env(ENV, &directory)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(child.success());
        assert_eq!(Some(b"updated".to_vec()), backend.get("shared").value);
        assert_eq!(Some(vec![7; 2048]), backend.get("oversized").value);
        assert!(backend.get("deleted").value.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_tier_serves_another_worker_without_a_disk_entry() {
        let directory =
            std::env::temp_dir().join(format!("ps-shm-factory-hit-{}", std::process::id()));
        let path = directory.to_str().unwrap();
        let (first, options) = shared_fixture(path);
        let backend = first.setup_caches(&options).unwrap().http_backend;
        backend.put("cached", b"value");
        assert_eq!(Some(b"value".to_vec()), backend.get("cached").value);
        let (second, options) = shared_fixture(path);
        // Simulate L3 eviction only: a valid shared object must remain useful.
        first
            .paths
            .values()
            .next()
            .unwrap()
            .file_cache
            .delete("cached");
        let other_worker = second.setup_caches(&options).unwrap().http_backend;
        assert_eq!(Some(b"value".to_vec()), other_worker.get("cached").value);
        assert!(first
            .paths
            .values()
            .next()
            .unwrap()
            .file_cache
            .get("cached")
            .value
            .is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn restart_namespace_prevents_old_shared_generation_hits() {
        let directory =
            std::env::temp_dir().join(format!("ps-shm-factory-restart-{}", std::process::id()));
        let path = directory.to_str().unwrap();
        let (first, options) = shared_fixture(path);
        let backend = first.setup_caches(&options).unwrap().http_backend;
        backend.put("persistent", b"old");
        assert_eq!(Some(b"old".to_vec()), backend.get("persistent").value);
        let namespace = first
            .paths
            .values()
            .next()
            .unwrap()
            .memory_namespace
            .clone();
        let file = first.paths.values().next().unwrap().file_cache.clone();
        drop(backend);
        drop(first);
        // No live cache participants remain; disk persistence changes offline.
        file.put("persistent", b"new");
        drop(file);
        let (second, options) = shared_fixture(path);
        assert_ne!(
            namespace,
            second.paths.values().next().unwrap().memory_namespace
        );
        let backend = second.setup_caches(&options).unwrap().http_backend;
        // Force the target shard to reuse its old numeric generation, proving
        // that the incarnation prefix, not just a different integer, protects it.
        let collision = (0..100_000)
            .map(|i| format!("different-{i}"))
            .find(|key| SharedCacheCoherence::slot(key) == SharedCacheCoherence::slot("persistent"))
            .unwrap();
        backend.put(&collision, b"other");
        assert_eq!(Some(b"new".to_vec()), backend.get("persistent").value);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_metadata_retains_html_expiry_purge_and_variant_validation() {
        let directory =
            std::env::temp_dir().join(format!("ps-shm-factory-ttl-{}", std::process::id()));
        let path = directory.to_str().unwrap();
        let (first, options) = shared_fixture(path);
        let cache = first.setup_caches(&options).unwrap().metadata_cache;
        let key_a = ps_cache::html::html_cache_key("page options-A chrome");
        let key_b = ps_cache::html::html_cache_key("page options-B firefox");
        let a = ps_cache::html::encode_value(1000, 2000, b"chrome html").unwrap();
        let b = ps_cache::html::encode_value(1000, 2000, b"firefox html").unwrap();
        cache.put(&key_a, &a);
        cache.put(&key_b, &b);
        assert_eq!(Some(a.clone()), cache.get(&key_a).value);
        let (second, options) = shared_fixture(path);
        let remote = second.setup_caches(&options).unwrap().metadata_cache;
        let from_shared = remote.get(&key_a).value.unwrap();
        assert_eq!(a, from_shared);
        assert_eq!(Some(b), remote.get(&key_b).value);
        let mut purge = ps_cache::purge::PurgeSet::new(4096);
        assert!(ps_cache::html::validate_value(
            &from_shared,
            "http://h/page",
            1500,
            1000,
            i64::MAX,
            &purge
        )
        .is_some());
        assert!(ps_cache::html::validate_value(
            &from_shared,
            "http://h/page",
            2001,
            1000,
            i64::MAX,
            &purge
        )
        .is_none());
        purge.put("http://h/page", 1600);
        assert!(ps_cache::html::validate_value(
            &from_shared,
            "http://h/page",
            1700,
            1000,
            i64::MAX,
            &purge
        )
        .is_none());
    }

    #[test]
    fn setup_assembly() {
        let mut caches = SystemCaches::new();
        let scratch =
            std::env::temp_dir().join(format!("ps-cache-assembly-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        // Defaults enable the bounded in-memory tier (writethrough
        // LRU over file, compressed metadata).
        let mut options = NgxOptions::new();
        options.system.file_cache_path = scratch.join("c").to_string_lossy().into_owned();
        caches.register_config(&options.system.clone(), false, false, 0);
        let assembled = caches.setup_caches(&options).expect("registered");
        assert!(assembled.http_cache.name().contains("WriteThrough"));
        assert!(assembled.metadata_cache.name().contains("WriteThrough"));
        // Explicit zeros restore the file-only C++ shape.
        let mut file_only = NgxOptions::new();
        file_only.system.file_cache_path = "/f".to_string();
        file_only.system.lru_cache_kb_per_process = 0;
        file_only.system.lru_cache_byte_limit = 0;
        caches.register_config(&file_only.system.clone(), false, false, 0);
        let assembled = caches.setup_caches(&file_only).expect("registered");
        assert_eq!(assembled.http_cache.name(), "HTTPCache(FileCache)");
        assert_eq!(assembled.metadata_cache.name(), "Compressed(FileCache)");
        // Unknown paths fail.
        let mut missing = NgxOptions::new();
        missing.system.file_cache_path = "/nope".to_string();
        assert!(caches.setup_caches(&missing).is_none());
        // Sized LRU adds writethrough layers.
        let mut lru_options = NgxOptions::new();
        lru_options.system.file_cache_path = scratch.join("l").to_string_lossy().into_owned();
        lru_options.system.lru_cache_kb_per_process = 1024;
        caches.register_config(&lru_options.system.clone(), false, false, 0);
        let assembled = caches.setup_caches(&lru_options).expect("registered");
        assert!(assembled.http_cache.name().contains("WriteThrough"));
        assert!(assembled.metadata_cache.name().contains("WriteThrough"));
        // Disabled compression stores raw.
        lru_options.system.compress_metadata_cache = false;
        let assembled = caches.setup_caches(&lru_options).expect("registered");
        assert!(!assembled.metadata_cache.name().contains("Compressed"));
    }
}

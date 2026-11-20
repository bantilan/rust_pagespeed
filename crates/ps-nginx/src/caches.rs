//! System caches (`SystemCaches`, `SystemCachePath`).
//!
//! Ports the cache composition: per-path file plus LRU caches
//! with cleaning-policy merge rules, config registration, the
//! RootInit/ChildInit lifecycle, and the per-context cache
//! assembly. Shared-memory metadata caches, external
//! (memcached/redis) caches, lock managers, and the property
//! store are unsupported: shm metadata caches already fail
//! closed at config time, configured external caches warn once
//! per path and fall back to file cache, and purge-file polling
//! waits for HTTPCache purge integration (the file format is
//! ported). Cleaning runs synchronously from the backends (C++
//! offloads it to a slow worker); interprocess clean locking is
//! a documented follow-up, as in the backend.

use crate::conf::NgxOptions;
use ps_cache::cache::{
    Cache, CompressedCache, FileCache, FileCachePolicy, LruCache, WriteThroughCache,
    DEFAULT_MAX_INFLATED_BYTES, DEFAULT_PATH_LENGTH_LIMIT,
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
        let lru_cache = if system.lru_cache_kb_per_process != 0 {
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
}

/// Cache system (`SystemCaches` without shm/external/locks).
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
        match self.paths.get_mut(&key) {
            Some(path) => warnings.extend(path.merge_config(system)),
            None => {
                self.paths.insert(
                    key.clone(),
                    SystemCachePath::new(&system.file_cache_path, system, purge, unplugged, now_ms),
                );
            }
        }
        warnings
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
        let levels = if path.lru_cache.is_some() { 2 } else { 1 };
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
        })
    }
}

/// Builds a path's HTTP backend: file, or LRU writethrough when
/// the path sizes an LRU.
#[must_use]
pub fn path_backend(path: &SystemCachePath, lru_byte_limit: u64) -> Arc<dyn Cache> {
    let file_cache = Arc::clone(&path.file_cache);
    let file: Arc<dyn Cache> = file_cache;
    match path.lru_cache.as_ref() {
        Some(lru) => {
            let lru_cache = Arc::clone(lru);
            let lru: Arc<dyn Cache> = lru_cache;
            Arc::new(WriteThroughCache::new(lru, file, lru_byte_limit))
        }
        None => file,
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
    fn setup_assembly() {
        let mut caches = SystemCaches::new();
        let mut options = NgxOptions::new();
        options.system.file_cache_path = "/c".to_string();
        caches.register_config(&options.system.clone(), false, false, 0);
        // File-only backend, compressed metadata by default.
        let assembled = caches.setup_caches(&options).expect("registered");
        assert_eq!(assembled.http_cache.name(), "HTTPCache(FileCache)");
        assert_eq!(assembled.metadata_cache.name(), "Compressed(FileCache)");
        // Unknown paths fail.
        let mut missing = NgxOptions::new();
        missing.system.file_cache_path = "/nope".to_string();
        assert!(caches.setup_caches(&missing).is_none());
        // Sized LRU adds writethrough layers.
        let mut lru_options = NgxOptions::new();
        lru_options.system.file_cache_path = "/l".to_string();
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

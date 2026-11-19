//! Cache system wiring (`system_caches.{h,cc}`, closes caching-gap-2).
//!
//! Builds the backend chains in `SetupCaches` order from plain config:
//!
//! - External backends: Redis wins over memcached (both configured logs
//!   a warning and uses Redis).
//! - HTTP L2 = external async, else the file cache; optional LRU L1 via
//!   write-through with the byte limit.
//! - Metadata L2 = SHM/file fallback when SHM configures, else the HTTP
//!   L2; optional LRU L1; optional metadata compression with the
//!   floored inflation cap.
//! - Property-store cache = external blocking, else the metadata L2.
//!
//! Shared-memory segments need cross-process primitives that land with
//! the server integration: requesting SHM fails the build with an
//! explicit error instead of silently degrading.
#![forbid(unsafe_code)]

use crate::cache::{
    Cache, CompressedCache, FileCache, FileCachePolicy, LruCache, MemoryCache, WriteThroughCache,
};
use crate::remote::{
    ClusterSpec, MemcachedCache, RedisCache, MEMCACHED_DEFAULT_PORT, MEMCACHED_DEFAULT_TIMEOUT_US,
    REDIS_DEFAULT_PORT, REDIS_DEFAULT_RECONNECT_DELAY_MS, REDIS_DEFAULT_TIMEOUT_US,
    REDIS_TTL_NOT_SET,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Default metadata inflation floor (16 MiB, `kDefaultMaxInflatedValueSize`).
pub const METADATA_INFLATED_FLOOR: i64 = 16 << 20;
/// Maximum metadata inflation cap pass-through for negative settings.
pub const INFLATED_UNLIMITED: i64 = -1;

/// Metadata inflation cap (`MaxMetadataInflatedValueSize`): negative
/// settings pass through, otherwise floored at 16 MiB.
#[must_use]
pub fn max_metadata_inflated_size(max_cacheable_response_content_length: i64) -> i64 {
    if max_cacheable_response_content_length < 0 {
        return max_cacheable_response_content_length;
    }
    max_cacheable_response_content_length.max(METADATA_INFLATED_FLOOR)
}

/// Cache system configuration (the `SetupCaches` inputs).
#[derive(Debug, Clone)]
pub struct CacheSystemConfig {
    /// Per-process LRU size in KB (0 disables the LRU tiers).
    pub lru_cache_kb_per_process: u64,
    /// Maximum single entry for LRU tiers, bytes.
    pub lru_cache_byte_limit: u64,
    /// File-cache root (empty disables the file tier).
    pub file_cache_path: String,
    /// File-cache clean interval, ms (-1 disables).
    pub file_cache_clean_interval_ms: i64,
    /// File-cache size target, KB.
    pub file_cache_clean_size_kb: i64,
    /// File-cache inode target (0 = unlimited).
    pub file_cache_clean_inode_limit: i64,
    /// Memcached cluster spec (`host[:port],...`; empty disables).
    pub memcached_servers: String,
    /// Memcached transaction timeout, µs.
    pub memcached_timeout_us: u64,
    /// Redis server (`host[:port]`; empty disables).
    pub redis_server: String,
    /// Redis database index (-1 skips `SELECT`).
    pub redis_database_index: i32,
    /// Redis key TTL, seconds (-1 = plain `SET`).
    pub redis_ttl_sec: i64,
    /// Redis operation timeout, µs.
    pub redis_timeout_us: u64,
    /// Redis reconnect backoff, ms.
    pub redis_reconnect_delay_ms: u64,
    /// Request shared-memory metadata cache (unsupported: fails builds).
    pub want_shm_metadata_cache: bool,
    /// Compress metadata/property caches.
    pub compress_metadata_cache: bool,
    /// Maximum cacheable response size, bytes (-1 unlimited).
    pub max_cacheable_response_content_length: i64,
    /// Current time, ms (schedules file-cache cleaning).
    pub now_ms: i64,
}

impl Default for CacheSystemConfig {
    fn default() -> Self {
        CacheSystemConfig {
            lru_cache_kb_per_process: 0,
            lru_cache_byte_limit: 0,
            file_cache_path: String::new(),
            file_cache_clean_interval_ms: 3_600_000,
            file_cache_clean_size_kb: 102_400,
            file_cache_clean_inode_limit: 0,
            memcached_servers: String::new(),
            memcached_timeout_us: MEMCACHED_DEFAULT_TIMEOUT_US,
            redis_server: String::new(),
            redis_database_index: 0,
            redis_ttl_sec: REDIS_TTL_NOT_SET,
            redis_timeout_us: REDIS_DEFAULT_TIMEOUT_US,
            redis_reconnect_delay_ms: REDIS_DEFAULT_RECONNECT_DELAY_MS,
            want_shm_metadata_cache: false,
            compress_metadata_cache: true,
            max_cacheable_response_content_length: 16 << 20,
            now_ms: 0,
        }
    }
}

/// Built cache tiers (`SetupCaches` outputs).
#[derive(Debug)]
pub struct CacheSystem {
    /// HTTP cache backend (L2, or L1+L2 write-through).
    pub http_cache: Arc<dyn Cache>,
    /// Metadata cache backend.
    pub metadata_cache: Arc<dyn Cache>,
    /// Property-store backend.
    pub property_cache: Arc<dyn Cache>,
    /// Filesystem metadata backend (external blocking, else file cache).
    pub filesystem_metadata_cache: Arc<dyn Cache>,
    /// Warnings emitted while building (e.g. Redis-over-memcached).
    pub warnings: Vec<String>,
}

/// System build failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemBuildError {
    /// Shared-memory metadata cache requested (needs server integration).
    ShmUnsupported,
    /// Bad memcached spec.
    BadMemcachedSpec(String),
    /// Bad Redis spec.
    BadRedisSpec(String),
    /// No usable backend tier at all.
    NoBackend,
}

impl std::fmt::Display for SystemBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SystemBuildError::ShmUnsupported => {
                write!(f, "shared-memory metadata cache needs server integration")
            }
            SystemBuildError::BadMemcachedSpec(error) => {
                write!(f, "bad memcached spec: {error}")
            }
            SystemBuildError::BadRedisSpec(error) => {
                write!(f, "bad redis spec: {error}")
            }
            SystemBuildError::NoBackend => {
                write!(f, "no cache backend configured")
            }
        }
    }
}

impl std::error::Error for SystemBuildError {}

/// Builds the cache tiers in `SetupCaches` order.
pub fn build_system(config: &CacheSystemConfig) -> Result<CacheSystem, SystemBuildError> {
    if config.want_shm_metadata_cache {
        return Err(SystemBuildError::ShmUnsupported);
    }
    let mut warnings = Vec::new();

    // External backends: Redis wins over memcached when both configure.
    let memcached = if config.memcached_servers.is_empty() {
        None
    } else {
        let cluster = ClusterSpec::parse(&config.memcached_servers, MEMCACHED_DEFAULT_PORT)
            .map_err(SystemBuildError::BadMemcachedSpec)?;
        Some(Arc::new(MemcachedCache::new(
            &cluster,
            Duration::from_micros(config.memcached_timeout_us),
        )) as Arc<dyn Cache>)
    };
    let redis = if config.redis_server.is_empty() {
        None
    } else {
        let spec = crate::remote::ServerSpec::parse(&config.redis_server, REDIS_DEFAULT_PORT)
            .map_err(SystemBuildError::BadRedisSpec)?;
        Some(Arc::new(RedisCache::new(
            &spec,
            Duration::from_micros(config.redis_timeout_us),
            config.redis_database_index,
            config.redis_ttl_sec,
            config.redis_reconnect_delay_ms,
        )) as Arc<dyn Cache>)
    };
    // Async/blocking split and batching are runtime-integration concerns;
    // the same backend serves both roles here.
    let external = match (&redis, &memcached) {
        (Some(redis), Some(_)) => {
            warnings.push("both Redis and memcached configured; using Redis".to_string());
            Some(redis.clone())
        }
        (Some(redis), None) => Some(redis.clone()),
        (None, Some(memcached)) => Some(memcached.clone()),
        (None, None) => None,
    };

    // File tier (absent without a path).
    let file_cache: Option<Arc<dyn Cache>> = if config.file_cache_path.is_empty() {
        None
    } else {
        Some(Arc::new(FileCache::new(
            &PathBuf::from(&config.file_cache_path),
            FileCachePolicy {
                clean_interval_ms: config.file_cache_clean_interval_ms,
                target_size_bytes: config.file_cache_clean_size_kb * 1024,
                target_inode_count: config.file_cache_clean_inode_limit,
                path_length_limit: crate::cache::DEFAULT_PATH_LENGTH_LIMIT,
            },
            config.now_ms,
        )))
    };

    // LRU tier (absent at 0 KB).
    let lru_cache: Option<Arc<dyn Cache>> = if config.lru_cache_kb_per_process == 0 {
        None
    } else {
        Some(Arc::new(LruCache::new(
            (config.lru_cache_kb_per_process * 1024) as usize,
        )))
    };

    // HTTP L2 = external async, else file.
    let http_l2: Arc<dyn Cache> = match (&external, &file_cache) {
        (Some(external), _) => external.clone(),
        (None, Some(file)) => file.clone(),
        (None, None) => Arc::new(MemoryCache::new()),
    };
    // HTTP cache: optional LRU L1 via write-through.
    let http_cache: Arc<dyn Cache> = match &lru_cache {
        Some(lru) => Arc::new(WriteThroughCache::new(
            lru.clone(),
            http_l2.clone(),
            config.lru_cache_byte_limit,
        )),
        None => http_l2.clone(),
    };

    // Filesystem metadata cache: external blocking, else file (else LRU,
    // else memory, so the handle is never missing).
    let filesystem_metadata_cache: Arc<dyn Cache> = match (&external, &file_cache, &lru_cache) {
        (Some(external), _, _) => external.clone(),
        (None, Some(file), _) => file.clone(),
        (None, None, Some(lru)) => lru.clone(),
        (None, None, None) => Arc::new(MemoryCache::new()),
    };

    // Metadata L1/L2 without SHM: LRU over the HTTP L2.
    let metadata_l2 = http_l2.clone();
    let mut metadata_cache: Arc<dyn Cache> = match &lru_cache {
        Some(lru) => Arc::new(WriteThroughCache::new(
            lru.clone(),
            metadata_l2.clone(),
            config.lru_cache_byte_limit,
        )),
        None => metadata_l2.clone(),
    };
    // Property store defaults to the metadata L2.
    let mut property_cache: Arc<dyn Cache> = metadata_l2;
    if config.compress_metadata_cache {
        let cap = max_metadata_inflated_size(config.max_cacheable_response_content_length);
        let cap = if cap < 0 { usize::MAX } else { cap as usize };
        metadata_cache = Arc::new(CompressedCache::with_limit(metadata_cache, cap));
        property_cache = Arc::new(CompressedCache::with_limit(property_cache, cap));
    }

    Ok(CacheSystem {
        http_cache,
        metadata_cache,
        property_cache,
        filesystem_metadata_cache,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflation_cap() {
        assert_eq!(max_metadata_inflated_size(-1), -1);
        assert_eq!(max_metadata_inflated_size(0), METADATA_INFLATED_FLOOR);
        assert_eq!(max_metadata_inflated_size(1024), METADATA_INFLATED_FLOOR);
        assert_eq!(max_metadata_inflated_size(64 << 20), 64 << 20);
    }

    #[test]
    fn default_chain_is_file_backed() {
        let mut config = CacheSystemConfig::default();
        let dir = std::env::temp_dir().join(format!("ps_system_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        config.file_cache_path = dir.to_string_lossy().into_owned();
        let system = build_system(&config).unwrap();
        assert!(system.warnings.is_empty());
        // Default has no LRU (0 KB): HTTP cache is the file tier itself.
        assert_eq!(system.http_cache.name(), "FileCache");
        // compress_metadata_cache defaults on: metadata/property wrapped.
        assert_eq!(system.metadata_cache.name(), "Compressed(FileCache)");
        assert_eq!(system.property_cache.name(), "Compressed(FileCache)");
        assert_eq!(system.filesystem_metadata_cache.name(), "FileCache");
        // Round-trip through the built chain.
        system.metadata_cache.put("k", b"v");
        assert_eq!(
            system.metadata_cache.get("k").state,
            crate::cache::KeyState::Available
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lru_and_redis_wiring() {
        let mut config = CacheSystemConfig {
            lru_cache_kb_per_process: 1024,
            lru_cache_byte_limit: 4096,
            // Redis beats memcached with a warning when both configure.
            redis_server: "127.0.0.1:6379".to_string(),
            memcached_servers: "127.0.0.1:11211".to_string(),
            ..CacheSystemConfig::default()
        };
        let system = build_system(&config).unwrap();
        assert_eq!(system.warnings.len(), 1);
        assert_eq!(
            system.http_cache.name(),
            "WriteThroughCache(l1=LRUCache,l2=RedisCache)"
        );
        assert_eq!(system.filesystem_metadata_cache.name(), "RedisCache");
        // SHM configs fail explicitly instead of degrading silently.
        config.want_shm_metadata_cache = true;
        assert_eq!(
            build_system(&config).unwrap_err(),
            SystemBuildError::ShmUnsupported
        );
        // Bad specs fail with detail.
        config.want_shm_metadata_cache = false;
        config.redis_server = "h:notaport".to_string();
        assert!(matches!(
            build_system(&config).unwrap_err(),
            SystemBuildError::BadRedisSpec(_)
        ));
    }
}

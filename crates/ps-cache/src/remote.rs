//! Remote backends (`apr_mem_cache.{h,cc}`, `redis_cache.{h,cc}`,
//! `external_server_spec.{h,cc}`).
//!
//! Blocking TCP clients speaking memcached ASCII and Redis RESP,
//! with the C++ sharding rules (apr CRC32 mod live servers;
//! CRC16 slots + at most one MOVED/ASK follow), health tracking,
//! and owned counters. Transports sit behind [`RemoteTransport`] so
//! fakes plug in for tests and benches; live-server tests gate on
//! `PAGESPEED_TEST_MEMCACHED` / `PAGESPEED_TEST_REDIS`.
#![forbid(unsafe_code)]

use crate::cache::{key_value_codec, Cache, KeyState, Lookup};
use crate::hash::{Hasher, Md5Hasher};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::time::Duration;

/// Default memcached port (`kMemcachedDefaultPort`).
pub const MEMCACHED_DEFAULT_PORT: u16 = 11211;
/// Default Redis port (`kRedisDefaultPort`).
pub const REDIS_DEFAULT_PORT: u16 = 6379;
/// Default memcached transaction timeout, µs (half a second).
pub const MEMCACHED_DEFAULT_TIMEOUT_US: u64 = 500_000;
/// Default Redis operation timeout, µs (50 ms).
pub const REDIS_DEFAULT_TIMEOUT_US: u64 = 50_000;
/// Default Redis reconnection delay, ms (1 s).
pub const REDIS_DEFAULT_RECONNECT_DELAY_MS: u64 = 1000;
/// Redis database index meaning "no SELECT" (`kRedisDatabaseIndexNotSet`).
pub const REDIS_DB_NOT_SET: i32 = -1;
/// Redis TTL meaning plain `SET` (`kRedisTTLNotSet`).
pub const REDIS_TTL_NOT_SET: i64 = -1;
/// Maximum redirect follows per command (`kMaxRedirections` = 1).
pub const REDIS_MAX_REDIRECTIONS: u32 = 1;
/// Maximum cluster connections (`kMaxClusterConnections`).
pub const REDIS_MAX_CLUSTER_CONNECTIONS: usize = 1024;
/// Memcached key hash length (`cache_hasher_(20)`).
pub const MEMCACHED_HASH_CHARS: usize = 20;
/// Health window (`kHealthCheckpointIntervalMs` = 30 s).
pub const HEALTH_WINDOW_MS: i64 = 30_000;
/// Errors tolerated per window (`kMaxErrorBurst` = 4).
pub const MAX_ERROR_BURST: i64 = 4;

/// One `host[:port]` server (`ExternalServerSpec`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerSpec {
    /// Hostname or IP.
    pub host: String,
    /// Port.
    pub port: u16,
}

impl ServerSpec {
    /// Parses `host[:port]` (`SetFromString`).
    pub fn parse(text: &str, default_port: u16) -> Result<ServerSpec, String> {
        // Split keeping empties; 1-2 segments required.
        let parts: Vec<&str> = text.split(':').collect();
        if parts.len() > 2 {
            return Err("Expected single server in format <host>[:<port>]".to_string());
        }
        let host = parts[0];
        if host.is_empty() {
            return Err("Server host cannot be empty".to_string());
        }
        let mut port = default_port;
        if parts.len() == 2 {
            let parsed: i64 = parts[1]
                .parse()
                .map_err(|_| format!("Port specified is not a valid number: '{}'", parts[1]))?;
            if !(1..=65535).contains(&parsed) {
                return Err(format!("Invalid port: {parsed}"));
            }
            port = parsed as u16;
        }
        Ok(ServerSpec {
            host: host.to_string(),
            port,
        })
    }

    /// `host:port` rendering (`ToString`).
    #[must_use]
    pub fn to_spec_string(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Whether both parts are empty/zero.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.host.is_empty() && self.port == 0
    }
}

/// Comma-separated server list (`ExternalClusterSpec`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClusterSpec {
    /// Servers in config order.
    pub servers: Vec<ServerSpec>,
}

impl ClusterSpec {
    /// Parses the list (`SetFromString`): empty clears; per-server errors
    /// join with `. ` and fail the whole parse.
    pub fn parse(text: &str, default_port: u16) -> Result<ClusterSpec, String> {
        if text.is_empty() {
            return Ok(ClusterSpec::default());
        }
        let mut servers = Vec::new();
        let mut errors = String::new();
        let parts: Vec<&str> = text.split(',').collect();
        for part in &parts {
            match ServerSpec::parse(part, default_port) {
                Ok(spec) => servers.push(spec),
                Err(error) => {
                    if parts.len() > 1 {
                        if !errors.is_empty() {
                            errors.push_str(". ");
                        }
                        errors.push_str(&format!("In server '{part}': "));
                    }
                    errors.push_str(&error);
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(ClusterSpec { servers })
    }

    /// Whether no servers are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

/// CRC32-IEEE (`apr_memcache_hash_crc32`): init `~0`, reflected table,
///
/// final `~`. The apr table is the standard IEEE one (starts
/// `0x00000000, 0x77073096, ...`), so bitwise evaluation matches.
#[must_use]
pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = !0;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = crc & 1;
            crc >>= 1;
            if mask != 0 {
                crc ^= 0xEDB8_8320;
            }
        }
    }
    !crc
}

/// Default apr hash (`apr_memcache_hash_default`): `(crc32 >> 16) & 0x7fff`.
#[must_use]
pub fn apr_memcache_hash(key: &str) -> u32 {
    (crc32_ieee(key.as_bytes()) >> 16) & 0x7fff
}

/// Picks a live server index (`apr_memcache_find_server_hash_default`):
/// `hash % ntotal` over the live list (dead servers skipped by callers
/// here; apr retries them after its own backoff).
#[must_use]
pub fn pick_server(hash: u32, num_servers: usize) -> Option<usize> {
    if num_servers == 0 {
        return None;
    }
    let hash = if hash == 0 { 1 } else { hash };
    Some(hash as usize % num_servers)
}

/// CRC16-CCITT (`redis_crc::crc16`, poly 0x1021, init 0).
#[must_use]
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Redis key slot (`RedisCache::HashSlot`): `{...}` hash tags honored,
/// low 14 bits of CRC16.
#[must_use]
pub fn redis_hash_slot(key: &str) -> u16 {
    let mut effective = key;
    if let Some(open) = key.find('{') {
        if let Some(close) = key[open..].find('}') {
            if close > 1 {
                effective = &key[open + 1..open + close];
            }
        }
    }
    crc16_ccitt(effective.as_bytes()) & 0x3FFF
}

/// Transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// Read/write/connect timeout.
    Timeout,
    /// Connection or I/O failure.
    Network(String),
    /// Malformed server reply.
    Protocol(String),
}

/// Minimal key/value transport behind the remote backends.
pub trait RemoteTransport: Send + Sync + std::fmt::Debug {
    /// Fetches a key; `Ok(None)` is a clean miss.
    fn transport_get(&self, key: &str) -> Result<Option<Vec<u8>>, TransportError>;
    /// Stores a key (`ttl_sec` honored by Redis; ignored by memcached).
    fn transport_set(
        &self,
        key: &str,
        value: &[u8],
        ttl_sec: Option<i64>,
    ) -> Result<(), TransportError>;
    /// Deletes a key (missing keys succeed silently).
    fn transport_delete(&self, key: &str) -> Result<(), TransportError>;
}

/// In-memory fake transport for tests and benches.
#[derive(Debug, Default)]
pub struct FakeTransport {
    entries: Mutex<HashMap<String, Vec<u8>>>,
    fail: Mutex<Option<TransportError>>,
}

impl FakeTransport {
    /// Creates an empty fake.
    #[must_use]
    pub fn new() -> FakeTransport {
        FakeTransport::default()
    }

    /// Makes every operation fail with the error (cleared with `None`).
    pub fn set_failure(&self, error: Option<TransportError>) {
        *lock(&self.fail) = error;
    }
}

impl RemoteTransport for FakeTransport {
    fn transport_get(&self, key: &str) -> Result<Option<Vec<u8>>, TransportError> {
        if let Some(error) = lock(&self.fail).clone() {
            return Err(error);
        }
        Ok(lock(&self.entries).get(key).cloned())
    }

    fn transport_set(
        &self,
        key: &str,
        value: &[u8],
        _ttl_sec: Option<i64>,
    ) -> Result<(), TransportError> {
        if let Some(error) = lock(&self.fail).clone() {
            return Err(error);
        }
        lock(&self.entries).insert(key.to_string(), value.to_vec());
        Ok(())
    }

    fn transport_delete(&self, key: &str) -> Result<(), TransportError> {
        if let Some(error) = lock(&self.fail).clone() {
            return Err(error);
        }
        lock(&self.entries).remove(key);
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_millis() as i64)
        .unwrap_or(0)
}

/// Error-burst health tracker (`AprMemCache::RecordError`/`IsHealthy`).
#[derive(Debug)]
pub struct ErrorBurst {
    last_checkpoint_ms: Mutex<i64>,
    burst_size: Mutex<i64>,
    shutdown: Mutex<bool>,
    /// Timeout-only counter (`memcache_timeouts`).
    pub timeouts: Mutex<u64>,
}

impl ErrorBurst {
    /// Creates a healthy tracker.
    #[must_use]
    pub fn new() -> ErrorBurst {
        ErrorBurst {
            last_checkpoint_ms: Mutex::new(0),
            burst_size: Mutex::new(0),
            shutdown: Mutex::new(false),
            timeouts: Mutex::new(0),
        }
    }

    /// Records an error (`RecordError`).
    pub fn record(&self, timeout: bool) {
        if timeout {
            *lock(&self.timeouts) += 1;
        }
        let now = now_ms();
        let last = *lock(&self.last_checkpoint_ms);
        if now - last > HEALTH_WINDOW_MS {
            *lock(&self.last_checkpoint_ms) = now;
            *lock(&self.burst_size) = 1;
        } else {
            *lock(&self.burst_size) += 1;
        }
    }

    /// Health check (`IsHealthy`): sick past 4 errors per window until
    /// the window passes (which resets the burst and probes recovery).
    #[must_use]
    pub fn healthy(&self) -> bool {
        if *lock(&self.shutdown) {
            return false;
        }
        let now = now_ms();
        let last = *lock(&self.last_checkpoint_ms);
        let burst = *lock(&self.burst_size);
        if now - last > HEALTH_WINDOW_MS {
            *lock(&self.burst_size) = 0;
            return true;
        }
        burst < MAX_ERROR_BURST
    }

    /// Shuts down (`ShutDown`).
    pub fn shutdown(&self) {
        *lock(&self.shutdown) = true;
    }

    /// Current burst size (test helper).
    #[must_use]
    pub fn burst_size(&self) -> i64 {
        *lock(&self.burst_size)
    }
}

impl Default for ErrorBurst {
    fn default() -> Self {
        ErrorBurst::new()
    }
}

/// One blocking TCP connection with timeouts.
///
/// A single persistent `BufReader` serves all reads: per-call readers
/// would lose prefetched bytes when dropped.
#[derive(Debug)]
struct TcpConnection {
    addr: String,
    timeout: Duration,
    writer: Option<TcpStream>,
    reader: Option<BufReader<TcpStream>>,
    generation: u64,
}

impl TcpConnection {
    fn new(spec: &ServerSpec, timeout: Duration) -> TcpConnection {
        TcpConnection {
            addr: spec.to_spec_string(),
            timeout,
            writer: None,
            reader: None,
            generation: 0,
        }
    }

    /// Connection epoch, bumped on every (re)connect so users can detect
    /// reconnects (e.g. to re-SELECT a Redis database).
    fn generation(&self) -> u64 {
        self.generation
    }

    /// Connects (or reconnects), applying timeouts.
    fn ensure(&mut self) -> Result<(), TransportError> {
        if self.writer.is_some() {
            return Ok(());
        }
        let mut addrs = self
            .addr
            .to_socket_addrs()
            .map_err(|err| TransportError::Network(err.to_string()))?;
        let first = addrs
            .next()
            .ok_or_else(|| TransportError::Network("no address".to_string()))?;
        let stream = TcpStream::connect_timeout(&first, self.timeout).map_err(|err| {
            if err.kind() == std::io::ErrorKind::TimedOut {
                TransportError::Timeout
            } else {
                TransportError::Network(err.to_string())
            }
        })?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|err| TransportError::Network(err.to_string()))?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|err| TransportError::Network(err.to_string()))?;
        let read_half = stream
            .try_clone()
            .map_err(|err| TransportError::Network(err.to_string()))?;
        self.reader = Some(BufReader::new(read_half));
        self.writer = Some(stream);
        self.generation += 1;
        Ok(())
    }

    /// Drops the connection so the next command reconnects.
    fn drop_connection(&mut self) {
        self.writer = None;
        self.reader = None;
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.ensure()?;
        let result = self.writer.as_mut().map_or(
            Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "down",
            )),
            |stream| stream.write_all(bytes).and_then(|()| stream.flush()),
        );
        map_io_error(result, self)
    }

    /// Reads one CRLF-terminated line (without the terminator).
    fn read_line(&mut self) -> Result<String, TransportError> {
        self.ensure()?;
        let mut line = Vec::new();
        let result = self.reader.as_mut().map_or(
            Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "down",
            )),
            |reader| {
                reader.read_until(b'\n', &mut line)?;
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                Ok(())
            },
        );
        map_io_error(result, self)?;
        String::from_utf8(line)
            .map_err(|_| TransportError::Protocol("non-UTF8 reply line".to_string()))
    }

    /// Reads exactly `len` bytes plus the trailing CRLF.
    fn read_block(&mut self, len: usize) -> Result<Vec<u8>, TransportError> {
        self.ensure()?;
        let mut data = vec![0u8; len];
        let result = self.reader.as_mut().map_or(
            Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "down",
            )),
            |reader| {
                reader.read_exact(&mut data)?;
                let mut crlf = [0u8; 2];
                reader.read_exact(&mut crlf)?;
                if crlf != *b"\r\n" {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "missing CRLF",
                    ));
                }
                Ok(())
            },
        );
        map_io_error(result, self).map(|()| data)
    }
}

fn map_io_error<T>(
    result: std::io::Result<T>,
    conn: &mut TcpConnection,
) -> Result<T, TransportError> {
    match result {
        Ok(value) => Ok(value),
        Err(err)
            if err.kind() == std::io::ErrorKind::TimedOut
                || err.kind() == std::io::ErrorKind::WouldBlock =>
        {
            conn.drop_connection();
            Err(TransportError::Timeout)
        }
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            Err(TransportError::Protocol(err.to_string()))
        }
        Err(err) => {
            conn.drop_connection();
            Err(TransportError::Network(err.to_string()))
        }
    }
}

/// Memcached ASCII transport (single server).
///
/// Commands: `set <key> 0 0 <len>`, `get <key>`, `delete <key>`.
/// Replies parsed per the ASCII protocol (`VALUE`/`END`/`STORED`/
/// `NOT_STORED`/`DELETED`/`NOT_FOUND`/`ERROR`).
#[derive(Debug)]
pub struct MemcacheTransport {
    conn: Mutex<TcpConnection>,
}

impl MemcacheTransport {
    /// Connects lazily (first command dials, like apr's deferred connect).
    #[must_use]
    pub fn new(spec: &ServerSpec, timeout: Duration) -> MemcacheTransport {
        MemcacheTransport {
            conn: Mutex::new(TcpConnection::new(spec, timeout)),
        }
    }
}

impl RemoteTransport for MemcacheTransport {
    fn transport_get(&self, key: &str) -> Result<Option<Vec<u8>>, TransportError> {
        let mut conn = lock(&self.conn);
        conn.send(format!("get {key}\r\n").as_bytes())?;
        let line = conn.read_line()?;
        if line == "END" {
            return Ok(None);
        }
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 4 || parts[0] != "VALUE" {
            return Err(TransportError::Protocol(format!("bad VALUE line: {line}")));
        }
        let len: usize = parts[3]
            .parse()
            .map_err(|_| TransportError::Protocol(format!("bad length: {line}")))?;
        let data = conn.read_block(len)?;
        let end = conn.read_line()?;
        if end != "END" {
            return Err(TransportError::Protocol(format!("missing END: {end}")));
        }
        Ok(Some(data))
    }

    fn transport_set(
        &self,
        key: &str,
        value: &[u8],
        _ttl_sec: Option<i64>,
    ) -> Result<(), TransportError> {
        let mut conn = lock(&self.conn);
        // flags 0, exptime 0 (never expire; C++ passes 0 TTL likewise).
        conn.send(format!("set {key} 0 0 {}\r\n", value.len()).as_bytes())?;
        conn.send(value)?;
        conn.send(b"\r\n")?;
        match conn.read_line()?.as_str() {
            "STORED" => Ok(()),
            other => Err(TransportError::Protocol(format!("set failed: {other}"))),
        }
    }

    fn transport_delete(&self, key: &str) -> Result<(), TransportError> {
        let mut conn = lock(&self.conn);
        conn.send(format!("delete {key}\r\n").as_bytes())?;
        match conn.read_line()?.as_str() {
            "DELETED" | "NOT_FOUND" => Ok(()),
            other => Err(TransportError::Protocol(format!("delete failed: {other}"))),
        }
    }
}

/// RESP reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RespValue {
    /// Simple string (`+`).
    Simple(String),
    /// Error (`-`).
    Error(String),
    /// Integer (`:`).
    Integer(i64),
    /// Bulk string (`$`, None = null).
    Bulk(Option<Vec<u8>>),
    /// Array (`*`, None = null).
    Array(Option<Vec<RespValue>>),
}

/// Redis RESP transport (single connection).
///
/// Sends commands as arrays; parses full RESP replies. `SELECT`s the
/// database on (re)connect unless the index is [`REDIS_DB_NOT_SET`].
#[derive(Debug)]
pub struct RespTransport {
    conn: Mutex<TcpConnection>,
    selected: Mutex<bool>,
    selected_generation: Mutex<u64>,
    database_index: i32,
}

impl RespTransport {
    /// Connects lazily; selects `database_index` unless [`REDIS_DB_NOT_SET`].
    #[must_use]
    pub fn new(spec: &ServerSpec, timeout: Duration, database_index: i32) -> RespTransport {
        RespTransport {
            conn: Mutex::new(TcpConnection::new(spec, timeout)),
            selected: Mutex::new(false),
            selected_generation: Mutex::new(0),
            database_index,
        }
    }

    fn selected_generation(&self) -> &Mutex<u64> {
        &self.selected_generation
    }

    /// Issues one command, returning the parsed reply.
    pub fn command(&self, args: &[&[u8]]) -> Result<RespValue, TransportError> {
        let mut conn = lock(&self.conn);
        self.ensure_selected(&mut conn)?;
        let mut request = format!("*{}\r\n", args.len()).into_bytes();
        for arg in args {
            request.extend_from_slice(format!("${}\r\n", arg.len()).as_bytes());
            request.extend_from_slice(arg);
            request.extend_from_slice(b"\r\n");
        }
        conn.send(&request)?;
        Self::read_reply(&mut conn)
    }

    fn ensure_selected(&self, conn: &mut TcpConnection) -> Result<(), TransportError> {
        conn.ensure()?;
        // Reconnects invalidate the database selection.
        if conn.generation() != *lock(self.selected_generation()) {
            *lock(self.selected_generation()) = conn.generation();
            *lock(&self.selected) = false;
        }
        if self.database_index == REDIS_DB_NOT_SET || *lock(&self.selected) {
            return Ok(());
        }
        conn.send(
            format!(
                "*2\r\n$6\r\nSELECT\r\n${}\r\n{}\r\n",
                self.database_index.to_string().len(),
                self.database_index
            )
            .as_bytes(),
        )?;
        match Self::read_reply(conn)? {
            RespValue::Simple(_) => {
                *lock(&self.selected) = true;
                Ok(())
            }
            other => Err(TransportError::Protocol(format!(
                "SELECT failed: {other:?}"
            ))),
        }
    }

    fn read_reply(conn: &mut TcpConnection) -> Result<RespValue, TransportError> {
        let line = conn.read_line()?;
        let (kind, rest) = line
            .split_at_checked(1)
            .ok_or_else(|| TransportError::Protocol("empty reply".to_string()))?;
        match kind {
            "+" => Ok(RespValue::Simple(rest.to_string())),
            "-" => Ok(RespValue::Error(rest.to_string())),
            ":" => rest
                .parse::<i64>()
                .map(RespValue::Integer)
                .map_err(|_| TransportError::Protocol(format!("bad int: {line}"))),
            "$" => {
                let len: i64 = rest
                    .parse()
                    .map_err(|_| TransportError::Protocol(format!("bad bulk: {line}")))?;
                if len < 0 {
                    return Ok(RespValue::Bulk(None));
                }
                Ok(RespValue::Bulk(Some(conn.read_block(len as usize)?)))
            }
            "*" => {
                let count: i64 = rest
                    .parse()
                    .map_err(|_| TransportError::Protocol(format!("bad array: {line}")))?;
                if count < 0 {
                    return Ok(RespValue::Array(None));
                }
                let mut items = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    items.push(Self::read_reply(conn)?);
                }
                Ok(RespValue::Array(Some(items)))
            }
            _ => Err(TransportError::Protocol(format!("bad reply: {line}"))),
        }
    }
}

/// Memcached backend (`AprMemCache`, blocking subset).
///
/// Keys hash to 20 chars; values carry the key (`MustEncodeKeyInValue`),
/// verified on read. Servers shard by apr hash mod live servers; dead
/// servers retry after [`MEMCACHED_RETRY_MS`]. Failures record into the
/// error-burst tracker (timeouts additionally counted).
#[derive(Debug)]
pub struct MemcachedCache {
    servers: Vec<MemcacheTransport>,
    dead_until_ms: Mutex<Vec<i64>>,
    hasher: Md5Hasher,
    health: ErrorBurst,
}

/// Dead-server retry delay (apr backs off internally; this port retries
/// after a fixed minute and documents the approximation).
pub const MEMCACHED_RETRY_MS: i64 = 60_000;

impl MemcachedCache {
    /// Creates a backend over live TCP transports.
    #[must_use]
    pub fn new(cluster: &ClusterSpec, timeout: Duration) -> MemcachedCache {
        let servers = cluster
            .servers
            .iter()
            .map(|spec| MemcacheTransport::new(spec, timeout))
            .collect::<Vec<_>>();
        let dead_until_ms = vec![0; servers.len()];
        MemcachedCache {
            servers,
            dead_until_ms: Mutex::new(dead_until_ms),
            hasher: Md5Hasher::with_size(MEMCACHED_HASH_CHARS),
            health: ErrorBurst::new(),
        }
    }

    /// Number of configured servers.
    #[must_use]
    pub fn num_servers(&self) -> usize {
        self.servers.len()
    }

    /// Hashed memcached key for a cache key.
    #[must_use]
    pub fn hashed_key(&self, key: &str) -> String {
        self.hasher.hash(key.as_bytes())
    }

    /// Picks a live server index by apr hash, skipping dead ones.
    fn pick_live(&self, hashed: &str) -> Option<usize> {
        let live: Vec<usize> = {
            let dead = lock(&self.dead_until_ms);
            let now = now_ms();
            (0..self.servers.len())
                .filter(|index| dead[*index] <= now)
                .collect()
        };
        if live.is_empty() {
            return None;
        }
        // Hash over the LIVE list (apr hashes over all servers and walks
        // past dead ones; with all-live lists the two agree exactly, and
        // single-server deployments are unaffected either way).
        let hash = apr_memcache_hash(hashed);
        pick_server(hash, live.len()).map(|slot| live[slot])
    }

    fn mark_dead(&self, index: usize) {
        lock(&self.dead_until_ms)[index] = now_ms() + MEMCACHED_RETRY_MS;
    }

    fn record_error(&self, error: &TransportError) {
        self.health.record(matches!(error, TransportError::Timeout));
    }

    /// Current error burst (test helper).
    #[must_use]
    pub fn burst_size(&self) -> i64 {
        self.health.burst_size()
    }
}

impl Cache for MemcachedCache {
    fn get(&self, key: &str) -> Lookup {
        let hashed = self.hashed_key(key);
        let Some(index) = self.pick_live(&hashed) else {
            return Lookup::miss(KeyState::NetworkError);
        };
        match self.servers[index].transport_get(&hashed) {
            Ok(None) => Lookup::miss(KeyState::NotFound),
            Ok(Some(blob)) => match key_value_codec::decode(&blob) {
                Some((stored_key, value)) if stored_key == key => Lookup::hit(value),
                _ => Lookup::miss(KeyState::NotFound), // key mismatch/corrupt
            },
            Err(error) => {
                let timeout = matches!(error, TransportError::Timeout);
                self.record_error(&error);
                self.mark_dead(index);
                Lookup::miss(if timeout {
                    KeyState::Timeout
                } else {
                    KeyState::NetworkError
                })
            }
        }
    }

    fn put(&self, key: &str, value: &[u8]) {
        let Some(blob) = key_value_codec::encode(key, value) else {
            return; // oversize keys cannot encode; C++ drops likewise.
        };
        self.put_with_key_in_value(key, &blob);
    }

    fn delete(&self, key: &str) {
        let hashed = self.hashed_key(key);
        if let Some(index) = self.pick_live(&hashed) {
            if let Err(error) = self.servers[index].transport_delete(&hashed) {
                self.record_error(&error);
                self.mark_dead(index);
            }
        }
    }

    fn name(&self) -> String {
        "AprMemCache".to_string()
    }

    fn is_healthy(&self) -> bool {
        self.health.healthy()
    }

    fn shutdown(&self) {
        self.health.shutdown();
    }

    fn must_encode_key_in_value(&self) -> bool {
        true
    }

    fn put_with_key_in_value(&self, key: &str, key_and_value: &[u8]) {
        let hashed = self.hashed_key(key);
        if let Some(index) = self.pick_live(&hashed) {
            if let Err(error) = self.servers[index].transport_set(&hashed, key_and_value, None) {
                self.record_error(&error);
                self.mark_dead(index);
            }
        }
    }
}

/// Redis backend (`RedisCache`, blocking subset).
///
/// Single-node plus cluster-slot routing (`CLUSTER SLOTS` map, at most
/// one MOVED/ASK follow per command, 1024-connection cap). `SETEX`
/// applies when a TTL configures; otherwise plain `SET`. Reconnects
/// back off by the configured delay.
#[derive(Debug)]
pub struct RedisCache {
    main: RespTransport,
    cluster: Mutex<RedisCluster>,
    timeout: Duration,
    database_index: i32,
    ttl_sec: i64,
    reconnect_delay_ms: u64,
    redirections: Mutex<u64>,
    slot_fetches: Mutex<u64>,
    shutdown: Mutex<bool>,
    last_failure_ms: Mutex<i64>,
}

#[derive(Debug, Default)]
struct RedisCluster {
    /// Sorted slot ranges to node transports.
    ranges: Vec<SlotRange>,
    /// All known nodes by `host:port` (cap 1024, as in C++).
    nodes: HashMap<String, std::sync::Arc<RespTransport>>,
}

#[derive(Debug)]
struct SlotRange {
    start: u16,
    end: u16,
    node: std::sync::Arc<RespTransport>,
}

impl RedisCache {
    /// Creates a backend (`RedisCache`, standalone unless cluster mode).
    #[must_use]
    pub fn new(
        spec: &ServerSpec,
        timeout: Duration,
        database_index: i32,
        ttl_sec: i64,
        reconnect_delay_ms: u64,
    ) -> RedisCache {
        RedisCache {
            main: RespTransport::new(spec, timeout, database_index),
            cluster: Mutex::new(RedisCluster::default()),
            timeout,
            database_index,
            ttl_sec,
            reconnect_delay_ms,
            redirections: Mutex::new(0),
            slot_fetches: Mutex::new(0),
            shutdown: Mutex::new(false),
            last_failure_ms: Mutex::new(0),
        }
    }

    /// Redirect-follow count (`redis_cluster_redirections`).
    #[must_use]
    pub fn redirections(&self) -> u64 {
        *lock(&self.redirections)
    }

    /// Slot-map fetch count (`redis_cluster_slots_fetches`).
    #[must_use]
    pub fn slot_fetches(&self) -> u64 {
        *lock(&self.slot_fetches)
    }

    /// Looks up the node transport for a slot (cloned out of the map).
    fn slot_node(&self, slot: u16) -> Option<std::sync::Arc<RespTransport>> {
        // Lower-bound over sorted ranges (C++ comparator on end_slot),
        // then verify containment (mappings may be discontinuous).
        lock(&self.cluster)
            .ranges
            .iter()
            .find(|range| slot >= range.start && slot <= range.end)
            .map(|range| range.node.clone())
    }

    /// Ensures a node transport for `host:port` (1024-node cap).
    fn ensure_node(
        &self,
        host: &str,
        port: u16,
    ) -> Result<std::sync::Arc<RespTransport>, TransportError> {
        let name = format!("{host}:{port}");
        let mut cluster = lock(&self.cluster);
        if let Some(node) = cluster.nodes.get(&name) {
            return Ok(node.clone());
        }
        if cluster.nodes.len() >= REDIS_MAX_CLUSTER_CONNECTIONS {
            return Err(TransportError::Network(
                "too many cluster connections".to_string(),
            ));
        }
        let node = std::sync::Arc::new(RespTransport::new(
            &ServerSpec {
                host: host.to_string(),
                port,
            },
            self.timeout,
            self.database_index,
        ));
        cluster.nodes.insert(name, node.clone());
        Ok(node)
    }

    /// Parses a `MOVED`/`ASK` target (`ParseRedirectionError`).
    fn parse_redirection(error: &str) -> Result<(String, u16), TransportError> {
        let bad = || TransportError::Protocol(format!("bad redirect: {error}"));
        let mut tokens = error.split_whitespace();
        let (Some(kind), Some(_slot), Some(addr)) = (tokens.next(), tokens.next(), tokens.next())
        else {
            return Err(bad());
        };
        if kind != "MOVED" && kind != "ASK" || tokens.next().is_some() {
            return Err(bad());
        }
        let Some((host, port)) = addr.split_once(':') else {
            return Err(bad());
        };
        if host.is_empty() {
            return Err(bad());
        }
        let port: i64 = port.parse().map_err(|_| bad())?;
        if !(1..=65535).contains(&port) {
            return Err(bad());
        }
        Ok((host.to_string(), port as u16))
    }

    /// Refreshes the slot map from a node (`FetchClusterSlotMapping`).
    fn refresh_slots(&self, node: &std::sync::Arc<RespTransport>) -> Result<(), TransportError> {
        *lock(&self.slot_fetches) += 1;
        let reply = node.command(&[b"CLUSTER", b"SLOTS"])?;
        let RespValue::Array(Some(entries)) = reply else {
            return Err(TransportError::Protocol(
                "bad CLUSTER SLOTS reply".to_string(),
            ));
        };
        let mut ranges = Vec::new();
        for entry in &entries {
            let RespValue::Array(Some(parts)) = entry else {
                continue;
            };
            if parts.len() < 3 {
                continue;
            }
            let (Some(start), Some(end)) = (as_int(&parts[0]), as_int(&parts[1])) else {
                continue;
            };
            // First nested array is the master: [host, port, id, ...].
            let RespValue::Array(Some(master)) = &parts[2] else {
                continue;
            };
            let (Some(host), Some(port)) =
                (as_bulk_str(&master[0]), master.get(1).and_then(as_int))
            else {
                continue;
            };
            if !(0..16384).contains(&start)
                || !(0..16384).contains(&end)
                || !(1..=65535).contains(&port)
            {
                continue;
            }
            let node = self.ensure_node(host, port as u16)?;
            ranges.push(SlotRange {
                start: start as u16,
                end: end as u16,
                node,
            });
        }
        ranges.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
        lock(&self.cluster).ranges = ranges;
        Ok(())
    }

    /// Routes one command, following at most one redirection
    /// (`RedisCommand` loop with `kMaxRedirections` = 1).
    fn route(&self, key: &str, args: &[&[u8]]) -> Result<RespValue, TransportError> {
        if *lock(&self.shutdown) {
            return Err(TransportError::Network("shut down".to_string()));
        }
        // Reconnection backoff after transport failures.
        let last_failure = *lock(&self.last_failure_ms);
        if last_failure > 0 && now_ms() - last_failure < self.reconnect_delay_ms as i64 {
            return Err(TransportError::Network("reconnect backoff".to_string()));
        }
        let result = self.route_inner(key, args);
        if matches!(
            result,
            Err(TransportError::Network(_)) | Err(TransportError::Timeout)
        ) {
            *lock(&self.last_failure_ms) = now_ms();
        }
        result
    }

    fn route_inner(&self, key: &str, args: &[&[u8]]) -> Result<RespValue, TransportError> {
        let slot = redis_hash_slot(key);
        // First attempt goes to the mapped node, or main when unmapped.
        let mut node = self.slot_node(slot);
        let mut asking = false;
        for _ in 0..=REDIS_MAX_REDIRECTIONS {
            let transport = match &node {
                // Clone the Arc out so I/O never holds the cluster lock.
                Some(node) => node.clone(),
                None => {
                    return self
                        .main
                        .command(args)
                        .and_then(|reply| self.follow_redirect(key, reply, args))
                }
            };
            if asking {
                match transport.command(&[b"ASKING"])? {
                    RespValue::Simple(_) => {}
                    other => {
                        return Err(TransportError::Protocol(format!(
                            "ASKING failed: {other:?}"
                        )))
                    }
                }
            }
            match transport.command(args)? {
                RespValue::Error(error) if error.starts_with("MOVED ") => {
                    *lock(&self.redirections) += 1;
                    asking = false;
                    let (host, port) = Self::parse_redirection(&error)?;
                    node = Some(self.ensure_node(&host, port)?);
                    // MOVED (not ASK) refreshes the map from the node that
                    // redirected us... C++ queries the redirecting node;
                    // query the new node (equivalent once converged).
                    let target = node.clone().unwrap_or_else(|| unreachable!());
                    let _ = self.refresh_slots(&target);
                    continue;
                }
                RespValue::Error(error) if error.starts_with("ASK ") => {
                    *lock(&self.redirections) += 1;
                    asking = true;
                    let (host, port) = Self::parse_redirection(&error)?;
                    node = Some(self.ensure_node(&host, port)?);
                    continue;
                }
                other => return Ok(other),
            }
        }
        Err(TransportError::Protocol(
            "too many redirections".to_string(),
        ))
    }

    /// Follows a standalone redirect from the main connection once.
    fn follow_redirect(
        &self,
        key: &str,
        reply: RespValue,
        args: &[&[u8]],
    ) -> Result<RespValue, TransportError> {
        let error = match &reply {
            RespValue::Error(error) if error.starts_with("MOVED ") || error.starts_with("ASK ") => {
                error.clone()
            }
            _ => return Ok(reply),
        };
        *lock(&self.redirections) += 1;
        let asking = error.starts_with("ASK ");
        let (host, port) = Self::parse_redirection(&error)?;
        let node = self.ensure_node(&host, port)?;
        if asking {
            if !matches!(node.command(&[b"ASKING"])?, RespValue::Simple(_)) {
                return Err(TransportError::Protocol("ASKING failed".to_string()));
            }
        } else {
            let _ = self.refresh_slots(&node);
        }
        // Single follow only (kMaxRedirections = 1 already spent).
        let _ = key;
        node.command(args)
    }
}

fn as_int(value: &RespValue) -> Option<i64> {
    match value {
        RespValue::Integer(n) => Some(*n),
        RespValue::Bulk(Some(bytes)) => std::str::from_utf8(bytes).ok()?.parse().ok(),
        _ => None,
    }
}

fn as_bulk_str(value: &RespValue) -> Option<&str> {
    match value {
        RespValue::Bulk(Some(bytes)) => std::str::from_utf8(bytes).ok(),
        _ => None,
    }
}

impl Cache for RedisCache {
    fn get(&self, key: &str) -> Lookup {
        match self.route(key, &[b"GET", key.as_bytes()]) {
            Ok(RespValue::Bulk(Some(value))) => Lookup::hit(value),
            Ok(RespValue::Bulk(None)) => Lookup::miss(KeyState::NotFound),
            Ok(other) => {
                let _ = other;
                Lookup::miss(KeyState::NetworkError)
            }
            Err(TransportError::Timeout) => Lookup::miss(KeyState::Timeout),
            Err(_) => Lookup::miss(KeyState::NetworkError),
        }
    }

    fn put(&self, key: &str, value: &[u8]) {
        if self.ttl_sec == REDIS_TTL_NOT_SET {
            let _ = self.route(key, &[b"SET", key.as_bytes(), value]);
        } else {
            let ttl = self.ttl_sec.to_string();
            let _ = self.route(key, &[b"SETEX", key.as_bytes(), ttl.as_bytes(), value]);
        }
    }

    fn delete(&self, key: &str) {
        // Deleted-count ignored, as in C++.
        let _ = self.route(key, &[b"DEL", key.as_bytes()]);
    }

    fn name(&self) -> String {
        "RedisCache".to_string()
    }

    fn is_healthy(&self) -> bool {
        !*lock(&self.shutdown)
    }

    fn shutdown(&self) {
        *lock(&self.shutdown) = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn server_spec_parsing() {
        assert_eq!(
            ServerSpec::parse("example.com", 11211).unwrap(),
            ServerSpec {
                host: "example.com".to_string(),
                port: 11211,
            }
        );
        assert_eq!(
            ServerSpec::parse("example.com:11212", 11211).unwrap().port,
            11212
        );
        assert_eq!(
            ServerSpec::parse("", 11211).unwrap_err(),
            "Server host cannot be empty"
        );
        assert_eq!(
            ServerSpec::parse("h:notaport", 11211).unwrap_err(),
            "Port specified is not a valid number: 'notaport'"
        );
        assert_eq!(
            ServerSpec::parse("h:99999", 11211).unwrap_err(),
            "Invalid port: 99999"
        );
        assert_eq!(
            ServerSpec::parse("a:b:c", 11211).unwrap_err(),
            "Expected single server in format <host>[:<port>]"
        );
        // Cluster lists: empty clears; errors join and fail all.
        assert!(ClusterSpec::parse("", 6379).unwrap().is_empty());
        let cluster = ClusterSpec::parse("a:1,b", 6379).unwrap();
        assert_eq!(cluster.servers.len(), 2);
        assert_eq!(cluster.servers[1].port, 6379);
        let error = ClusterSpec::parse(":1,bad:port", 6379).unwrap_err();
        assert!(error.contains("In server ':1'"), "{error}");
        assert!(error.contains("In server 'bad:port'"), "{error}");
    }

    #[test]
    fn hash_goldens() {
        // CRC32-IEEE (zlib) and CRC16-CCITT (init 0) check values.
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc16_ccitt(b"123456789"), 0x31C3);
        assert_eq!(crc32_ieee(b""), 0);
        assert_eq!(crc16_ccitt(b""), 0);
        // apr default hash derives from CRC32.
        assert_eq!(
            apr_memcache_hash("key"),
            (crc32_ieee(b"key") >> 16) & 0x7fff
        );
        // Sharding: 0 maps to slot 1, empty clusters pick nothing.
        assert_eq!(pick_server(0, 4), Some(1));
        assert_eq!(pick_server(9, 4), Some(1));
        assert_eq!(pick_server(9, 0), None);
        // Redis slots honor hash tags.
        assert_eq!(
            redis_hash_slot("{user}.name"),
            redis_hash_slot("{user}.age")
        );
        assert_ne!(redis_hash_slot("a"), redis_hash_slot("b"));
        assert!(redis_hash_slot("anything") < 16384);
    }

    #[test]
    fn memcached_key_hashing() {
        let cluster = ClusterSpec::parse("127.0.0.1:11211", MEMCACHED_DEFAULT_PORT).unwrap();
        let cache = MemcachedCache::new(&cluster, Duration::from_millis(50));
        assert_eq!(cache.num_servers(), 1);
        assert_eq!(cache.hashed_key("http://x/a").len(), MEMCACHED_HASH_CHARS);
        assert_eq!(MEMCACHED_HASH_CHARS, 20);
        // Deterministic.
        assert_eq!(cache.hashed_key("k"), cache.hashed_key("k"));
    }

    #[test]
    fn error_burst_health() {
        let health = ErrorBurst::new();
        assert!(health.healthy());
        for _ in 0..3 {
            health.record(false);
        }
        assert!(health.healthy());
        health.record(true); // 4th error: sick (one timeout counted)
        assert!(!health.healthy());
        assert_eq!(*lock(&health.timeouts), 1);
        health.shutdown();
        assert!(!health.healthy());
    }

    /// Minimal in-process memcached ASCII server for transport tests.
    /// One thread per connection over a shared store.
    fn spawn_memcache_server() -> (ServerSpec, std::thread::JoinHandle<()>) {
        use std::sync::Arc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let store = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(64) {
                let Ok(stream) = stream else { break };
                let store = store.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        let parts: Vec<&str> = line.trim_end().split(' ').collect();
                        match parts.as_slice() {
                            ["get", key] => match lock(&store).get(*key).cloned() {
                                Some(value) => {
                                    let reply = format!("VALUE {key} 0 {}\r\n", value.len());
                                    writer.write_all(reply.as_bytes()).unwrap();
                                    writer.write_all(&value).unwrap();
                                    writer.write_all(b"\r\nEND\r\n").unwrap();
                                }
                                None => {
                                    writer.write_all(b"END\r\n").unwrap();
                                }
                            },
                            ["set", key, _, _, len] => {
                                let len: usize = len.parse().unwrap();
                                let mut data = vec![0u8; len + 2];
                                reader.read_exact(&mut data).unwrap();
                                lock(&store).insert(key.to_string(), data[..len].to_vec());
                                writer.write_all(b"STORED\r\n").unwrap();
                            }
                            ["delete", key] => {
                                if lock(&store).remove(*key).is_some() {
                                    writer.write_all(b"DELETED\r\n").unwrap();
                                } else {
                                    writer.write_all(b"NOT_FOUND\r\n").unwrap();
                                }
                            }
                            _ => {
                                writer.write_all(b"ERROR\r\n").unwrap();
                            }
                        }
                    }
                });
            }
        });
        (
            ServerSpec {
                host: "127.0.0.1".to_string(),
                port,
            },
            handle,
        )
    }

    /// Minimal in-process RESP server for transport tests.
    /// One thread per connection over a shared store.
    fn spawn_redis_server() -> (ServerSpec, std::thread::JoinHandle<()>) {
        use std::sync::Arc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let store = Arc::new(Mutex::new(HashMap::<Vec<u8>, Vec<u8>>::new()));
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(64) {
                let Ok(stream) = stream else { break };
                let store = store.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        let count: usize = match line.trim_end().strip_prefix('*') {
                            Some(count) => count.parse().unwrap_or(0),
                            None => break,
                        };
                        let mut args = Vec::new();
                        for _ in 0..count {
                            let mut header = String::new();
                            reader.read_line(&mut header).unwrap();
                            let len: usize = header
                                .trim_end()
                                .strip_prefix('$')
                                .unwrap()
                                .parse()
                                .unwrap();
                            let mut data = vec![0u8; len + 2];
                            reader.read_exact(&mut data).unwrap();
                            args.push(data[..len].to_vec());
                        }
                        let upper = args
                            .first()
                            .map(|arg| String::from_utf8_lossy(arg).to_ascii_uppercase())
                            .unwrap_or_default();
                        match upper.as_str() {
                            "PING" => {
                                writer.write_all(b"+PONG\r\n").unwrap();
                            }
                            "SELECT" | "ASKING" => {
                                writer.write_all(b"+OK\r\n").unwrap();
                            }
                            "GET" => match lock(&store).get(&args[1]).cloned() {
                                Some(value) => {
                                    writer
                                        .write_all(format!("${}\r\n", value.len()).as_bytes())
                                        .unwrap();
                                    writer.write_all(&value).unwrap();
                                    writer.write_all(b"\r\n").unwrap();
                                }
                                None => {
                                    writer.write_all(b"$-1\r\n").unwrap();
                                }
                            },
                            "SET" => {
                                lock(&store).insert(args[1].clone(), args[2].clone());
                                writer.write_all(b"+OK\r\n").unwrap();
                            }
                            "SETEX" => {
                                lock(&store).insert(args[1].clone(), args[3].clone());
                                writer.write_all(b"+OK\r\n").unwrap();
                            }
                            "DEL" => {
                                let removed = u64::from(lock(&store).remove(&args[1]).is_some());
                                writer
                                    .write_all(format!(":{removed}\r\n").as_bytes())
                                    .unwrap();
                            }
                            _ => {
                                writer.write_all(b"-ERR unknown command\r\n").unwrap();
                            }
                        }
                    }
                });
            }
        });
        (
            ServerSpec {
                host: "127.0.0.1".to_string(),
                port,
            },
            handle,
        )
    }

    #[test]
    fn memcache_transport_round_trip() {
        let (spec, _server) = spawn_memcache_server();
        let transport = MemcacheTransport::new(&spec, Duration::from_secs(5));
        assert_eq!(transport.transport_get("k").unwrap(), None);
        transport.transport_set("k", b"value", None).unwrap();
        assert_eq!(
            transport.transport_get("k").unwrap(),
            Some(b"value".to_vec())
        );
        transport.transport_delete("k").unwrap();
        assert_eq!(transport.transport_get("k").unwrap(), None);
        transport.transport_delete("k").unwrap(); // silent success
    }

    #[test]
    fn memcached_backend_key_in_value() {
        let (spec, _server) = spawn_memcache_server();
        let cluster = ClusterSpec {
            servers: vec![spec],
        };
        let cache = MemcachedCache::new(&cluster, Duration::from_secs(5));
        assert!(cache.must_encode_key_in_value());
        cache.put("http://x/a", b"v");
        assert_eq!(cache.get("http://x/a"), Lookup::hit(b"v".to_vec()));
        // The stored blob carries the key (other keys never collide).
        assert_eq!(cache.get("http://x/other").state, KeyState::NotFound);
        cache.delete("http://x/a");
        assert_eq!(cache.get("http://x/a").state, KeyState::NotFound);
        assert!(cache.is_healthy());
        // Dead servers surface as network errors and record bursts.
        let dead = ClusterSpec::parse("127.0.0.1:1", MEMCACHED_DEFAULT_PORT).unwrap();
        let cache = MemcachedCache::new(&dead, Duration::from_millis(100));
        assert_eq!(cache.get("http://x/a").state, KeyState::NetworkError);
        assert_eq!(cache.burst_size(), 1);
    }

    #[test]
    fn redis_transport_and_backend() {
        let (spec, _server) = spawn_redis_server();
        let transport = RespTransport::new(&spec, Duration::from_secs(5), 0);
        assert_eq!(
            transport.command(&[b"PING"]).unwrap(),
            RespValue::Simple("PONG".to_string())
        );
        let cache = RedisCache::new(&spec, Duration::from_secs(5), 0, -1, 1000);
        cache.put("k", b"v");
        assert_eq!(cache.get("k"), Lookup::hit(b"v".to_vec()));
        assert_eq!(cache.get("missing").state, KeyState::NotFound);
        cache.delete("k");
        assert_eq!(cache.get("k").state, KeyState::NotFound);
        // TTL-configured backend issues SETEX (accepted by the fake).
        let cache = RedisCache::new(&spec, Duration::from_secs(5), 0, 60, 1000);
        cache.put("t", b"v");
        assert_eq!(cache.get("t"), Lookup::hit(b"v".to_vec()));
        // Redirect parsing goldens.
        assert_eq!(
            RedisCache::parse_redirection("MOVED 12182 127.0.0.1:7215").unwrap(),
            ("127.0.0.1".to_string(), 7215)
        );
        assert!(RedisCache::parse_redirection("MOVED bogus").is_err());
        assert!(RedisCache::parse_redirection("MOVED 1 h:99999").is_err());
    }

    /// Live memcached check, gated by `PAGESPEED_TEST_MEMCACHED=host:port`.
    /// Skipped (with a note) when unset.
    #[test]
    fn live_memcached_opt_in() {
        let address = match std::env::var("PAGESPEED_TEST_MEMCACHED") {
            Ok(address) => address,
            Err(_) => {
                eprintln!("skipped: set PAGESPEED_TEST_MEMCACHED=host:port");
                return;
            }
        };
        let spec = ServerSpec::parse(&address, MEMCACHED_DEFAULT_PORT).unwrap();
        let cluster = ClusterSpec {
            servers: vec![spec],
        };
        let cache = MemcachedCache::new(&cluster, Duration::from_secs(5));
        let key = format!("ps-test-{}", std::process::id());
        cache.put(&key, b"live");
        assert_eq!(cache.get(&key), Lookup::hit(b"live".to_vec()));
        cache.delete(&key);
        assert_eq!(cache.get(&key).state, KeyState::NotFound);
    }

    /// Live Redis check, gated by `PAGESPEED_TEST_REDIS=host:port`.
    /// Skipped (with a note) when unset.
    #[test]
    fn live_redis_opt_in() {
        let address = match std::env::var("PAGESPEED_TEST_REDIS") {
            Ok(address) => address,
            Err(_) => {
                eprintln!("skipped: set PAGESPEED_TEST_REDIS=host:port");
                return;
            }
        };
        let spec = ServerSpec::parse(&address, REDIS_DEFAULT_PORT).unwrap();
        let cache = RedisCache::new(&spec, Duration::from_secs(5), 0, REDIS_TTL_NOT_SET, 1000);
        let key = format!("ps-test-{}", std::process::id());
        cache.put(&key, b"live");
        assert_eq!(cache.get(&key), Lookup::hit(b"live".to_vec()));
        cache.delete(&key);
        assert_eq!(cache.get(&key).state, KeyState::NotFound);
    }
}

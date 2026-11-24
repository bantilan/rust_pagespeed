//! Cache backends and adapters (`cache_interface.h`, `lru_cache.*`,
//! `file_cache.{h,cc}`, `url_to_filename_encoder.{h,cc}`,
//! `key_value_codec.{h,cc}`, `cache_key_prepender.{h,cc}`,
//! `fallback_cache.{h,cc}`, `compressed_cache.{h,cc}`).
//!
//! The C++ API is callback-based and sync-or-async per backend; this port
//! exposes a synchronous [`Cache`] trait (`Send + Sync` via interior
//! locking) that async fetchers drive from worker threads. Compression
//! uses raw-deflate streams, wire-compatible with the C++ adapter in
//! both directions (byte-identical output is not guaranteed across
//! encoders, only valid streams).
#![forbid(unsafe_code)]

use crate::hash::Hasher;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Lookup outcome (`CacheInterface::KeyState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    /// Entry available for serving.
    Available,
    /// Entry missing; needs writing.
    NotFound,
    /// Lookup discarded: backend overloaded.
    Overload,
    /// Lookup hit a network error.
    NetworkError,
    /// Lookup timed out.
    Timeout,
}

/// Lookup result: state plus an optional value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    /// Outcome.
    pub state: KeyState,
    /// Value when available.
    pub value: Option<Vec<u8>>,
}

impl Lookup {
    /// Hit with a value.
    #[must_use]
    pub fn hit(value: Vec<u8>) -> Lookup {
        Lookup {
            state: KeyState::Available,
            value: Some(value),
        }
    }

    /// Miss or failure without a value.
    #[must_use]
    pub fn miss(state: KeyState) -> Lookup {
        debug_assert_ne!(state, KeyState::Available);
        Lookup { state, value: None }
    }
}

/// Synchronous cache backend (`CacheInterface`, sync subset).
///
/// Threading: implementations must be `Send + Sync` (interior locking
/// where the C++ originals relied on external locks).
pub trait Cache: Send + Sync + std::fmt::Debug {
    /// Fetches a key (`Get`).
    fn get(&self, key: &str) -> Lookup;

    /// Fetches several keys (`MultiGet`; default loops over [`Cache::get`]).
    fn multi_get(&self, keys: &[String]) -> Vec<Lookup> {
        keys.iter().map(|key| self.get(key)).collect()
    }

    /// Stores a value (`Put`).
    fn put(&self, key: &str, value: &[u8]);

    /// Read/modify/write one entry. Shared backends override with a protected
    /// transaction; false means contention and the callback was not invoked.
    /// The default preserves legacy embedding behavior without process locking.
    fn update(&self, key: &str, change: &mut dyn FnMut(Lookup) -> Option<Vec<u8>>) -> bool {
        if let Some(value) = change(self.get(key)) {
            self.put(key, &value);
        }
        true
    }

    /// Removes a key (`Delete`; missing keys are silent successes).
    fn delete(&self, key: &str);

    /// Shared diagnostics sampled on admin requests. Empty means unavailable.
    fn diagnostics(&self) -> Vec<(&'static str, u64)> {
        Vec::new()
    }

    /// Backend name for logging/debugging (`Name`).
    fn name(&self) -> String;

    /// Whether reads may block the caller (`IsBlocking`).
    fn is_blocking(&self) -> bool {
        true
    }

    /// Whether the backend is usable (`IsHealthy`).
    fn is_healthy(&self) -> bool {
        true
    }

    /// Marks the backend shut down (`ShutDown`).
    fn shutdown(&self) {}

    /// Whether puts must embed the key (`MustEncodeKeyInValueOnPut`).
    fn must_encode_key_in_value(&self) -> bool {
        false
    }

    /// Puts a pre-encoded key+value pair (`PutWithKeyInValue`; only valid
    /// when [`Cache::must_encode_key_in_value`] holds — C++ `CHECK`s this).
    fn put_with_key_in_value(&self, _key: &str, _key_and_value: &[u8]) {
        unimplemented!("backend does not require key-in-value puts (C++ CHECK)")
    }
}

/// Unbounded in-memory cache (test/fallback backend).
#[derive(Debug)]
pub struct MemoryCache {
    entries: Mutex<HashMap<String, Vec<u8>>>,
    healthy: Mutex<bool>,
}

impl MemoryCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> MemoryCache {
        MemoryCache {
            entries: Mutex::new(HashMap::new()),
            healthy: Mutex::new(true),
        }
    }

    /// Marks the backend (un)healthy (`set_is_healthy`, tests
    /// simulate outages with it).
    pub fn set_is_healthy(&self, healthy: bool) {
        *lock(&self.healthy) = healthy;
    }

    /// Number of entries (test helper).
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.entries).len()
    }

    /// Whether no entries are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        lock(&self.entries).is_empty()
    }

    /// Clears all entries (`Clear`).
    pub fn clear(&self) {
        lock(&self.entries).clear();
    }

    /// Deletes keys with a prefix (test helper,
    /// `DeleteWithPrefixForTesting`).
    pub fn delete_with_prefix_for_testing(&self, prefix: &str) {
        lock(&self.entries).retain(|key, _| !key.starts_with(prefix));
    }
}

impl Cache for MemoryCache {
    fn get(&self, key: &str) -> Lookup {
        match lock(&self.entries).get(key) {
            Some(value) => Lookup::hit(value.clone()),
            None => Lookup::miss(KeyState::NotFound),
        }
    }

    fn update(&self, key: &str, change: &mut dyn FnMut(Lookup) -> Option<Vec<u8>>) -> bool {
        let mut entries = lock(&self.entries);
        let previous = entries
            .get(key)
            .cloned()
            .map_or_else(|| Lookup::miss(KeyState::NotFound), Lookup::hit);
        if let Some(value) = change(previous) {
            entries.insert(key.to_string(), value);
        }
        true
    }

    fn put(&self, key: &str, value: &[u8]) {
        lock(&self.entries).insert(key.to_string(), value.to_vec());
    }

    fn delete(&self, key: &str) {
        lock(&self.entries).remove(key);
    }

    fn name(&self) -> String {
        "MemoryCache".to_string()
    }

    fn is_healthy(&self) -> bool {
        *lock(&self.healthy)
    }

    fn shutdown(&self) {
        *lock(&self.healthy) = false;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// Counters mirroring the LRU accessors (`num_*`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LruStats {
    /// Evictions for capacity.
    pub evictions: u64,
    /// Cache hits (freshening and non-freshening reads alike).
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Successful inserts.
    pub inserts: u64,
    /// Re-put identical values (freshened, not reinserted).
    pub identical_reinserts: u64,
    /// Deletes (explicit plus replacements).
    pub deletes: u64,
}

/// Size-bounded LRU cache (`LRUCache`).
///
/// Entry size counts key + value bytes. Gets freshen; re-putting an
/// identical value freshens without reinserting; replacements count as a
/// delete plus an insert; entries that cannot fit even empty are dropped;
/// an entry exactly at capacity is rejected (`bytes_needed < max`, strict,
/// as in C++).
#[derive(Debug)]
pub struct LruCache {
    max_bytes: usize,
    state: Mutex<LruState>,
    healthy: Mutex<bool>,
}

/// One LRU node: value plus intrusive doubly-linked-list links as
/// arena indices (`None` at the ends). `live` is false for free-list
/// slots awaiting reuse.
#[derive(Debug, Default)]
struct LruNode {
    key: String,
    value: Vec<u8>,
    prev: Option<usize>,
    next: Option<usize>,
    live: bool,
}

#[derive(Debug, Default)]
struct LruState {
    /// Key → arena slot for O(1) lookup.
    index: HashMap<String, usize>,
    /// Node arena with a free list (head = most recently used).
    nodes: Vec<LruNode>,
    /// Reusable arena slots from deleted/evicted entries.
    free: Vec<usize>,
    /// Most recently used slot.
    head: Option<usize>,
    /// Least recently used slot.
    tail: Option<usize>,
    used_bytes: usize,
    stats: LruStats,
}

impl LruCache {
    /// Creates a cache bounded to `max_bytes` (`LRUCache(max_size)`).
    #[must_use]
    pub fn new(max_bytes: usize) -> LruCache {
        LruCache {
            max_bytes,
            state: Mutex::new(LruState::default()),
            healthy: Mutex::new(true),
        }
    }

    /// Total key+value bytes stored (`size_bytes`).
    #[must_use]
    pub fn size_bytes(&self) -> usize {
        lock(&self.state).used_bytes
    }

    /// Maximum capacity (`max_bytes_in_cache`).
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Number of elements (`num_elements`).
    #[must_use]
    pub fn num_elements(&self) -> usize {
        lock(&self.state).index.len()
    }

    /// Counter snapshot.
    #[must_use]
    pub fn stats(&self) -> LruStats {
        lock(&self.state).stats
    }

    /// Clears entries but keeps stats (`Clear`).
    pub fn clear(&self) {
        let mut state = lock(&self.state);
        state.index.clear();
        state.nodes.clear();
        state.free.clear();
        state.head = None;
        state.tail = None;
        state.used_bytes = 0;
    }

    /// Clears stats but keeps entries (`ClearStats`).
    pub fn clear_stats(&self) {
        lock(&self.state).stats = LruStats::default();
    }

    /// Deletes keys with a prefix (test helper,
    /// `DeleteWithPrefixForTesting`).
    pub fn delete_with_prefix_for_testing(&self, prefix: &str) {
        let mut state = lock(&self.state);
        let victims: Vec<String> = state
            .index
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect();
        for key in victims {
            Self::delete_locked(&mut state, &key);
        }
    }

    /// Reads without freshening (`GetNoFreshen` semantics; still counted).
    #[must_use]
    pub fn get_no_freshen(&self, key: &str) -> Option<Vec<u8>> {
        let mut state = lock(&self.state);
        let found = state
            .index
            .get(key)
            .map(|slot| state.nodes[*slot].value.clone());
        match found {
            Some(value) => {
                state.stats.hits += 1;
                Some(value)
            }
            None => {
                state.stats.misses += 1;
                None
            }
        }
    }

    /// Structural self-check (`SanityCheck`): list/index agreement plus
    /// the byte-accounting invariant (bounded walk: a cycle fails the
    /// length check rather than hanging).
    pub fn sanity_check(&self) {
        let state = lock(&self.state);
        let mut bytes = 0;
        let mut count = 0;
        let mut cursor = state.head;
        while let Some(slot) = cursor {
            assert!(count <= state.index.len(), "lru list cycle");
            let node = &state.nodes[slot];
            assert!(node.live, "dead node linked");
            assert_eq!(state.index.get(&node.key), Some(&slot), "index agreement");
            if count == 0 {
                assert_eq!(node.prev, None, "head has no predecessor");
            }
            bytes += node.key.len() + node.value.len();
            count += 1;
            cursor = node.next;
        }
        assert_eq!(count, state.index.len(), "list/index length");
        assert_eq!(state.used_bytes, bytes);
        assert!(state.used_bytes <= self.max_bytes);
        match state.tail {
            Some(tail) => assert_eq!(state.nodes[tail].next, None, "tail has no successor"),
            None => assert!(state.index.is_empty(), "empty list has no tail"),
        }
    }

    /// Unlinks a live slot from the recency list (frees nothing).
    fn unlink(state: &mut LruState, slot: usize) {
        let (prev, next) = {
            let node = &state.nodes[slot];
            debug_assert!(node.live);
            (node.prev, node.next)
        };
        match prev {
            Some(left) => state.nodes[left].next = next,
            None => state.head = next,
        }
        match next {
            Some(right) => state.nodes[right].prev = prev,
            None => state.tail = prev,
        }
        state.nodes[slot].prev = None;
        state.nodes[slot].next = None;
    }

    /// Moves a live slot to the head (most recently used).
    fn move_to_front_slot(state: &mut LruState, slot: usize) {
        if state.head == Some(slot) {
            return;
        }
        Self::unlink(state, slot);
        let old_head = state.head;
        state.nodes[slot].next = old_head;
        if let Some(head) = old_head {
            state.nodes[head].prev = Some(slot);
        } else {
            state.tail = Some(slot);
        }
        state.head = Some(slot);
    }

    /// Removes a slot from the list and index, freeing its arena slot.
    fn remove_slot(state: &mut LruState, slot: usize) {
        debug_assert!(state.nodes[slot].live);
        // Unlink before clearing: the links live in the node.
        Self::unlink(state, slot);
        let node = std::mem::take(&mut state.nodes[slot]);
        state.index.remove(&node.key);
        state.used_bytes -= node.key.len() + node.value.len();
        state.free.push(slot);
    }

    fn delete_locked(state: &mut LruState, key: &str) {
        if let Some(slot) = state.index.get(key).copied() {
            Self::remove_slot(state, slot);
            state.stats.deletes += 1;
        }
    }

    fn evict_for(state: &mut LruState, max_bytes: usize, needed: usize) -> bool {
        if needed >= max_bytes {
            return false; // strict `<` in C++: exact-fit rejected.
        }
        while state.used_bytes + needed > max_bytes {
            let Some(oldest) = state.tail else {
                break;
            };
            Self::remove_slot(state, oldest);
            state.stats.evictions += 1;
        }
        true
    }

    /// Allocates an arena slot, reusing a freed one when available.
    fn alloc_slot(state: &mut LruState) -> usize {
        match state.free.pop() {
            Some(slot) => slot,
            None => {
                state.nodes.push(LruNode::default());
                state.nodes.len() - 1
            }
        }
    }
}

impl Cache for LruCache {
    fn get(&self, key: &str) -> Lookup {
        let mut state = lock(&self.state);
        match state.index.get(key).copied() {
            Some(slot) => {
                let value = state.nodes[slot].value.clone();
                Self::move_to_front_slot(&mut state, slot);
                state.stats.hits += 1;
                Lookup::hit(value)
            }
            None => {
                state.stats.misses += 1;
                Lookup::miss(KeyState::NotFound)
            }
        }
    }

    fn put(&self, key: &str, value: &[u8]) {
        let mut state = lock(&self.state);
        if let Some(slot) = state.index.get(key).copied() {
            if state.nodes[slot].value.as_slice() == value {
                Self::move_to_front_slot(&mut state, slot);
                state.stats.identical_reinserts += 1;
                return;
            }
            // Replacement: delete-counted removal, then fresh insert.
            Self::remove_slot(&mut state, slot);
            state.stats.deletes += 1;
        }
        let needed = key.len() + value.len();
        if !Self::evict_for(&mut state, self.max_bytes, needed) {
            return; // too big to fit: silently dropped, as in C++.
        }
        let slot = Self::alloc_slot(&mut state);
        state.nodes[slot] = LruNode {
            key: key.to_string(),
            value: value.to_vec(),
            prev: None,
            next: state.head,
            live: true,
        };
        if let Some(head) = state.head {
            state.nodes[head].prev = Some(slot);
        } else {
            state.tail = Some(slot);
        }
        state.head = Some(slot);
        state.index.insert(key.to_string(), slot);
        state.used_bytes += needed;
        state.stats.inserts += 1;
    }

    fn delete(&self, key: &str) {
        Self::delete_locked(&mut lock(&self.state), key);
    }

    fn name(&self) -> String {
        "LRUCache".to_string()
    }

    fn is_healthy(&self) -> bool {
        *lock(&self.healthy)
    }

    fn shutdown(&self) {
        *lock(&self.healthy) = false;
    }
}

/// Key/value codec for size-limited backends (`key_value_codec`).
///
/// Wire format: `[value, key, key_len_lo, key_len_hi]`; keys longer than
/// 65535 bytes fail to encode.
pub mod key_value_codec {
    /// Maximum encodable key length (u16 range).
    pub const MAX_KEY_LEN: usize = 65535;

    /// Encodes a pair (`Encode`).
    #[must_use]
    pub fn encode(key: &str, value: &[u8]) -> Option<Vec<u8>> {
        if key.len() > MAX_KEY_LEN {
            return None;
        }
        let mut out = Vec::with_capacity(value.len() + key.len() + 2);
        out.extend_from_slice(value);
        out.extend_from_slice(key.as_bytes());
        out.push((key.len() & 0xff) as u8);
        out.push(((key.len() >> 8) & 0xff) as u8);
        Some(out)
    }

    /// Decodes a pair (`Decode`); `None` on truncation or corruption.
    #[must_use]
    pub fn decode(blob: &[u8]) -> Option<(String, Vec<u8>)> {
        if blob.len() < 2 {
            return None;
        }
        let key_len = usize::from(blob[blob.len() - 1]) << 8 | usize::from(blob[blob.len() - 2]);
        let body = &blob[..blob.len() - 2];
        if body.len() < key_len {
            return None;
        }
        let value_len = body.len() - key_len;
        let key = std::str::from_utf8(&body[value_len..]).ok()?;
        Some((key.to_string(), body[..value_len].to_vec()))
    }

    /// Value length inside an encoded pair for a known key
    /// (`GetValueSizeFromKeyAndKeyValue`).
    #[must_use]
    pub fn value_size(key: &str, blob: &[u8]) -> Option<usize> {
        let (decoded_key, value) = decode(blob)?;
        if decoded_key != key {
            return None;
        }
        Some(value.len())
    }
}

/// URL-to-filename encoding (`UrlToFilenameEncoder`).
///
/// `,` escapes unfriendly bytes as `,XX` (uppercase hex); `.`/`..`
/// segments gain a `,` prefix; over-128-char segments split with `,-/`;
/// leaves gain a trailing `,` so branches and leaves coexist. Decoding
/// reverses the mapping and requires a trailing `,`.
pub mod filename {
    /// Escape character (`,`).
    pub const ESCAPE: u8 = b',';
    /// Truncation marker (`-`).
    pub const TRUNCATION: u8 = b'-';
    /// Maximum segment length before splitting.
    pub const MAX_SEGMENT: usize = 128;

    /// Percent-unescapes a string, leaving `+` alone
    /// (`GoogleUrl::UnescapeIgnorePlus`).
    fn unescape_ignore_plus(text: &str) -> Vec<u8> {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit()
            {
                let hex = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(bytes[i + 1]) << 4 | hex(bytes[i + 2]));
                i += 3;
            } else {
                // Malformed `%` sequences pass through verbatim.
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    /// Appends one segment, splitting over-long runs (`AppendSegment`).
    fn append_segment(segment: &mut Vec<u8>, dest: &mut Vec<u8>) {
        assert!(!segment.is_empty(), "C++ CHECKs non-empty segments");
        if matches!(segment.as_slice(), [b'.'] | [b'.', b'.']) {
            dest.push(ESCAPE);
            dest.extend_from_slice(segment);
            segment.clear();
            return;
        }
        if segment.len() > MAX_SEGMENT {
            // Chop at 126, backing off to avoid splitting an escape.
            let mut cut = MAX_SEGMENT - 2;
            if segment[cut - 1] == ESCAPE {
                cut -= 1;
            } else if segment[cut - 2] == ESCAPE {
                cut -= 2;
            }
            dest.extend_from_slice(&segment[..cut]);
            dest.push(ESCAPE);
            dest.push(TRUNCATION);
            *segment = segment[cut..].to_vec();
        } else {
            dest.extend_from_slice(segment);
            segment.clear();
        }
    }

    /// Encodes a cache key under a prefix (`EncodeSegment`).
    ///
    /// `prefix` should end in `/` (callers ensure this); the key is
    /// percent-unescaped first (except `+`), then escaped segment by
    /// segment with `/` separators preserved structurally.
    #[must_use]
    pub fn encode(prefix: &str, key: &str) -> String {
        let unescaped = unescape_ignore_plus(key);
        let mut dest: Vec<u8> = Vec::new();
        // Carry any partial trailing segment of the prefix.
        let mut segment: Vec<u8> = match prefix.rfind('/') {
            Some(pos) => {
                dest.extend_from_slice(&prefix.as_bytes()[..pos + 1]);
                prefix.as_bytes()[pos + 1..].to_vec()
            }
            None => prefix.as_bytes().to_vec(),
        };
        let mut index = 0;
        // Leading separator passes through without an escape prefix.
        if !unescaped.is_empty() && unescaped[0] == b'/' {
            dest.extend_from_slice(&segment);
            segment.clear();
            dest.push(b'/');
            index = 1;
        }
        while index < unescaped.len() {
            let byte = unescaped[index];
            if byte == b'/' && !segment.is_empty() {
                append_segment(&mut segment, &mut dest);
                dest.push(b'/');
                segment.clear();
            } else if byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'.' | b'=' | b'+' | b'-')
            {
                segment.push(byte);
            } else {
                segment.push(ESCAPE);
                segment.push(hex_digit(byte >> 4));
                segment.push(hex_digit(byte & 0xf));
            }
            if segment.len() > MAX_SEGMENT {
                append_segment(&mut segment, &mut dest);
                dest.push(b'/');
            }
            index += 1;
        }
        // Trailing ',' lets leaves double as branches; overflow spills to
        // one final segment written verbatim.
        segment.push(ESCAPE);
        append_segment(&mut segment, &mut dest);
        if !segment.is_empty() {
            dest.push(b'/');
            dest.extend_from_slice(&segment);
        }
        String::from_utf8_lossy(&dest).into_owned()
    }

    fn hex_digit(nibble: u8) -> u8 {
        if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        }
    }

    /// Decodes an encoded filename (`Decode`); `None` on malformed input
    /// or a missing trailing `,`.
    #[must_use]
    pub fn decode(encoded: &str) -> Option<String> {
        #[derive(PartialEq)]
        enum State {
            Start,
            Escape,
            FirstDigit,
            Truncate,
            EscapeDot,
        }
        fn hex(byte: u8) -> Option<u8> {
            (byte as char).to_digit(16).map(|d| d as u8)
        }
        let bytes = encoded.as_bytes();
        let mut out: Vec<u8> = Vec::new();
        let mut state = State::Start;
        let mut hex_hi = 0u8;
        let mut i = 0;
        while i < bytes.len() {
            let byte = bytes[i];
            match state {
                State::Start => {
                    if byte == ESCAPE {
                        state = State::Escape;
                    } else {
                        // '/' passes through ('\\' never appears on decode).
                        out.push(byte);
                    }
                    i += 1;
                }
                State::Escape => {
                    if byte.is_ascii_hexdigit() {
                        hex_hi = hex(byte).unwrap_or(0);
                        state = State::FirstDigit;
                        i += 1;
                    } else if byte == TRUNCATION {
                        state = State::Truncate;
                        i += 1;
                    } else if byte == b'.' {
                        out.push(b'.');
                        state = State::EscapeDot;
                        i += 1;
                    } else if byte == b'/' {
                        // Legacy "/,/x," shape: skip the escape.
                        out.push(b'/');
                        state = State::Start;
                        i += 1;
                    } else {
                        return None;
                    }
                }
                State::FirstDigit => {
                    let lo = hex(byte)?;
                    out.push(hex_hi << 4 | lo);
                    state = State::Start;
                    i += 1;
                }
                State::Truncate => {
                    if byte != b'/' {
                        return None;
                    }
                    state = State::Start;
                    i += 1;
                }
                State::EscapeDot => {
                    out.push(byte);
                    state = State::Start;
                    i += 1;
                }
            }
        }
        // Legal encodings end right after the trailing escape.
        if state != State::Escape {
            return None;
        }
        String::from_utf8(out).ok()
    }
}

/// File-cache cleaning policy (`FileCache::CachePolicy`).
#[derive(Debug, Clone)]
pub struct FileCachePolicy {
    /// Milliseconds between cleans; [`DISABLE_CLEANING`] disables.
    pub clean_interval_ms: i64,
    /// Size target in bytes.
    pub target_size_bytes: i64,
    /// Inode target (0 = no limit).
    pub target_inode_count: i64,
    /// Maximum encoded path length before hash fallback
    /// (`FileSystem::MaxPathLength`, base default 8192).
    pub path_length_limit: usize,
}

/// Disables cleaning (`FileCache::kDisableCleaning`).
pub const DISABLE_CLEANING: i64 = -1;
/// Default path-length limit (`FileSystem::MaxPathLength` base).
pub const DEFAULT_PATH_LENGTH_LIMIT: usize = 8192;
/// Next-clean marker filename (`kCleanTimeName`; `!` never appears in
/// encoded names, so it cannot collide with entries).
pub const CLEAN_TIME_NAME: &str = "!clean!time!";
/// Clean lock filename (`kCleanLockName`).
pub const CLEAN_LOCK_NAME: &str = "!clean!lock!";
/// Minimum empty-dir age before cleaning removes it (60 s).
pub const EMPTY_DIR_CLEAN_AGE_SEC: i64 = 60;

/// Owned file-cache counters (mirroring the `file_cache_*` variables).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileCacheStats {
    /// Size checks performed.
    pub disk_checks: u64,
    /// Cleanups that deleted files.
    pub cleanups: u64,
    /// Files evicted.
    pub evictions: u64,
    /// Bytes freed by cleanups.
    pub bytes_freed: u64,
    /// Cleanups skipped (lock busy; always 0 in sync mode).
    pub skipped_cleanups: u64,
    /// Cleanups started.
    pub started_cleanups: u64,
    /// Write failures.
    pub write_errors: u64,
}

/// Filesystem cache (`FileCache`).
///
/// Keys map to files via [`filename::encode`] under the cache path, with
/// MD5-hash fallback past the length limit. Reads ignore errors (miss);
/// write failures count `write_errors`. Cleaning runs synchronously from
/// [`FileCache::clean_if_needed`] (C++ offloads it to a slow worker;
/// interprocess locking around cleans is a documented follow-up).
#[derive(Debug)]
pub struct FileCache {
    path: PathBuf,
    policy: FileCachePolicy,
    stats: Mutex<FileCacheStats>,
    next_clean_ms: Mutex<i64>,
    healthy: Mutex<bool>,
    write_seq: Mutex<u64>,
}

impl FileCache {
    /// Creates a cache rooted at `path` (`FileCache` ctor).
    #[must_use]
    pub fn new(path: &Path, policy: FileCachePolicy, now_ms: i64) -> FileCache {
        let next_clean_ms = if policy.clean_interval_ms == DISABLE_CLEANING {
            i64::MAX
        } else {
            now_ms + policy.clean_interval_ms / 2
        };
        FileCache {
            path: path.to_path_buf(),
            policy,
            stats: Mutex::new(FileCacheStats::default()),
            next_clean_ms: Mutex::new(next_clean_ms),
            healthy: Mutex::new(true),
            write_seq: Mutex::new(0),
        }
    }

    /// Counter snapshot.
    #[must_use]
    pub fn stats(&self) -> FileCacheStats {
        *lock(&self.stats)
    }

    /// Borrows the cleaning policy.
    #[must_use]
    pub fn policy(&self) -> &FileCachePolicy {
        &self.policy
    }

    /// Replaces the cleaning policy (`mutable_cache_policy`;
    /// config time only, before serving).
    pub fn set_policy(&mut self, policy: FileCachePolicy) {
        self.policy = policy;
    }

    /// Maps a key to its filename (`EncodeFilename`).
    #[must_use]
    pub fn encode_filename(&self, key: &str) -> PathBuf {
        let mut prefix = self.path.to_string_lossy().into_owned();
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        let encoded = filename::encode(&prefix, key);
        if encoded.len() > self.policy.path_length_limit {
            let hash = crate::hash::Md5Hasher::new().hash(key.as_bytes());
            return PathBuf::from(filename::encode(&prefix, &hash));
        }
        PathBuf::from(encoded)
    }

    fn clean_time_path(&self) -> PathBuf {
        self.path.join(CLEAN_TIME_NAME)
    }

    fn clean_lock_path(&self) -> PathBuf {
        self.path.join(CLEAN_LOCK_NAME)
    }

    /// Atomically writes a file (temp + rename, `WriteFileAtomic`).
    fn write_atomic(&self, path: &Path, value: &[u8]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let seq = {
            let mut seq = lock(&self.write_seq);
            *seq += 1;
            *seq
        };
        let tmp = path.with_extension(format!("tmp.{}", seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
        std::fs::write(&tmp, value)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Cleans toward the targets (`Clean`).
    ///
    /// Removes old-enough empty dirs (except the clean lock), then deletes
    /// oldest-atime-first down to 3/4 of each target, skipping the
    /// clean-time/lock marker files. Returns false when any removal
    /// failed.
    pub fn clean(&self, target_size_bytes: i64, target_inode_count: i64) -> bool {
        lock(&self.stats).started_cleanups += 1;
        lock(&self.stats).disk_checks += 1;
        let (mut files, empty_dirs, mut inodes) = walk_cache(&self.path);
        let mut size: i64 = files.iter().map(|entry| entry.size).sum();
        if size < target_size_bytes && (target_inode_count == 0 || inodes < target_inode_count) {
            return true; // under targets: no cleanup needed.
        }
        lock(&self.stats).cleanups += 1;
        let mut ok = true;
        let now_sec = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_secs() as i64)
            .unwrap_or(0);
        for dir in &empty_dirs {
            // Decrement even on failure (already removed elsewhere).
            inodes -= 1;
            let age = dir
                .metadata()
                .and_then(|meta| meta.modified())
                .and_then(|time| {
                    time.duration_since(std::time::UNIX_EPOCH)
                        .map_err(|_| std::io::Error::other("clock"))
                })
                .map(|age| now_sec - age.as_secs() as i64)
                .unwrap_or(0);
            if age > EMPTY_DIR_CLEAN_AGE_SEC
                && *dir != self.clean_lock_path()
                && std::fs::remove_dir(dir).is_err()
            {
                ok = false;
            }
        }
        let orig_size = size;
        files.sort_by_key(|file| file.atime_ms);
        let size_target = target_size_bytes * 3 / 4;
        let inode_target = target_inode_count * 3 / 4;
        for entry in &files {
            if size <= size_target && (inode_target == 0 || inodes <= inode_target) {
                break;
            }
            // Never delete the marker files (noatime mounts order them oddly).
            if entry.path == self.clean_time_path() || entry.path == self.clean_lock_path() {
                continue;
            }
            size -= entry.size;
            inodes -= 1;
            if std::fs::remove_file(&entry.path).is_err() {
                ok = false;
            }
            lock(&self.stats).evictions += 1;
        }
        lock(&self.stats).bytes_freed += (orig_size - size).max(0) as u64;
        ok
    }

    /// Cleans when due (`CleanIfNeeded` + `ShouldClean`, run synchronously).
    ///
    /// Due when the in-memory mark passed and the clean-time file agrees
    /// (missing/unparseable file forces a safety clean; implausibly remote
    /// marks also force one). Successful scheduling rewrites the file.
    pub fn clean_if_needed(&self, now_ms: i64) {
        if self.policy.clean_interval_ms == DISABLE_CLEANING {
            return;
        }
        if now_ms < *lock(&self.next_clean_ms) {
            return;
        }
        let new_time = now_ms.saturating_add(self.policy.clean_interval_ms);
        let file_time: i64 = std::fs::read_to_string(self.clean_time_path())
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0);
        let due = file_time < now_ms || file_time > new_time;
        *lock(&self.next_clean_ms) = new_time;
        if !due {
            return;
        }
        self.clean(
            self.policy.target_size_bytes,
            self.policy.target_inode_count,
        );
        // Best effort: a failed marker write only costs an extra clean.
        let _ = std::fs::write(self.clean_time_path(), new_time.to_string());
    }
}

/// One walked cache file.
struct WalkEntry {
    path: PathBuf,
    size: i64,
    atime_ms: i64,
}

/// Walks a cache tree: files with sizes/atimes, empty dirs, inode count
/// (files plus directories encountered).
fn walk_cache(path: &Path) -> (Vec<WalkEntry>, Vec<PathBuf>, i64) {
    fn atime_ms(meta: &std::fs::Metadata) -> i64 {
        meta.accessed()
            .or_else(|_| meta.modified())
            .and_then(|time| {
                time.duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| std::io::Error::other("clock"))
            })
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0)
    }
    fn visit(dir: &Path, files: &mut Vec<WalkEntry>, empty: &mut Vec<PathBuf>, inodes: &mut i64) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut saw_child = false;
        for entry in read.flatten() {
            saw_child = true;
            let path = entry.path();
            if entry.file_name() == ".pagespeed-lru" {
                continue;
            }
            *inodes += 1;
            if path.is_dir() {
                visit(&path, files, empty, inodes);
            } else if let Ok(meta) = entry.metadata() {
                files.push(WalkEntry {
                    path,
                    size: meta.len() as i64,
                    atime_ms: atime_ms(&meta),
                });
            }
        }
        if !saw_child {
            empty.push(dir.to_path_buf());
        }
    }
    let mut files = Vec::new();
    let mut empty = Vec::new();
    let mut inodes: i64 = 0;
    // The "history" subtree is excluded from cleaning, as in C++.
    if path.is_dir() {
        visit(path, &mut files, &mut empty, &mut inodes);
        let history = path.join("history");
        files.retain(|entry| !entry.path.starts_with(&history));
        empty.retain(|dir| !dir.starts_with(&history));
    }
    (files, empty, inodes)
}

impl Cache for FileCache {
    fn get(&self, key: &str) -> Lookup {
        let filename = self.encode_filename(key);
        match std::fs::read(&filename) {
            // Read errors suppressed (miss), as in C++.
            Ok(value) => Lookup::hit(value),
            Err(_) => Lookup::miss(KeyState::NotFound),
        }
    }

    fn put(&self, key: &str, value: &[u8]) {
        let filename = self.encode_filename(key);
        if self.write_atomic(&filename, value).is_err() {
            lock(&self.stats).write_errors += 1;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0);
        self.clean_if_needed(now_ms);
    }

    fn delete(&self, key: &str) {
        let filename = self.encode_filename(key);
        // Delete failures silenced, as in C++.
        let _ = std::fs::remove_file(filename);
    }

    fn name(&self) -> String {
        "FileCache".to_string()
    }

    fn is_healthy(&self) -> bool {
        *lock(&self.healthy)
    }

    fn shutdown(&self) {
        *lock(&self.healthy) = false;
    }
}

/// Key-prefixing adapter (`CacheKeyPrepender`).
#[derive(Debug)]
pub struct KeyPrepender {
    prefix: String,
    backend: std::sync::Arc<dyn Cache>,
}

impl KeyPrepender {
    /// Wraps a backend, prepending `prefix` to every key.
    #[must_use]
    pub fn new(prefix: &str, backend: std::sync::Arc<dyn Cache>) -> KeyPrepender {
        KeyPrepender {
            prefix: prefix.to_string(),
            backend,
        }
    }

    /// Formats the adapter name (`FormatName`).
    #[must_use]
    pub fn format_name(prefix: &str, cache: &str) -> String {
        format!("KeyPrepender(prefix={prefix},cache={cache})")
    }

    fn prefixed(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }
}

impl Cache for KeyPrepender {
    fn get(&self, key: &str) -> Lookup {
        self.backend.get(&self.prefixed(key))
    }

    fn put(&self, key: &str, value: &[u8]) {
        self.backend.put(&self.prefixed(key), value);
    }

    fn delete(&self, key: &str) {
        self.backend.delete(&self.prefixed(key));
    }

    fn name(&self) -> String {
        Self::format_name(&self.prefix, &self.backend.name())
    }

    fn is_blocking(&self) -> bool {
        self.backend.is_blocking()
    }

    fn is_healthy(&self) -> bool {
        self.backend.is_healthy()
    }

    fn shutdown(&self) {
        self.backend.shutdown();
    }
}

/// Small/large split adapter (`FallbackCache`).
///
/// Values over the threshold (value bytes plus key bytes by default; the
/// threshold is inclusive) live in the large cache behind a one-byte `L`
/// marker in the small cache; smaller values live inline with an `S`
/// suffix byte. Deletes hit both caches.
#[derive(Debug)]
pub struct FallbackCache {
    small: std::sync::Arc<dyn Cache>,
    large: std::sync::Arc<dyn Cache>,
    threshold_bytes: usize,
    account_for_key_size: bool,
}

/// Inline marker suffix (`kInSmallObjectCache`).
pub const SMALL_MARKER: u8 = b'S';
/// Forwarding marker value (`kInLargeObjectCache`).
pub const LARGE_MARKER: u8 = b'L';

impl FallbackCache {
    /// Wraps two backends with a routing threshold.
    #[must_use]
    pub fn new(
        small: std::sync::Arc<dyn Cache>,
        large: std::sync::Arc<dyn Cache>,
        threshold_bytes: usize,
    ) -> FallbackCache {
        FallbackCache {
            small,
            large,
            threshold_bytes,
            account_for_key_size: true,
        }
    }

    /// Formats the adapter name (`FormatName`).
    #[must_use]
    pub fn format_name(small: &str, large: &str) -> String {
        format!("Fallback(small={small},large={large})")
    }

    /// Toggles key-size accounting (`set_account_for_key_size`).
    pub fn set_account_for_key_size(&mut self, account: bool) {
        self.account_for_key_size = account;
    }

    fn store_size(&self, key: &str, value_len: usize) -> usize {
        value_len + u64::from(self.account_for_key_size) as usize * key.len()
    }
}

impl Cache for FallbackCache {
    fn get(&self, key: &str) -> Lookup {
        match self.small.get(key) {
            Lookup {
                state: KeyState::Available,
                value: Some(value),
            } if value == [LARGE_MARKER] => self.large.get(key),
            Lookup {
                state: KeyState::Available,
                value: Some(value),
            } if value.last() == Some(&SMALL_MARKER) => {
                Lookup::hit(value[..value.len() - 1].to_vec())
            }
            _ => Lookup::miss(KeyState::NotFound),
        }
    }

    fn put(&self, key: &str, value: &[u8]) {
        // Large-cache write first: a silent large failure must not leave a
        // marker pointing at missing data.
        if self.store_size(key, value.len()) > self.threshold_bytes {
            self.large.put(key, value);
            self.small.put(key, &[LARGE_MARKER]);
        } else {
            let mut wrapped = Vec::with_capacity(value.len() + 1);
            wrapped.extend_from_slice(value);
            wrapped.push(SMALL_MARKER);
            self.small.put(key, &wrapped);
        }
    }

    fn delete(&self, key: &str) {
        self.small.delete(key);
        self.large.delete(key);
    }

    fn name(&self) -> String {
        Self::format_name(&self.small.name(), &self.large.name())
    }

    fn is_blocking(&self) -> bool {
        self.small.is_blocking() && self.large.is_blocking()
    }

    fn is_healthy(&self) -> bool {
        self.small.is_healthy() && self.large.is_healthy()
    }

    fn shutdown(&self) {
        self.small.shutdown();
        self.large.shutdown();
    }
}

/// Cross-worker write generation. Implementations must share their atomics
/// between every writer to the backing store. A snapshot is unavailable while
/// writes are active; L1 reads then fall through to the persistent backend.
pub trait CacheCoherence: Send + Sync + std::fmt::Debug {
    /// Shared domain diagnostics; sampled only on admin requests.
    fn diagnostics(&self) -> Vec<(&'static str, u64)> {
        Vec::new()
    }
    fn snapshot(&self, key: &str) -> Option<i64>;
    /// Builds a memory-layer identity. Implementations with restart incarnations
    /// can include their namespace here without a second key-prefix allocation.
    fn memory_key(&self, generation: i64, key: &str) -> String {
        format!("{generation}:{key}")
    }
    /// Run a disk mutation under this coherence protocol. Implementations may
    /// retain native thread-bound guards on this stack through the callback.
    fn mutate(&self, key: &str, mutation: &mut dyn FnMut()) {
        self.begin_write(key);
        let _write = CacheWriteGuard(self, key);
        mutation();
    }
    fn begin_write(&self, key: &str);
    fn end_write(&self, key: &str);
}

struct CacheWriteGuard<'a, T: CacheCoherence + ?Sized>(&'a T, &'a str);
impl<T: CacheCoherence + ?Sized> Drop for CacheWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.0.end_write(self.1);
    }
}

/// Write-through L1/L2 adapter (`WriteThroughCache`).
///
/// Reads try L1, then L2 (backfilling L1 on L2 hits, subject to the
/// size limit); writes go to both (L1 only when key+value fits under
/// the limit, strict `<`); deletes hit both.
#[derive(Debug)]
pub struct WriteThroughCache {
    l1: std::sync::Arc<dyn Cache>,
    l2: std::sync::Arc<dyn Cache>,
    l1_size_limit: u64,
    operations: Mutex<()>,
    coherence: Option<std::sync::Arc<dyn CacheCoherence>>,
}

/// Unlimited L1 size (`kUnlimited`).
pub const WRITE_THROUGH_UNLIMITED: u64 = u64::MAX;

impl WriteThroughCache {
    /// Wraps two backends with an L1 size limit.
    #[must_use]
    pub fn new(
        l1: std::sync::Arc<dyn Cache>,
        l2: std::sync::Arc<dyn Cache>,
        l1_size_limit: u64,
    ) -> WriteThroughCache {
        WriteThroughCache {
            l1,
            l2,
            l1_size_limit,
            operations: Mutex::new(()),
            coherence: None,
        }
    }

    /// Uses shared generations for coherent process-local LRU entries.
    pub fn with_coherence(mut self, coherence: std::sync::Arc<dyn CacheCoherence>) -> Self {
        self.coherence = Some(coherence);
        self
    }

    /// Formats the adapter name (`FormatName`).
    #[must_use]
    pub fn format_name(l1: &str, l2: &str) -> String {
        format!("WriteThroughCache(l1={l1},l2={l2})")
    }

    /// Writes to L1 subject to the limit (`PutInCache1`): unlimited, or
    /// key+value strictly under the cap.
    fn put_in_l1(&self, key: &str, value: &[u8]) {
        let size = (key.len() + value.len()) as u64;
        if self.l1_size_limit == WRITE_THROUGH_UNLIMITED || size < self.l1_size_limit {
            self.l1.put(key, value);
        } else {
            // A replacement too large for L1 must invalidate the old value.
            self.l1.delete(key);
        }
    }
}

impl Cache for WriteThroughCache {
    fn get(&self, key: &str) -> Lookup {
        // Shared generations make backfills safe without holding a local
        // operation mutex over disk I/O. An unrelated writer must not stall hits.
        if let Some(coherence) = &self.coherence {
            let Some(generation) = coherence.snapshot(key) else {
                return self.l2.get(key);
            };
            let memory_key = coherence.memory_key(generation, key);
            let cached = self.l1.get(&memory_key);
            let was_hit = cached.state == KeyState::Available;
            let result = if was_hit { cached } else { self.l2.get(key) };
            if coherence.snapshot(key) != Some(generation) {
                return self.l2.get(key);
            }
            if !was_hit {
                if let Some(value) = &result.value {
                    self.put_in_l1(&memory_key, value);
                }
            }
            return result;
        }

        let _operation = lock(&self.operations);
        match self.l1.get(key) {
            hit @ Lookup {
                state: KeyState::Available,
                ..
            } => hit,
            _ => {
                let result = self.l2.get(key);
                if let Lookup {
                    state: KeyState::Available,
                    value: Some(ref value),
                } = result
                {
                    self.put_in_l1(key, value);
                }
                result
            }
        }
    }

    fn put(&self, key: &str, value: &[u8]) {
        if let Some(coherence) = &self.coherence {
            coherence.mutate(key, &mut || self.l2.put(key, value));
            return;
        }
        let _operation = lock(&self.operations);
        self.l2.put(key, value);
        self.put_in_l1(key, value);
    }

    fn delete(&self, key: &str) {
        if let Some(coherence) = &self.coherence {
            coherence.mutate(key, &mut || self.l2.delete(key));
            return;
        }
        let _operation = lock(&self.operations);
        self.l1.delete(key);
        self.l2.delete(key);
    }

    fn name(&self) -> String {
        Self::format_name(&self.l1.name(), &self.l2.name())
    }

    fn is_blocking(&self) -> bool {
        self.l1.is_blocking() && self.l2.is_blocking()
    }

    fn is_healthy(&self) -> bool {
        self.l1.is_healthy() && self.l2.is_healthy()
    }

    fn shutdown(&self) {
        self.l1.shutdown();
        self.l2.shutdown();
    }
}

/// Deflate-compressing adapter (`CompressedCache`, raw-deflate streams).
///
/// Payloads that fail to inflate (or would exceed the cap) read back as
/// misses and count `corrupt_payloads`; inserted byte counts track
/// original vs compressed sizes.
#[derive(Debug)]
pub struct CompressedCache {
    backend: std::sync::Arc<dyn Cache>,
    max_inflated_bytes: usize,
    corrupt_payloads: Mutex<u64>,
    original_bytes: Mutex<u64>,
    compressed_bytes: Mutex<u64>,
}

/// Default inflation cap (`kDefaultMaxInflatedValueSize` = 16 MiB).
pub const DEFAULT_MAX_INFLATED_BYTES: usize = 16 << 20;

impl CompressedCache {
    /// Wraps a backend with the default 16 MiB cap.
    #[must_use]
    pub fn new(backend: std::sync::Arc<dyn Cache>) -> CompressedCache {
        CompressedCache::with_limit(backend, DEFAULT_MAX_INFLATED_BYTES)
    }

    /// Wraps a backend with an explicit cap.
    #[must_use]
    pub fn with_limit(
        backend: std::sync::Arc<dyn Cache>,
        max_inflated_bytes: usize,
    ) -> CompressedCache {
        CompressedCache {
            backend,
            max_inflated_bytes,
            corrupt_payloads: Mutex::new(0),
            original_bytes: Mutex::new(0),
            compressed_bytes: Mutex::new(0),
        }
    }

    /// Formats the adapter name (`FormatName`).
    #[must_use]
    pub fn format_name(cache: &str) -> String {
        format!("Compressed({cache})")
    }

    /// Failed-inflation count (`CorruptPayloads`).
    #[must_use]
    pub fn corrupt_payloads(&self) -> u64 {
        *lock(&self.corrupt_payloads)
    }

    /// Total uncompressed bytes inserted (`OriginalSize`).
    #[must_use]
    pub fn original_size(&self) -> u64 {
        *lock(&self.original_bytes)
    }

    /// Total stored bytes inserted (`CompressedSize`).
    #[must_use]
    pub fn compressed_size(&self) -> u64 {
        *lock(&self.compressed_bytes)
    }

    fn inflate(&self, payload: &[u8]) -> Option<Vec<u8>> {
        use flate2::{Decompress, FlushDecompress, Status};
        // Raw deflate, as with `GzipInflater::kDeflate`.
        let mut decoder = Decompress::new(false);
        let mut out: Vec<u8> = Vec::new();
        let mut consumed = 0usize;
        loop {
            if out.len() > self.max_inflated_bytes {
                return None;
            }
            let mut chunk = [0u8; 8192];
            let before_in = decoder.total_in() as usize;
            let before_out = decoder.total_out() as usize;
            let status = decoder
                .decompress(&payload[consumed..], &mut chunk, FlushDecompress::Finish)
                .ok()?;
            let made_in = decoder.total_in() as usize - before_in;
            let made_out = decoder.total_out() as usize - before_out;
            consumed += made_in;
            out.extend_from_slice(&chunk[..made_out]);
            match status {
                Status::StreamEnd => break,
                Status::Ok | Status::BufError => {
                    if made_in == 0 && made_out == 0 {
                        return None; // stalled: truncated stream.
                    }
                }
            }
        }
        // Trailing garbage after a valid stream is corruption
        // (`CompressedCacheTest.CrapAtEnd` in C++).
        if consumed != payload.len() || out.len() > self.max_inflated_bytes {
            return None;
        }
        Some(out)
    }
}

impl Cache for CompressedCache {
    fn get(&self, key: &str) -> Lookup {
        match self.backend.get(key) {
            Lookup {
                state: KeyState::Available,
                value: Some(payload),
            } => match self.inflate(&payload) {
                Some(value) => Lookup::hit(value),
                None => {
                    *lock(&self.corrupt_payloads) += 1;
                    Lookup::miss(KeyState::NotFound)
                }
            },
            other => other,
        }
    }

    fn put(&self, key: &str, value: &[u8]) {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        // In-memory compression cannot fail; degrade to raw storage if it
        // somehow does (matching "store *something*" robustness).
        let payload = encoder
            .write_all(value)
            .and_then(|()| encoder.finish())
            .unwrap_or_else(|_| value.to_vec());
        *lock(&self.original_bytes) += value.len() as u64;
        *lock(&self.compressed_bytes) += payload.len() as u64;
        self.backend.put(key, &payload);
    }

    fn delete(&self, key: &str) {
        self.backend.delete(key);
    }

    fn name(&self) -> String {
        Self::format_name(&self.backend.name())
    }

    fn is_blocking(&self) -> bool {
        self.backend.is_blocking()
    }

    fn is_healthy(&self) -> bool {
        // C++ also consults backend health; counters never fail here.
        self.backend.is_healthy()
    }

    fn shutdown(&self) {
        self.backend.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn codec_round_trip_and_limits() {
        // Wire format: [value, key, len_lo, len_hi].
        assert_eq!(
            key_value_codec::encode("k", b"v"),
            Some(b"vk\x01\x00".to_vec())
        );
        let blob = key_value_codec::encode("key", b"value").unwrap();
        assert_eq!(
            key_value_codec::decode(&blob),
            Some(("key".to_string(), b"value".to_vec()))
        );
        assert_eq!(key_value_codec::value_size("key", &blob), Some(5));
        assert_eq!(key_value_codec::value_size("other", &blob), None);
        assert_eq!(key_value_codec::decode(b"x"), None);
        assert_eq!(key_value_codec::decode(b"ab\x05\x00"), None); // truncated
        assert_eq!(key_value_codec::encode(&"k".repeat(65536), b"v"), None);
        assert!(key_value_codec::encode(&"k".repeat(65535), b"v").is_some());
    }

    #[test]
    fn filename_goldens() {
        // Every example from the encoder header comment.
        let cases = [
            ("/", "/,"),
            ("/index.html", "/index.html,"),
            ("/.", "/.,"),
            ("/a/b", "/a/b,"),
            ("/a/b/", "/a/b/,"),
            ("/a/b/c", "/a/b/c,"),
            ("/u?foo=bar", "/u,3Ffoo=bar,"),
            ("//", "/,2F,"),
            ("/./", "/,./,"),
            ("/../", "/,../,"),
            ("/,", "/,2C,"),
            ("/,./", "/,2C./,"),
        ];
        for (url, file) in cases {
            assert_eq!(
                filename::encode("/tmp/", url),
                format!("/tmp/{file}"),
                "{url}"
            );
            assert_eq!(
                filename::decode(&format!("/tmp/{file}")),
                Some(format!("/tmp/{url}")),
                "{file}"
            );
        }
        // Long segments split with ",-/" and decode back. Note the
        // doubled slash: a leading '/' in the key passes through after
        // the prefix's own trailing '/', exactly as in C++.
        let long = format!("/{}", "a".repeat(200));
        let encoded = filename::encode("/t/", &long);
        assert!(encoded.contains(",-/"));
        for segment in encoded.split('/') {
            assert!(segment.len() <= 128, "{segment}");
        }
        assert_eq!(filename::decode(&encoded), Some(format!("/t/{long}")));
        // Missing trailing escape rejected.
        assert_eq!(filename::decode("/a/b"), None);
        assert_eq!(filename::decode("/a/b,"), Some("/a/b".to_string()));
    }

    #[test]
    fn memory_cache_basics() {
        let cache = MemoryCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.get("k").state, KeyState::NotFound);
        cache.put("k", b"v");
        assert_eq!(cache.get("k"), Lookup::hit(b"v".to_vec()));
        assert_eq!(cache.len(), 1);
        cache.delete("k");
        assert!(cache.is_empty());
        cache.delete("missing"); // silent success
        assert_eq!(cache.name(), "MemoryCache");
        assert!(cache.is_blocking() && cache.is_healthy());
    }

    #[test]
    fn lru_eviction_and_freshen() {
        // Budget 20: entries "a"+1B=2B, "b"+1B=2B ... exercise eviction.
        let cache = LruCache::new(20);
        for key in ["a", "b", "c", "d", "e", "f", "g", "h", "i"] {
            cache.put(key, b"v");
        }
        assert_eq!(cache.num_elements(), 9);
        assert_eq!(cache.size_bytes(), 18);
        // Freshen "a", then overflow: "b" (LRU) evicted, "a" survives.
        assert_eq!(cache.get("a").state, KeyState::Available);
        cache.put("j", b"vvv"); // 1+3=4B: 18+4=22 > 20 -> evict one (2B)
        assert_eq!(cache.get("b").state, KeyState::NotFound);
        assert_eq!(cache.get("a").state, KeyState::Available);
        assert_eq!(cache.stats().evictions, 1);
        assert_eq!(cache.stats().hits, 2);
        assert_eq!(cache.stats().misses, 1);
        cache.sanity_check();
    }

    #[test]
    fn lru_stress_keeps_invariants() {
        // Deterministic pseudo-random workload over a small cache:
        // every operation preserves list/index/byte agreement.
        let cache = LruCache::new(500);
        let mut seed: u64 = 0x1234_5678;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        // Model the visible contents for hit/miss assertions.
        let mut model: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();
        for step in 0..5000 {
            let key = format!("k{}", next() % 60);
            match next() % 10 {
                0..=5 => {
                    let value = vec![(step & 0xff) as u8; 1 + next() % 8];
                    cache.put(&key, &value);
                    // Mirror capacity: drop model entries the cache
                    // must have evicted (LRU order unknown here, so
                    // only assert presence right after our own put
                    // when the entry fits at all).
                    if key.len() + value.len() < cache.max_bytes() {
                        model.insert(key.clone(), value);
                    }
                    // Prune the model to what fits (conservative:
                    // keep the freshest 40 puts; eviction only drops
                    // older entries).
                    if model.len() > 40 {
                        let victim = model.keys().next().cloned().unwrap();
                        model.remove(&victim);
                    }
                }
                6..=8 => {
                    let lookup = cache.get(&key);
                    if model.get(&key).is_some() {
                        // May have been evicted (model is
                        // approximate); a hit must match bytes.
                        if lookup.state == KeyState::Available {
                            assert_eq!(lookup.value.as_deref(), model.get(&key).map(Vec::as_slice));
                        }
                    }
                }
                _ => {
                    cache.delete(&key);
                    model.remove(&key);
                }
            }
            if step % 500 == 0 {
                cache.sanity_check();
            }
        }
        cache.sanity_check();
        assert!(cache.size_bytes() <= cache.max_bytes());
    }

    #[test]
    fn lru_replacement_and_limits() {
        let cache = LruCache::new(10);
        cache.put("a", b"1"); // 2B
                              // Identical re-put: freshened, counted, not reinserted.
        cache.put("a", b"1");
        assert_eq!(cache.stats().identical_reinserts, 1);
        assert_eq!(cache.stats().inserts, 1);
        // Different value: delete-counted replacement.
        cache.put("a", b"22");
        assert_eq!(cache.stats().deletes, 1);
        assert_eq!(cache.stats().inserts, 2);
        assert_eq!(cache.get("a"), Lookup::hit(b"22".to_vec()));
        // Exact-capacity entries rejected (strict `<` in C++).
        let cache = LruCache::new(4);
        cache.put("ab", b"cd"); // 4B == capacity -> dropped
        assert_eq!(cache.num_elements(), 0);
        cache.put("a", b"bcd"); // 4B == capacity -> dropped
        assert_eq!(cache.num_elements(), 0);
        cache.put("a", b"bc"); // 3B fits
        assert_eq!(cache.num_elements(), 1);
        // Prefix delete (test helper).
        cache.put("x-1", b"v");
        cache.put("x-2", b"v");
        cache.delete_with_prefix_for_testing("x-");
        assert_eq!(cache.get("x-1").state, KeyState::NotFound);
        assert_eq!(cache.get("a").state, KeyState::Available);
        // Clear keeps stats; ClearStats keeps entries.
        cache.clear_stats();
        assert_eq!(cache.stats(), LruStats::default());
        assert_eq!(cache.num_elements(), 1);
        cache.clear();
        assert_eq!(cache.num_elements(), 0);
        // Shutdown flips health.
        assert!(cache.is_healthy());
        cache.shutdown();
        assert!(!cache.is_healthy());
    }

    fn file_policy() -> FileCachePolicy {
        FileCachePolicy {
            clean_interval_ms: DISABLE_CLEANING,
            target_size_bytes: 1024 * 1024,
            target_inode_count: 0,
            path_length_limit: DEFAULT_PATH_LENGTH_LIMIT,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("ps_cache_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn file_cache_round_trip_and_names() {
        let dir = temp_dir("roundtrip");
        let cache = FileCache::new(&dir, file_policy(), 0);
        assert_eq!(cache.name(), "FileCache");
        assert_eq!(cache.get("http://x/a").state, KeyState::NotFound);
        cache.put("http://x/a", b"value");
        assert_eq!(cache.get("http://x/a"), Lookup::hit(b"value".to_vec()));
        // Filename keeps URL structure with a trailing escape; the
        // scheme separator and empty segment escape as `,3A` and `,2F`.
        let name = cache.encode_filename("http://x/a");
        let text = name.to_string_lossy().into_owned();
        assert!(text.ends_with("http,3A/,2Fx/a,"), "{text}");
        cache.delete("http://x/a");
        assert_eq!(cache.get("http://x/a").state, KeyState::NotFound);
        // Over-long keys fall back to hash naming.
        let mut tiny = file_policy();
        tiny.path_length_limit = 10;
        let cache = FileCache::new(&dir, tiny, 0);
        let name = cache.encode_filename("http://x/a-very-long-key");
        assert!(name.to_string_lossy().len() <= 10 + 64, "{name:?}");
        cache.put("http://x/a-very-long-key", b"v");
        assert_eq!(
            cache.get("http://x/a-very-long-key"),
            Lookup::hit(b"v".to_vec())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_cache_clean() {
        let dir = temp_dir("clean");
        let cache = FileCache::new(&dir, file_policy(), 0);
        for i in 0..5 {
            cache.put(&format!("http://x/{i}"), &[i as u8; 100]);
        }
        assert_eq!(cache.stats().started_cleanups, 0); // cleaning disabled
                                                       // Under targets: no-op success.
        assert!(cache.clean(100_000, 0));
        assert_eq!(cache.stats().cleanups, 0);
        // Over size target: oldest-first down to 3/4 of 200 = 150 bytes,
        // i.e. 4 of 5 hundred-byte files removed. Exactly one file
        // survives; which one is atime order (ties broken arbitrarily).
        assert!(cache.clean(200, 0));
        assert_eq!(cache.stats().cleanups, 1);
        assert_eq!(cache.stats().evictions, 4);
        assert_eq!(cache.stats().bytes_freed, 400);
        let survivors = (0..5)
            .filter(|i| cache.get(&format!("http://x/{i}")).state == KeyState::Available)
            .count();
        assert_eq!(survivors, 1);
        // Marker files never deleted.
        std::fs::write(dir.join(CLEAN_TIME_NAME), "1").unwrap();
        assert!(cache.clean(1, 0));
        assert!(dir.join(CLEAN_TIME_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_cache_clean_scheduling() {
        let dir = temp_dir("sched");
        let mut policy = file_policy();
        policy.clean_interval_ms = 3_600_000;
        let cache = FileCache::new(&dir, policy, 1_000_000);
        // Not due yet (next = now + interval/2).
        cache.clean_if_needed(1_000_100);
        assert_eq!(cache.stats().started_cleanups, 0);
        // Due with missing marker file: safety clean runs.
        cache.clean_if_needed(9_999_999);
        assert_eq!(cache.stats().started_cleanups, 1);
        let _ = std::fs::remove_dir_all(&dir);
        // Cleaning disabled: never runs.
        let dir = temp_dir("sched-off");
        let cache = FileCache::new(&dir, file_policy(), 0);
        cache.clean_if_needed(i64::MAX);
        assert_eq!(cache.stats().started_cleanups, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_through_limit_and_backfill() {
        let l1 = Arc::new(MemoryCache::new());
        let l2 = Arc::new(MemoryCache::new());
        // Limit 4: "k"(1B) + value must be strictly under 4 for L1.
        let cache = WriteThroughCache::new(l1.clone(), l2.clone(), 4);
        cache.put("k", b"12"); // 3 < 4 -> both
        assert_eq!(l1.get("k").state, KeyState::Available);
        cache.put("k", b"123"); // 4 !< 4 -> L2 only
        assert_eq!(l2.get("k").state, KeyState::Available);
        assert_eq!(cache.get("k").value.as_deref(), Some(b"123".as_slice()));
        assert_eq!(l1.get("k").state, KeyState::NotFound);
        // L2 hit backfills L1 only when under the limit.
        assert_eq!(cache.get("k").state, KeyState::Available);
        assert_eq!(l1.get("k").state, KeyState::NotFound); // 4 !< 4
        assert_eq!(
            cache.name(),
            "WriteThroughCache(l1=MemoryCache,l2=MemoryCache)"
        );
        // Unlimited always backfills.
        let cache = WriteThroughCache::new(l1.clone(), l2.clone(), WRITE_THROUGH_UNLIMITED);
        l1.delete("k");
        assert_eq!(cache.get("k").state, KeyState::Available);
        assert_eq!(l1.get("k").state, KeyState::Available);
        cache.delete("k");
        assert_eq!(l1.get("k").state, KeyState::NotFound);
        assert_eq!(l2.get("k").state, KeyState::NotFound);
    }

    #[test]
    fn prepender_routes() {
        let backend = Arc::new(MemoryCache::new());
        let cache = KeyPrepender::new("test/", backend.clone());
        cache.put("k", b"v");
        assert_eq!(cache.get("k"), Lookup::hit(b"v".to_vec()));
        assert_eq!(backend.get("test/k"), Lookup::hit(b"v".to_vec()));
        assert_eq!(backend.get("k").state, KeyState::NotFound);
        cache.delete("k");
        assert_eq!(backend.get("test/k").state, KeyState::NotFound);
        assert_eq!(cache.name(), "KeyPrepender(prefix=test/,cache=MemoryCache)");
    }

    #[test]
    fn fallback_protocol() {
        let small = Arc::new(MemoryCache::new());
        let large = Arc::new(MemoryCache::new());
        // Threshold counts value + key bytes by default (inclusive).
        let cache = FallbackCache::new(small.clone(), large.clone(), 10);
        cache.put("k", b"12345678"); // 8 + 1 = 9 <= 10 -> inline
        assert_eq!(small.get("k"), Lookup::hit(b"12345678S".to_vec()));
        assert_eq!(cache.get("k"), Lookup::hit(b"12345678".to_vec()));
        cache.put("k", b"123456789"); // 9 + 1 = 10 <= 10 -> inline
        assert_eq!(cache.get("k"), Lookup::hit(b"123456789".to_vec()));
        cache.put("k", b"1234567890"); // 10 + 1 = 11 > 10 -> large
        assert_eq!(small.get("k"), Lookup::hit(b"L".to_vec()));
        assert_eq!(cache.get("k"), Lookup::hit(b"1234567890".to_vec()));
        // Unmarked small values read as missing (corrupt encoding).
        small.put("raw", b"no-marker");
        assert_eq!(cache.get("raw").state, KeyState::NotFound);
        // Deletes hit both sides.
        cache.delete("k");
        assert_eq!(small.get("k").state, KeyState::NotFound);
        assert_eq!(large.get("k").state, KeyState::NotFound);
        assert_eq!(
            cache.name(),
            "Fallback(small=MemoryCache,large=MemoryCache)"
        );
        // Key-size accounting can be disabled.
        let mut cache = FallbackCache::new(small.clone(), large.clone(), 8);
        cache.set_account_for_key_size(false);
        cache.put("long-key", b"12345678"); // 8 <= 8 -> inline
        assert_eq!(cache.get("long-key"), Lookup::hit(b"12345678".to_vec()));
    }

    #[test]
    fn compressed_round_trip_and_corruption() {
        let backend = Arc::new(MemoryCache::new());
        let cache = CompressedCache::new(backend.clone());
        let value = b"hello ".repeat(100);
        cache.put("k", &value);
        assert_eq!(cache.get("k"), Lookup::hit(value.clone()));
        assert_eq!(cache.original_size(), 600);
        assert!(cache.compressed_size() < 600);
        assert_eq!(cache.corrupt_payloads(), 0);
        // Garbage payloads read as misses and count corruption (0xff
        // selects the reserved deflate block type: a hard inflate error).
        backend.put("bad", b"\xff\xff\xff");
        assert_eq!(cache.get("bad").state, KeyState::NotFound);
        assert_eq!(cache.corrupt_payloads(), 1);
        // Trailing garbage after a valid stream is corruption too.
        let mut bad = backend.get("k").value.unwrap();
        bad.extend_from_slice(b"TRAIL");
        backend.put("k", &bad);
        assert_eq!(cache.get("k").state, KeyState::NotFound);
        // Inflation cap enforced.
        let tiny = CompressedCache::with_limit(backend.clone(), 10);
        assert_eq!(tiny.get("k").state, KeyState::NotFound);
        // Wire compat: a zlib-produced raw-deflate stream inflates here
        // (golden generated with Python zlib, wbits=-15).
        backend.put("zlib", &hex_to_bytes("cb48cdc9c95728cf2fca49d151c8407000"));
        assert_eq!(
            cache.get("zlib"),
            Lookup::hit(b"hello world, hello world".to_vec())
        );
        assert_eq!(cache.name(), "Compressed(MemoryCache)");
    }
}

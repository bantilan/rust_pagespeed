//! Cross-process shared counters over a memory-mapped file.
//!
//! Backs `ps-stats` counters across nginx worker processes: every
//! worker maps the same file (`MAP_SHARED`) and
//! bumps slots with lock-free atomics, so the admin console reads global
//! totals instead of per-worker slices. Reads and writes take `&self`
//! and never block on I/O after open.
//!
//! Layout (little-endian, 8-aligned): a 64-byte header (magic, table
//! hash, fixed/dynamic counts, an allocation spinlock, a bump cursor),
//! one 8-byte fixed slot per pre-registered name (positional, in the
//! order given at creation), then spare dynamic slots of
//! `{ name: [u8; 64], hash: u64, value: i64 }` for names first seen at
//! runtime. The filename embeds an FNV-1a hash of the fixed-name list,
//! so binaries with different registries never share a file; the
//! creator unlinks stale sibling files best-effort.
//!
//! Open protocol: exactly one process wins creation (`O_EXCL`) or the
//! re-initialization flock and writes the header, publishing with a
//! release store to `magic`; everyone else spin-waits on an acquire
//! load of `magic` (fresh files read zero until published). The winner
//! holds its flock for life (workers inherit it across fork), so taking
//! the flock uncontended proves no live users: a full stop/start resets
//! counters like C++'s recreated segments, while reloads and graceful
//! upgrades keep counting, and `nginx -t` against a live server maps
//! read-shared and changes nothing. Dynamic allocation
//! takes a test-and-set spinlock (allocation happens once per name per
//! process, so contention is negligible): re-attach returns the existing
//! slot, so concurrent first uses of one name never split counts.
//!
//! # Safety boundary
//!
//! This is the one sibling crate without `#![forbid(unsafe_code)]`:
//! atomic operations on shared memory require `AtomicI64::from_ptr`,
//! which is an `unsafe fn`. Counter unsafe code is confined to mapping
//! and [`SharedCounters::atomic_at`]. The Linux [`region`] module additionally
//! encapsulates process-shared pthread mutexes and guarded raw payload access.
//! Both public APIs are safe. Counter soundness rests
//! on: slots are 8-aligned and bounds-checked; the mapping is never
//! resized or remapped after open; no `&mut` to shared bytes ever exists
//! (header initialization takes exclusive `&mut self` before sharing,
//! everything afterwards goes through atomics); the header is
//! published-before-read via the magic handshake; dynamic names publish
//! words-before-hash under the spinlock. `Send`/`Sync` hold because
//! cross-thread/process sharing happens only through atomics and the
//! spinlock protocol.
//!
//! Counters and up/down gauges share one slot namespace (a name must
//! never register as both kinds). Histograms and timed variables stay
//! per-process by design.
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

/// Bounded shared byte caching (Linux robust pthread mutexes).
#[cfg(target_os = "linux")]
pub mod byte_cache;

/// Recoverable fixed-size shared regions (Linux robust pthread mutexes).
#[cfg(target_os = "linux")]
pub mod region;

use fs2::FileExt;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

/// File magic (`"psshmct1"`).
const MAGIC: u64 = 0x3174636d_68737370;
/// Header length in bytes.
const HEADER_LEN: usize = 64;
/// Dynamic slot name capacity (NUL-padded).
const NAME_LEN: usize = 64;
/// Dynamic slot stride: name + hash + value.
const DYN_STRIDE: usize = NAME_LEN + 8 + 8;
/// Spare dynamic slots (runtime-first-seen names).
const DYN_SLOTS: usize = 128;
/// Header offsets.
const OFF_MAGIC: usize = 0;
const OFF_HASH: usize = 8;
const OFF_FIXED: usize = 16;
const OFF_DYN: usize = 20;
const OFF_LOCK: usize = 24;
/// Magic-wait budget: 200 × 10ms.
const MAGIC_RETRIES: usize = 200;
/// Spinlock budget before yielding, then giving up.
const SPIN_YIELD_AFTER: usize = 1000;
const SPIN_GIVE_UP_AFTER: usize = 1_000_000;

/// FNV-1a 64 over the fixed-name list (NUL-separated).
#[must_use]
pub fn table_hash(names: &[String]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for name in names {
        for byte in name.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Shared-counter filename for a table hash.
#[must_use]
pub fn file_name(table_hash: u64) -> String {
    format!("shared_counters_{table_hash:016x}.dat")
}

/// Open failure.
#[derive(Debug)]
pub enum OpenError {
    /// Filesystem or mapping error.
    Io(io::Error),
    /// Header never published (creator died mid-write).
    Unpublished,
    /// File holds a different table (hash or count mismatch).
    TableMismatch,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Io(error) => write!(f, "shared counters I/O: {error}"),
            OpenError::Unpublished => write!(f, "shared counters header never published"),
            OpenError::TableMismatch => write!(f, "shared counters table mismatch"),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<io::Error> for OpenError {
    fn from(error: io::Error) -> Self {
        OpenError::Io(error)
    }
}

/// Memory-mapped shared counters (`Send + Sync` via atomics only).
pub struct SharedCounters {
    /// The mapping (never resized after open).
    map: memmap2::MmapMut,
    /// Fixed names in slot order (index = slot).
    fixed_names: Vec<String>,
    /// Fixed slot count (mirrors the header).
    fixed_count: usize,
    /// Lifetime file description. Legacy open retains an exclusive lock only
    /// for its initializer; open_coherent retains a shared lock in every
    /// mapping. Inherited descriptions stay live across worker fork/exit.
    _locked_file: Option<std::fs::File>,
    /// Whether every mapping retains a shared lifetime lock (v2 protocol).
    coherent_lifetime: bool,
}

impl SharedCounters {
    /// Opens (or creates) the counter file under `dir` for `fixed_names`.
    ///
    /// The filename embeds the table hash, so different registries never
    /// collide. Creation and fresh-start resets happen under an exclusive
    /// flock. This legacy protocol retains lifetime ownership only in its
    /// initializer; new cache coherence must use `open_coherent` instead.
    pub fn open(dir: &Path, fixed_names: &[String]) -> Result<Self, OpenError> {
        let hash = table_hash(fixed_names);
        let path = dir.join(file_name(hash));
        let total = HEADER_LEN + fixed_names.len() * 8 + DYN_SLOTS * DYN_STRIDE;
        // Fast path: create exclusively (we become the initializer).
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                file.set_len(total as u64)?;
                // Interlock against a racing opener that won the flock
                // between our create and this call (it initializes; we
                // fall through to mapping below).
                if file.try_lock_exclusive().is_ok() {
                    let map = Self::map_file(&file, total)?;
                    let mut this = SharedCounters {
                        map,
                        fixed_names: fixed_names.to_vec(),
                        fixed_count: fixed_names.len(),
                        _locked_file: None,
                        coherent_lifetime: false,
                    };
                    this.write_header(hash, fixed_names.len());
                    this.atomic_at(OFF_MAGIC)
                        .store(MAGIC as i64, Ordering::Release);
                    Self::unlink_stale_siblings(dir, &path);
                    let this = SharedCounters {
                        _locked_file: Some(file),
                        ..this
                    };
                    return Ok(this);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(OpenError::Io(error)),
        }
        // The file exists: re-initialize only when no live users hold it
        // (a full stop/start resets counters, like C++'s recreated
        // segments; reloads and graceful upgrades keep counting).
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        if file.try_lock_exclusive().is_ok() {
            file.set_len(total as u64)?;
            let map = Self::map_file(&file, total)?;
            let mut this = SharedCounters {
                map,
                fixed_names: fixed_names.to_vec(),
                fixed_count: fixed_names.len(),
                _locked_file: None,
                coherent_lifetime: false,
            };
            this.write_header(hash, fixed_names.len());
            this.atomic_at(OFF_MAGIC)
                .store(MAGIC as i64, Ordering::Release);
            Self::unlink_stale_siblings(dir, &path);
            let this = SharedCounters {
                _locked_file: Some(file),
                ..this
            };
            return Ok(this);
        }
        // Live users exist (or `nginx -t` against them): map read-shared.
        let map = Self::map_file(&file, total)?;
        let this = SharedCounters {
            map,
            fixed_names: fixed_names.to_vec(),
            fixed_count: fixed_names.len(),
            _locked_file: None,
            coherent_lifetime: false,
        };
        this.wait_for_header(hash, fixed_names.len())?;
        Ok(this)
    }

    /// Opens counters with recoverable lifetime participation and an explicit
    /// incarnation namespace. Unlike legacy `open`, every attached reader holds
    /// a shared lock, so losing the creator cannot permit a premature reset.
    /// The separate v2 directory protects live files from legacy sibling cleanup.
    /// Lock acquisition is bounded and occurs only at startup/attachment.
    pub fn open_coherent(dir: &Path, fixed_names: &[String]) -> Result<Self, OpenError> {
        let directory = dir.join("coherent_counters_v2");
        std::fs::create_dir_all(&directory)?;
        let hash = table_hash(fixed_names);
        let base = directory.join(format!("{hash:016x}"));
        let open_lock = |suffix: &str| -> io::Result<std::fs::File> {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(base.with_extension(suffix))
        };
        let init = open_lock("init")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            match init.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "counter initialization busy",
                        )
                        .into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(error) => return Err(error.into()),
            }
        }
        // Independent open descriptions are essential: inherited flock handles
        // from fork are fine, but independent processes must all retain a lock.
        let life = open_lock("live")?;
        let fresh = match life.try_lock_exclusive() {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                FileExt::try_lock_shared(&life)?;
                false
            }
            Err(error) => return Err(error.into()),
        };
        let total = fixed_names
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(HEADER_LEN + DYN_SLOTS * DYN_STRIDE))
            .filter(|_| fixed_names.len() <= u32::MAX as usize)
            .ok_or(OpenError::TableMismatch)?;
        let file = open_lock("dat")?;
        if fresh {
            // Exclusive lifetime ownership proves no live mapping exists.
            file.set_len(total as u64)?;
        } else if file.metadata()?.len() != total as u64 {
            return Err(OpenError::TableMismatch);
        }
        let mut this = SharedCounters {
            map: Self::map_file(&file, total)?,
            fixed_names: fixed_names.to_vec(),
            fixed_count: fixed_names.len(),
            _locked_file: Some(life),
            coherent_lifetime: true,
        };
        if fresh {
            this.write_header(hash, fixed_names.len());
            // std's randomized hashing seeds are process-randomized without
            // adding an entropy-source dependency. Two independently keyed
            // states give the restart namespace 128 bits.
            use std::hash::{BuildHasher, Hasher};
            for offset in [48, 56] {
                let mut random = std::collections::hash_map::RandomState::new().build_hasher();
                random.write_u64(hash);
                random.write_u32(std::process::id());
                random.write_u128(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos(),
                );
                this.atomic_at(offset)
                    .store(random.finish() as i64, Ordering::Release);
            }
            this.atomic_at(OFF_MAGIC)
                .store(MAGIC as i64, Ordering::Release);
            // Downgrade while still holding init's exclusive lock. Other opens
            // cannot inspect or change lifetime state during the conversion.
            FileExt::try_lock_shared(this._locked_file.as_ref().unwrap())?;
        } else {
            this.wait_for_header(hash, fixed_names.len())?;
        }
        // Closing init releases startup serialization. The shared life handle
        // stays in this object and survives fork until its final process closes.
        drop(init);
        Ok(this)
    }

    /// Restart namespace, available only with the v2 lifetime protocol. Pair
    /// this with generation counters when keying persistent shared/L1 entries.
    pub fn incarnation(&self) -> Option<u128> {
        self.coherent_lifetime.then(|| {
            let low = self.atomic_at(48).load(Ordering::Acquire) as u64;
            let high = self.atomic_at(56).load(Ordering::Acquire) as u64;
            u128::from(low) | (u128::from(high) << 64)
        })
    }

    /// Maps `total` bytes (the file must already be that long).
    fn map_file(file: &std::fs::File, total: usize) -> Result<memmap2::MmapMut, OpenError> {
        let len = file.metadata()?.len() as usize;
        if len < total {
            return Err(OpenError::TableMismatch);
        }
        // SAFETY: the file is already at its final length and is never
        // truncated afterwards; every `&mut` lent from the mapping is
        // scoped and externally synchronized (creation flock, magic
        // handshake, allocation spinlock), so no two exist concurrently.
        unsafe { memmap2::MmapOptions::new().len(total).map_mut(file) }.map_err(OpenError::Io)
    }

    /// Writes the header and zeroes the body (creator/reinitializer only,
    /// the caller must publish `magic` afterwards). Exclusive `&mut`
    /// access: the mapping is not shared yet at this point.
    fn write_header(&mut self, hash: u64, fixed: usize) {
        let bytes = self.bytes_mut_for_init();
        bytes[OFF_HASH..OFF_HASH + 8].copy_from_slice(&hash.to_le_bytes());
        bytes[OFF_FIXED..OFF_FIXED + 4].copy_from_slice(&(fixed as u32).to_le_bytes());
        bytes[OFF_DYN..OFF_DYN + 4].copy_from_slice(&(DYN_SLOTS as u32).to_le_bytes());
        // Fixed slots, lock, cursor, and dynamic slots stay zeroed (fresh
        // pages read zero; re-initialization explicitly clears them).
        bytes[OFF_LOCK..].fill(0);
    }

    /// Mutable bytes for header initialization (creator only, before any
    /// other process can validate the magic).
    fn bytes_mut_for_init(&mut self) -> &mut [u8] {
        &mut self.map[..]
    }

    /// Spins until the header publishes, then validates the table.
    fn wait_for_header(&self, hash: u64, fixed: usize) -> Result<(), OpenError> {
        for _ in 0..MAGIC_RETRIES {
            #[allow(clippy::cast_sign_loss)]
            let magic = MAGIC as i64;
            if self.atomic_at(OFF_MAGIC).load(Ordering::Acquire) == magic {
                let bytes = self.map.as_ref();
                let file_hash =
                    u64::from_le_bytes(bytes[OFF_HASH..OFF_HASH + 8].try_into().unwrap());
                let file_fixed =
                    u32::from_le_bytes(bytes[OFF_FIXED..OFF_FIXED + 4].try_into().unwrap())
                        as usize;
                if file_hash == hash && file_fixed == fixed {
                    return Ok(());
                }
                return Err(OpenError::TableMismatch);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(OpenError::Unpublished)
    }

    /// Removes sibling counter files from older tables (best-effort;
    /// live processes hold their own mappings, so unlinking is safe).
    fn unlink_stale_siblings(dir: &Path, ours: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(text) = name.to_str() else { continue };
            if text.starts_with("shared_counters_") && entry.path() != ours {
                std::fs::remove_file(entry.path()).ok();
            }
        }
    }

    /// Fixed names in slot order.
    #[must_use]
    pub fn fixed_names(&self) -> &[String] {
        &self.fixed_names
    }

    /// Total slot count (fixed plus dynamic).
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.fixed_count + DYN_SLOTS
    }

    /// Attaches a runtime-first-seen name, returning its global slot
    /// (`None` for empty/overlong names or a full table).
    pub fn attach_dynamic(&self, name: &str) -> Option<usize> {
        if name.is_empty() || name.len() > NAME_LEN - 1 {
            return None;
        }
        let wanted = fnv1a_str(name);
        let _guard = self.lock_alloc()?;
        // Re-attach wins over double allocation (concurrent first uses
        // of one name share the slot instead of splitting counts).
        for index in 0..DYN_SLOTS {
            let (stored_hash, stored_name) = self.dynamic_key(index);
            if stored_hash == wanted && stored_name.as_deref() == Some(name) {
                return Some(self.fixed_count + index);
            }
        }
        let cursor = self.dynamic_cursor();
        if cursor >= DYN_SLOTS {
            return None;
        }
        self.write_dynamic_name(cursor, name, wanted);
        self.set_dynamic_cursor(cursor + 1);
        Some(self.fixed_count + cursor)
    }

    /// Reads a slot.
    #[must_use]
    pub fn get(&self, slot: usize) -> i64 {
        self.atomic_at(self.slot_offset(slot))
            .load(Ordering::Relaxed)
    }

    /// Reads a coordination slot in the global sequentially consistent order.
    /// Use this for protocols spanning several slots; ordinary statistics only
    /// need the cheaper relaxed operations.
    #[must_use]
    pub fn coordination_get(&self, slot: usize) -> i64 {
        self.atomic_at(self.slot_offset(slot))
            .load(Ordering::SeqCst)
    }

    /// Updates a coordination slot in the same order as `coordination_get`.
    pub fn coordination_add(&self, slot: usize, delta: i64) -> i64 {
        self.atomic_at(self.slot_offset(slot))
            .fetch_add(delta, Ordering::SeqCst)
            .wrapping_add(delta)
    }

    /// Adds a delta, returning the new value.
    pub fn add(&self, slot: usize, delta: i64) -> i64 {
        self.atomic_at(self.slot_offset(slot))
            .fetch_add(delta, Ordering::Relaxed)
            + delta
    }

    /// Sets a slot.
    pub fn set(&self, slot: usize, value: i64) {
        self.atomic_at(self.slot_offset(slot))
            .store(value, Ordering::Relaxed);
    }

    /// Atomically sets a slot, returning the previous value.
    pub fn swap(&self, slot: usize, value: i64) -> i64 {
        self.atomic_at(self.slot_offset(slot))
            .swap(value, Ordering::AcqRel)
    }

    /// Resets a slot to zero.
    pub fn clear(&self, slot: usize) {
        self.set(slot, 0);
    }

    /// Byte offset of a global slot's value.
    fn slot_offset(&self, slot: usize) -> usize {
        assert!(slot < self.slot_count(), "slot out of range");
        if slot < self.fixed_count {
            HEADER_LEN + slot * 8
        } else {
            let index = slot - self.fixed_count;
            HEADER_LEN + self.fixed_count * 8 + index * DYN_STRIDE + NAME_LEN + 8
        }
    }

    /// Takes the allocation spinlock (test-and-set; `None` on exhaustion,
    /// in which case the caller falls back to a local counter).
    fn lock_alloc(&self) -> Option<SpinGuard<'_>> {
        let lock = self.atomic_at(OFF_LOCK);
        for spin in 0..SPIN_GIVE_UP_AFTER {
            if lock
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return Some(SpinGuard { lock });
            }
            if spin >= SPIN_YIELD_AFTER {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        }
        None
    }

    /// Reads a dynamic slot's (hash, name), if published.
    fn dynamic_key(&self, index: usize) -> (u64, Option<String>) {
        let base = HEADER_LEN + self.fixed_count * 8 + index * DYN_STRIDE;
        #[allow(clippy::cast_sign_loss)]
        let hash = self.atomic_at(base + NAME_LEN).load(Ordering::Acquire) as u64;
        if hash == 0 {
            return (0, None);
        }
        let bytes = &self.map.as_ref()[base..base + NAME_LEN];
        let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(NAME_LEN);
        let name = String::from_utf8(bytes[..end].to_vec()).ok();
        (hash, name)
    }

    /// Reads the allocation cursor (caller holds the spinlock).
    fn dynamic_cursor(&self) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cursor = self.atomic_at(OFF_LOCK + 8).load(Ordering::Relaxed) as usize;
        cursor
    }

    /// Writes the allocation cursor (caller holds the spinlock).
    fn set_dynamic_cursor(&self, cursor: usize) {
        #[allow(clippy::cast_possible_wrap)]
        let cursor = cursor as i64;
        self.atomic_at(OFF_LOCK + 8)
            .store(cursor, Ordering::Relaxed);
    }

    /// Publishes a dynamic name (caller holds the spinlock): name words
    /// first, hash last with release ordering. Readers load the hash
    /// with acquire ordering before touching the name bytes.
    fn write_dynamic_name(&self, index: usize, name: &str, hash: u64) {
        let base = HEADER_LEN + self.fixed_count * 8 + index * DYN_STRIDE;
        let mut padded = [0u8; NAME_LEN];
        padded[..name.len()].copy_from_slice(name.as_bytes());
        let (words, _) = padded.as_chunks::<8>();
        for (word, chunk) in words.iter().enumerate() {
            let value = i64::from_le_bytes(*chunk);
            self.atomic_at(base + word * 8)
                .store(value, Ordering::Relaxed);
        }
        self.atomic_at(base + NAME_LEN)
            .store(hash as i64, Ordering::Release);
    }

    /// Views slot bytes as an atomic (the crate's single `unsafe` site).
    fn atomic_at(&self, byte_offset: usize) -> &AtomicI64 {
        assert!(
            byte_offset.is_multiple_of(8),
            "slot offsets stay 8-aligned by construction"
        );
        assert!(
            byte_offset + 8 <= self.map.len(),
            "slot offsets stay in-bounds by construction"
        );
        // SAFETY: `byte_offset` is 8-aligned and in-bounds of a mapping
        // that is never resized or remapped, so the address always names
        // 8 initialized bytes (fresh files read zero; existing files hold
        // prior values). No `&mut` to these bytes can exist: header
        // initialization finishes before any process validates the magic,
        // dynamic publication runs under the spinlock with bytes-before-
        // hash ordering, and every other access in this process goes
        // through this function. Concurrent `&AtomicI64` aliases (across
        // threads and processes) are sound: atomics synchronize through
        // the shared mapping.
        unsafe { AtomicI64::from_ptr(self.map.as_ptr().add(byte_offset) as *mut i64) }
    }
}

// SAFETY: sharing crosses threads/processes only through atomics (see
// `atomic_at`) and the spinlock protocol; the mapping outlives all uses.
unsafe impl Send for SharedCounters {}
// SAFETY: same as `Send`.
unsafe impl Sync for SharedCounters {}

impl std::fmt::Debug for SharedCounters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedCounters")
            .field("fixed_count", &self.fixed_count)
            .finish_non_exhaustive()
    }
}

/// Spinlock guard (releases on drop).
struct SpinGuard<'a> {
    lock: &'a AtomicI64,
}

impl Drop for SpinGuard<'_> {
    fn drop(&mut self) {
        self.lock.store(0, Ordering::Release);
    }
}

/// FNV-1a 64 of one string (dynamic name hashes).
fn fnv1a_str(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // Zero is the empty-slot sentinel, so it can never be a real hash.
    if hash == 0 {
        1
    } else {
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Scratch directory for counter files.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            let id = COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
            let dir = std::env::temp_dir().join(format!("psshm-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch { dir }
        }

        fn open(&self, fixed: &[&str]) -> SharedCounters {
            let fixed: Vec<String> = fixed.iter().map(ToString::to_string).collect();
            SharedCounters::open(&self.dir, &fixed).expect("open counters")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    fn coherent(scratch: &Scratch) -> SharedCounters {
        SharedCounters::open_coherent(&scratch.dir, &["shared".to_string()]).unwrap()
    }

    #[test]
    fn coherent_readers_prevent_creator_drop_reset() {
        let scratch = Scratch::new();
        let creator = coherent(&scratch);
        let incarnation = creator.incarnation().unwrap();
        creator.add(0, 41);
        let reader = coherent(&scratch);
        assert_eq!(Some(incarnation), reader.incarnation());
        drop(creator);
        let newcomer = coherent(&scratch);
        assert_eq!(41, reader.get(0));
        assert_eq!(41, newcomer.get(0));
        assert_eq!(Some(incarnation), newcomer.incarnation());
        drop(reader);
        newcomer.add(0, 1);
        assert_eq!(42, coherent(&scratch).get(0));
    }

    #[test]
    fn coherent_full_stop_resets_with_a_new_namespace() {
        let scratch = Scratch::new();
        let first = coherent(&scratch);
        let before = first.incarnation().unwrap();
        first.add(0, 99);
        drop(first);
        let second = coherent(&scratch);
        assert_eq!(0, second.get(0));
        assert_ne!(Some(before), second.incarnation());
        assert!(scratch.open(&["legacy"]).incarnation().is_none());
    }

    #[test]
    fn legacy_sibling_cleanup_cannot_unlink_live_coherent_tables() {
        let scratch = Scratch::new();
        let first = coherent(&scratch);
        first.add(0, 7);
        let _legacy = scratch.open(&["legacy"]);
        let _other_legacy = scratch.open(&["other"]);
        let second = coherent(&scratch);
        assert_eq!(7, second.get(0));
        assert_eq!(first.incarnation(), second.incarnation());
    }

    #[test]
    fn coherent_subprocess_entry() {
        let Some(path) = std::env::var_os("PS_COHERENT_TEST_DIRECTORY") else {
            return;
        };
        let directory = Path::new(&path);
        let counters = SharedCounters::open_coherent(directory, &["shared".to_string()]).unwrap();
        counters.add(0, 1);
        std::fs::write(directory.join("ready"), b"ready").unwrap();
        loop {
            std::thread::park_timeout(std::time::Duration::from_secs(1));
        }
    }

    #[test]
    fn surviving_process_keeps_incarnation_after_creator_exits() {
        let scratch = Scratch::new();
        let creator = coherent(&scratch);
        creator.add(0, 40);
        let incarnation = creator.incarnation();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::coherent_subprocess_entry", "--nocapture"])
            .env("PS_COHERENT_TEST_DIRECTORY", &scratch.dir)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !scratch.dir.join("ready").exists() {
            assert!(child.try_wait().unwrap().is_none());
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("coherent child timeout");
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        drop(creator);
        let newcomer = coherent(&scratch);
        let observed = newcomer.get(0);
        let observed_incarnation = newcomer.incarnation();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(41, observed);
        assert_eq!(incarnation, observed_incarnation);
        assert_eq!(41, coherent(&scratch).get(0));
    }

    #[test]
    fn fixed_slots_add_get_set_swap() {
        let scratch = Scratch::new();
        let counters = scratch.open(&["alpha", "beta"]);
        assert_eq!(0, counters.get(0));
        assert_eq!(5, counters.add(0, 5));
        assert_eq!(5, counters.get(0));
        assert_eq!(8, counters.add(0, 3));
        counters.set(1, 42);
        assert_eq!(42, counters.get(1));
        assert_eq!(42, counters.swap(1, 7));
        assert_eq!(7, counters.get(1));
        counters.clear(0);
        assert_eq!(0, counters.get(0));
    }

    #[test]
    fn second_mapping_shares_values() {
        // Two independent opens of one file behave as two processes.
        let scratch = Scratch::new();
        let first = scratch.open(&["shared"]);
        let second = scratch.open(&["shared"]);
        first.add(0, 10);
        assert_eq!(10, second.get(0));
        second.add(0, 5);
        assert_eq!(15, first.get(0));
    }

    #[test]
    fn dynamic_attach_reattaches_and_persists() {
        let scratch = Scratch::new();
        let first = scratch.open(&["fixed"]);
        let slot = first.attach_dynamic("runtime_seen").expect("allocates");
        assert_eq!(1, slot);
        first.add(slot, 3);
        // Re-attach returns the same slot (no split counts).
        assert_eq!(slot, first.attach_dynamic("runtime_seen").unwrap());
        // A fresh mapping sees the published name and value.
        let second = scratch.open(&["fixed"]);
        assert_eq!(slot, second.attach_dynamic("runtime_seen").unwrap());
        assert_eq!(3, second.get(slot));
    }

    #[test]
    fn dynamic_rejects_empty_and_overlong() {
        let scratch = Scratch::new();
        let counters = scratch.open(&[]);
        assert_eq!(None, counters.attach_dynamic(""));
        assert_eq!(None, counters.attach_dynamic(&"x".repeat(64)));
        assert!(counters.attach_dynamic(&"x".repeat(63)).is_some());
    }

    #[test]
    fn different_tables_get_different_files() {
        let scratch = Scratch::new();
        let one = scratch.open(&["a"]);
        one.add(0, 11);
        let two = scratch.open(&["a", "b"]);
        // A different registry never aliases another table's slots.
        assert_eq!(0, two.get(0));
        assert_eq!(0, two.get(1));
        assert_eq!(11, one.get(0));
    }

    #[test]
    fn concurrent_adds_do_not_lose_updates() {
        use std::sync::Arc;
        let scratch = Scratch::new();
        let counters = Arc::new(scratch.open(&["hot"]));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let counters = Arc::clone(&counters);
            threads.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    counters.add(0, 1);
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(8000, counters.get(0));
    }

    #[test]
    fn concurrent_dynamic_attach_shares_one_slot() {
        use std::sync::Arc;
        let scratch = Scratch::new();
        let counters = Arc::new(scratch.open(&[]));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let counters = Arc::clone(&counters);
            threads.push(std::thread::spawn(move || {
                counters.attach_dynamic("raced").expect("allocates")
            }));
        }
        let mut slots = Vec::new();
        for thread in threads {
            slots.push(thread.join().unwrap());
        }
        assert!(slots.iter().all(|slot| *slot == slots[0]));
    }

    #[test]
    fn table_hash_is_stable_and_sensitive() {
        let names = vec!["a".to_string(), "b".to_string()];
        assert_eq!(table_hash(&names), table_hash(&names));
        assert_ne!(
            table_hash(&names),
            table_hash(&["a".to_string(), "c".to_string()])
        );
        assert_ne!(
            table_hash(&names),
            table_hash(&["b".to_string(), "a".to_string()])
        );
    }
}

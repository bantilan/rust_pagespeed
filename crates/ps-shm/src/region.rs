//! Linux shared regions with bounded, owner-death-recoverable shard access.
//!
//! Every payload access holds its process-shared robust pthread mutex. A dead
//! holder invalidates the entire shard before another caller receives access.
//! No mapped Rust references escape the guard, and guards cannot cross threads.
//! Existing mappings are never resized, reset, or unlinked by this module.

use fs2::FileExt;
use memmap2::{MmapOptions, MmapRaw};
use std::fs::{File, OpenOptions};
use std::io;
use std::marker::PhantomData;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

const MAGIC: u64 = 0x3130_3052_4853_5350; // PSSHR001, little endian
const VERSION: u64 = 2; // v2 reserves the atomic epoch and binds pthread state to a boot.
const HEADER: usize = 128;
const ALIGN: usize = 64;
const MAX_BYTES: usize = 1024 * 1024 * 1024;
const INIT_TIMEOUT: Duration = Duration::from_millis(500);

/// Checked allocation and immutable on-disk layout for a shared region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionLayout {
    shards: usize,
    payload_bytes: usize,
    stride: usize,
    total: usize,
}

impl RegionLayout {
    /// Allocates 1–256 shards, each with a nonempty bounded payload. Mutexes
    /// and payloads are cache-line aligned; total mapping size is at most 1 GiB.
    pub fn new(shards: usize, payload_bytes: usize) -> io::Result<Self> {
        if shards == 0 || shards > 256 || payload_bytes == 0 {
            return Err(invalid("invalid shard count or empty payload"));
        }
        if size_of::<libc::pthread_mutex_t>() > ALIGN - 8
            || align_of::<libc::pthread_mutex_t>() > ALIGN
        {
            return Err(invalid("unsupported pthread mutex ABI"));
        }
        let stride = payload_bytes
            .checked_add(ALIGN * 2 - 1)
            .map(|n| n / ALIGN * ALIGN)
            .ok_or_else(|| invalid("region size overflow"))?;
        let total = stride
            .checked_mul(shards)
            .and_then(|n| n.checked_add(HEADER))
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| invalid("region exceeds 1 GiB limit"))?;
        Ok(Self {
            shards,
            payload_bytes,
            stride,
            total,
        })
    }

    /// Number of independently locked shards.
    pub fn shards(self) -> usize {
        self.shards
    }
    /// Usable payload bytes per shard, excluding synchronization/header overhead.
    pub fn payload_bytes(self) -> usize {
        self.payload_bytes
    }
    /// Entire fixed mapping size.
    pub fn mapped_bytes(self) -> usize {
        self.total
    }

    fn header(self) -> [u8; HEADER] {
        let mut header = [0u8; HEADER];
        for (i, value) in [
            MAGIC,
            VERSION,
            self.total as u64,
            self.shards as u64,
            self.payload_bytes as u64,
            self.stride as u64,
            size_of::<libc::pthread_mutex_t>() as u64,
            align_of::<libc::pthread_mutex_t>() as u64,
            u64::from(cfg!(target_endian = "little")),
            usize::BITS as u64,
        ]
        .into_iter()
        .enumerate()
        {
            header[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        // Distinguish native pthread layouts across libc implementations even
        // when their public sizes happen to match on the same machine.
        let libc_abi: u64 = if cfg!(target_env = "gnu") {
            1
        } else if cfg!(target_env = "musl") {
            2
        } else {
            0
        };
        header[120..128].copy_from_slice(&libc_abi.to_le_bytes());
        header
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Fixed-size mmap with recoverable shard mutexes. Safe to share across threads
/// and independent processes; it owns no process-local pointer in mapped bytes.
///
/// Store the file in an application-owned directory. External code must not
/// truncate, rewrite, or replace its contents while processes use the mapping.
/// Layout-incompatible callers must select a different filename, not resize it.
pub struct SharedRegion {
    map: MmapRaw,
    layout: RegionLayout,
    header: [u8; HEADER],
}

impl std::fmt::Debug for SharedRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedRegion")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl SharedRegion {
    /// Creates or attaches without truncation. Initialization/header validation
    /// uses a bounded flock only at open; data operations never acquire a file
    /// lock or perform filesystem reads. Invalid existing files fail closed.
    pub fn open(path: &Path, layout: RegionLayout) -> io::Result<Self> {
        // Persistent pthread owner words must never be reused after a machine
        // reboot: the old kernel cannot deliver owner-death recovery then.
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let boot = boot.trim();
        if boot.len() != 36 {
            return Err(invalid("invalid Linux boot identity"));
        }
        let mut header = layout.header();
        header[80..116].copy_from_slice(boot.as_bytes());
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let started = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= INIT_TIMEOUT {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "shared-region initialization busy",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error),
            }
        }
        let _init_lock = InitLock(&file);
        let new = file.metadata()?.len() == 0;
        if new {
            file.set_len(layout.total as u64)?;
        } else if file.metadata()?.len() != layout.total as u64 {
            return Err(invalid("shared-region file length mismatch"));
        }
        let map = MmapOptions::new().len(layout.total).map_raw(&file)?;
        let region = Self {
            map,
            layout,
            header,
        };
        if new {
            region.initialize()?;
        } else {
            // SAFETY: mapped/header bounds checked; initialization is serialized
            // by flock. The published header is immutable for this file's life.
            let header = unsafe { std::slice::from_raw_parts(region.map.as_ptr(), HEADER) };
            if header != region.header {
                return Err(invalid("shared-region header/ABI mismatch"));
            }
        }
        Ok(region)
    }

    /// Validated immutable allocation layout.
    pub fn layout(&self) -> RegionLayout {
        self.layout
    }

    fn initialize(&self) -> io::Result<()> {
        // SAFETY: initialization flock excludes every opener; no successful
        // opener exists until the header is published. Each pthread object is
        // initialized in its own aligned, non-overlapping cache line exactly once.
        unsafe {
            let mut attrs = std::mem::MaybeUninit::<libc::pthread_mutexattr_t>::uninit();
            pthread_result(libc::pthread_mutexattr_init(attrs.as_mut_ptr()))?;
            let attrs = attrs.assume_init_mut();
            let init: io::Result<()> = (|| {
                pthread_result(libc::pthread_mutexattr_setpshared(
                    attrs,
                    libc::PTHREAD_PROCESS_SHARED,
                ))?;
                pthread_result(libc::pthread_mutexattr_setrobust(
                    attrs,
                    libc::PTHREAD_MUTEX_ROBUST,
                ))?;
                for shard in 0..self.layout.shards {
                    pthread_result(libc::pthread_mutex_init(self.mutex(shard), attrs))?;
                }
                Ok(())
            })();
            libc::pthread_mutexattr_destroy(attrs);
            init?;
            // Publish last. Failed/aborted initialization leaves a nonempty
            // invalid file, which future openers reject rather than resetting
            // potentially live pthread objects. Operators can replace it offline.
            std::ptr::copy_nonoverlapping(self.header.as_ptr(), self.map.as_mut_ptr(), HEADER);
        }
        Ok(())
    }

    fn mutex(&self, shard: usize) -> *mut libc::pthread_mutex_t {
        // Caller checks shard bounds before this raw pointer is dereferenced.
        self.map
            .as_mut_ptr()
            .wrapping_add(HEADER + shard * self.layout.stride)
            .cast()
    }

    fn payload(&self, shard: usize) -> *mut u8 {
        self.map
            .as_mut_ptr()
            .wrapping_add(HEADER + shard * self.layout.stride + ALIGN)
    }

    // Mutation failures must invalidate cached payloads even while a holder
    // owns the shard mutex. This atomic occupies reserved bytes after the native
    // pthread object, disjoint from every guarded payload slice.
    fn epoch_cell(&self, shard: usize) -> io::Result<&std::sync::atomic::AtomicU64> {
        if shard >= self.layout.shards {
            return Err(invalid("shard index out of bounds"));
        }
        let offset = HEADER + shard * self.layout.stride + ALIGN - 8;
        // SAFETY: 8-aligned in the reserved tail of the mutex cache line; the
        // checked native mutex is at most 56 bytes. Only atomic access occurs
        // here, and payload references start at byte 64. Mapping outlives self.
        Ok(unsafe {
            std::sync::atomic::AtomicU64::from_ptr(self.map.as_mut_ptr().add(offset).cast())
        })
    }

    pub(crate) fn epoch(&self, shard: usize) -> io::Result<u64> {
        Ok(self
            .epoch_cell(shard)?
            .load(std::sync::atomic::Ordering::SeqCst))
    }

    pub(crate) fn invalidate(&self, shard: usize) -> io::Result<()> {
        // Never wrap: exhausted epochs disable admissions/hits until a new
        // segment is selected. No old epoch may become current again.
        // Keep support for toolchains predating AtomicU64::try_update.
        #[allow(deprecated)]
        let _ = self.epoch_cell(shard)?.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |value| value.checked_add(1),
        );
        Ok(())
    }

    /// Attempts once, without waiting: `None` means another holder is active.
    /// After owner death the payload is cleared before access is granted. A
    /// corrupt/unrecoverable mutex returns an error for higher-layer fallback.
    pub fn try_lock(&self, shard: usize) -> io::Result<Option<ShardGuard<'_>>> {
        if shard >= self.layout.shards {
            return Err(invalid("shard index out of bounds"));
        }
        // SAFETY: mutex was initialized before publication, remains aligned and
        // mapped for self's lifetime, and is shared only through pthread APIs.
        let code = unsafe { libc::pthread_mutex_trylock(self.mutex(shard)) };
        if code == libc::EBUSY {
            return Ok(None);
        }
        if code != 0 && code != libc::EOWNERDEAD {
            return Err(io::Error::from_raw_os_error(code));
        }
        let mut guard = ShardGuard {
            region: self,
            shard,
            recovered: code == libc::EOWNERDEAD,
            _not_send: PhantomData,
        };
        if guard.recovered {
            guard.bytes_mut().fill(0);
            // SAFETY: this thread owns the inconsistent mutex and repaired the
            // entire protected payload. No copying happens outside this lock.
            let code = unsafe { libc::pthread_mutex_consistent(self.mutex(shard)) };
            pthread_result(code)?;
        }
        Ok(Some(guard))
    }
}

fn pthread_result(code: libc::c_int) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}

struct InitLock<'a>(&'a File);
impl Drop for InitLock<'_> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(self.0);
    }
}

/// Exclusive, thread-affine access to one shard. Release never destroys shared
/// mutexes: other live mappings may still reference them after this handle drops.
pub struct ShardGuard<'a> {
    region: &'a SharedRegion,
    shard: usize,
    recovered: bool,
    _not_send: PhantomData<Rc<()>>,
}

impl ShardGuard<'_> {
    /// Whether this acquisition recovered an abruptly terminated holder.
    pub fn recovered(&self) -> bool {
        self.recovered
    }
    /// Immutable bytes, valid only while this guard remains locked.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: guard owns the unique shard mutex and the slice stays within
        // its validated allocation. SharedRegion never exposes other references.
        unsafe {
            std::slice::from_raw_parts(
                self.region.payload(self.shard),
                self.region.layout.payload_bytes,
            )
        }
    }
    /// Mutable bytes, valid only under this guard's exclusive borrow.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above; &mut self additionally prevents overlapping borrows
        // from the same guard. Other shards are disjoint by checked construction.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.region.payload(self.shard),
                self.region.layout.payload_bytes,
            )
        }
    }
}

impl Drop for ShardGuard<'_> {
    fn drop(&mut self) {
        // A Rust panic must not expose a half-completed mutation as healthy.
        if std::thread::panicking() {
            self.bytes_mut().fill(0);
        }
        // SAFETY: guards cannot cross threads and still own this live mutex.
        let code = unsafe { libc::pthread_mutex_unlock(self.region.mutex(self.shard)) };
        debug_assert_eq!(code, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "ps-region-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> std::path::PathBuf {
            self.0.join("region-v1")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn layout() -> RegionLayout {
        RegionLayout::new(2, 256).unwrap()
    }
    fn child(path: &Path, mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "region::tests::subprocess_entry", "--nocapture"])
            .env("PS_REGION_TEST_PATH", path)
            .env("PS_REGION_TEST_MODE", mode)
            .stdout(Stdio::null());
        command
    }

    #[test]
    fn subprocess_entry() {
        let Some(path) = std::env::var_os("PS_REGION_TEST_PATH") else {
            return;
        };
        let path = Path::new(&path);
        let region = SharedRegion::open(path, layout()).unwrap();
        let mode = std::env::var("PS_REGION_TEST_MODE").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut guard = loop {
            if let Some(guard) = region.try_lock(0).unwrap() {
                break guard;
            }
            assert!(Instant::now() < deadline, "child shard remained busy");
            std::thread::yield_now();
        };
        guard.bytes_mut().fill(0xa5);
        if mode == "hold" {
            std::fs::write(path.with_extension("ready"), b"ready").unwrap();
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
    }

    #[test]
    fn allocation_is_checked_and_payloads_are_disjoint() {
        assert!(RegionLayout::new(0, 1).is_err());
        assert!(RegionLayout::new(257, 1).is_err());
        assert!(RegionLayout::new(1, 0).is_err());
        assert!(RegionLayout::new(2, usize::MAX).is_err());
        assert!(RegionLayout::new(256, MAX_BYTES).is_err());
        let temp = Temp::new();
        let region = SharedRegion::open(&temp.path(), layout()).unwrap();
        assert_eq!(2, region.layout().shards());
        assert_eq!(256, region.layout().payload_bytes());
        assert_eq!(HEADER + 2 * 320, region.layout().mapped_bytes());
        let mut one = region.try_lock(0).unwrap().unwrap();
        let mut two = region.try_lock(1).unwrap().unwrap();
        one.bytes_mut().fill(1);
        two.bytes_mut().fill(2);
        assert!(one.bytes().iter().all(|byte| *byte == 1));
        assert!(two.bytes().iter().all(|byte| *byte == 2));
        assert!(region.try_lock(0).unwrap().is_none());
        assert!(region.try_lock(2).is_err());
    }

    #[test]
    fn separate_mapping_observes_bytes_without_reset_or_resize() {
        let temp = Temp::new();
        let first = SharedRegion::open(&temp.path(), layout()).unwrap();
        first.try_lock(1).unwrap().unwrap().bytes_mut()[42] = 99;
        let second = SharedRegion::open(&temp.path(), layout()).unwrap();
        assert_eq!(99, second.try_lock(1).unwrap().unwrap().bytes()[42]);
        let wrong = RegionLayout::new(2, 128).unwrap();
        assert!(SharedRegion::open(&temp.path(), wrong).is_err());
        assert_eq!(
            layout().mapped_bytes() as u64,
            std::fs::metadata(temp.path()).unwrap().len()
        );
        drop(first);
        drop(second);
        let reopened = SharedRegion::open(&temp.path(), layout()).unwrap();
        assert_eq!(99, reopened.try_lock(1).unwrap().unwrap().bytes()[42]);
    }

    #[test]
    fn invalid_or_partial_header_fails_closed() {
        use std::io::{Seek, SeekFrom, Write};
        let temp = Temp::new();
        let region = SharedRegion::open(&temp.path(), layout()).unwrap();
        drop(region);
        let mut file = OpenOptions::new().write(true).open(temp.path()).unwrap();
        file.seek(SeekFrom::Start(8)).unwrap();
        file.write_all(&999u64.to_le_bytes()).unwrap();
        assert!(SharedRegion::open(&temp.path(), layout()).is_err());
        // Same length, unfinished initialization (zero header): never silently reset.
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[0u8; HEADER]).unwrap();
        assert!(SharedRegion::open(&temp.path(), layout()).is_err());
    }

    #[test]
    fn atomic_invalidation_is_shared_and_never_wraps() {
        let temp = Temp::new();
        let region = SharedRegion::open(&temp.path(), layout()).unwrap();
        let other = SharedRegion::open(&temp.path(), layout()).unwrap();
        let guard = region.try_lock(0).unwrap().unwrap();
        region.invalidate(0).unwrap();
        assert_eq!(1, other.epoch(0).unwrap());
        assert!(guard.bytes().iter().all(|b| *b == 0));
        region
            .epoch_cell(0)
            .unwrap()
            .store(u64::MAX - 1, std::sync::atomic::Ordering::SeqCst);
        region.invalidate(0).unwrap();
        region.invalidate(0).unwrap();
        assert_eq!(u64::MAX, other.epoch(0).unwrap());
    }

    #[test]
    fn another_machine_boot_cannot_reuse_pthread_state() {
        use std::io::{Seek, SeekFrom, Write};
        let temp = Temp::new();
        drop(SharedRegion::open(&temp.path(), layout()).unwrap());
        let mut file = OpenOptions::new().write(true).open(temp.path()).unwrap();
        file.seek(SeekFrom::Start(80)).unwrap();
        file.write_all(&[b'x'; 36]).unwrap();
        assert!(SharedRegion::open(&temp.path(), layout()).is_err());
    }

    #[test]
    fn initialization_contention_has_a_deadline() {
        let temp = Temp::new();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(temp.path())
            .unwrap();
        file.lock_exclusive().unwrap();
        let error = SharedRegion::open(&temp.path(), layout()).unwrap_err();
        assert_eq!(io::ErrorKind::TimedOut, error.kind());
        FileExt::unlock(&file).unwrap();
        SharedRegion::open(&temp.path(), layout()).unwrap();
    }

    #[test]
    fn separate_processes_can_initialize_and_attach() {
        let temp = Temp::new();
        let mut children: Vec<_> = (0..8)
            .map(|_| child(&temp.path(), "write").spawn().unwrap())
            .collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let region = SharedRegion::open(&temp.path(), layout()).unwrap();
        assert!(region
            .try_lock(0)
            .unwrap()
            .unwrap()
            .bytes()
            .iter()
            .all(|b| *b == 0xa5));
    }

    #[test]
    fn killed_holder_is_recovered_without_touching_another_shard() {
        let temp = Temp::new();
        let region = SharedRegion::open(&temp.path(), layout()).unwrap();
        region.try_lock(1).unwrap().unwrap().bytes_mut().fill(7);
        let mut process = child(&temp.path(), "hold").spawn().unwrap();
        let ready = temp.path().with_extension("ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            if process.try_wait().unwrap().is_some() {
                panic!("child failed before holding mutex");
            }
            if Instant::now() >= deadline {
                process.kill().unwrap();
                process.wait().unwrap();
                panic!("child readiness timeout");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(region.try_lock(0).unwrap().is_none());
        process.kill().unwrap();
        process.wait().unwrap();
        let recovered = region.try_lock(0).unwrap().unwrap();
        assert!(recovered.recovered());
        assert!(recovered.bytes().iter().all(|b| *b == 0));
        drop(recovered);
        assert!(!region.try_lock(0).unwrap().unwrap().recovered());
        assert!(region
            .try_lock(1)
            .unwrap()
            .unwrap()
            .bytes()
            .iter()
            .all(|b| *b == 7));
    }

    #[test]
    fn panic_clears_partial_payload_and_releases_lock() {
        let temp = Temp::new();
        let region = SharedRegion::open(&temp.path(), layout()).unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = region.try_lock(0).unwrap().unwrap();
            guard.bytes_mut().fill(9);
            panic!("simulated interrupted mutation");
        }));
        assert!(panic.is_err());
        assert!(region
            .try_lock(0)
            .unwrap()
            .unwrap()
            .bytes()
            .iter()
            .all(|b| *b == 0));
    }

    #[test]
    fn threads_serialize_payload_updates() {
        let temp = Temp::new();
        let region = Arc::new(SharedRegion::open(&temp.path(), layout()).unwrap());
        let mut threads = Vec::new();
        for _ in 0..8 {
            let region = region.clone();
            threads.push(std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                for _ in 0..1000 {
                    loop {
                        if let Some(mut guard) = region.try_lock(0).unwrap() {
                            let value = u64::from_le_bytes(guard.bytes()[..8].try_into().unwrap());
                            guard.bytes_mut()[..8].copy_from_slice(&(value + 1).to_le_bytes());
                            break;
                        }
                        assert!(Instant::now() < deadline);
                        std::thread::yield_now();
                    }
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            8000,
            u64::from_le_bytes(
                region.try_lock(0).unwrap().unwrap().bytes()[..8]
                    .try_into()
                    .unwrap()
            )
        );
    }

    #[test]
    fn open_permission_failure_propagates() {
        use std::os::unix::fs::PermissionsExt;
        // Root can bypass permissions; the current development user cannot.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = Temp::new();
        std::fs::write(temp.path(), b"").unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(
            io::ErrorKind::PermissionDenied,
            SharedRegion::open(&temp.path(), layout())
                .unwrap_err()
                .kind()
        );
    }
}

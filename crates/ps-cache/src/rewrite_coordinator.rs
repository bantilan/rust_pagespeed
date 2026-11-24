//! Background-only optimization ownership and metadata publication fencing.
use crate::cache::{Cache, Lookup};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Coordinator claims run on pool workers, never HTML scheduling threads.
pub trait RewriteCoordinator: Send + Sync + std::fmt::Debug {
    /// The exact options/UA-aware key is scoped by the coordinator's cache domain.
    fn claim(&self, key: &str) -> RewriteClaim;
}

/// Admission result; retry means keep original URLs and try on a later request.
pub enum RewriteClaim {
    /// Execute with every metadata mutation protected by this lease.
    Owned(Arc<dyn RewriteLease>),
    /// Another worker owns the job, or bounded admission is currently unavailable.
    Retry,
    /// Key cannot be represented; preserve the existing local optimization path.
    Local,
}

/// A live optimization claim. Dropping the final handle must release or fence it.
pub trait RewriteLease: Send + Sync + std::fmt::Debug {
    /// Executes the mutation at most once, only while ownership is valid.
    /// Returns false on revocation or a bounded publication deadline.
    fn publish(&self, mutation: &mut dyn FnMut()) -> bool;
}

/// Worker-private cache view that fences every metadata mutation, including
/// nested rewrite metadata; reads keep the existing codec/validation path.
#[derive(Debug)]
pub struct LeasedMetadataCache {
    backend: Arc<dyn Cache>,
    lease: Arc<dyn RewriteLease>,
    failed: AtomicBool,
}
impl LeasedMetadataCache {
    /// Wrap the same shared backend for one optimization job only.
    pub fn new(backend: Arc<dyn Cache>, lease: Arc<dyn RewriteLease>) -> Self {
        Self {
            backend,
            lease,
            failed: AtomicBool::new(false),
        }
    }
    /// Whether all attempted publications were accepted by the lease.
    pub fn publications_valid(&self) -> bool {
        !self.failed.load(Ordering::Relaxed)
    }
    fn mutate(&self, mut mutation: impl FnMut()) {
        if !self.lease.publish(&mut mutation) {
            self.failed.store(true, Ordering::Relaxed);
        }
    }
}
impl Cache for LeasedMetadataCache {
    fn get(&self, key: &str) -> Lookup {
        self.backend.get(key)
    }
    fn put(&self, key: &str, value: &[u8]) {
        self.mutate(|| self.backend.put(key, value));
    }
    fn delete(&self, key: &str) {
        self.mutate(|| self.backend.delete(key));
    }
    fn name(&self) -> String {
        format!("LeasedMetadata({})", self.backend.name())
    }
    fn is_blocking(&self) -> bool {
        self.backend.is_blocking()
    }
    fn is_healthy(&self) -> bool {
        self.backend.is_healthy()
    }
    fn must_encode_key_in_value(&self) -> bool {
        self.backend.must_encode_key_in_value()
    }
    fn put_with_key_in_value(&self, key: &str, value: &[u8]) {
        self.mutate(|| self.backend.put_with_key_in_value(key, value));
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use ps_shm::ownership::{OwnershipClaim, OwnershipFinish, OwnershipTable, OwnershipToken};
    use std::{
        io,
        path::Path,
        time::{Duration, Instant},
    };

    /// Opt-in coordinator over one canonical cache-path ownership domain.
    #[derive(Debug)]
    pub struct SharedRewriteCoordinator {
        table: Arc<OwnershipTable>,
        publication_budget: Duration,
    }
    impl SharedRewriteCoordinator {
        /// Open at configuration time. Budget applies only to worker publication
        /// lock contention (capped at 500ms), never to HTML delivery or disk I/O.
        pub fn open(path: &Path, slots: usize, publication_budget: Duration) -> io::Result<Self> {
            Ok(Self {
                table: Arc::new(OwnershipTable::open(path, slots)?),
                publication_budget: publication_budget.min(Duration::from_millis(500)),
            })
        }
    }
    impl RewriteCoordinator for SharedRewriteCoordinator {
        fn claim(&self, key: &str) -> RewriteClaim {
            match self.table.try_claim(key) {
                Ok(OwnershipClaim::Owned(token)) => RewriteClaim::Owned(Arc::new(SharedLease {
                    table: self.table.clone(),
                    token,
                    budget: self.publication_budget,
                })),
                Ok(OwnershipClaim::KeyTooLong) => RewriteClaim::Local,
                _ => RewriteClaim::Retry,
            }
        }
    }
    #[derive(Debug)]
    struct SharedLease {
        table: Arc<OwnershipTable>,
        token: OwnershipToken,
        budget: Duration,
    }
    impl RewriteLease for SharedLease {
        fn publish(&self, mutation: &mut dyn FnMut()) -> bool {
            let deadline = Instant::now() + self.budget;
            loop {
                match self.table.try_publish(self.token, &mut *mutation) {
                    Ok(OwnershipFinish::Finished(())) => return true,
                    Ok(OwnershipFinish::Busy) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    _ => return false,
                }
            }
        }
    }
    impl Drop for SharedLease {
        fn drop(&mut self) {
            match self.table.try_finish(self.token, || ()) {
                Ok(OwnershipFinish::Finished(())) | Ok(OwnershipFinish::Stale) => (),
                _ => {
                    let _ = self.table.revoke_through(self.token);
                }
            }
        }
    }
}
#[cfg(target_os = "linux")]
pub use linux::SharedRewriteCoordinator;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::cache::MemoryCache;
    use std::time::Duration;
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "ps-coordinator-{}-{}-{}",
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
        fn coordinator(&self, name: &str) -> SharedRewriteCoordinator {
            SharedRewriteCoordinator::open(&self.0.join(name), 2, Duration::from_millis(2)).unwrap()
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn lease(coordinator: &dyn RewriteCoordinator, key: &str) -> Arc<dyn RewriteLease> {
        match coordinator.claim(key) {
            RewriteClaim::Owned(lease) => lease,
            _ => panic!("claim refused"),
        }
    }
    #[test]
    fn independent_handles_deduplicate_release_and_separate_domains() {
        let temp = Temp::new();
        let first = temp.coordinator("a");
        let peer = temp.coordinator("a");
        let other = temp.coordinator("b");
        let owned = lease(&first, "options:ua:key");
        assert!(matches!(peer.claim("options:ua:key"), RewriteClaim::Retry));
        let _other = lease(&other, "options:ua:key");
        drop(owned);
        let _next = lease(&peer, "options:ua:key");
        let _variant = lease(&peer, "different-options:ua:key");
        assert!(matches!(peer.claim("third"), RewriteClaim::Retry));
        assert!(matches!(peer.claim(&"x".repeat(1025)), RewriteClaim::Local));
    }
    #[test]
    fn metadata_writes_and_deletes_are_fenced_and_panic_releases() {
        let temp = Temp::new();
        let coordinator = temp.coordinator("a");
        let backend = Arc::new(MemoryCache::new());
        let view = LeasedMetadataCache::new(backend.clone(), lease(&coordinator, "job"));
        view.put("partition", b"first");
        view.put("nested", b"second");
        assert_eq!(backend.get("partition").value.unwrap(), b"first");
        view.delete("nested");
        assert!(backend.get("nested").value.is_none());
        assert!(view.publications_valid());
        drop(view);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let owned = lease(&coordinator, "job");
            owned.publish(&mut || panic!("publication failed"));
        }));
        assert!(result.is_err());
        let _next = lease(&coordinator, "job");
    }
    #[test]
    fn dropping_a_busy_lease_is_bounded_and_revokes_publication() {
        let temp = Temp::new();
        let coordinator = temp.coordinator("a");
        let first = lease(&coordinator, "first");
        let second = lease(&coordinator, "second");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            first.publish(&mut || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = std::time::Instant::now();
        let backend = Arc::new(MemoryCache::new());
        let view = LeasedMetadataCache::new(backend.clone(), second);
        view.put("partition", b"must not publish");
        let valid = view.publications_valid();
        drop(view);
        let elapsed = start.elapsed();
        release_tx.send(()).unwrap();
        assert!(worker.join().unwrap());
        assert!(!valid);
        assert!(backend.get("partition").value.is_none());
        assert!(elapsed < Duration::from_secs(1));
        let _next = lease(&coordinator, "second");
    }
}

//! Background rewrite execution (bounded worker pool).
//!
//! Ports the *shape* of C++'s async rewrite completion without its
//! event machinery: expensive cache misses run on pool threads while
//! the requesting worker waits up to its deadline, then falls back
//! to original content. Completions always store to the shared
//! caches, so the next request hits. There is no AsyncFetch/event
//! integration (the nginx layer is synchronous): waiting parks the
//! calling thread with a timeout, which C++ also bounds per flush
//! window — never longer than configured, unlike today's unbounded
//! blocking fetches.
//!
//! Guarantees: bounded threads and queue (overflow rejects fast to
//! the caller's fallback); duplicate suppression by key (concurrent
//! waiters share one execution); request-independent lifetimes (jobs
//! own everything they touch); graceful shutdown (waiters release,
//! queued work drops, in-flight work finishes within its own
//! bounds); no locks held across fetches (one short mutex per
//! submit/complete).

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

use ps_stats::core::{Statistics, Variable};

/// Submitted work: runs once on a pool thread, sharing its result.
type Task = Box<dyn FnOnce() -> Arc<dyn Any + Send + Sync> + Send>;

/// Shared completion cell: exactly one execution wakes all waiters.
struct SharedResult {
    state: Mutex<SharedState>,
    updated: Condvar,
}

struct SharedState {
    value: Option<Arc<dyn Any + Send + Sync>>,
    shutdown: bool,
}

/// Handle to one execution, shared by duplicate submitters.
pub struct JobHandle {
    result: Arc<SharedResult>,
}

impl JobHandle {
    /// Waits for completion without limit (`None` only on shutdown).
    pub fn wait_unbounded<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        let mut state = self
            .result
            .state
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        loop {
            if let Some(value) = &state.value {
                let any: Arc<dyn Any + Send + Sync> = Arc::clone(value);
                return Arc::downcast(any).ok();
            }
            if state.shutdown {
                return None;
            }
            state = self
                .result
                .updated
                .wait(state)
                .unwrap_or_else(|err| err.into_inner());
        }
    }

    /// Waits up to `timeout` for completion (`None` on timeout or
    /// shutdown; the job still runs to completion unless dropped).
    pub fn wait_timeout<R: Send + Sync + 'static>(&self, timeout: Duration) -> Option<Arc<R>> {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self
            .result
            .state
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        loop {
            if let Some(value) = &state.value {
                let any: Arc<dyn Any + Send + Sync> = Arc::clone(value);
                return Arc::downcast(any).ok();
            }
            if state.shutdown {
                return None;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, timed_out) = self
                .result
                .updated
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|err| err.into_inner());
            state = guard;
            if timed_out.timed_out() && state.value.is_none() && !state.shutdown {
                return None;
            }
        }
    }
}

/// Submission rejection (caller falls back immediately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    /// Bounded queue full.
    QueueFull,
    /// Pool shut down.
    Shutdown,
}

/// Pool counters (shared registry; timeouts are bumped by waiters,
/// which alone observe them).
struct PoolCounters {
    submitted: Arc<Variable>,
    completed: Arc<Variable>,
    dedup_hits: Arc<Variable>,
    queue_rejections: Arc<Variable>,
    shutdown_rejections: Arc<Variable>,
}

/// Pool sizing: worker threads plus the bounded pending queue.
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Worker threads.
    pub threads: usize,
    /// Pending-job bound (overflow rejects).
    pub queue_bound: usize,
}

impl PoolConfig {
    /// Production sizing: one worker per core (at least two),
    /// deep-enough queue for fetch bursts.
    #[must_use]
    pub fn production() -> Self {
        let threads = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(4)
            .clamp(2, 8);
        PoolConfig {
            threads,
            queue_bound: 1024,
        }
    }
}

/// Bounded rewrite worker pool with duplicate suppression.
pub struct BackgroundPool {
    sender: Mutex<Option<mpsc::SyncSender<QueuedJob>>>,
    in_flight: Mutex<HashMap<String, Arc<SharedResult>>>,
    shutdown: AtomicBool,
    counters: PoolCounters,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

struct QueuedJob {
    key: String,
    task: Task,
    result: Arc<SharedResult>,
}

impl std::fmt::Debug for BackgroundPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundPool")
            .field("shutdown", &self.shutdown.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl BackgroundPool {
    /// Starts a pool (`StartThreads`).
    pub fn new(config: PoolConfig, statistics: &Arc<Statistics>) -> Arc<Self> {
        let (sender, receiver) = mpsc::sync_channel::<QueuedJob>(config.queue_bound.max(1));
        let counters = PoolCounters {
            submitted: statistics.add_variable("background_jobs_submitted"),
            completed: statistics.add_variable("background_jobs_completed"),
            dedup_hits: statistics.add_variable("background_jobs_dedup_hits"),
            queue_rejections: statistics.add_variable("background_jobs_queue_full"),
            shutdown_rejections: statistics.add_variable("background_jobs_shutdown"),
        };
        let pool = Arc::new(BackgroundPool {
            sender: Mutex::new(Some(sender)),
            in_flight: Mutex::new(HashMap::new()),
            shutdown: AtomicBool::new(false),
            counters,
            workers: Mutex::new(Vec::new()),
        });
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = pool.workers.lock().unwrap_or_else(|err| err.into_inner());
        for index in 0..config.threads.max(1) {
            let receiver = Arc::clone(&receiver);
            let pool = Arc::clone(&pool);
            let name = format!("pagespeed-rewrite-{index}");
            let handle = std::thread::Builder::new()
                .name(name)
                .spawn(move || pool.worker_loop(receiver))
                .expect("background pool thread spawn");
            workers.push(handle);
        }
        drop(workers);
        drop(receiver);
        pool
    }

    /// Submits keyed work (`AddTask` + duplicate suppression):
    /// concurrent submissions with the same key join the in-flight
    /// execution instead of running twice. The key namespace is the
    /// caller's (prefix per result type: downcasts assume it).
    pub fn submit<R, F>(&self, key: &str, task: F) -> Result<JobHandle, SubmitError>
    where
        R: Send + Sync + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        if self.shutdown.load(Ordering::SeqCst) {
            self.counters.shutdown_rejections.add(1);
            return Err(SubmitError::Shutdown);
        }
        let mut in_flight = self.in_flight.lock().unwrap_or_else(|err| err.into_inner());
        if self.shutdown.load(Ordering::SeqCst) {
            self.counters.shutdown_rejections.add(1);
            return Err(SubmitError::Shutdown);
        }
        if let Some(result) = in_flight.get(key) {
            self.counters.submitted.add(1);
            self.counters.dedup_hits.add(1);
            return Ok(JobHandle {
                result: Arc::clone(result),
            });
        }
        let result = Arc::new(SharedResult {
            state: Mutex::new(SharedState {
                value: None,
                shutdown: false,
            }),
            updated: Condvar::new(),
        });
        let boxed: Task = Box::new(move || {
            let value: R = task();
            Arc::new(value) as Arc<dyn Any + Send + Sync>
        });
        let queued = QueuedJob {
            key: key.to_string(),
            task: boxed,
            result: Arc::clone(&result),
        };
        let sender = self.sender.lock().unwrap_or_else(|err| err.into_inner());
        let accepted = match sender.as_ref() {
            Some(sender) => sender.try_send(queued).is_ok(),
            None => false,
        };
        drop(sender);
        if !accepted {
            if self.shutdown.load(Ordering::SeqCst) {
                self.counters.shutdown_rejections.add(1);
                return Err(SubmitError::Shutdown);
            }
            self.counters.queue_rejections.add(1);
            return Err(SubmitError::QueueFull);
        }
        in_flight.insert(key.to_string(), Arc::clone(&result));
        self.counters.submitted.add(1);
        Ok(JobHandle { result })
    }

    /// Shuts down (`StopThreads`): rejects new work, releases
    /// waiters, drops queued jobs, and joins workers after in-flight
    /// work finishes within its own bounds. Idempotent.
    pub fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        // Close the queue: workers drain nothing further and exit
        // once receivers disconnect.
        drop(
            self.sender
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .take(),
        );
        // Release every waiter; queued jobs never run.
        let pending: Vec<Arc<SharedResult>> = {
            let mut in_flight = self.in_flight.lock().unwrap_or_else(|err| err.into_inner());
            in_flight.drain().map(|(_, result)| result).collect()
        };
        for result in pending {
            let mut state = result.state.lock().unwrap_or_else(|err| err.into_inner());
            state.shutdown = true;
            result.updated.notify_all();
        }
        let workers =
            std::mem::take(&mut *self.workers.lock().unwrap_or_else(|err| err.into_inner()));
        for worker in workers {
            let _ = worker.join();
        }
    }

    /// Whether shutdown started.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    fn worker_loop(&self, receiver: Arc<Mutex<mpsc::Receiver<QueuedJob>>>) {
        loop {
            let job = {
                let receiver = receiver.lock().unwrap_or_else(|err| err.into_inner());
                receiver.recv()
            };
            let Ok(job) = job else {
                break;
            };
            if self.shutdown.load(Ordering::SeqCst) {
                // Dropped (already released at shutdown); count it
                // as never run, not as completed.
                continue;
            }
            let shared: Arc<dyn Any + Send + Sync> = (job.task)();
            // Bookkeeping lands before the value is visible: a waiter
            // that observes completion must also observe the count.
            {
                let mut in_flight = self.in_flight.lock().unwrap_or_else(|err| err.into_inner());
                in_flight.remove(&job.key);
            }
            self.counters.completed.add(1);
            {
                let mut state = job
                    .result
                    .state
                    .lock()
                    .unwrap_or_else(|err| err.into_inner());
                state.value = Some(shared);
                job.result.updated.notify_all();
            }
        }
    }
}

impl Drop for BackgroundPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Instant;

    fn pool() -> (Arc<BackgroundPool>, Arc<Statistics>) {
        let statistics = Arc::new(Statistics::new());
        let pool = BackgroundPool::new(
            PoolConfig {
                threads: 2,
                queue_bound: 16,
            },
            &statistics,
        );
        (pool, statistics)
    }

    fn counter(statistics: &Arc<Statistics>, name: &str) -> i64 {
        statistics.get_variable(name).unwrap().get()
    }

    #[test]
    fn executes_and_returns() {
        let (pool, statistics) = pool();
        let handle = pool.submit("a", || 40 + 2).unwrap();
        assert_eq!(
            *handle.wait_timeout::<i32>(Duration::from_secs(5)).unwrap(),
            42
        );
        assert_eq!(counter(&statistics, "background_jobs_submitted"), 1);
        assert_eq!(counter(&statistics, "background_jobs_completed"), 1);
        pool.shutdown();
    }

    #[test]
    fn duplicates_share_one_execution() {
        let (pool, statistics) = pool();
        let runs = Arc::new(AtomicUsize::new(0));
        let probe = Arc::clone(&runs);
        let gate = Arc::new(std::sync::Barrier::new(3));
        let gate_in = Arc::clone(&gate);
        let first = pool
            .submit("dup", move || {
                probe.fetch_add(1, Ordering::SeqCst);
                gate_in.wait();
                7
            })
            .unwrap();
        // Second submitter joins while the first runs: it submits
        // first (deduping onto the in-flight job, which cannot
        // finish before the gate releases), then holds the gate so
        // the release is deterministic.
        let pool2 = Arc::clone(&pool);
        let gate_out = Arc::clone(&gate);
        let waiter = std::thread::spawn(move || {
            let handle = pool2.submit("dup", || 8).unwrap();
            gate_out.wait();
            handle.wait_timeout::<i32>(Duration::from_secs(5))
        });
        gate.wait();
        assert_eq!(
            *first.wait_timeout::<i32>(Duration::from_secs(5)).unwrap(),
            7
        );
        assert_eq!(*waiter.join().unwrap().unwrap(), 7);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(counter(&statistics, "background_jobs_dedup_hits"), 1);
        pool.shutdown();
    }

    #[test]
    fn timeout_releases_waiter_but_job_completes() {
        let (pool, _statistics) = pool();
        let handle = pool
            .submit("slow", || {
                std::thread::sleep(Duration::from_millis(200));
                1
            })
            .unwrap();
        assert!(handle
            .wait_timeout::<i32>(Duration::from_millis(20))
            .is_none());
        assert_eq!(
            *handle.wait_timeout::<i32>(Duration::from_secs(5)).unwrap(),
            1
        );
        pool.shutdown();
    }

    #[test]
    fn queue_overflow_rejects_fast() {
        let statistics = Arc::new(Statistics::new());
        let pool = BackgroundPool::new(
            PoolConfig {
                threads: 1,
                queue_bound: 1,
            },
            &statistics,
        );
        let gate = Arc::new(std::sync::Barrier::new(2));
        let probe = Arc::clone(&gate);
        let started = Arc::new(AtomicBool::new(false));
        let started_in = Arc::clone(&started);
        pool.submit("block", move || {
            started_in.store(true, Ordering::SeqCst);
            probe.wait();
            0
        })
        .unwrap();
        // Wait until the worker picked the job up: only then is the
        // unit queue slot provably free (else "queued" below races
        // the pickup and flakes).
        let deadline = Instant::now() + Duration::from_secs(5);
        while !started.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "worker never picked up the blocking job"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // Worker busy + queue full: this rejects instead of blocking.
        // (Queue holds one; the running job holds the worker.)
        pool.submit("queued", || 1).unwrap();
        assert!(matches!(
            pool.submit("overflow", || 2),
            Err(SubmitError::QueueFull)
        ));
        assert_eq!(counter(&statistics, "background_jobs_queue_full"), 1);
        gate.wait();
        pool.shutdown();
    }

    #[test]
    fn shutdown_releases_and_joins() {
        let (pool, _statistics) = pool();
        let gate = Arc::new(std::sync::Barrier::new(2));
        let probe = Arc::clone(&gate);
        let handle = pool
            .submit("stuck", move || {
                probe.wait();
                3
            })
            .unwrap();
        // Shut down beside the stuck worker: waiters release
        // immediately while the join waits for the worker.
        let shutdown_pool = Arc::clone(&pool);
        let shutdown = std::thread::spawn(move || shutdown_pool.shutdown());
        assert!(handle.wait_timeout::<i32>(Duration::from_secs(5)).is_none());
        assert!(pool.is_shutdown());
        assert!(matches!(
            pool.submit("late", || 4),
            Err(SubmitError::Shutdown)
        ));
        gate.wait();
        shutdown.join().unwrap();
        pool.shutdown();
    }
}

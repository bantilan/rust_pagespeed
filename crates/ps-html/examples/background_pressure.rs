//! Controlled local-queue pressure: scheduling cost, not resource/HTML latency.
use ps_html::background::{BackgroundPool, PoolConfig};
use ps_stats::core::Statistics;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

fn main() {
    let statistics = Arc::new(Statistics::new());
    let pool = BackgroundPool::new(
        PoolConfig {
            threads: 1,
            queue_bound: 4,
        },
        &statistics,
    );
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = pool
        .submit("blocked", move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            42usize
        })
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut duplicates = Vec::new();
    let mut dedup_ns = Vec::new();
    for _ in 0..256 {
        let started = Instant::now();
        duplicates.push(
            pool.submit("blocked", || panic!("duplicate must not execute"))
                .unwrap(),
        );
        dedup_ns.push(started.elapsed().as_nanos());
    }
    let mut accepted = Vec::new();
    let mut rejected_ns = Vec::new();
    for key in 0..256 {
        let started = Instant::now();
        let result = pool.submit(&format!("queued-{key}"), move || key);
        let elapsed = started.elapsed().as_nanos();
        if let Ok(handle) = result {
            accepted.push(handle);
        } else {
            rejected_ns.push(elapsed);
        }
    }
    assert_eq!(accepted.len(), 4);
    assert_eq!(rejected_ns.len(), 252);
    release_tx.send(()).unwrap();
    assert_eq!(
        *first.wait_timeout::<usize>(Duration::from_secs(5)).unwrap(),
        42
    );
    for duplicate in duplicates {
        assert_eq!(
            *duplicate
                .wait_timeout::<usize>(Duration::from_secs(5))
                .unwrap(),
            42
        );
    }
    for job in accepted {
        assert!(job.wait_timeout::<i32>(Duration::from_secs(5)).is_some());
    }
    pool.shutdown();
    let counter = |name| statistics.get_variable(name).unwrap().get();
    assert_eq!(counter("background_jobs_completed"), 5);
    assert_eq!(counter("background_jobs_dedup_hits"), 256);
    assert_eq!(counter("background_jobs_queue_full"), 252);
    dedup_ns.sort_unstable();
    rejected_ns.sort_unstable();
    println!("{{\"threads\":1,\"queue_bound\":4,\"executed\":5,\"duplicates_joined\":256,\"queue_rejections\":252,\"dedup_median_ns\":{},\"dedup_p95_ns\":{},\"rejection_median_ns\":{},\"rejection_p95_ns\":{}}}",
        dedup_ns[128], dedup_ns[243], rejected_ns[126], rejected_ns[239]);
}

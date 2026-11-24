//! Isolated protocol microbenchmark; does not measure HTML or Nginx latency.
#[cfg(target_os = "linux")]
fn main() {
    use ps_shm::{write_tracker::WriteTracker, SharedCounters};
    use std::{hint::black_box, time::Instant};
    let directory = std::env::temp_dir().join(format!(
        "ps-writer-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let tracker = WriteTracker::open(&directory).unwrap();
    let counters =
        SharedCounters::open_coherent(&directory, &["generation".into(), "writers".into()])
            .unwrap();
    fn measure(mut operation: impl FnMut(), iterations: u32) -> Vec<f64> {
        (0..7)
            .map(|_| {
                let start = Instant::now();
                for _ in 0..iterations {
                    operation();
                }
                start.elapsed().as_nanos() as f64 / f64::from(iterations)
            })
            .collect()
    }
    let baseline_read = measure(
        || {
            let value = if counters.coordination_get(1) == 0 {
                let generation = counters.coordination_get(0);
                (counters.coordination_get(1) == 0).then_some(generation)
            } else {
                None
            };
            black_box(value);
        },
        250_000,
    );
    let tracker_read = measure(
        || {
            black_box(tracker.snapshot(0).unwrap());
        },
        250_000,
    );
    let baseline_write = measure(
        || {
            counters.coordination_add(1, 1);
            counters.coordination_add(0, 1);
            counters.coordination_add(1, -1);
        },
        100_000,
    );
    let tracker_write = measure(
        || {
            drop(tracker.try_begin(0).unwrap().unwrap());
        },
        100_000,
    );
    let idle_recovery = measure(
        || {
            black_box(tracker.recover(0).unwrap());
        },
        100_000,
    );
    println!("{{\"scope\":\"uncontended protocol only; no disk IO or HTML\",\"samples\":7,\"read_iterations\":250000,\"write_iterations\":100000,\"mapped_bytes\":{},\"legacy_snapshot_ns\":{:?},\"tracker_snapshot_ns\":{:?},\"legacy_begin_end_ns\":{:?},\"tracker_begin_end_ns\":{:?},\"idle_recovery_ns\":{:?}}}", tracker.mapped_bytes(), baseline_read, tracker_read, baseline_write, tracker_write, idle_recovery);
    drop(tracker);
    drop(counters);
    std::fs::remove_dir_all(directory).unwrap();
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux-only protocol probe");
}

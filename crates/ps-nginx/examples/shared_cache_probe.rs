//! Cache-layer probe only: no Nginx reload, no HTML/request latency claims.
use ps_nginx::{caches::SystemCaches, conf::NgxOptions};
use std::hint::black_box;
use std::time::Instant;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "ps-shared-cache-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root)?;
    let value = vec![42u8; 4096];
    println!("{{\"iterations_per_sample\":10000,\"samples\":7,\"value_bytes\":4096,\"units\":\"ns per cache get\",\"cases\":{{");
    for (index, (name, l1, shared)) in [
        ("file_only", false, false),
        ("shared_l2_file", false, true),
        ("l1_file", true, false),
        ("l1_shared_l2_file", true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let directory = root.join(name);
        let mut options = NgxOptions::new();
        options.system.file_cache_path = directory.to_string_lossy().into_owned();
        options.system.lru_cache_kb_per_process = if l1 { 8192 } else { 0 };
        let mut caches = SystemCaches::new();
        caches.register_config(&options.system, false, false, 0);
        if shared {
            caches.enable_shared_memory(
                &options.system.file_cache_path,
                ps_shm::byte_cache::ByteCacheOptions {
                    shards: 16,
                    entries_per_shard: 32,
                    bytes_per_entry: 8192,
                },
            )?;
        }
        let backend = caches.setup_caches(&options).unwrap().http_backend;
        backend.put("probe-key", &value);
        for _ in 0..1000 {
            assert_eq!(
                Some(value.as_slice()),
                backend.get("probe-key").value.as_deref()
            );
        }
        let mut samples = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            for _ in 0..10000 {
                black_box(backend.get(black_box("probe-key")));
            }
            samples.push(started.elapsed().as_nanos() as f64 / 10000.0);
        }
        assert_eq!(
            Some(value.as_slice()),
            backend.get("probe-key").value.as_deref()
        );
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        println!(
            "{}\"{name}\":{{\"median_ns\":{},\"samples_ns\":{:?}}}",
            if index == 0 { "" } else { "," },
            sorted[3],
            samples
        );
    }
    println!("}}}}");
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("shared cache probe requires Linux");
}

# Current-build benchmark: ef9aeb5

Measured 2026-11-24 with a fresh ordinary release module, matching staged Nginx 1.31.8, isolated single worker, same benchmark template/filters. No live service reload or cache purge. Module SHA256: `9b5839d96d0109e645ae2b2c3b7dee11ef94a076a0512fccbc8b440442073922`.

| Test case | Codex 2bcaea5 | Rust c984b33 (fix) | Codex c984b33 | Codex ef9aeb5 (default) |
|---|---:|---:|---:|---:|
| Moto first load | 42 ms | 9 ms | Not tested | **3.64 ms** |
| Moto repeat | 16 ms | 2 ms | 1.78 ms | **0.46 ms** |
| Aroma first load | 12 ms | 11 ms | Not tested | **4.93 ms** |
| Aroma repeat | 18 ms | 2 ms | 1.89 ms | **0.49 ms** |
| Cache hit | 18 ms | 2 ms | 1.89 ms | **0.49 ms** |

Historical columns above reproduce the user's supplied values; later historical first-load retests are retained in the parent benchmark.md. Current values use the Python HTTP client in tools/profile_html.py; historical c984b33 measurements used curl/system Nginx 1.31.10. These are different measurement setups, so the table is not a controlled speedup ratio.

First load is the median of five deliveries with fresh processes/application caches, OS caches potentially warm; background optimization may still be running. Repeat is the pooled median of 500 confirmed HTML HIT samples (100 per run). Cache hit repeats Aroma's HIT value by the historical table convention. Each run also measured warm-metadata HTML MISS separately.

| Page | Tier | Cold median | HTML HIT median | Warm HTML MISS median |
|---|---|---:|---:|---:|
| Moto | default | 3.641 ms | 0.461 ms | 1.857 ms |
| Aroma | default | 4.927 ms | 0.485 ms | 2.704 ms |
| Moto | shared | 4.323 ms | 0.466 ms | 1.895 ms |
| Aroma | shared | 5.090 ms | 0.498 ms | 2.734 ms |

Shared tier: explicit 16 MiB. One sequential default/shared pair, not randomized repeated pairs; no general contention/throughput conclusion. Shared remains opt-in. Normalized optimized HTML matches across all runs/modes and the previous checkpoint: Moto 15 references, Aroma 23. Ordinary module has no pipeline counters; resource fetch counts are unavailable, not zero.

Commands (from repository root):

```sh
cargo build -p ps-nginx --release > docs/performance/shared-memory/ef9aeb5-benchmark-build.log 2>&1
python3 tools/benchmark_shared_memory.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/ef9aeb5-default.json --samples 100 --cold-runs 5 --shared-kb 0
python3 tools/benchmark_shared_memory.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/ef9aeb5-shared.json --samples 100 --cold-runs 5 --shared-kb 16384
```

Build and both benchmark commands passed; raw JSON/logs are committed beside this report. This does not attribute the historical production 13 ms.

# Worker scaling, cache pressure and queue saturation

Measured ordinary release at source 5d434c9 (runtime code from 83c0a82), 2026-11-24. Module SHA256: `f39a4104a52b2ffcbe7978b819559055315068c8889f4a556577a904b19cfaf0`. Isolated NGINX 1.31.8, eight concurrent Python clients, fresh application caches per run; OS caches warm. Three repetitions per worker/tier pair with deterministic randomized tier order (seed 20261124). No live deployment/configuration changes.

## Worker matrix

Pooled median milliseconds, 300 samples per warm phase/page/cell. These concurrent timings differ from the single-client README table. Cold bursts have 96 samples per cell; they are burst deliveries during warmup, not isolated first loads. Raw cold, warm, p95, throughput and worker distribution samples are retained in worker-matrix.json and worker-matrix-summary.json.

| Workers | Tier | Moto HIT | Moto MISS | Aroma HIT | Aroma MISS | Worker PSS total, MiB |
|---|---|---:|---:|---:|---:|---:|
| 1 | Default | 3.021 | 13.587 | 3.201 | 20.282 | 26.2 |
| 1 | Shared 16 MiB | 3.109 | 13.730 | 3.229 | 20.542 | 36.4 |
| 2 | Default | 1.905 | 7.025 | 1.994 | 10.378 | 44.7 |
| 2 | Shared 16 MiB | 1.826 | 6.989 | 2.190 | 10.291 | 52.4 |
| 4 | Default | 1.969 | 4.048 | 2.000 | 6.063 | 78.2 |
| 4 | Shared 16 MiB | 1.956 | 4.127 | 2.115 | 6.021 | 83.3 |

More workers reduced CPU-bound warm MISS latency on this workload; shared-cache effects were mixed. Keep the shared tier opt-in. PSS totals are sampled after both pages/phases, across all workers (not master); PSS apportions shared pages. RSS is also recorded but cannot be summed as unique memory. This is a point sample, not peak-memory or leak analysis. Client scheduling limits throughput; requests/second is not server maximum capacity.

All 24 runs (18 main plus six small-cache/default runs) retained identical normalized optimized HTML hashes, with 15/23 optimized references on Moto/Aroma. Confirmed HITs never parsed the page; forced MISS samples use distinct request-header identities with unchanged resource options/URLs. Cold bursts may contain both fallback and already-warm HTML; no cold output identity claim is made.

## Cache reuse and pressure

The main single-worker shared runs had zero measured L2 hits, consistent with hot L1 service in this workload. Two-worker runs recorded 288–315 L2 hits and 10–14 duplicate-owner claims; four-worker runs recorded 1002–1033 L2 hits and 48–59 duplicate-owner claims. These are shared-domain counters for both pages plus warmup, not per-request critical-path costs.

Three four-worker runs with a 1 MiB configured byte budget exercised 930–977 evictions and 13–88 busy accesses. Storage remained at 48/48 slots and 790,656 mapped byte-cache bytes, below budget, with zero disabled writer-memory shards. Other bounded coordination mappings are separately reported in admin gauges and remain outside the byte-cache budget. Output hashes stayed identical despite eviction/fallback. See worker-matrix-pressure.json.

Completed background job counts did NOT show a consistent shared-cache reduction: default vs shared16MiB ranges were 629 vs 626–629 (one worker), 785 vs 800–824 (two), and 1067–1073 vs 1160–1200 (four). Local dedup counters and global ownership counters cover different stages and cannot be summed into resource work saved. These workloads show cross-worker claim collisions, not a measured reduction in unique fetch/encode bytes. Direct per-resource fetch/encode counting remains follow-up work before claiming redundant-work savings. The ordinary module has no pipeline fetch counters.

## Controlled bounded-queue probe

`background_pressure` blocks one worker, fills four queue slots, then verifies that 252 excess unique submissions are rejected and 256 duplicate submissions share the original result. Exactly five jobs execute (the running job plus four queued jobs); duplicate closures must never execute. Seven repeated release runs passed. Median of run medians: duplicate join 50 ns; queue rejection 90 ns. See background-pressure-repeat.json for raw per-run p95s. These are in-process scheduling costs under a controlled gate, not HTML, resource encoding, or distributed queue timings. The HTTP matrix itself did not force queue saturation.

## Installed localhost observation and attribution limit

The supplied Aerosky endpoint now responds: initial read-only request 45.160 ms, then 20 enabled requests median 0.488 ms and 20 PageSpeed-off requests median 0.189 ms. Raw headers/status/markers are in matrix-localhost-warm.json. This sequence retained existing caches; the first observation is not a controlled cold-cache benchmark. Different installed module SHA256 `af99f9f585fe9189eb13d9a06893a7f5d793fa2ae0b3a253474ef2eb61ec0455` means these are not measurements of the staged build.

The historical ~13 ms did not reproduce in this small warm sample. Enabled-minus-off is not pure NGINX overhead or causal stage attribution. Production strace attachment was denied (Operation not permitted, recorded in matrix-production-trace-access.json); no permissions/settings were changed. Historical production attribution remains incomplete. Fully asynchronous resource reconstruction and direct duplicate-fetch/encode measurements also remain open.

## Reproduce

```sh
cargo build -p ps-nginx --release
python3 tools/benchmark_worker_matrix.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/worker-matrix.json
python3 tools/benchmark_worker_matrix.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/worker-matrix-pressure.json --shared-kb 1024 --workers 4
cargo run --release -p ps-html --example background_pressure
```

Build, all matrix assertions, seven queue-probe repetitions, Python syntax and diff checks passed. No production Rust implementation changes were made to improve reported results. No full-workspace rerun was needed for this benchmark/example-only milestone.

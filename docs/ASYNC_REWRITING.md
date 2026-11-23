# Progressive Asynchronous Resource Rewriting

C++-style progressive optimization for the Rust port: serve cached
rewrites immediately, bound the wait for uncached resources by a
rewrite deadline, and warm the cache in the background so later
requests reuse completed work.

## How it works

`ServerContext` optionally owns a `BackgroundPool`
(`crates/ps-html/src/background.rs`): N worker threads
(`NumRewriteThreads`, default 1), a bounded pending queue (1024),
in-flight dedup by fetch key, and shared statistics. The nginx
factory creates one pool per worker at child init and installs it
on the worker's `ServerContext`; every request driver then shares
that server's caches and pool.

On an HTTP-cache miss, `CacheableResource::load` submits a
fetch+store job and waits up to the driver's remaining flush-window
budget (`RewriteDeadlinePerFlushMs` per flush, rearmed by
`ps-nginx` on every flush, default 10ms NDEBUG parity):

- Result in time: link it exactly like a sync completion. Fetch,
  store, stats, and failure paths are one shared implementation
  (`FetchPlan`/`execute_fetch`/`store_fetch_outcome`), so sync,
  async, and in-place fetches cannot diverge.
- Deadline exceeded: serve the fallback (original references for
  HTML, failed fetch for IPRO origins so the server falls back to
  its native method). The job keeps running and warms the cache.
  Timeouts never touch failure memory and mark inputs pending, so
  contexts skip writing partitions — C++ drops uncompleted
  contexts the same way.
- Pool missing, saturated, shut down, or the URL unmappable: run
  the direct sync path. Degraded, never failed. Held fetches stay
  inconclusive (`pending`, never stored).

Simultaneous identical fetches collapse onto one job (dedup key:
fetch URL + serialized request headers + fallback shape), so a
thundering herd pays for one origin fetch.

## Deadline selection

`ResourceDriver::fetch_wait_budget_ms` mirrors C++
`RewriteContext::GetRewriteDeadlineAlarmMs` with the
`InPlaceRewriteContext` override:

- Serving in place (`fetch_in_place_resource` scope) with
  `InPlaceWaitForOptimized` on: `InPlaceRewriteDeadlineMs`.
- Otherwise: `RewriteDeadlinePerFlushMs`.
- Non-positive configured value: unbounded wait (deadline
  disabled, C++ parity).

In-place origin fetches (`reconstruct`, `revalidate_input`,
proxy `serve_head`) run through the same bounded path. Without a
pool they fetch direct, exactly as before.

## Statistics

`background_jobs_submitted`, `background_jobs_completed`,
`background_jobs_timeouts`, `background_jobs_dedup_hits`,
`background_jobs_queue_full`, `background_jobs_shutdown`
(`crates/ps-html/src/rewrite_stats.rs`).

## Intentional differences from C++

- **Fetch only; compute stays inline.** Image conversion, CSS/JS
  optimization, and rewriting run on the requesting thread. C++
  schedules whole rewrite closures on its thread pool, but the
  port's `rewrite_single` takes `&Rc<ResourceDriver>` (`!Send`),
  so compute backgrounding needs per-behavior refactors. Fetch
  dominates cold-request latency (network vs microseconds of
  minification), which is why the deadline bounds the fetch.
- **Bounded blocking, not full async.** The worker thread waits up
  to the deadline per flush window; C++ never blocks (async
  scheduler). Worst case per flush is the configured deadline
  (default 10ms), not the full origin latency.
- **Deadline-bounded IPRO origins fail fast.** A slow origin past
  the deadline behaves like a failed origin fetch (existing
  failure path); the server/proxy layer serves the original by its
  native method, and the background completion warms the cache.
- **IPRO keeps its direct authorization posture.** URLs the
  `DomainLawyer` won't map still fetch direct (as before) rather
  than failing; only mappable URLs take the bounded path.
- **Dedup keys include request headers.** HTML-embedded and
  in-place fetches for one URL dedup together only when their
  request headers match; same-path concurrent fetches always
  collapse.
- **Per-worker pools.** Each nginx worker has its own pool and
  memory caches; there is no cross-worker dedup yet (shared-memory
  caches still need server integration, as before).
- **NDEBUG defaults.** Both deadline options default to 10ms
  (C++ release); C++ debug builds use 20ms.

## Benchmarks

`cargo bench -p ps-html --bench background` (criterion; sync legs
register no pool, i.e. pre-change behavior). Indicative numbers
from the dev machine:

| Benchmark | Sync | Async |
|---|---|---|
| `cold_first_request` (50ms origin, 10ms deadline) | 50.6ms | 10.5ms |
| `warm_hit` (steady-state metadata hit) | 68.9µs | 70.0µs |
| `concurrent_cold` (8 threads, 100ms origin) | 101.8ms | 11.3ms |

Bulk workload
(`crates/ps-html/examples/async_workload.rs`, 50 pages,
20ms origin, 10ms deadline, 1 pool thread):

| | Sync | Async |
|---|---|---|
| Cold pass (50 requests) | 1037ms | 531ms |
| Warm pass (50 requests) | 5ms | 6ms |
| Origin fetches | 50 | 50 |
| Timeouts / completions | 0 / 0 | 50 / 50 |
| Max RSS (`/usr/bin/time -v`) | 6384 KB | 6904 KB |
| CPU | 3% | 3% |

Run it with:

```sh
cargo build --release -p ps-html --example async_workload
/usr/bin/time -v ./target/release/examples/async_workload --pool --pages 50
/usr/bin/time -v ./target/release/examples/async_workload --no-pool --pages 50
```

## Limitations

- Optimization compute has no deadline: a slow *rewrite* (not
  fetch) still blocks the worker inline. Only origin latency is
  bounded.
- No mid-stream failure staging or async staleness freshening yet
  (`serve_stale_rewritten` revalidates inline, bounded by the
  deadline when a pool is registered).
- Queue-full and shutdown degrade to inline execution, which can
  exceed the deadline under saturation (visible via
  `background_jobs_queue_full` / `background_jobs_shutdown`).

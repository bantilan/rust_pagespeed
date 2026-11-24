# Shared-memory implementation ledger

## Mandatory resume protocol

1. Read this entire file before editing code.
2. Run `git status --short`, `git log -6 --oneline`, and review the diff. Preserve all unrelated changes.
3. Verify the last completed milestone's tests/build. Resume at the first incomplete checkbox, not at Phase 0.
4. Update this ledger after every completed task, before starting the next task. Commit only tested milestones; never push.
5. If interrupted, record partial edits and the exact next command under **Next Session Resume Point**.

## Objective and scope

Add bounded, cross-process shared resource/metadata caching and optimization coordination to the existing Rust PageSpeed implementation. Preserve all rewrite filters, disk persistence, TTL/purge validation, background queue bounds, HTML variant separation, browser-specific image handling, and per-request RUM behavior. Implement in independently functional opt-in milestones before changing defaults. No cache or background metadata optimization may fetch/process resource bytes on the HTML critical path.

## Starting repository state

- Baseline commit: `d0f055c` (README update), parent `9063416` (startup tables and lazy serving filters).
- Initial `git status --short`: clean.
- Existing `ps-shm` provides mmap shared counters, not a shared byte cache or process-wide job ownership.
- Earlier local measurements are historical context only; establish a fresh reproducible baseline before changing implementation code.
- No implementation code has been changed for this task yet.

## Architecture and decisions

### Existing and intended layers

- L1: existing bounded per-worker LRU, optional, cheap frequently used entries.
- L2: new bounded shared-memory byte cache, common to Nginx workers.
- L3: existing persistent file cache. It remains authoritative for persistence and cache codec validation.
- Resource HTTP and rewrite metadata codecs retain their existing expiration, option-signature, UA and purge semantics. The shared byte cache must not invent different freshness rules.
- Start opt-in. Measure capacity, contention, admission limits, startup overhead and fallback before selecting a default.
- `ps-shm` remains the narrow unsafe boundary. `ps-cache`, `ps-html`, and higher layers retain safe APIs.

### Proposed shared storage (confirm in Phase 0/1)

- Versioned, fixed-size mmap file; validate magic, layout version, capacity, alignment and bounds before attachment. Never resize a live mapping. Different incompatible layouts use distinct filenames.
- Shard both data and synchronization. Bound key/value admission and total memory. Prefer a simple bounded allocator/eviction scheme before introducing compaction; oversized entries bypass shared storage and remain available on disk.
- Synchronization must recover from process death, not merely thread panic. Evaluate Linux process-shared robust mutexes versus atomic publication; do not use unbounded spinlocks or lease expiry to grant concurrent access to non-atomic bytes.
- Never truncate/reinitialize a mapping still used by old workers. Open/map before use; workers can overlap across graceful reloads. Explicit full restart/recovery behavior must be tested.
- Shared slot/hash matches must compare complete keys; a hash collision is never a hit.
- Cross-worker coherence must cover put/delete/backfill and HTML publication; use a generation/publication protocol, including disk write ordering and invalidation after failed admissions.
- No long resource fetch/optimization while holding a cache mutex. Cross-process job ownership is a separate bounded table with owner identity/generation, completion and crash recovery.
- PID reuse and stale-owner writes require fencing; a timeout alone does not establish owner death.
- Beacon evidence requires transactional read/modify/write or equivalent synchronization. Shared storage alone does not prevent lost beacon updates.
- Checkpointing must not add disk flushes to HTML hits. Define optional checkpoint/restore independently from existing write-through disk persistence.

### Dependencies / questions to resolve with evidence

- Linux-specific robust synchronization support and portability/fallback behavior.
- Existing Nginx cache factory lifetime, pre-fork versus post-fork construction, reload and `nginx -t` behavior.
- Cache trait and adapter ordering; existing generation-based L1 coherence.
- BackgroundPool deduplication and owner-death behavior; original C++ shared-memory behavior/source.
- Beacon finder read/modify/write sites, HTML keys and cache publication gates.
- Config parser/directive inheritance, server/cache-path sharing, admin statistics registration.
- Permission/size/corrupt mapping failures must fall back safely without repeated filesystem operations per request.

## Acceptance criteria

- Existing tests continue passing; no functionality removed or reduced to make measurements look faster.
- Shared cache is byte-correct across independent processes, bounded under churn, and safe under abrupt writer death.
- TTL, delete, purge, options/UA variants, disk backfill and process-local L1 cannot serve invalid cached entries.
- Cross-process single flight demonstrably executes one optimization per key, with bounded ownership/queue tables and recovery from dead owners.
- Concurrent beacon reports preserve evidence; sampled nonce-bearing output is not frozen in cached HTML; ordinary RUM continues on cached HTML.
- Graceful reload has simultaneous old/new readers and writers without invalidating live mappings. Restart restore rejects stale/incompatible/corrupt state.
- Config supports explicit enable/disable/size and graceful failure; defaults are justified by measurements.
- Admin reflects aggregated cross-worker cache/job operations without falsely aggregating local-only histograms.
- Benchmark cold cache, warm HTML MISS and warm HTML HIT separately; retain raw data, output hashes, cache markers, binaries/config provenance and exact commands.

## Phase checklist

### Phase 0 — Audit and performance baseline [COMPLETED]

- [x] Inspect status/recent commits; record clean starting point.
- [x] Create this plan before implementation code edits.
- [x] Verify baseline build/test state; persist command logs.
- [x] Audit cache adapters/factory, shared-counter lifecycle, background jobs, beacon writes and reload handling; record file/line references and confirmed design.
- [x] Compare relevant original C++ cache/single-flight lifecycle source.
- [x] Capture baseline with current source and matched isolated Nginx; persist raw cold/MISS/HIT data and configuration/output evidence.
- [x] Commit the verified planning/audit/baseline milestone (commit command below; inspect recent log on resume).

### Phase 1 — Layout, allocation, synchronization and lifecycle [COMPLETED]

- [x] Specify binary layout, version/capacity validation, admission limits and error handling.
- [x] Implement mapping lifecycle without resizing/truncating live mappings.
- [x] Implement bounded allocation and recoverable cross-process synchronization.
- [x] Test attach, incompatible/corrupt headers, permissions, owner death and simultaneous initialization.
- [x] Run checks; update ledger; commit independently functional primitives (commit command/result recorded below).

### Phase 2 — Shared-memory cache backend [IN PROGRESS]

- [ ] Implement full-key lookup, insert, replace, delete and bounded eviction over Phase 1.
- [ ] Define oversized-entry/admission/eviction behavior and expose diagnostics.
- [ ] Verify independent processes, collisions, churn, writer death and bounded memory.
- [ ] Add safe `ps-cache` adapter with existing Cache trait semantics.
- [ ] Run checks; update ledger; commit tested backend (not yet enabled).

### Phase 3 — L1/L2/L3 integration/coherence [PENDING]

- [ ] Introduce opt-in factory wiring and preserve L3 persistence.
- [ ] Make reads/backfill/writes/deletes coherent across worker L1s, shared L2 and disk.
- [ ] Verify negative entries, expiration, purge, failed writes/admission, concurrent backfill and multi-worker races.
- [ ] Benchmark integration before changing defaults; commit tested integration.

### Phase 4 — Cross-process single flight/crash recovery [PENDING]

- [ ] Design bounded owner/fencing table keyed by options-aware rewrite identity.
- [ ] Integrate async resource and combination scheduling without HTML waits.
- [ ] Recover killed owners, queue rejection, panic, shutdown and PID reuse safely.
- [ ] Multiprocess duplicate-work and stale-owner tests; commit tested coordination.

### Phase 5 — Shared beacon evidence/HTML cache [PENDING]

- [ ] Audit/report all beacon evidence read/modify/write operations.
- [ ] Add transactional evidence merging without lost updates.
- [ ] Preserve per-request sampling/nonces, learned-evidence cache keys and cached-HTML RUM.
- [ ] Multiprocess reports/sampling plus HTML HIT/MISS regressions; commit.

### Phase 6 — NGINX config/defaults/fallback [PENDING]

- [ ] Add enable/size/admission configuration with inheritance and validation.
- [ ] Handle unsupported platform, permission, exhaustion, incompatible mapping and startup failure gracefully.
- [ ] Choose/document defaults using measured results, not assumptions.
- [ ] Config/unit/live isolated tests; commit.

### Phase 7 — Reload/checkpoint/restart recovery [PENDING]

- [ ] Test overlapping worker generations and configuration/layout size changes.
- [ ] Implement optional bounded checkpoint/restore if needed beyond authoritative disk persistence; document decision explicitly.
- [ ] Test full restart, killed workers, corrupt/truncated checkpoint and failed writes.
- [ ] Verify no hit-path flush/checkpoint I/O; commit.

### Phase 8 — Admin/aggregated statistics [PENDING]

- [ ] Add shared cache capacity/usage/hit/miss/eviction/failure and ownership/recovery counters.
- [ ] Expose admin diagnostics with accurate aggregation scopes.
- [ ] Verify shared counters across workers and reset/reload behavior; commit.

### Phase 9 — Concurrency/multiprocess/regressions [PENDING]

- [ ] Stress independent processes with mixed get/put/delete/eviction/expiry/purge workloads.
- [ ] Fault injection: kill owners during mutation, optimization, initialization and reload.
- [ ] Cover resource/image/CSS/JS/RUM/lazy/LCP/preload/responsive/HTML caching regressions.
- [ ] Full workspace tests and available sanitizers/model checks; document limits; commit.

### Phase 10 — Performance/docs [PENDING]

- [ ] Repeat matched baseline cold/MISS/HIT benchmarks, including multi-worker/concurrent requests.
- [ ] Attribute cache operations, initialization, parsing, resource processing, integration residual and total request time.
- [ ] Assess contention, memory use and redundant-work reduction; optimize only measured bottlenecks.
- [ ] Update README/config/admin documentation and report measured tradeoffs.
- [ ] Final tests/build, clean staged milestone commits, no push, final resume/completion state.

## Modified files and commits

- `SHARED_MEMORY_IMPLEMENTATION.md`: this ledger (new).
- No task commits yet. Do not include unrelated future user changes in milestone commits.

## Exact commands / latest results

Run from `/home/gamingph/Projects/Rust/pagespeed-rust` unless noted.

- `git status --short`: clean at task start.
- `git log -5 --oneline`: HEAD `d0f055c`, parents `9063416`, `c984b33`, `2bcaea5`, `cef5556`.
- `cat crates/ps-shm/Cargo.toml; head -95 crates/ps-shm/src/lib.rs`: existing dependencies `memmap2`, `fs2`; mmap counter protocol audited at high level, full cache audit pending.
- Build/test/baseline commands: not yet run for this task. Prior session results are not accepted as current verification.

## Known issues / blocked tasks

- No blockers established. Baseline tooling must use matching Nginx/module ABI: repository defaults target staged Nginx 1.31.8, while the installed service previously used 1.31.10.
- Permission to change live deployment is not required for isolated tests; do not reload or purge the live service just to establish a baseline.
- Do not claim shared storage alone solves beacon races, single flight, or L1 coherence.

## Next Session Resume Point

**State:** Phase 0 committed (`4d23d3e`); Phase 1 region implementation fully verified, milestone commit follows. Cache/Nginx behavior unchanged. Phase 2 first incomplete task.

**Exact next action:** implement bounded `ps_shm::byte_cache` over SharedRegion, with complete-key comparisons, explicit busy/admission results, bounded LRU replacement, deletion and oversized replacement invalidation. Start with fixed slots per shard (document memory-utilization tradeoff), keeping every copy under the shard guard so killed-writer recovery can clear a shard safely. Add an unwired ps-cache adapter and separate-process/concurrency/eviction tests. Do not wire Nginx or change defaults until Phase 3.


## Phase 0 audit findings (persisted before implementation)

- `crates/ps-nginx/src/caches.rs:140`: existing coherence is a 128-shard generation/active-writer counter table. Dead writers strand active counts and force disk bypass; this is safe degradation, not recovery.
- `caches.rs:SystemCachePath::new`, `SystemCaches::register_config/setup_caches`: factory owns per-path handles; initialization spans master/worker lifecycle. Metadata compression wraps the LRU/file composition, so shared bytes should occupy the same codec layer, not uncompressed evidence on a separate path.
- `crates/ps-cache/src/cache.rs:1268`: CacheCoherence has no transaction/CAS; WriteThroughCache serializes only local operations. Shared L2 integration cannot reuse plain nested write-through adapters without a cross-layer publication protocol. Cache::put returns no status; a new adapter must explicitly handle failed shared admission and stale entries.
- `crates/ps-shm/src/lib.rs:SharedCounters::open`: current counters use lifetime flock and may reset on uncontended startup; do not reuse this reset protocol for persistent cache data. Counter mapping filename sibling cleanup must not delete new byte-cache files.
- `crates/ps-html/src/background.rs:BackgroundPool::submit`: bounded local queue and local in_flight map only. HTML scheduling snapshots are Send-owned; remote single-flight ownership must wrap submissions without waiting on HTML.
- `crates/ps-nginx/src/beacon.rs:handle_beacon_evidence`, `crates/ps-html/src/filters/critical_images_beacon.rs`, `BeaconFinder::load/store`: separate read/modify/write races across processes. Phase 5 needs a shared transaction covering both nonce issuance and report acceptance, while preserving existing codecs and HTML evidence identity.
- C++ source: `../mod_pagespeed/pagespeed/kernel/sharedmem/shared_mem_cache.h` uses sectors, 4-way directory associativity, bounded blocks and explicit Initialize/Attach. Its `shared_mem_cache_data.cc:Sector::OnOwnerDied` disables a damaged sector because copying may occur outside its mutex. Rust can reset a shard after owner death only if every payload access stays inside the recoverable lock.
- C++ `shared_mem_lock_manager.cc:TryLockImpl` allows timeout stealing; use owner fencing for Rust optimization completion and do not apply timeout stealing to ordinary byte locks. Checkpoint support in C++ is optional (`RegisterSnapshotFileCache`).
- Phase 1 design confirmed: Linux process-shared robust pthread mutexes with bounded try-lock access; all shard payload access stays under the mutex. Owner-death recovery clears that shard before marking mutex consistent. No blocking cache lock acquisition on HTML. Unsupported platforms return startup errors for fallback. Initial allocation will use fixed aligned shard payload regions; Phase 2 specifies the bounded entry/block directory within them. Header initialization uses a bounded file lock, validates existing headers, and never truncates or reinitializes live mappings.

## Phase 0 verification results

- `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx > docs/performance/shared-memory/baseline-tests.log 2>&1`: exit 0; **2,046 passed, 0 failed**, 69 test-result groups including empty doc groups.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/baseline-build.log 2>&1`: exit 0; ordinary optimized release, 0.51 s incremental build.
- `python3 tools/benchmark_shared_memory.py --nginx /home/gamingph/Projects/Rust/.nginx-build/nginx-1.31.8/objs/nginx --module /home/gamingph/Projects/Rust/pagespeed-rust/target/release/libps_nginx.so --output docs/performance/shared-memory/baseline.json --samples 100 --cold-runs 5`: exit 0. Five fresh application-cache cold runs and 100 warm HIT/MISS requests per page; same static pages/config, OS caches warm. Moto cold **6.185 ms**, HIT **1.322 ms**, MISS **4.768 ms**. Aroma cold **10.026 ms**, HIT **1.463 ms**, MISS **7.293 ms**. Raw samples, normalized output hashes, module checksum and Nginx version in baseline.json. No live reload/purge.
- Added reproducible `tools/benchmark_shared_memory.py` and `docs/performance/shared-memory/nginx-template.conf`. Existing implementation code remains unchanged.
- Milestone commit: `git add SHARED_MEMORY_IMPLEMENTATION.md tools/benchmark_shared_memory.py docs/performance/shared-memory/nginx-template.conf docs/performance/shared-memory/baseline.json; git add -f docs/performance/shared-memory/baseline-tests.log docs/performance/shared-memory/baseline-build.log; git commit -m "Document shared-memory implementation phases and measured baseline"`. No push.

## Phase 1 work in progress

- Phase 0 commit: `4d23d3e`.
- Added draft `crates/ps-shm/src/region.rs`, exported only on Linux; added libc dependency. Header is 128 bytes; each shard has one 64-byte aligned mutex area plus fixed payload rounded to cache-line stride. Checked bounds: 1–256 shards, nonempty payload, total <= 1 GiB. Layout includes endian/word size/pthread ABI dimensions.
- Uses MmapRaw so concurrent shared bytes are accessed only through raw pointers inside the guard, never a globally aliased Rust slice. Shard guards are !Send/!Sync and recover EOWNERDEAD by clearing the shard. Try-lock contention immediately returns None. Initialization flock budget is 500 ms; partial nonempty initialization fails closed without reinitializing live pthread storage.
- Modified files: `crates/ps-shm/Cargo.toml`, `crates/ps-shm/src/lib.rs`, new `crates/ps-shm/src/region.rs`; Cargo.lock may change when dependency resolution runs. No Nginx/cache integration.
- Current verification: not compiled yet; tests still to add. Exact next action: compile/check `cargo test -p ps-shm`, add independent-process attach, killed-holder, contention, header corruption, size mismatch, allocation overflow, and panic recovery tests. Do not mark Phase 1 complete yet.

## Phase 1 verified tasks (commit pending final checks)

- `cargo test -p ps-shm > docs/performance/shared-memory/phase1-tests.log 2>&1`: **18 passed, 0 failed**, including existing shared counters and new separate-process tests. Initial compile failed due to an ambiguous closure Result error type; fixed with explicit io::Result.
- Verified checked allocation/overflow, disjoint shards, live mapping size mismatch refusal, reopen without reset, bad/partial header refusal, initialization deadline, 8 independent concurrent openers, actual SIGKILL holder recovery, neighboring shard preservation, panic clearing, 8-thread/8,000-increment serialization, and permission-denied propagation under the non-root development user.
- No value allocator or cache eviction yet: Phase 1 allocates bounded fixed shard payloads only. Phase 2 owns entry allocation inside these payloads.
- Removed the obsolete claim that ps-shm has only two unsafe sites; documented the new region guard boundary.
- Remaining before Phase 1 commit: inspect clippy log, repeat the process/thread tests, verify dependent crates still build/test, and update exact resume point.

- Phase 1 clippy: `cargo clippy -p ps-shm --all-targets -- -D warnings > docs/performance/shared-memory/phase1-clippy.log 2>&1`: exit 0, no warnings.
- Baseline metadata clarification: ordinary modules compile out pipeline/fetch instrumentation. Benchmark tooling now records `pipeline_instrumentation_available: false` and null fetch metrics, rather than implying zero fetches. Baseline latency samples are unchanged.

## Phase 1 completion

- `python3` loop invoking `cargo test -p ps-shm region::tests -- --test-threads=4` ten times: **100 region test executions passed**, 0 failed; log `docs/performance/shared-memory/phase1-stress.log`.
- `cargo test -p ps-cache -p ps-nginx --lib > docs/performance/shared-memory/phase1-dependent-tests.log 2>&1`: exit 0; 338 passed, 0 failed.
- `rustfmt --edition 2021 --check crates/ps-shm/src/region.rs crates/ps-shm/src/lib.rs` and `git diff --check`: passing before commit.
- Remaining limits: Linux-only primitive, no cache values/eviction yet, no Nginx integration, no new shared-memory defaults. A nonempty unpublished/corrupt initialization file intentionally fails closed until replaced offline; production fallback will be wired in Phase 6. Runtime owner death is tested and recoverable.
- Phase 1 commit command: `git add Cargo.lock crates/ps-shm/Cargo.toml crates/ps-shm/src/lib.rs crates/ps-shm/src/region.rs SHARED_MEMORY_IMPLEMENTATION.md tools/benchmark_shared_memory.py docs/performance/shared-memory/baseline.json; git add -f docs/performance/shared-memory/phase1-*.log; git commit -m "Add recoverable bounded shared-memory region primitives"`. No push.

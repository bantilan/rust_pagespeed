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

### Phase 2 — Shared-memory cache backend [COMPLETED]

- [x] Implement full-key lookup, insert, replace, delete and bounded eviction over Phase 1.
- [x] Define oversized-entry/admission/eviction behavior and expose diagnostics.
- [x] Verify independent processes, collisions, churn, writer death and bounded memory.
- [x] Add safe `ps-cache` adapter with existing Cache trait semantics.
- [x] Run checks; update ledger; commit tested backend (not yet enabled).

### Phase 3 — L1/L2/L3 integration/coherence [COMPLETED — explicit API only]

- [x] Introduce opt-in factory wiring and preserve L3 persistence.
- [x] Make reads/backfill/writes/deletes coherent across worker L1s, shared L2 and disk.
- [x] Verify negative entries, expiration, purge, failed writes/admission, concurrent backfill and multi-worker races.
- [x] Benchmark integration before changing defaults; commit tested integration.

### Phase 4 — Cross-process single flight/crash recovery [COMPLETED — opt-in domain]

- [x] Implement and verify conservative Linux PID/start-time liveness dependency.
- [x] Implement bounded exact-key owner/fencing primitive with protected publication and subprocess crash tests.
- [x] Specify cache-domain/options job identity and integrate coordinator with optimization publication.
- [x] Integrate async resource and combination scheduling without HTML waits.
- [x] Recover killed owners, queue rejection, panic, shutdown and PID reuse safely.
- [x] Multiprocess duplicate-work and stale-owner tests; commit tested coordination.
- [x] Recover abruptly killed coherent cache writers without persistent memory bypass or per-request liveness I/O.

### Phase 5 — Shared beacon evidence/HTML cache [COMPLETED — opt-in domain]

- [x] Audit/report all beacon evidence read/modify/write operations.
- [x] Add transactional evidence merging without lost updates.
- [x] Preserve per-request sampling/nonces, learned-evidence cache keys and cached-HTML RUM.
- [x] Multiprocess reports/sampling plus HTML HIT/MISS regressions; commit.

### Phase 6 — NGINX config/defaults/fallback [COMPLETED — explicit opt-in]

- [x] Add enable/size/admission configuration with inheritance and validation.
- [x] Handle unsupported platform, permission, exhaustion, incompatible mapping and startup failure gracefully.
- [x] Choose/document defaults using measured results, not assumptions.
- [x] Config/unit/live isolated tests; commit.

### Phase 7 — Reload/checkpoint/restart recovery [COMPLETED — disk remains authoritative]

- [x] Test overlapping worker generations and configuration/layout size changes.
- [x] Implement optional bounded checkpoint/restore if needed beyond authoritative disk persistence; document decision explicitly.
- [x] Test full restart, killed workers, corrupt/truncated checkpoint and failed writes.
- [x] Verify no hit-path flush/checkpoint I/O; commit.

### Phase 8 — Admin/aggregated statistics [COMPLETED]

- [x] Add shared cache capacity/usage/hit/miss/eviction/failure and ownership/recovery counters.
- [x] Expose admin diagnostics with accurate aggregation scopes.
- [x] Verify shared counters across workers and reset/reload behavior; commit.

### Phase 9 — Concurrency/multiprocess/regressions [COMPLETED — Linux tests; sanitizer limits recorded]

- [x] Stress independent processes with mixed get/put/delete/eviction/expiry/purge workloads.
- [x] Fault injection: kill owners during mutation, optimization, initialization and reload.
- [x] Cover resource/image/CSS/JS/RUM/lazy/LCP/preload/responsive/HTML caching regressions.
- [x] Full workspace tests and available sanitizers/model checks; document limits; commit.

### Phase 10 — Performance/docs [IN PROGRESS]

- [x] Repeat matched baseline cold/MISS/HIT benchmarks, including multi-worker/concurrent requests.
- [ ] Attribute cache operations, initialization, parsing, resource processing, integration residual and total request time.
- [ ] Assess contention, memory use and redundant-work reduction; optimize only measured bottlenecks.
- [x] Update README/config/admin documentation and report measured tradeoffs.
- [ ] Final tests/build, clean staged milestone commits, no push, final resume/completion state.

## Current milestone index

Historical chronological entries below retain their original status and test
results. They must not override the phase checklist or current resume point.

| Commit | Tested milestone |
|---|---|
| `ef9aeb5` | Audit fixes F1–F7, additional regressions/lifecycle coverage |
| `b989f48` | Current-build default/shared cold/HIT/MISS benchmarks |
| `83c0a82` | Remove NGINX resource coordination polling; 124 image + 12 terminal tests and release build |
| `5d434c9` | Explicit beacon reload and offline domain recovery; 74 cache tests |
| `f5e21a3` | Worker scaling/cache pressure matrix and bounded-queue probe |

See [current audit disposition](SHARED_MEMORY_AUDIT.md#current-status--supersedes-the-historical-assessment-below)
and [remaining work](docs/performance/shared-memory/REMAINING_WORK.md).
No remote pushes or live deployment changes were performed by these milestones.

## Latest verification and historical baseline

Commands run from `/home/gamingph/Projects/Rust/pagespeed-rust` unless noted.

- Latest ordinary release build: `cargo build -p ps-nginx --release`, passed in 22.01 s; matrix-build.log. Latest benchmark runtime code remains `83c0a82` (later additions are tests/examples/docs).
- Latest scoped regression: 74 cache tests passed (recovery-cache-suite.log); earlier resource-wait milestone passed 124 image and 12 terminal tests.
- Latest performance milestone: 24 worker/cache-pressure runs and seven queue probes passed; worker-matrix*.json and background-pressure-repeat.json. These do not substitute for a full workspace rerun after future source changes.
- Retained pre-follow-up full workspace evidence: phase10-final-workspace-tests.log, 2,920 primary passes plus two nested subprocess reports, 99 result groups. Audit follow-up separately records its own runs; do not present the older log as a fresh run of subsequent source changes.
- Initial baseline snapshot was `d0f055c`, clean tree; original command/audit observations remain in Phase 0 history. All later commands/results are recorded under their milestone headings.

## Known issues and limits

- Phase 10 remains incomplete: historical production attribution and direct duplicate-fetch/encode measurement are open.
- Installed Aerosky responds now; 20 warm enabled requests measured ~0.488 ms median. Installed module hash differs from staged; production strace attach was denied. No security settings changed.
- Module ABI must match NGINX: staged build targets 1.31.8, installed service previously 1.31.10. Do not reload or purge the live service for isolated tests.
- Shared tier stays opt-in. Initial migration from legacy protocol workers requires full stop/start. Test coverage is not a blanket production-readiness or sanitizer claim.

## Next Session Resume Point

**State after documentation reconciliation:** Phases 0–9 have implemented/verified milestones; Phase 10 remains IN PROGRESS. Items 1/2 and the worker/pressure measurement milestone are committed; item 4 now separates current status from historical findings. No incomplete source edits. Inspect recent commits; latest runtime source change is `83c0a82`, latest performance evidence `f5e21a3`.

**Exact next action:** Phase4 is IN PROGRESS, not complete. Current guard optimization compiles and passes171HTMLlibrary+29driver+26LCP+9srcset tests (phase4-tests.log). Matched NGINX1.31.10 release build29.53s and diagnostic release build37.40s pass. No commands running, no live deployment. First next action: run `target/release/examples/srcset_driver_latency > docs/performance/srcset/phase4-driver-after-guards.json` alone with no concurrent builds/tests. Compare with phase4-driver-before-guards.json; do not claim improvement before measuring. Then rerun browser, both beacon orderings and benchmark tools against current module, because stored browser/cache/beacon evidence predates guard changes. Commands below. Next peak RSS and final results/doc reconciliation; do not mark Phase4 done if unverified cases remain. Prior broader Phase10 latency attribution still separate. Read docs/performance/srcset/PLAN.md Next Session Resume Point for copyable commands.

**Verified build/test state:** Current ordinary release build passed (matrix-build.log); resource-wait image/terminal regressions and recovery cache suite passed at their respective milestones. Full workspace result in phase10-final-workspace-tests.log predates later follow-ups; do not claim it freshly verifies them. No background command is intentionally left running. Documentation-only reconciliation uses relative-link/anchor validation and git diff --check, without unnecessary test/build reruns.

**Deployment constraints:** shared tier remains opt-in; no consistent single-worker gain supports enabling it by default. Initial migration from legacy workers requires a full stop/start. Staged Nginx is 1.31.8; installed service previously 1.31.10, so do not install staged module blindly. Disk remains authoritative checkpoint. Exceptional writer-tracker overflow disables affected memory shards while preserving disk mutation; ordinary killed writers recover automatically.

**Known limits:** perf kernel events denied; no sanitizer run claimed. Isolated profiling residual includes Nginx, uninstrumented Rust, client/socket and scheduling costs. Background CPU/syscall totals overlap requests and cannot be added to HTML latency. Temporary module copies are under /tmp; hashes/build commands are persisted, so rebuild if removed. All new benchmark/profiling tools and raw evidence are included in the checkpoint commit.

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

- `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx > docs/performance/shared-memory/baseline-tests.log 2>&1`: exit 0; **2,045 primary cases passed, 0 failed** (plus one nested subprocess report), 69 test-result groups including empty doc groups.
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
- `cargo test -p ps-cache -p ps-nginx --lib > docs/performance/shared-memory/phase1-dependent-tests.log 2>&1`: exit 0; 337 primary cases passed, 0 failed (plus one nested subprocess report).
- `rustfmt --edition 2021 --check crates/ps-shm/src/region.rs crates/ps-shm/src/lib.rs` and `git diff --check`: passing before commit.
- Remaining limits: Linux-only primitive, no cache values/eviction yet, no Nginx integration, no new shared-memory defaults. A nonempty unpublished/corrupt initialization file intentionally fails closed until replaced offline; production fallback will be wired in Phase 6. Runtime owner death is tested and recoverable.
- Phase 1 commit command: `git add Cargo.lock crates/ps-shm/Cargo.toml crates/ps-shm/src/lib.rs crates/ps-shm/src/region.rs SHARED_MEMORY_IMPLEMENTATION.md tools/benchmark_shared_memory.py docs/performance/shared-memory/baseline.json; git add -f docs/performance/shared-memory/phase1-*.log; git commit -m "Add recoverable bounded shared-memory region primitives"`. No push.

## Phase 2 confirmed design / next work

- Phase 1 committed as `ff67357`.
- Initial byte backend uses configurable fixed slots per shard, with complete key+value bytes, serialized lengths/hash/LRU age and publication epoch. This has predictable bounds and no fragmentation/compaction, at a cost of internal slack; variable-size block allocation is a measured follow-up rather than an unverified first implementation. No production default is selected yet.
- Critical correctness finding: dropping a contended/oversized Cache::put must not leave an older value readable. Oversized replacements delete the old slot. A busy mutation will atomically advance a shard invalidation epoch outside the payload lock; lookups validate each slot epoch before and after copying. This invalidates old entries on failed writes without waiting on HTML. Epoch exhaustion permanently bypasses that shard rather than wrapping. Normal successful puts do not invalidate unrelated entries.
- The epoch uses the last 8 bytes of the existing 64-byte mutex allocation, outside the native pthread mutex object. Tighten the supported pthread ABI bound to <=56 bytes and document/disjoint-check this use. Mapped epoch access is atomic only; guarded payload references start after the 64-byte allocation.
- Phase 3 still needs an additional disk/backfill/L1 publication protocol; the byte backend alone does not solve those races.

- Phase 2 draft code now exists in `crates/ps-shm/src/byte_cache.rs`; not yet compiled/verified. Region epoch tail API added with bounds/ABI validation. Next action: add byte-cache regression tests (LRU, oversized/busy mutation invalidation, cross-process keys and crash recovery), then compile. Do not wire cache factory.

## Phase 2 verification progress

- Added safe `ps-cache::shared_memory::SharedMemoryCache` adapter, Linux-only and not factory-wired. It maps shared busy/error lookups to Overload and never introduces unsafe code above ps-shm. Added ps-cache -> ps-shm dependency (no dependency cycle).
- `cargo test -p ps-shm -p ps-cache > docs/performance/shared-memory/phase2-tests.log 2>&1`: first passing run completed after fixing a test-local function shadow and retaining older compiler compatibility for AtomicU64::fetch_update (scoped deprecation allowance). Extra multiprocess stress test added afterwards; rerun required.
- Busy invalidation operates only on the reserved atomic tail, outside all guarded payload slices. ABI limit is now <=56-byte native pthread mutex, preserving the 64-byte shard overhead. Slot key hashes are computed once per operation; complete-key comparison remains mandatory.
- Remaining before commit: rerun tests, clippy, stress and dependent compile.
- Phase 3 audit issue: existing shared-counter initializer resets on an uncontended lifetime flock; attached readers do not all retain lifetime locks. Reusing these counters blindly for persistent shared L2 could reuse generation numbers after restart or creator death. Before integration, design/test an incarnation/coherence protocol spanning live old/new workers and crash recovery. Do not simply nest adapters with resettable generations and claim correctness.

- Additional lifecycle safety decision before any integration: region format v2 stores Linux boot identity. Persisted pthread owner words from an earlier kernel boot cannot be recovered by the new kernel; attachment now rejects such mappings. A procfs boot-id read occurs only at open. Phase 6 must choose a boot/layout-qualified filename and fall back if procfs is unavailable. Same-boot process restarts/reloads preserve values. Added a simulated different-boot header test and shared epoch saturation test.

- Phase 2 tests passed after the boot check: `cargo test -p ps-shm -p ps-cache`; exact counts recorded at commit below. Plain `cargo clippy ... -- -D warnings` is blocked by pre-existing ps-config/ps-cache lints; left those files unchanged. `cargo clippy -p ps-shm -p ps-cache --all-targets --no-deps -- -D warnings -A clippy::new_without_default -A clippy::unnecessary_get_then_check` passed, with only the two existing ps-cache lints suppressed. Full failing log retained alongside scoped passing log.
- Header additionally distinguishes GNU versus musl native mutex ABI, not just size/alignment/endian. No shared-memory cache is enabled by these primitives.

## Phase 2 completion and exact verification

- New files: `crates/ps-shm/src/byte_cache.rs`, `crates/ps-cache/src/shared_memory.rs`.
- Modified: `crates/ps-shm/src/{lib,region}.rs`, `crates/ps-cache/src/lib.rs`, `crates/ps-cache/Cargo.toml`, `Cargo.lock`, this ledger. Raw checks saved under `docs/performance/shared-memory/phase2-*`.
- `cargo test -p ps-shm -p ps-cache > docs/performance/shared-memory/phase2-tests.log 2>&1`: **95 passed, 0 failed** (31 ps-shm, 64 ps-cache).
- Python loop invoking `cargo test -p ps-shm -- --test-threads=4` ten times: **310 passed**, 0 failed; `phase2-stress.log`.
- `cargo test -p ps-nginx --lib > docs/performance/shared-memory/phase2-dependent-tests.log 2>&1`: **274 primary unit cases passed**, plus one nested subprocess harness report, 0 failed.
- `cargo clippy -p ps-shm -p ps-cache --all-targets --no-deps -- -D warnings -A clippy::new_without_default -A clippy::unnecessary_get_then_check > docs/performance/shared-memory/phase2-clippy-scoped.log 2>&1`: passes. Unsuppressed failures in `phase2-clippy.log` are existing MemoryCache and cache model-test lints; no unrelated fixes made.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase2-build.log 2>&1`: **passes**, 27.55 s, ordinary optimized release against staged 1.31.8. This is not necessarily ABI-compatible with installed system Nginx; do not deploy blindly.
- `rustfmt --edition 2021 --check crates/ps-shm/src/{lib,region,byte_cache}.rs crates/ps-cache/src/{lib,shared_memory}.rs`; `git diff --check`: pass before commit.
- Verified: actual independent processes write/read the same mapping; mixed 4-process/8-thread mutation stress never exposes torn returned values; full-key comparisons defeat forged hash collisions; 10,000-key churn keeps mapping allocation fixed; admission and busy mutations invalidate old values; SIGKILL recovery clears damaged shards; cross-boot headers reject stale pthread state; atomic epoch saturates without wrapping.
- Cache policy: opaque bytes, no independent TTL decision, fixed slot LRU, explicit TooLarge/Busy/Miss outcomes. Fixed slots trade memory utilization for bounded predictable first implementation. This is NOT evidence of an end-to-end speedup yet: no production assembly/defaults changed. Phase 3 and later phases must assess integration performance and capacity.
- Baseline/counting clarification: baseline logs include one nested Nginx subprocess test result. Primary baseline count is 2,045, not the naive sum 2,046. The Phase 1 dependent primary count is 337 rather than naive 338. Raw logs retained unchanged.
- Commit: `git add Cargo.lock SHARED_MEMORY_IMPLEMENTATION.md crates/ps-shm/src/{lib,region,byte_cache}.rs crates/ps-cache/Cargo.toml crates/ps-cache/src/{lib,shared_memory}.rs; git add -f docs/performance/shared-memory/phase2-*.log; git commit -m "Add bounded shared byte cache and safe cache adapter"`. Do not push.

## Phase 3 audit handoff — confirmed generation lifetime defect

- Phase 2 committed as `6aaefe3`; all implementation code remains compiling and verified. No shared cache production wiring yet.
- New runnable diagnostic: `crates/ps-shm/examples/shared_lifetime_probe.rs`. Uses only a private temporary directory; never opens deployment paths.
- Exact command: `cargo run -p ps-shm --example shared_lifetime_probe > docs/performance/shared-memory/phase3-lifetime-probe.log 2>&1`.
- Result (exit 0): `surviving_reader_before=1`, `surviving_reader_after=0`, `generation_reset_while_reader_alive=true`. Dropping the original initializer releases its exclusive lifetime flock; another opener resets counters even though a previously attached handle still maps the file. This confirms the audit finding; it is not a passing correctness regression for desired behavior.
- Recommended next design constraints: every attached reader must retain lifetime participation; separate bounded initialization serialization from lifetime locking; attach/restart must expose a stable shared incarnation so old persistent L2 generation keys cannot collide after an intentional reset. Keep old/new worker compatibility explicit; do not silently reset a live segment or delete a different live generation's file. A distinct format/filename is safer than changing pthread/counter layout in place.
- Existing active-writer counts can still strand on a killed writer. Correct fallback to disk is acceptable during Phase 3, but Phase 4 must add fenced ownership/dead-owner recovery before declaring crash recovery complete.
- First Phase 3 implementation milestone should be the small lifetime/incarnation protocol and tests, separately committed before factory wiring. Then L1/L2/L3 composition can use shared generations consistently for both nested adapters. Verify reads/backfills across mutations, oversized admissions, deletes, TTL/purge and restart before claiming coherence.
- No approval or external access blocker. The remaining work is implementation, not a user decision. No remote push, live reload, cache purge or deployment performed.
- This diagnostic/audit commit records the resume point; it does NOT complete Phase 3.

## Phase 3 lifetime milestone design (before code changes)

- Add an explicit `SharedCounters::open_coherent` API with separate bounded initialization and shared lifetime locks. Every attached mapping participates in lifetime locking; only an opener proving no live mappings may reset counters. New incarnation is a randomized 128-bit namespace, preserved across attached readers and changed after full stop/start.
- Keep legacy `SharedCounters::open` and its filenames unchanged for existing stats/L1 users: changing the default format now would split coherence across mixed old/new binary workers. New protocol uses an isolated `coherent_counters_v2` subdirectory so old sibling cleanup cannot unlink its live files. Production migration remains for Phase 7, not silently applied now.
- Add desired-behavior regression tests against open_coherent and extend the diagnostic with `--coherent`. No Nginx wiring in this sub-milestone.

## Phase 3 lifetime implementation progress

- Added `SharedCounters::open_coherent` and optional `incarnation()` without changing legacy open behavior/default cache assembly. Every coherent handle owns a SH lifetime flock; open is serialized separately with a 500 ms bounded initialization lock. Full-stop reset creates a new randomized 128-bit namespace; attach preserves it. Files are under `coherent_counters_v2`, outside legacy sibling cleanup.
- Added tests for reader survival after creator drop, full-stop reset/new namespace, legacy cleanup isolation and an actual surviving subprocess after creator exit. Existing legacy diagnostic remains valid, while `--coherent` should report no reset.
- Current task: verify the tests and diagnostics; then commit this protocol as a Phase 3 sub-milestone. Factory wiring/coherence composition remains incomplete.

## Phase 3 lifetime sub-milestone verified

- `cargo test -p ps-shm -p ps-cache > docs/performance/shared-memory/phase3-lifetime-tests.log 2>&1`: 100 passed, 0 failed. New tests cover surviving attached reader/process, intentional full-stop reset with different incarnation, and legacy cleanup isolation.
- `cargo run -p ps-shm --example shared_lifetime_probe -- --coherent > docs/performance/shared-memory/phase3-lifetime-fixed-probe.log 2>&1`: before=1, after=1, reset=false. The legacy version of the same probe still reproduces the old defect; default behavior remains untouched for binary compatibility.
- `cargo clippy -p ps-shm --all-targets -- -D warnings > docs/performance/shared-memory/phase3-lifetime-clippy.log 2>&1`: passes without lint suppressions. Shared downgrade uses try_lock_shared as well; no blocking flock remains in the new API.
- Next independently testable milestone: opt-in factory API applying one coherent incarnation/generation domain to ALL SystemCachePath objects sharing a disk path before any backend escapes. Namespace memory layers only (L1/L2), preserve disk keys. Nested WriteThroughCache adapters must share the coherent generation source; reboot/layout-qualified shared segment filename lives under excluded .pagespeed-lru subtree. Configuration parsing/defaults remain Phase 6.

## Phase 3 factory milestone design (before code edits)

- Lifetime sub-milestone commit: `7cf9997`.
- Opt-in SystemCaches API will enroll every registered non-unplugged SystemCachePath sharing the same disk pathname. Reject enablement once any file backend handle escaped; reject duplicate enablement. Default assembly is unchanged until the API is explicitly called.
- Open shared bytes and coherent counters under `.pagespeed-lru/shared-v2/<Linux boot-id>/`; existing cleaner exclusion covers the entire subtree. Byte-cache filenames include allocation dimensions; the coherent counter table is independent of allocation size so simultaneous old/new size mappings still receive the same writes' generations.
- Prefix only L1/L2 keys with coherent incarnation; preserve persistent file keys. Both nested WriteThroughCache adapters use the same shared generation source; reads/backfills and writes/deletes retain existing checks.
- Mixed legacy/new coherence participants on the same disk path are not safe to deploy. Production directive/default and migration handling remain deferred to Phases 6/7; this milestone is an explicit API exercised by isolated tests only.

- Factory integration draft is now in `crates/ps-nginx/src/caches.rs`; first existing cache tests compile/run pending result. Added explicit opt-in enrollment, boot/layout-qualified files, memory-only incarnation prefixes and coherent L1/L2/L3 adapters. New tests cover all path variants, late-switch refusal, real child-process writes/delete/oversized replacement, L2 use without an L3 file, and numeric-generation reuse after restart with different disk content. No production directive/default changed.
- Exact next command: `cargo test -p ps-nginx --lib caches::tests`; inspect failures before proceeding. The current Phase 3 integration draft is not yet verified.

- Existing + first new factory tests passed (11 primary tests). Added canonical disk-path alias matching and enrollment inheritance for configurations registered later, avoiding mixed domains inside one factory.
- Added controlled concurrent backfill tests over a real shared byte backend and nested generation adapters: pause an old disk read, mutate/delete through a second worker adapter, then verify no old value can enter shared memory or be returned. Added compressed shared metadata tests preserving HTML expiry/purge and options/UA identity separation.
- Next verification command: `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx`, then ordinary release build and targeted factory stress before commit.

## Phase 3 current verification in progress

- All 11 first factory tests passed; new alias/late-registration and TTL/purge/variant tests plus the controlled nested-cache backfill test are now included in the broader run.
- Running `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx > docs/performance/shared-memory/phase3-integration-tests.log 2>&1` (build/linking phase; inspect log and active cargo sessions on resume). No test failure observed so far.
- Existing warnings in file_fetch.rs/send.rs and integration-test harnesses are unrelated; leave unchanged. Once this run passes, do an ordinary release build and targeted factory repetition, then commit the verified integration API.

## Phase 4 design notes to avoid repeating audit

- Acquire cross-process optimization ownership on pool workers, not HTML scheduling threads, so /proc/PID-reuse checks never add request-path filesystem work. Deduplicate exact options-aware partition/combination job keys; keep existing local bounded queues.
- Use a separate bounded owner table (not cache entry eviction slots). Owner identity includes PID plus process-start identity and boot incarnation; release/completion uses a fencing token. Do not steal a live owner's non-atomic state merely because a timeout expired.
- Job panic/queue rejection/shutdown must release ownership; killed-process recovery must distinguish dead owners from permission-denied/unknown liveness. If table is full/busy, serve originals without HTML waits.
- A transactional byte-backend primitive may also support Phase 5 evidence RMW, but ownership state must not be evicted as ordinary LRU data while jobs run. Completion must not let an old token overwrite a new owner's metadata. These are design notes, not verified implementations.

## Phase 3 measurements / prefix optimization in progress

- Broad tests passed: `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx`: **2,080 primary cases**, 0 failed; includes full HTML/resource regression suites.
- Ordinary release build passes (56.57 s). Factory suite repeated ten times: 120 primary factory cases passed, 0 failed. Scoped ps-shm/ps-cache clippy passes. Logs phase3-build.log, phase3-factory-stress.log, phase3-clippy-scoped.log.
- Added `crates/ps-nginx/examples/shared_cache_probe.rs`: 7 x 10,000 warm 4 KiB gets per case. `cargo run -p ps-nginx --release --example shared_cache_probe > docs/performance/shared-memory/phase3-cache-probe.json 2> docs/performance/shared-memory/phase3-cache-probe-build.log`. First results saved as phase3-cache-probe-before-prefix.json: file-only 4,940 ns; shared-L2/file 272 ns; L1/file 151 ns; L1/shared-L2/file 228 ns. These are isolated cache operations, not HTML latency.
- Measured extra prefix allocation hurt hot L1 by ~77 ns. Draft optimization: CacheCoherence gets a default memory_key formatter; coherent factory formats incarnation + generation + key in one allocation instead of nesting KeyPrepender. Physical cache key bytes and disk keys remain identical. Retest and repeat probe before committing. This adds modifications to `crates/ps-cache/src/cache.rs` (public trait method has a backwards-compatible default).

- Single-allocation prefix probe completed: L1/shared median fell from 228 ns to 170 ns; raw after-prefix data saved separately in phase3-cache-probe-after-prefix.json. File-only ~4,891 ns, shared L2 ~269 ns, L1-only ~134 ns in that run. Tiny absolute L1 overhead remains; no end-to-end speed claim.
- Additional concurrency fix in progress: coherent WriteThroughCache no longer holds its process-local operations mutex over disk I/O. Generation-keyed double-validation already makes backfill safe; non-coherent adapters retain the mutex. A controlled slow-writer regression checks an unrelated hot hit completes before the writer is released. Retest before commit; no assertion about the historical 13 ms production gap is inferred from this synthetic test.

## Phase 3 completion / final verification

- Modified: `crates/ps-nginx/src/caches.rs`, `crates/ps-cache/src/{cache,shared_memory}.rs`; new `crates/ps-nginx/examples/shared_cache_probe.rs`; this ledger and phase3 artifacts.
- Opt-in `SystemCaches::enable_shared_memory` enrolls all registered aliases/variants sharing a canonical disk path, and later registrations inherit that domain. Late switches after handle escape are rejected. Shared data/coherent lifecycle files live under the cleaner-excluded .pagespeed-lru tree, boot/layout qualified. Memory keys carry incarnation and generation; disk keys remain unchanged.
- Generation-aware get/put/delete no longer acquire the process-local operation mutex over filesystem I/O. Uncoordinated adapters still serialize. The controlled slow-writer regression proves unrelated hot hits finish before the writer is released; concurrent old-read replacement/delete tests prove stale backfill is not republished.
- `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx > docs/performance/shared-memory/phase3-integration-tests.log 2>&1`: **2,080 primary cases passed**, 0 failed, before the small final key-formatter/mutex fast-path change.
- Final `cargo test -p ps-shm -p ps-cache -p ps-nginx --lib > docs/performance/shared-memory/phase3-prefix-tests.log 2>&1`: **381 primary cases passed** (36 + 66 + 279), 0 failed; excludes one nested subprocess harness report.
- `cargo test -p ps-html --test async_progressive --test background_fetch --test css_combine --test js_combine > docs/performance/shared-memory/phase3-resource-regressions.log 2>&1`: **138 passed**, 0 failed after final fast-path changes.
- Ten `cargo test -p ps-nginx --lib caches::tests` repetitions: **120 primary factory cases passed** (plus 10 nested legacy subprocess reports), 0 failed. Earlier than formatter-only refinement; core topology unchanged.
- `cargo clippy -p ps-shm -p ps-cache --all-targets --no-deps -- -D warnings -A clippy::new_without_default -A clippy::unnecessary_get_then_check > docs/performance/shared-memory/phase3-clippy-scoped.log 2>&1`: passed. No new warnings; existing Nginx test warnings untouched.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase3-build.log 2>&1`: passes, 0.12 s incremental after the cache probe rebuilt the release library. Profiling feature disabled. Target ABI is staged 1.31.8, not an automatic deployment artifact for any system Nginx version.
- `cargo run -p ps-nginx --release --example shared_cache_probe > docs/performance/shared-memory/phase3-cache-probe.json 2> docs/performance/shared-memory/phase3-cache-probe-build.log`: passed; seven samples x 10,000 warm 4 KiB gets. Final medians:
  - file_only: 4986.688 ns/get.
  - shared_l2_file: 297.649 ns/get.
  - l1_file: 132.739 ns/get.
  - l1_shared_l2_file: 168.708 ns/get.

- Shared-L2 hit ~0.298 us versus warm file-only ~4.987 us. Fully hot L1 ~0.133 us without shared L2 versus ~0.169 us with it: a real ~36 ns overhead remains. Defaults are not selected from a microbenchmark. Initial prefix-only overhead (~228 ns shared L1) and intermediate single-allocation result (~170 ns) are retained in phase3-cache-probe-before-prefix.json / phase3-cache-probe-after-prefix.json. Final no-unrelated-I/O-wait change establishes a concurrency guarantee; tiny uncontended timing differences are not treated as statistically isolated latency improvements.
- HTML request baseline remains baseline.json. No new whole-request shared-cache result is claimed because Nginx configuration activation is intentionally Phase 6. Final multi-worker request-level profiling and defaults remain Phase 10.
- Known limitations: fixed-slot internal slack; killed active-write counters need Phase 4 recovery; mixed legacy/v2 participants need a migration strategy before production enablement; full workspace/fault/reload/checkpoint tests remain pending. No filters, resource codecs or background processing disabled.
- Final checks before commit: rustfmt on modified Rust files/examples and `git diff --check`. Commit command: `git add SHARED_MEMORY_IMPLEMENTATION.md crates/ps-nginx/src/caches.rs crates/ps-nginx/examples/shared_cache_probe.rs crates/ps-cache/src/cache.rs crates/ps-cache/src/shared_memory.rs docs/performance/shared-memory/phase3-cache-probe*.json; git add -f docs/performance/shared-memory/phase3-*.log; git commit -m "Integrate coherent L1 shared-memory L2 and persistent L3 caches"`. No push.

## Phase 4 identity sub-milestone — design before edits

- Resume audit: clean tree at db778c3; no repository AGENTS.md found. Preflight unit suite running in phase4-preflight.log.
- First independently testable dependency: Linux process identity = PID + /proc stat start-time ticks. Read current identity on the worker, never cache a pre-fork PID. Parse stat after its last closing parenthesis because comm may contain whitespace and parentheses.
- Liveness is three-state: matching non-zombie identity is Alive; kernel-confirmed absent process or mismatched start time is Dead; zombie leaders and permission/I/O/malformed identity are Unknown and must never authorize stealing. Identity is scoped to the region's already validated Linux boot.
- This primitive does not complete job fencing, publication, or active-writer recovery. Next sub-milestone remains a separate bounded owner table and publication protocol.

## Phase 4 identity dependency — verified milestone

- Added Linux-only `crates/ps-shm/src/process_identity.rs` and export in lib.rs. Current identity is read after fork, never inherited as a cached master PID. Matching PID/start ticks identifies the worker within the region boot.
- Conservative recovery: zombie leaders may retain live threads and therefore return Unknown. Procfs NotFound alone may reflect hidepid; require signal-zero ESRCH before declaring absence. Permission, malformed stat, interrupted reads and invalid zero PID return Unknown. No lease timeout authorizes stealing.
- Regression coverage: comm containing spaces/parentheses; malformed stat; PID reuse with mismatched start ticks; unknown errors; zombie leader state; actual independent killed/reaped process. Initial strict clippy found missing API docs/byte literal style, corrected. A stale zombie test expectation failed after conservative policy change; corrected and entire suite rerun successfully.
- `cargo test -p ps-shm -p ps-cache -p ps-nginx --lib > docs/performance/shared-memory/phase4-preflight.log 2>&1`: exit 0, 381 primary cases (plus one nested harness report).
- `cargo test -p ps-shm > docs/performance/shared-memory/phase4-identity-tests.log 2>&1`: exit 0, 40 cases, zero failures.
- `cargo clippy -p ps-shm --all-targets -- -D warnings > docs/performance/shared-memory/phase4-identity-clippy.log 2>&1`: exit 0, no suppressions.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase4-identity-build.log 2>&1`: exit 0, 22.00s optimized build. No deployment.
- `rustfmt --edition 2021 --check crates/ps-shm/src/{lib,process_identity}.rs` and `git diff --check`: required before commit.
- No runtime integration yet, so no new HTML latency result is claimed. Baseline and Phase 3 cache microbenchmark remain the measured performance evidence.
- Phase 4 remains IN PROGRESS. Owner-table layout, publication fencing, background integration, stranded coherence-writer recovery and subprocess deduplication remain pending; Phases 5–10 unchanged.
- Milestone commit command: `git add SHARED_MEMORY_IMPLEMENTATION.md crates/ps-shm/src/lib.rs crates/ps-shm/src/process_identity.rs; git add -f docs/performance/shared-memory/phase4-*.log; git commit -m "Add conservative Linux process identity for ownership recovery"`. No push.

## Phase 4 owner-table sub-milestone — design before edits

- Separate fixed owner table: one robust region shard, 1–1024 non-evictable running records, exact keys up to 1024 bytes. Full/oversized/busy outcomes are explicit; integrations must retain valid-original fallback and retry/fallback processing for unsupported keys.
- Allocate tokens from the region's saturating atomic epoch, outside resettable payload. Whole-shard owner-death/panic clearing revokes old records; later claims cannot reuse their tokens. Exhaustion fails closed. Never hold this lock during fetch/optimization.
- A `try_finish` closure checks exact token and holds the table lock through final publication, then clears the record. Callers must retry Busy completion/release on workers; there is intentionally no best-effort Drop that could strand a live PID record silently. Integration must implement bounded retry/deferred cleanup before this can ship.
- Liveness reads occur only during background claims; unknown owners remain protected. Dead unrelated owners are reclaimed only when capacity requires it. Byte-serving pool jobs are not affected.

## Phase 4 owner-table verification progress

- Added `crates/ps-shm/src/ownership.rs`, exported on Linux. One bounded robust shard, 1–1024 slots, 1024-byte exact job keys, explicit Busy/Full/KeyTooLong/Exhausted outcomes. Live/unknown owners are never evicted. Token serial comes from the non-resettable atomic epoch and cannot wrap; tokens also bind device/inode to reject accidental cross-table use.
- Final publication and token validation occur inside the same guard. No fetching/optimization belongs there. Panic/owner-death clears records but leaves token serial intact, revoking all old claims safely. Caller must explicitly retry contended completion/release; runtime integration is NOT implemented yet.
- Actual child-process tests verify protected duplicate job, SIGKILL outside/inside robust lock, newer token after recovery and rejection of stale publication. Additional tests cover full admission, long keys, exact identity, two independent handles, wrong mapping, panic revocation, lock contention, publication lock coverage and capacity mismatch.
- Broader testing exposed a pre-existing full-stop test assumption: concurrent fork/exec can temporarily inherit an open lifetime flock. The full-stop reset test now executes in an isolated single-test child, so it proves a genuine full stop rather than weakening live mapping safety.
- `cargo test -p ps-shm` to phase4-ownership-tests.log: 45 primary cases passed before final publication-lock test.
- Python loop of `cargo test -p ps-shm --lib`, 20 iterations to phase4-ownership-stress.log: 900 primary cases passed (plus 20 nested isolated test reports), before final publication-lock test.
- Final `cargo test -p ps-shm -p ps-cache -p ps-nginx --lib > docs/performance/shared-memory/phase4-ownership-dependent-tests.log 2>&1`: exit 0, 391 primary cases (46 + 66 + 279), plus two nested subprocess harness reports, zero failures.
- `cargo clippy -p ps-shm --all-targets -- -D warnings > docs/performance/shared-memory/phase4-ownership-clippy.log 2>&1`: exit 0, no lint suppressions.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase4-ownership-build.log 2>&1`: exit 0, 22.17s optimized build. No deployment or request-level benchmark claim.
- Remaining Phase 4 requirements: HTML coordinator/publication integration, bounded release/retry/shutdown cleanup, killed coherence-writer counter recovery, end-to-end duplicate work regression. This sub-milestone alone does not enable shared single flight in Nginx.

- Final scoped rustfmt and `git diff --check` pass. Commit: `git add SHARED_MEMORY_IMPLEMENTATION.md crates/ps-shm/src/{lib,ownership}.rs; git add -f docs/performance/shared-memory/phase4-ownership-*.log; git commit -m "Add bounded fenced cross-process ownership primitives"`. No push.

## Phase 4 coordinator resume — design before edits

- Resume: clean tree at 6ac47a7; no repository AGENTS.md found. Preflight `cargo test -p ps-shm -p ps-cache -p ps-nginx --lib` passed (391 primary cases), log phase4-coordinator-preflight.log.
- Close cleanup gap before integration: separate dedicated robust-region atomic revocation watermark alongside owner mapping. Atomic max(serial) fences that token and earlier claims without taking the publication mutex. Only use this conservative cancellation on failed release; normal release affects one record. Later claims lazily recycle revoked slots. No live claim can publish after validation observes revocation; publication already holding the mutex linearizes before replacement. Mapping files must remain together and must not be unlinked while live.
- Add protected `try_publish` that retains a claim across multiple metadata writes; `try_finish` still publishes/releases together. No fetching or optimization inside either closure.
- Introduce safe ps-cache coordination traits and Linux implementation; claims and bounded publication retries run only on background workers. RAII release cancels atomically on contention/error, including panic, with no cleanup queue or unbounded wait.
- HTML workers will wrap their metadata backend in a lease-aware adapter. Every metadata mutation (including nested CSS/image metadata) validates ownership under the publication lock; gets preserve codecs and validation. Keep immutable content-addressed HTTP outputs on existing paths. Queue rejection happens before acquiring cross-process ownership.
- Coordinator is explicit opt-in ServerContext state until Nginx factory/config integration is verified. Shared table path supplies cache-domain isolation; partition keys already include effective options/UA. Unsupported long keys use existing local optimization; Full/Busy owners shed for retry on subsequent HTML requests.

## Phase 4 coordinator dependency milestone — verified

- Added `ps-cache::rewrite_coordinator` traits, Linux SharedRewriteCoordinator, and LeasedMetadataCache. Claims are worker-only; metadata publication retries have explicit budget (max 500ms); release has no wait and uses atomic revocation if contended. Long keys retain local processing; normal capacity/contention retries on later requests.
- Owner v2 payload adds 8 reserved bytes beyond fixed records so v1 header dimensions cannot match any v2 capacity. Use a new owner filename; the adjacent revocations file is part of the domain. No runtime deployment currently uses either format. Symlink aliases canonicalize; hard-link aliases are unsupported and documented.
- Regression tests prove contended release reclaims capacity, earlier revoked tokens cannot publish, late cleanup cannot revoke a newer token, multiple writes retain ownership, cache-domain/options separation, bounded publication failure, and panic cleanup. Publication already holding the mutex completes before any replacement can claim.
- `cargo test -p ps-shm -p ps-cache > docs/performance/shared-memory/phase4-coordinator-tests.log 2>&1`: exit 0, 116 primary cases (47+69), plus one nested subprocess report.
- `cargo clippy -p ps-shm -p ps-cache --all-targets --no-deps -- -D warnings -A clippy::new_without_default -A clippy::unnecessary_get_then_check > docs/performance/shared-memory/phase4-coordinator-clippy.log 2>&1`: passed; suppressions cover existing ps-cache lints only.
- Modified: ps-shm region/ownership, ps-cache lib/new rewrite_coordinator, ledger/logs. Runtime HTML integration follows as a separate milestone; no request latency/defaults claim.

## Phase 4 HTML worker integration — verification in progress

- Lease dependency committed as a707279. Added optional coordinator OnceLock to ServerContext and Send-owned snapshots. Only run_rewrite_job (pool worker) claims; no-pool embeddings and foreground/IPRO paths remain unchanged. Queue rejection/shutdown occurs before claims. Successful-job stats now count only accepted metadata publication.
- Worker-local LeasedMetadataCache fences all metadata mutations, preserving the existing backend/codec/validation and resource optimization logic. The separate HTTP cache continues publishing normal resource outputs; no fetch/optimization is performed while holding the ownership lock.
- Opt-in enable_shared_memory now allocates one canonical path owner domain (256 slots, 5ms worker publication-contention budget) and shares it across aliases/variants/later registrations. ContextCaches passes it through Nginx worker init. No directive/default enabled; these provisional parameters remain Phase 6 configuration work.
- `cargo test -p ps-html --lib html_async > docs/performance/shared-memory/phase4-html-worker-tests.log 2>&1`: 8 passed, including real shared coordinator cold-original/warm-optimized CSS and inline views, duplicate rejection, no-pool/shutdown paths, actual separate worker processes doing one fetch for concurrent same-key work, and SIGKILL of an optimizer while origin fetch is gated followed by successful replacement and optimizable CSS metadata publication. Initial test compile used nonexistent FileCachePolicy::default; replaced with explicit isolated policy and reran successfully.
- `cargo test -p ps-nginx --lib caches::tests > docs/performance/shared-memory/phase4-factory-coordinator-tests.log 2>&1`: 12 primary passed plus one nested subprocess report. Tests verify late aliases share coordinator claims as well as byte coherence.
- Broad four-crate regression running: `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx > docs/performance/shared-memory/phase4-worker-integration-tests.log 2>&1`. Inspect completion before committing.
- Modified: ps-html html_async/server_context, ps-nginx caches/module, ledger/logs. Exact next action if interrupted: inspect cargo sessions/log, finish broad tests/release build/scoped lint and stress, then commit this worker integration. Killed coherence-writer counter recovery is still pending, so Phase 4 is not complete.

## Phase 4 worker integration — verified milestone

- Broad `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx > docs/performance/shared-memory/phase4-worker-integration-tests.log 2>&1`: exit 0, **2,098 primary cases**, plus two nested subprocess reports; 70 result groups. This compiled the 8-test html_async suite before the final test-only combination addition.
- Final `cargo test -p ps-html --lib html_async > docs/performance/shared-memory/phase4-html-worker-tests.log 2>&1`: **9 passed**, including coordinated CSS and JS combination cold-original/warm-combined behavior added after broad compilation. The process test verifies one origin fetch during duplicate work, successful optimized CSS partition URL (not merely any/negative metadata), and replacement after SIGKILL during origin fetch.
- `cargo clippy -p ps-shm -p ps-cache --all-targets --no-deps -- -D warnings -A clippy::new_without_default -A clippy::unnecessary_get_then_check > docs/performance/shared-memory/phase4-worker-clippy.log 2>&1`: exit 0, existing lint suppressions only.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase4-worker-build.log 2>&1`: exit 0, **40.28s** optimized build targeting staged Nginx 1.31.8. Not installed or deployed.
- Scoped rustfmt and git diff --check pass. No resource optimization/filter/RUM features removed; broad regression includes image, CSS/JS, async, HTML caching and beacon tests. No new whole-request latency claim: production activation remains Phase 6 and matched profiling Phase 10.
- Known tradeoff: contended/panicking cleanup may revoke earlier live jobs in the same owner domain. They cannot publish stale metadata; later HTML requests retry. This bounds cleanup without per-request waits or an unbounded cancellation queue, at a possible cost of wasted background work under contention. Measure and expose this in Phases 8/10.
- Phase 4 still incomplete: coherent cache-writer counts can remain active after SIGKILL. The next resume action addresses this separate failure; do not mark crash recovery complete solely from optimization-owner tests.

- Python loop of `cargo test -p ps-html --lib html_async` ten times, log phase4-worker-stress.log: **90 primary cases passed**, including 20 parent kill/dedup scenarios and their worker subprocesses.
- Commit: `git add SHARED_MEMORY_IMPLEMENTATION.md crates/ps-html/src/{html_async,server_context}.rs crates/ps-nginx/src/{caches,module}.rs; git add -f docs/performance/shared-memory/phase4-{factory-coordinator-tests,html-worker-tests,worker-integration-tests,worker-clippy,worker-build,worker-stress}.log; git commit -m "Coordinate HTML background rewrites across worker processes"`. No push.

## Phase 4 writer recovery — design before edits

- Resume: clean d7fe0a3; preflight four-crate unit verification running in phase4-writer-preflight.log. No existing runtime code changed yet.
- Replace aggregate count crash accounting with a bounded writer-slot primitive first, independently verified before adapter integration. Each slot holds a robust pthread mutex for the entire disk mutation. A per-cache-shard atomic bitmap records active slots; a separate monotonic generation invalidates memory after completion/recovery. Robust slot ownership proves an interrupted thread is gone without PID/procfs reads.
- Order: acquire a free robust slot; repair stale bitmap references to that slot; publish active bit; return guard to caller; execute disk mutation; increment generation; clear bit; unlock. Every death boundary remains either inactive-before-any-write, active/recoverable, or completed with a new generation. Recovery holds the same slot mutex, so it cannot steal a live operation. No payload journal can lose another writer: only reserved atomic words are authoritative.
- Hot snapshot: active bitmap, generation, active bitmap; no lock/syscall/filesystem work. Explicit nonblocking recovery attempts only candidate active slots. Slot reuse repairs all stale references before publishing any new active bit. Generations saturate and disable memory admission rather than wrap.
- Initial primitive bounds: 64 concurrent disk writers and 128 key shards (~41 KiB mapped). All slots busy returns explicit admission failure; do NOT wire it to a void Cache::put by silently dropping replacements/deletes. Integration must define bounded admission/overload and preserve nested L1/L2 write-through without recursive claims. This milestone is a tested protocol dependency, not completed cache-writer integration.

## Phase 4 writer tracker — verified protocol progress

- Added Linux `ps-shm::write_tracker::{WriteTracker, TrackedWrite}`; atomic bitmap operations remain inside ps-shm's unsafe boundary. Three dedicated regions total 41,344 bytes. Disk mutations have 64 slots and 128 key shards. Hot snapshots are atomics only; nonblocking recovery uses robust slot locks and repairs every reference before slot reuse. Runtime CacheCoherence remains unchanged until admission/adapter integration is solved.
- `cargo test -p ps-shm > docs/performance/shared-memory/phase4-writer-tracker-tests.log 2>&1`: 53 primary passed, zero failures, plus one isolated counter-lifetime subprocess report.
- Added independent-process SIGKILL at before-active, after-active, after-disk, after-generation and after-bit-clear boundaries. Tests preserve old/new disk bytes as appropriate and recover usable stable generations. Terminated-thread (same live process) tests bypass Rust destructors, proving kernel owner-death recovery and preservation of a different live overlapping writer. Also verify capacity, panic cleanup, slot reuse across key shards, invalid indices, generation exhaustion/no wrap, and exact allocation.
- Twenty `cargo test -p ps-shm write_tracker::tests -- --test-threads=4` iterations passed: 120 primary executions, including 100 subprocess kill-boundary scenarios; phase4-writer-tracker-stress.log.
- Added release microbenchmark example write_tracker_probe comparing current counter snapshot/begin-end and proposed protocol, with seven raw samples, fixed iteration counts and explicit protocol-only scope. Initial JSON format literal had an unmatched closing brace; corrected before rerunning. Re-run strict clippy after that correction; its earlier run caught the same example compile error. No request-level or enabled-cache performance improvement is claimed.
- Before committing inspect phase4-writer-tracker-{dependent-tests,clippy,build,probe-build}.log and probe JSON. Exact next action remains CacheCoherence transactional integration with bounded admission and nested write-through handling, NOT phase 5. Do not treat the existence of this primitive as repaired production writer counters.

## Phase 4 writer protocol dependency — final verification

- `cargo test -p ps-shm -p ps-cache -p ps-nginx --lib > docs/performance/shared-memory/phase4-writer-preflight.log 2>&1`: 395 primary cases passed (47+69+279), plus two nested subprocess reports, clean starting d7fe0a3. No repository AGENTS.md found.
- `cargo test -p ps-shm -p ps-cache -p ps-nginx --lib > docs/performance/shared-memory/phase4-writer-tracker-dependent-tests.log 2>&1`: **401 primary passed** (53+69+279), plus two nested subprocess reports; zero failures.
- `cargo clippy -p ps-shm --all-targets -- -D warnings > docs/performance/shared-memory/phase4-writer-tracker-clippy.log 2>&1`: exit 0, no suppressions after fixing the example format literal.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase4-writer-tracker-build.log 2>&1`: exit 0, **22.43s** optimized build, not deployed.
- `cargo run -p ps-shm --release --example write_tracker_probe > docs/performance/shared-memory/phase4-writer-tracker-probe.json 2> docs/performance/shared-memory/phase4-writer-tracker-probe-build.log`: exit 0. Seven samples, 250,000 snapshot iterations/sample and 100,000 mutation/recovery iterations/sample. Median uncontended protocol cost: legacy snapshot **1.788ns**, tracker snapshot **0.908ns**; legacy begin/end **5.557ns**, tracker begin/end **62.770ns**; idle recovery **1.791ns**. New writes cost ~57ns more in this isolated run. These constants/inlined atomic microbenchmarks exclude cache-key hashing, disk, contention, adapter formatting and HTML; they do not establish end-to-end gains or justify defaults. Raw samples retained.
- Scoped rustfmt and git diff --check required before commit. Modified: ps-shm lib/region, new write_tracker module/example, ledger and phase4-writer-* artifacts. No existing runtime cache path changed.
- Phase 4 remains IN PROGRESS: adapter transaction/admission integration is pending. Exact next action updated above. No uncommitted partial runtime integration should remain after committing this independently tested protocol.

- Milestone commit command: `git add SHARED_MEMORY_IMPLEMENTATION.md crates/ps-shm/src/{lib,region,write_tracker}.rs crates/ps-shm/examples/write_tracker_probe.rs docs/performance/shared-memory/phase4-writer-*; git commit -m "Add crash-consistent bounded disk-writer tracking primitives"`. No push. All current protocol tests/builds verified; runtime integration remains the first incomplete Phase 4 task.

## Phase 4 transaction integration — design before edits

- Resume e8a4e00, clean status. Introduce object-safe CacheCoherence::mutate callback with backwards-compatible default begin/end guard; tracked implementation holds native guard on the same stack through disk mutation. Nested memory adapters may independently register overlapping claims; no borrowed guard escapes or thread-local recursion state is needed.
- Capacity/error fallback must preserve void Cache put/delete semantics: atomically disable memory generations for that shard before executing the disk mutation. Saturation fails closed for memory caching, while disk writes/deletes remain functional. This explicit, persistent bypass is exceptional capacity exhaustion, not ordinary killed-writer recovery. Expose/document diagnostics in Phase 8; no silent dropped writes. Full-stop new protocol domain/incarnation restores normal caching.
- Snapshot normally uses atomics only. On active bitmap, attempt nonblocking robust-slot repair once; live writers remain protected; no sleeps, PID or filesystem checks. Use distinct tracker v1 subdirectory and memory namespace; keep legacy default unchanged.

## Phase 5 evidence transaction — design before edits

- Phase 4 adapter tests pass; broader regression/release/cache microbenchmark running. Phase 5 begins with byte-cache transaction support; do not claim phase completion before beacon tests.
- Add Cache::update callback: default preserves existing embedding get/put semantics; shared metadata gets an outer transaction adapter (after compression) using 128 robust try-lock shards. Transaction covers read/decode/change/encode/write; same exact evidence key maps to same mutex across workers. Busy returns false without invoking callback; callers must skip nonce issuance/report acknowledgement, not fallback to an unprotected update. No new wait on HTML. MemoryCache test backend implements atomic update under its existing entries mutex.
- Evidence operations to migrate: prepare sampling/nonce; end-document candidate registration; report nonce validation/merge. Immutable evidence snapshots still feed HTML-cache identity; sampled nonce-bearing output remains in existing no-cache path. Ordinary RUM bootstrap unchanged.

## Phase 4/5 integration progress and validation caveat

- TrackedCoherence and transaction-scoped CacheCoherence::mutate now protect nested L1/shared-L2/file mutations. LifetimeCoherence retains full-stop incarnation participation. Default legacy assembly remains unchanged; opt-in memory keys use shm-tracked-v1. Capacity overflow disables memory for that shard before preserving disk put/delete; disabled tracker segments require offline replacement/new tracker domain (incarnation prefix alone does NOT reset the stable tracker files). This corrects the earlier design note's reset assumption.
- Actual ps-cache subprocess regression kills a writer after disk replacement/deletion but before completion. Parent's warm nested caches return new/missing bytes and regain stable memory generations. A deliberate subsequent disk-only removal proves recovered memory admissions, rather than permanent safe disk bypass. Capacity exhaustion regression proves replacement/deletion are preserved.
- `phase4-tracked-integration-tests.log`: 403 primary unit cases passed (53+71+279), plus two nested reports, before transaction primitive additions. `phase4-tracked-clippy.log` passes with existing ps-cache suppressions. `phase4-tracked-factory-tests.log`: 12 primary passed (plus one nested report).
- Phase 5: Cache::update, MemoryCache atomic override, Linux TransactionalCache outer metadata wrapper, BeaconFinder::update, transactional nonce issuance/candidate update/report acceptance implemented. Domain mappings are opened once in factory and shared across configuration aliases; wrapper sits outside compression so same read/decode/write transaction preserves codecs.
- Four subprocesses each issue/accept/replay 32 unique nonces: regression verifies 128 accepted reports, exactly 128 generation increments, no pending nonce loss, all replayed nonces rejected. Nonblocking lock contention returns no result; no report/nonce state mutates outside the lock. Transaction adapter concurrency test verifies 2,000 increments with two independent mappings and no invoked contended callback.
- One early release-probe attempt failed after a source edit raced compilation of a dependent crate (old BeaconFinder lacked new update API); not a passing measurement. Its empty JSON is not performance evidence. All final checks now run on a fixed source snapshot; rerun release probe to phase5-cache-probe.json and retain final regression/build logs.
- `phase5-transactions-unit-tests.log` had overlapping command runs and duplicate result text; primary verification uses the new uniquely named phase5-final-regressions.log. Do not sum duplicate reports or use old failed/empty probe artifacts.
- Original C++ reference revisited: system_caches.cc CreateShmMetadataCache indexes explicit caches by FileCachePath and separately supports an optional default; fixed Rust slots differ from C++ block allocation. Directive/default work remains Phase 6.

## Phase 4/5 final resumed verification

- The IDE/session interruption terminated the earlier full regression/release processes before their result lines. Commands resumed to fresh logs; no incomplete log is treated as passing verification.
- `cargo test -p ps-shm -p ps-cache -p ps-html -p ps-nginx > docs/performance/shared-memory/phase5-resumed-regressions.log 2>&1`: exit 0, **2,109 primary cases**, plus two nested subprocess reports (sum 2,111), 70 result groups, zero failures. Includes transaction, killed disk writer, multiprocess evidence, critical beacon, cached-HTML instrumentation/RUM, image, CSS/JS, lazy/LCP/preload/responsive regressions.
- `cargo clippy -p ps-shm -p ps-cache --all-targets --no-deps -- -D warnings -A clippy::new_without_default -A clippy::unnecessary_get_then_check > docs/performance/shared-memory/phase5-resumed-clippy.log 2>&1`: passes, existing lint suppressions only.
- `cargo run -p ps-nginx --release --example shared_cache_probe > docs/performance/shared-memory/phase5-resumed-cache-probe.json 2> docs/performance/shared-memory/phase5-resumed-cache-probe-build.log`: exit 0. Final median warm 4KiB get: file-only **4,951.801ns**, shared-L2/file **279.380ns**, L1/file **155.999ns**, L1/shared-L2/file **204.919ns**. Compared with Phase 3's ~168.708ns shared-L1 hit, ~36ns additional absolute overhead in this run; different run/no isolation, not a statistical attribution. Shared-L2 remains ~18x less than warm file-only. Preserve raw samples and avoid HTML latency claims.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase5-resumed-build.log 2>&1`: passes, 0.08s incremental after probe rebuilt release. No deployment or push.
- `send::tests::html_sink_builds_filters_only_past_lookup` verifies cached HTML retains per-load JS timestamp, instrumentation init and beacon endpoint while HIT builds no filters; critical-images beacon integration and 4-process evidence update tests pass.
- Shared bytes are still explicitly opt-in API only. Defaults/config activation, old/new binary migration, live Nginx reload tests, admin aggregation, complete final profiling remain incomplete Phases 6–10.

## Phase 6 configuration — design before edits

- Phases 4/5 committed cd6c507. Add process-scope CreateSharedMemoryMetadataCache using explicit FileCachePath name, size in KiB. 0 disables that named cache; accepted enabled range 1MiB–256MiB. Fixed 16 shards, 16KiB combined key/value slots, dimension count chosen so mmap allocation does not exceed requested bytes. Tracking/ownership/transaction overhead is separately bounded and documented.
- Factory stores requests while parsing; root_init enrolls all contexts before handles escape. Unsupported OS, missing named path, permission/layout/startup errors log a reason and retain the existing disk/L1 backend without per-request retries. Do not enable defaults automatically: local L1 already defaults on, and shared coherence has a measured hot-L1 overhead.
- Activation from legacy binaries/coherence requires full stop/start; no live deployment modified here. New-protocol reload/layout changes remain Phase 7 tests. Directive semantics compared with original system_caches.cc; explicit cache names match FileCachePath. Rust additionally accepts 0 as explicit disable and bounds allocation range.

## Phase 6 final verification and measured requests

- Added DriverFactory named shared-cache requests, 0=disabled, 1024..262144KiB enabled validation, fixed-slot dimensions bounded by byte budget, startup enrollment of all canonical path contexts, once-at-startup fallback logging. Existing lower-scope ignore behavior preserved. Fixed 16KiB key+value admission; oversized entries retain disk persistence. No automatic shared default.
- `cargo test -p ps-nginx --lib factory::tests > docs/performance/shared-memory/phase6-config-tests.log 2>&1`: 17 passed. `cargo test -p ps-nginx --lib > docs/performance/shared-memory/phase6-nginx-tests.log 2>&1`: 280 primary passed plus one nested report. Budget tests verify actual RegionLayout bytes <= configured budget at min/default/max; enable/zero-disable/unregistered-path fallback covered. Region permission/layout/corruption tests from earlier phases remain passing.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase6-build.log 2>&1`: passes, 5.51s optimized. Scoped clippy passes (phase6-clippy.log). Python benchmark syntax verified.
- `python3 tools/benchmark_shared_memory.py --nginx /home/gamingph/Projects/Rust/.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/phase6-default-requests.json --samples 100 --cold-runs 5` and same with `--shared-kb 16384 --output docs/performance/shared-memory/phase6-shared-requests.json`: both exit 0; stdout phase6-request-benchmarks.log. Fresh isolated processes/caches, OS caches warm; no installed server changes. Shared runs verify all expected mapping files exist, refusing silent fallback. Module SHA identical for both: 24ed32bf5b72f65dcfed977a982cb5e2a6b6f0aa0a3be345fea26a61fa8114b0.
- Current default (cold / HIT / MISS ms): Moto **4.299 / 0.452 / 1.843**; Aroma **5.709 / 0.512 / 2.627**. Shared16MiB: Moto **4.469 / 0.633 / 1.829**; Aroma **5.645 / 0.488 / 2.668**. No consistent single-worker win, so shared defaults remain off. Each has five cold samples and 100 warm HIT/MISS samples, raw data retained; not a statistical multi-run conclusion. Historical baseline used another runtime state and must not be used to attribute these overall lower medians to shared caching.
- Output quality: default/shared Moto normalized SHA 889b7c10af1d2ce7c7c158cfe6700347dc8e5d8169fd0df84ce58ae125386298, 15031 bytes, 15 pagespeed refs; Aroma SHA c691571e4644b28bebd771d586a7625b62b3bdcb8a3848cd87b5216a4847fcf6, 34068 bytes, 23 refs. Exact equality, no removed optimizations. Ordinary build lacks pipeline instrumentation, so residual latency is not attributed yet.
- Benchmark records module/config hashes and now records dirty source state; these runs used uncommitted Phase 6 factory on cd6c507, followed by this milestone commit. Added CONFIGURATION.md and README link covering admission, separate overhead, conservative fallback, C++ naming comparison and mandatory initial full restart.
- Phase 7–10 remain incomplete. No deployment, push or unrelated changes discarded.

## Phase 7 reload/recovery — design before edits

- Phase 6 commit 71bf7f5. Add isolated Nginx lifecycle validation using the matched staged executable/module, two workers and a real master. Keep a throttled old-worker response active while SIGHUP switches shared allocation size, then verify simultaneous old/new worker existence and valid optimized HTML. Stop/start all workers on the same cache directory and verify persisted optimized HTML survives.
- Existing disk write-through is the authoritative checkpoint: do not serialize native pthread mutex bytes into a portable checkpoint. Same-boot mapped bytes are opportunistic; boot identity/layout checks reject unsafe state, missing memory reconstructs from validated disk entries. Optional separate byte checkpoints add no correctness benefit yet and are intentionally not implemented. Document this decision and measure restart behavior.
- Full legacy-to-new activation remains full-stop-only; graceful overlap test covers the same new coherence protocol, not unsupported mixed old binaries.

## Phase 7 final verification

- Added `tools/verify_shared_lifecycle.py`: private prefix/listen port, real master/two workers, held throttled static response guarantees old worker remains alive during SIGHUP. A changed budget creates another bounded byte layout while retaining the same writer/coordinator domain. New-worker PID must differ from both original PIDs; master child list proves generation overlap. Then complete stop/start verifies disk-backed optimized HTML persists. Cleanup signals only the isolated process group, including failure paths.
- `python3 tools/verify_shared_lifecycle.py --nginx /home/gamingph/Projects/Rust/.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/phase7-lifecycle.json > docs/performance/shared-memory/phase7-lifecycle.log 2>&1`: exit 0. Raw JSON includes prefix, module SHA, before/reload/restart worker PIDs and identical normalized output hash/length/ref count; two byte mappings confirm size change.
- Five repetitions of the same command to /tmp result files: exit 0, all passed; phase7-lifecycle-stress.log includes raw outcomes. Initial raw-body-length assertion failed due solely to cache timestamp-comment width; corrected to normalized bytes, with unchanged normalized hash, then all repetitions passed.
- `cargo test -p ps-nginx --lib factory::tests > docs/performance/shared-memory/phase7-restart-tests.log 2>&1`: 18 passed. New regression closes all handles, corrupts/truncates the shared byte file, restarts the factory, verifies startup fallback message and unchanged persisted metadata via disk/L1. Other tests cover SIGKILL mutation boundaries, partial headers, permissions, layout mismatch and boot rejection from Phases 1–5.
- Checkpoint decision: existing write-through disk is authoritative; no additional byte checkpoint format/thread/flush is warranted. Native pthread state is never serialized/restored. Same-boot mappings are opportunistic and boot/layout-validated. Documented in CONFIGURATION.md. No hit-path checkpoint I/O introduced.
- Scoped rustfmt, Python syntax and git diff --check required before milestone commit. Only factory test code changed since the Phase 6 release module; lifecycle module hash unchanged. No deployment or remote push.

## Phase 8 aggregated diagnostics — design before edits

- Phase 7 committed 634140e. Add a small saturating atomic shared-metrics primitive over reserved region words (safe ps-shm wrapper), then instrument byte-cache events and owner/writer recovery. Counters persist for the current boot/domain; do not reset while workers overlap. Admin samples actual shared values and sets aggregate gauges, rather than summing repeated worker snapshots. Existing local histograms remain labeled/process-local and untouched.
- Byte usage is scanned under nonblocking shard locks only when diagnostics are requested; expose incomplete-shard count so a contended sample is never presented as exact. Report mapped capacity separately from resident key/value bytes. Stats collection must not cause filesystem/procfs reads on cache hits.

## Phase 8 implementation progress

- Added Linux SharedMetrics (saturating reserved atomic words; no new unsafe above region). Byte cache has persistent shared hit/miss/busy/put/oversize/eviction/delete/error counters and on-demand guarded usage scan. Writer tracker has recovery/capacity/untracked-write counts, disabled-shard gauge and exact mapping bytes. Owner table has owned/duplicate/busy/full/long-key/error/revocation/stale/dead-reclaim counts and capacity. Metrics filenames versioned; payload/coherence formats unchanged.
- Admin samples shared values through SystemCaches::refresh_shared_statistics and publishes UpDownCounter gauges prefixed shared_cache_. Aliases share one Arc/domain and count once. Fixed gauge slots are reserved in open_shared_statistics before fork; no dynamic-slot exhaustion silently degrades these to local counters. Existing histograms are untouched. Byte sample exposes usage_busy_shards; diagnostics failures expose diagnostics_unavailable. Transaction/counter/owner/writer/byte metric mappings reported separately.
- Counters are cumulative since the corresponding boot-qualified mapping was created; admin clear changes the presentation gauge only and next sample restores its source value. Capacity is current configuration, not all old mappings held by draining workers. Resident bytes count physical valid-epoch slots, including previous incarnation keys pending eviction; they are not a guarantee of logical reusable values.
- Initial compile used Variable::set, which does not exist for monotonic counters; corrected to UpDownCounter::set for sampled gauges. Scoped clippy passes. Current unit log phase8-metrics-tests.log passes; final rerun after dead-owner metric additions/build/admin lifecycle is in progress.
- Added cross-process byte diagnostic assertion to existing independent-process test, eviction/admission/partial-usage tests, alias dedup/no-accumulation gauge tests. Lifecycle tool --check-metrics samples admin stats_json from two worker PIDs, verifies one domain/non-multiplied capacity, and checks the new capacity after reload/restart.
- Workspace final regression started to phase9-workspace-tests.log on the fixed Phase 8 source snapshot. Inspect phase8-build.log and phase8-admin-lifecycle.log before committing; then finish Phase 9 stress and Phase 10 profiler attribution. No completion claim yet for these phases.

## Phase 8 final verification

- `cargo test -p ps-shm -p ps-cache -p ps-nginx --lib > docs/performance/shared-memory/phase8-metrics-tests.log 2>&1`: exit 0, 407 primary passed (54+72+281), plus two nested reports. Metrics regressions cover cross-process totals, evictions/oversized admission, partial resident samples, canonical alias dedup and repeated gauge sampling without double accumulation.
- `cargo clippy -p ps-shm -p ps-cache --all-targets --no-deps -- -D warnings -A clippy::new_without_default -A clippy::unnecessary_get_then_check > docs/performance/shared-memory/phase8-clippy.log 2>&1`: exit 0, only existing suppressions.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/phase8-build.log 2>&1`: exit 0, optimized. Extra metrics mappings remain outside byte budget and are separately reported.
- `python3 tools/verify_shared_lifecycle.py --nginx /home/gamingph/Projects/Rust/.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/phase8-admin-lifecycle.json --check-metrics > docs/performance/shared-memory/phase8-admin-lifecycle.log 2>&1`: exit 0. Worker PIDs 3969500/3969501 both expose one shared domain and 790656-byte byte mapping; after size change/restart, one domain/1841280-byte byte mapping. 34 shared metrics in JSON. No per-worker capacity multiplication; normalized HTML identical across reload/restart.
- First metrics lifecycle comparison found incomplete early HTML (2 optimized refs versus final 15) before background rewrites finished, not an output-corruption defect. Strengthened warm() to require an actual HTML cache HIT in addition to a stable normalized digest, then the check passed. Earlier Phase 7 run was on a lighter load and already had matching fully optimized hashes; do not rely on arbitrary repeated body stability alone.
- Read-only localhost request to the originally supplied Aerosky URL timed out; no production timing is claimed. Exact request loop and failure recorded in phase10-localhost-observation.json. Profiling the isolated equivalent remains Phase 10.
- Scoped rustfmt/Python syntax/git diff --check before commit. Workspace/stress runs started on final Phase 8 source; pending results remain Phase 9. No installed deployment or remote push.

## Phase 9 final verification

- `cargo test --workspace > docs/performance/shared-memory/phase9-workspace-tests.log 2>&1`: exit 0, **2,920 primary passed**, plus two nested subprocess reports (sum 2,922), 99 result groups, zero failures. Covers all workspace crates, rewrite/image/CSS/JS/RUM/filter/cache/config/admin regressions.
- Python loop of `cargo test -p ps-shm -p ps-cache --lib` ten times to phase9-stress.log: **1,260 primary cases**, plus ten isolated lifetime reports; zero failures. Includes mixed four-process/eight-thread torn-payload stress, bounded churn, robust-mutex/thread death, five disk-write kill boundaries, actual cache-adapter killed writer tests and transactional races.
- Actual two-worker lifecycle/admin tests in Phase 7/8 cover reload allocation changes, overlapped generations and restart; actual four-process evidence tests preserve nonce/merge counts/replay rejection. Existing invalid dimension/header/permission/expiry/purge/backfill/old-token tests all passed. No unavailable sanitizer/model checker is claimed as run; robust behavior was exercised on this Linux host with normal Rust tests and subprocess fault injection. ThreadSanitizer does not model independent mmap-sharing processes; no nightly toolchain/sanitizer environment installed by this task.
- Phase 8 count correction: final unit log has **408 primary** (54+72+282), not 407; plus two nested reports. Source change was one Nginx diagnostic regression; raw log retained.
- `perf stat -e task-clock true`: kernel refuses supported events (perf_event_paranoid=4). Do not change host security settings. Phase 10 uses stage timers/strace/Callgrind instead. Read-only installed localhost target timed out; isolated counterpart is available under /var/www/test/aerosky-real-estate-html-responsive-website-template/index.html.

## Phase 10 checkpoint — final benchmarks and profiling

Completed and verified: new `tools/profile_shared_cache.py`; optional concurrent benchmark in `tools/verify_shared_lifecycle.py`; three alternating ordinary default/shared benchmark pairs; isolated Aerosky ordinary and instrumented runs; strace and Callgrind captures; final workspace tests and ordinary release restoration. See `docs/performance/shared-memory/RESULTS.md` and `phase10-*` artifacts for results/provenance. Production latency attribution and final documentation acceptance remain pending.

Exact commands (run from repository root; NGINX=/home/gamingph/Projects/Rust/.nginx-build/nginx-1.31.8/objs/nginx; MODULE is the path in phase10-ordinary-module.json or phase10-profile-module.json):

```sh
cargo test --workspace > docs/performance/shared-memory/phase10-final-workspace-tests.log 2>&1
cargo build -p ps-nginx --release > docs/performance/shared-memory/phase10-final-release-build.log 2>&1
cargo build -p ps-nginx --release --features pipeline-profile
python3 tools/benchmark_shared_memory.py --nginx "$NGINX" --module "$MODULE" --output RESULT.json --samples 100 --cold-runs 5 --shared-kb 16384
# Alternate shared-kb 0/16384 over three pairs; outputs phase10-default-{1,2,3}.json and phase10-shared-{1,2,3}.json.
python3 tools/verify_shared_lifecycle.py --nginx "$NGINX" --module "$MODULE" --output docs/performance/shared-memory/phase10-concurrency.json --benchmark-requests 400
python3 tools/profile_shared_cache.py --nginx "$NGINX" --module "$MODULE" --mode plain --samples 100 --output RESULT.json
# Profile module: plain 100, strace 10, callgrind 5; ordinary module: plain 100. See phase10-profilers.log.
python3 -m py_compile tools/profile_shared_cache.py tools/verify_shared_lifecycle.py
git diff --check
```

Final tests/build passed; Python syntax and diff checks verified at checkpoint. No implementation change was made during wrap-up. Historical baseline is retained, with no claim that runtime differences isolate an individual phase's effect.

## Report-audit milestone (usage-bounded continuation)

- Clean tree and recent commits inspected before work; no Rust implementation changes.
- `cargo check -p ps-nginx --release`: exit 0, 7.85 seconds. Previous full workspace/release results remain in phase10-final logs; no unnecessary full rerun for documentation-only changes.
- Independently recomputed every final default/shared latency-table row from the raw request samples using Python json/statistics: all four rows agree; 15 cold / 300 HIT / 300 MISS samples each. Saved exact medians/sample counts in phase10-report-audit.json.
- Corrected CONFIGURATION.md's stale pending Phase 7/8 statements, linked final results from README, and documented why shared caching remains opt-in. Admin behavior text already matches the implemented shared gauges.
- `git diff --check`: passed. No deployment, push, new production profiling, or claimed resolution of historical 13 ms. Phase 10 remains IN PROGRESS; continue from Next Session Resume Point.

## Requested current-commit benchmark — completed

User requested a new comparison column. HEAD is ef9aeb5 (new audit fixes since prior checkpoint); initial working tree clean. Building ordinary release to ef9aeb5-benchmark-build.log, then isolated matched Nginx cold/HIT/MISS benchmarks with default and explicit 16 MiB shared tiers, 100 warm samples and five cold runs per page. Preserve historical columns; label current isolated methodology explicitly. Next action: inspect build completion, run tools/benchmark_shared_memory.py for both modes, persist raw results and add column to ../benchmark.md plus repository report. No live deployment changes.

- Completed ordinary release build (27.23 s) and both five-run/100-sample-per-warm-phase benchmarks, exit 0. Default Moto cold/HIT/MISS: 3.641/0.461/1.857 ms; Aroma 4.927/0.485/2.704 ms. Shared Moto 4.323/0.466/1.895; Aroma 5.090/0.498/2.734. 500 confirmed HIT samples/page/mode. All ten warm bodies/page match prior normalized output hashes (15/23 optimized references).
- Saved ef9aeb5-{default,shared}.json/log, build log and ef9aeb5-comparison.md. Added new rightmost column in ../benchmark.md; that parent file is outside this Git repository, so the committed comparison report preserves the requested table inside the repository too.
- No source edits, deployment or pushes. Build and benchmark quality/HIT checks provide verification for this data-only milestone. Phase 10 production attribution remains incomplete; resume from existing Next Session Resume Point.

## README refresh — completed

Updated README performance section to measured ef9aeb5 cold/HIT/MISS values, sample counts, client/NGINX methodology, preserved output evidence, optional shared-tier example and deployment/recovery links. Replaced stale 9063416 "now" table and 97-suite status with latest recorded verification. Historical comparisons remain linked with measurement caveats. Documentation only; checked table rounding against ef9aeb5 JSON, relative repository links, and git diff --check. No source/build/deployment changes; next implementation session continues remaining Phase 10 acceptance work.

## Remaining-work review — source inspection only

Reviewed clean tree at b3fdb21, current audit checklist, fetch reconstruction polling, process identity and lifecycle harness. Saved prioritized findings in docs/performance/shared-memory/REMAINING_WORK.md. No new correctness failure established, no code changes or test execution claimed. Earlier F1–F7 are resolved by ef9aeb5; audit's historical missing-feature sections are not current outstanding bugs. Updated Next Session Resume Point to current commits and concrete next investigation.

## Resource wait refinement — in progress

Confirmed module.rs PagespeedResource content-handler branch calls terminal::fetch_resource_body -> ResourceDriver -> RewriteContext fetch directly. Existing contended-owner polling sleeps up to 250 ms on that call stack. Added per-driver bounded budget (default unchanged at 250 ms); NGINX terminal explicitly selects zero, still checking completed bytes once then preserving local reconstruction. Added contended zero-budget regression alongside original bounded-wait test. Pending format/build/tests and timing output; do not mark complete yet.

- Targeted fetch regressions: 5 passed. Serial timing run: zero budget 248.634 ms vs 250 ms budget 519.508 ms, both successful; debug fixture/mock contender only, not actual NGINX latency. Full 124-case image suite and terminal tests/release build running sequentially to resource-wait-{image-suite,nginx-tests,release-build}.log. Poll active command/logs before claiming completion.

## Resource wait refinement — completed and verified

- Request-local `ResourceDriver::set_fetch_dedup_wait(Duration)` caps at 250 ms; existing library callers retain default 250 ms. NGINX terminal sets zero because module.rs invokes it on the event-loop content-handler call stack. One cache probe still precedes existing local reconstruction. Positive-budget sleeps are capped to remaining time.
- Preserves claims, fencing, completed cached output, actual image/resource reconstruction and response success. Under contention NGINX may duplicate reconstruction instead of sleeping; no resource optimization disabled. Cache/reconstruction work remains synchronous, and this is not a full async-serving implementation or strict total-response deadline.
- `cargo test -p ps-html --test image_rewrite fetch_ -- --nocapture --test-threads=1 > docs/performance/shared-memory/resource-wait-serial-timing.log 2>&1`: 5 passed, exit 0. Debug/mock-contender elapsed: zero wait 248.634 ms; 250 ms budget 519.508 ms. Not a production benchmark.
- `cargo test -p ps-html --test image_rewrite -- --test-threads=1 > docs/performance/shared-memory/resource-wait-image-suite.log 2>&1`: 124 passed, zero failures, 60.60 s.
- `cargo test -p ps-nginx --lib terminal::tests > docs/performance/shared-memory/resource-wait-nginx-tests.log 2>&1`: 12 passed, zero failures.
- `cargo build -p ps-nginx --release > docs/performance/shared-memory/resource-wait-release-build.log 2>&1`: passed; ordinary module rebuilt. Scoped formatting applied; git diff --check passed. Updated configuration and remaining-work documentation. No push, deployment, live service changes or further benchmark claims.

## Operational recovery coverage — in progress

User authorized item 2. Add isolated real-NGINX beacon issuance/acceptance/replay across old/new workers, validating persisted evidence rather than HTTP status alone; add disk-backed offline tracker replacement test with all old handles dropped before removing tracker files. Do not remove live domains. Tests pending; no completion claim.

## Operational recovery coverage — completed and verified

- Added `tools/verify_beacon_reload.py`: fresh isolated two-worker NGINX, actual browser-supported UA, emitted JS nonce/options identity, real form beacon POST, SIGHUP, old-worker hold response and new-worker PID verification. A read-only oracle decodes persisted compressed beacon-evidence/1 records; does not use HTTP success as evidence of acceptance.
- Two orderings: pre-reload nonce accepted by new worker (pending nonce survives); accepted pre-reload evidence retained and replay rejected by new worker. Checks exact valid count/generation, nonce consumption, unchanged record on replay, old worker alive during final checks. Six additional repetitions passed (three per ordering), recorded with module/config hashes and PIDs.
- First harness draft used an unsupported test UA and correctly found no beacon; switched to a supported Chrome UA. This was a test setup issue, not a production change. Production code unchanged for this milestone.
- Added disk-backed `offline_domain_replacement_restores_admission_and_preserves_disk`: fill all 64 tracker slots, replace one key/delete another, verify persistent bypass and disk correctness, drop every old cache/tracker handle, replace only offline tracker directory, reopen with new namespace, verify persisted replacement/deletion and actual memory backfill admission. No live mappings removed. This covers slot-exhaustion bypass recovery; end-to-end u64 epoch exhaustion remains separate.

Commands/results (repository root):

```sh
cargo test -p ps-cache --lib tracked_coherence::tests > docs/performance/shared-memory/offline-domain-recovery-tests.log 2>&1
# 3 passed, including existing killed-writer and saturation tests.
cargo test -p ps-cache --lib > docs/performance/shared-memory/recovery-cache-suite.log 2>&1
# 74 passed, zero failures; nested killed-process report also passes.
python3 tools/verify_beacon_reload.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/beacon-reload.json
python3 tools/verify_beacon_reload.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/beacon-accepted-before-reload.json --accept-before-reload
# Both passed; repeated three times each to beacon-reload-repetitions.json/log.
python3 -m py_compile tools/verify_beacon_reload.py
git diff --check
```

Used ordinary release from resource-wait milestone (module SHA in JSON). No production Rust changes; no unnecessary rebuild/full workspace rerun for test/harness-only changes. Existing HTML-cache HIT/RUM tests remain previous verified evidence; this new minimal-page test exercises actual nonce/report state across reload rather than claiming new HTML HIT coverage. Phase 10 still incomplete.

## Performance acceptance matrix — in progress

Item 3 authorized. Plan: fresh isolated master/two-tier/default configurations, 1/2/4 workers, randomized reproducible default/shared order over three repetitions; cold concurrent HTML burst then confirmed warm HIT and warm-metadata MISS phases, same ordinary module/template. Record full samples, output digests, shared gauges, background-job counters and Linux per-worker RSS/PSS separately. Treat aggregate jobs/ownership sheds as proxies, not direct duplicate resource bytes. No production attribution or queue-saturation claim without measurement. Verify current release first; no source optimization justified yet.

## Performance matrix — measured milestone completed, attribution still open

- `cargo build -p ps-nginx --release > docs/performance/shared-memory/matrix-build.log 2>&1`: exit 0, 22.01 seconds, ordinary module hash f39a4104a52b2ffcbe7978b819559055315068c8889f4a556577a904b19cfaf0.
- New tools/benchmark_worker_matrix.py: 18 default/shared16MiB runs (1/2/4 workers × three repetitions × two tiers), eight clients; 32-request cold bursts +100 HIT+100 forced MISS per page/run; randomized seeded mode order, per-run hashes, counters and worker RSS/PSS. All assertions passed. Additional six default/shared1MiB runs at four workers exercise eviction; all output hashes preserved. Shared storage stayed <= configured budget. Exact commands and results in WORKER_MATRIX.md and worker-matrix*.json/log. Module hash checked against final build; script additionally checks unchanged module at completion.
- New ps-html example background_pressure: one gated worker/four queued tasks, 252 rejected unique submissions, 256 duplicate joins, exactly five executions; bounded test waits, no unbounded wait. Seven release repetitions passed (background-pressure-repeat.json); median join/reject costs 50/90 ns. Initial example draft treated submit result as Option; corrected to Result before successful verification. No production code changed.
- `cargo run --release -p ps-html --example background_pressure > docs/performance/shared-memory/background-pressure.json 2> docs/performance/shared-memory/background-pressure-build.log`: passed; seven direct target/release/examples/background_pressure executions verified counters/results. Python syntax and git diff --check pass.
- Read-only localhost endpoint now returns 200: first request45.160 ms;20 enabled median0.488 ms;20 off median0.189 ms. No cache purges. Installed module hash differs from staged; no source-version equivalence inferred. Strace attach to installed worker denied (ptrace Operation not permitted); no security changes. Data in matrix-localhost* and matrix-production-trace-access.json/log.
- Conclusions: mixed shared-cache latency, bounded pressure behavior, lower concurrent MISS with more workers; no consistent completed-job reduction, no measured unique resource bytes/encodes savings, no historical13ms attribution. Leave Phase10 incomplete. Progress saved; no remote push/deployment. Benchmark-only changes do not require full workspace rerun.

- Final script smoke check with `--workers 1 --repeats 1 --samples 8 --cold-requests 4` passed both modes; raw worker-matrix-smoke.json/log saved. README links and all matrix pass flags verified. Remaining steps are evidence gaps documented above, not incomplete source edits.

## Audit/progress presentation reconciliation — completed

Added current disposition table for F1–F7 and later verification, explicitly labeled original executive/phase/missing-coverage sections historical, preserved original findings and logs, and distinguished initial review-only constraints from authorized fixes. Replaced stale ledger "no task commits/builds yet" sections with milestone index and dated-by-source verification scope. Updated resume point and remaining-work status. No runtime/source behavior changed. Relative Markdown file/anchor checks and git diff --check passed; no unnecessary tests for documentation-only changes. Phase10 remains incomplete; no default-on recommendation or historical latency-resolution claim.

## Installed-update latency review — in progress

User installed a new module; on-disk installed SHA changed to fe5ea6a6ae3d4f798bcc20811050c11e42b38b4e562171e573541ce169a1aef0 (staged remains f39a...). Do not infer identical binaries or exact loaded source from file hash. Recheck live Moto/Aroma/Aerosky sequential HIT/MISS/OFF, connection reuse and resource serving with no purge/reload. Record counters where available and source-check only measured candidates before any optimization.

## Installed-update latency review — completed, no speculative runtime changes

- New tools/check_installed_latency.py: 100 interleaved normal/header-forced MISS/OFF per Moto/Aroma/Aerosky and 100 separate keepalive normal requests/page;13 existing optimized URLs ×5 requests; all HTTP200. Raw marker/status/latency/body size/headers/counters saved. On-disk installed SHA fe5ea6a6ae3d4f798bcc20811050c11e42b38b4e562171e573541ce169a1aef0; staged differs. Admin shared_cache_domains=0;12 installed workers.
- `python3 tools/check_installed_latency.py --output docs/performance/shared-memory/installed-update-latency.json --samples 100`: exit0. Normal medians0.552/0.638/0.554ms; forced MISS2.011/2.836/1.985ms; keepalive0.391/0.430/0.417ms. Raw marker counts qualify Aerosky normal as mixed (95HIT,1MISS,4BYPASS), not a pure HIT distribution.
- Newly observed ~12–13ms transient BYPASS on Aerosky; historical cause still unattributed. Follow-up24 paced normal requests allHIT (median1.257ms),100 eight-client requests allHIT (median1.679,max11.423ms). No cache purge/deployment/config/source changes. Report separates observations from causal claims and existing isolated stage data.
- Python syntax, local README/report links and git diff --check verified; no build/full tests necessary for new diagnostic script/docs only. Preserve ordinary build state. Next action updated above; no production optimization justified by these observations alone.

## HTML-hit allocation refinement — in progress

User requested an implementation improvement after installed latency review. Source shows HtmlSink eagerly clones all RewriteOptions into DeferredDriver and request headers before it knows whether HTML is a HIT. NGINX already owns an equivalent request-local ResourceDriver options snapshot. Plan: explicit driver-backed sink constructor to defer borrowing options until an actual rewrite; retain existing snapshot constructor semantics for other callers. Keep request-header snapshot only on non-HIT paths. Baseline ordinary isolated benchmark before source edit; preserve RUM/options/cache separation and validate with send tests + matched benchmark. No claim this removes unprofiled production tail causes.

- Implemented explicit HtmlSink::new_for_resource_driver; old new() retains independent options-snapshot semantics. NGINX uses request-driver options only when present; original no-driver path preserved. Header clone now only on non-HIT. Existing cached-RUM regression asserts HIT retains neither an options copy nor header snapshot and never builds parser/filters.
- `cargo test -p ps-nginx --lib send::tests`:9 passed. `cargo test -p ps-nginx --lib`:284 passed. `cargo build -p ps-nginx --release`:passed10.97s. Baseline html-hit-before.json complete; setup example build/after benchmark still running (inspect logs before commit). No deployment.

- Setup probe7×10000:copying1710.889ns vs driver-backed1362.918ns median (~20% setup reduction,0.348us). First end-to-end before/after drifted materially, including OFF baseline (up to0.57ms vs0.16ms), so no causal request-speed claim. Saved before/after modules via temporary old-source build, restoring new source in finally; manifest html-hit-ab-state.json and durable html-hit-change.patch allow recovery if interrupted (patch applies to HEAD send.rs/module.rs; also preserve new example/ledger). Final new-source build and three alternating one-run/100-sample benchmark pairs running; inspect log/state before next action. No live service touched.

## HTML-hit allocation refinement — verified work reduction, net latency unproven

- Completed driver-backed deferred options and non-HIT-only publication-header copy; legacy snapshot constructor preserved. Cached RUM test asserts no copies/parser on HIT. All284 NGINX tests passed; final ordinary source restored and buildpassed5.80s. Real beacon reload with new module passed; raw html-hit-beacon-reload.json/log. No behavior/filters removed.
- Setup benchmark1710.889->1362.918ns (~348ns,20.3%) for construction/drop only. Initial request tests drifted with OFF cost; three alternating A/B pairs retained identical hashes but revised pooled medians were higher (Moto HIT1.362 vs1.295ms; Aroma1.449 vs1.289ms). Cannot claim net request-latency gain; regression not ruled out. Report HTML_HIT_REFINEMENT.md states this explicitly. Do not deploy/recommend latency gain solely from microprobe.
- Raw before/after and all alternating samples, module provenance, logs and summary saved. Temporary baseline builds restored new source in finally; final target/release is new ordinary module. Patch safety artifact is retained for historical recovery only; do not apply again to committed source.
- git diff --check passed; no push/deployment, no full workspace rerun beyond full affected NGINX crate suite. Phase10 and historical tail attribution remain open. Exact resume point updated.

- Additional HIT-only paired follow-up: tools/compare_html_hit_modules.py starts simultaneous private instances at127.0.0.2/127.0.0.3 on the same18093 port/virtualhost.500 alternating request pairs/page, allHIT and identical output: Moto old1.366/new1.395ms (paired delta+0.027ms), Aroma old1.433/new1.416ms (delta-0.014ms). Mixed result; no net consistent speedup claim. Initial different-port fixture failed exact output hashes (CSS origin-dependent hashes); corrected same-port binding, no masked content differences. Raw paired JSON/log saved. Syntax/diff checks passed. This is a setup-allocation optimization candidate, not a validated production-tail fix.

## Requested latency retest — in progress

HEAD9929fc2, clean tree initially. Installed on-disk module now03b71dcaff4a3bc4125688dc45e397bd376a6159e62cacf0be2cdf09e68c7987; stagedd812d... remains unchanged. Saved old/new module hashes verified. First checks overlapped; outputs preserved under exploratory-* and excluded from comparison. Clean run now executes three500-pair/page isolated comparisons, then installed100-sample phases sequentially (html-hit-retest-clean.log). Inspect completion/pass flags before reporting; no code/deployment changes.

## Requested latency retest — completed

- Clean sequential three×500paired requests/page: Moto before1.409/after1.384ms (median paired delta-0.022ms); Aroma1.401/1.408ms (delta+0.004ms). AllHIT/equal output; mixed result, no broad latency-gain claim. Initial overlapping measurements are explicitly excluded exploratory artifacts.
- Installed100normal samples/page allHIT and200: Moto1.506ms, Aroma1.596ms, Aerosky1.588ms. MISS4.526/6.335/4.896ms;OFF0.666/0.692/0.690ms;keepalive1.108/1.301/1.137ms. Installed on-disk hash03b71d... differs from staged and previous installedfile. OFF baseline also slower than prior run; no causal source/regression claim. Data/report in LATENCY_RETEST.md and raw JSON/logs.
- No code/source/deployment changes. Benchmark assertions, sample/status/marker checks and git diff --check passed. Resume pending stage attribution/direct resource counting, not more unpaired timing claims.

## compress_zstd latency refinement — in progress

User authorized improving new filter at7875327; initial tree clean. Inspecting codec/NGINX paths and local zstd0.14.5 source. Baseline first; retain compression level/frame/flush behavior and decoding/negotiation/header correctness. Candidate: bounded thread-local reuse of one bulk and one completed streaming context, no shared locks/background waits; discard failed/aborted/large-response contexts. Remove MIME normalization allocation. Must verify roundtrip, independent/concurrent/aborted streams, disabled paths, throughput/ratio and actual HTML cache HIT/MISS response timings before completion. No live deployment.

- Implemented bounded one-bulk/one-stream completed-context caches per thread; large inputs (>1MiB) remain encoded but contexts discarded, active/aborted streams never share codec state. Compression level3 and flush policy unchanged. MIME checks and split Accept-Encoding negotiation no longer allocate normalized/joined strings.
-169 ps-http tests and291 NGINX library tests passed; new regressions cover exact bulk bytes vsfresh context, interleaved flushes, abandoned/empty streams, thread independence, and large-input non-retention. Codec after benchmark/release build in progress; original and new NGINX HTTP benchmarks must finish before performance claim. No deployment/source shortcuts.

## compress_zstd context milestone — verified

- Baseline codec before.json vs context-reuse after.json: HTML bulk132.023->26.877us; HTML streaming36.362->27.987us; CSS bulk13.768->9.404us; CSS streaming21.228->10.933us. Same level3, same encoded sizes3012/3000/91/88bytes. Cold/first-call values are individual observations, not distributions.
- All169 HTTP tests +291 NGINX tests passed. Three alternating real-NGINX mode pairs (filter disabled/enabled; identity/zstd clients; HIT/MISS/resources) passed decoding/output checks. Enabled zstd HTML HIT median0.515->0.441ms; identity HIT0.452->0.410ms, so some drift. Zstd resource0.443->0.435ms: repeated full-body encoding remains measured~0.214ms premium over identity despite workspace reuse.
- PSS after enabled workload ~25.3MiB? Actual values25287–25317KiB (~24.7MiB), baseline26078–26146KiB (~25.5MiB); point sample, not universal/peak memory bound. Context caches are per-thread and input<=1MiB retention, at most one each. Frames/outputVecs remain independent; codec internal memory is reused.
- Next small functional milestone: bounded exact-input encoded-body LRU (512KiB, max8 entries/thread, only compressible profitable complete bodies) to avoid repeated resource compression while never reusing stream/RUM state. Keys require exact body equality after fingerprint, header eligibility stays before encoding, output body identity alone controls reuse. No new negotiation/quality shortcuts. Context-only source is tested and ready to commit before this change.

## compress_zstd complete-body reuse — in progress

Added512KiB/max8-entry thread-local exact-input encoded-body LRU, only profitable complete bodies, no streaming frame cache. Fingerprint is CRC32fast for fast candidate selection; byte equality prevents collision/stale/cross-input errors. New tests cover collision rejection, LRU/capacity eviction and changed-body misses. First full parallel NGINX run hit the previously documented restart_namespace flake (290passed/1failed), preserved final-nginx-parallel-attempt.log; rerun complete suite serially, do not claim success until verified. Initial SipHash draft slowed repetitive CSS relative to context-only; replaced candidate checksum using existing crc32fast dependency and will remeasure. No compression level/quality change; all cold/miss bodies still encode normally.

- Exact-body cache HTTP phase (pre-admission refinement) passed3alternating pairs: negotiated zstd resource0.439->0.231ms, HTML HIT0.467->0.438ms, MISS1.837->1.816ms; disabled/nonnegotiating controls mixed, no zero-cost claim. Real zstd-encoded beacon reload + pre-reload acceptance/replay both pass. Unique-body probe found HTML133.052->26.646us but CSS13.084->13.593us (~4% slower). Added bounded repeat-admission metadata so one-off bodies avoid copying into cache; retest before completion. This changes caching only, never compression/eligibility/quality.

## compress_zstd complete-body milestone — verified

- Completed512KiB/max8 payload entries/thread plus8 bounded repeat-admission metadata entries; checksum candidate matching always verifies full input bytes, never reuses streaming frames/headers. One-off responses do not copy into memo cache. New unit coverage confirms admission/collisions/eviction/correct changed bodies. Output level3/ratios preserved. Codec workspaces per-thread maxonebulk/onestream; >1MiB inputs encode but do not retain workspace afterward.
-172 HTTP tests passed;291 NGINX tests passed serially. One earlier known restart_namespace parallel failure retained in final-nginx-parallel-attempt.log. All final release/codec examples passed; no unrelated cache fixes or disabled tests. Final HTTP runs:3 alternating pairs,100 samples/phase=300pooled. Negotiated HTML HIT0.544->0.451ms, MISS1.971->1.990ms, optimized CSS resource0.471->0.230ms. Disabled identityHIT0.445->0.442ms. Controls/drift reported; no blanketzero-overhead/cold-process or historical13ms claim.
- Unique-body probe after admission:HTML128.785->25.463us;CSS14.122->11.357us. Repeated-body codec andstream frame lengths unchanged; raw evidence/commands in docs/performance/zstd/RESULTS.md. Two actual zstd-encoded beacon reload ordering tests passed, confirming acceptance/evidence/replay semantics. Ordinary output/optimized HTML hashes preserved.
- Added README usage/memory guidance; pooledcodec/result cache bounded but per-thread adds retention; PSS point sample is not peak/RSS guarantee. Live server unchanged/no push. Exact resume point updated. Scope runtime source ps-http/zstd.rs+request.rs; NGINX behavior/API preserved; tools/examples are reproducible diagnostics.

## Live WordPress FastCGI stall — in progress (2026-11-25)

Authorized fixing rustpagespeed.com via SSH. Before: public TTFB9.455s then12s timeouts; PHP-FPM max_children5 exhausted. Read-only tracing: PHP workers recvfrom completed sockets FIN-WAIT-2, NGINX CLOSE-WAIT with32745 unread bytes; NGINX event loop idle. Reload PHP temporarily restores first~5 responses then repeats. Identified HTML HIT body_filter early return never advances incoming buffer cursors. Trial fastcgi_buffering off did not fix (6successful then2timeouts); must revert before final verification. No optimization disablement intended.

Remote changes so far: www.conf backup www.conf.before-slowlog-20261125; enabled5s slowlog at /opt/php-8.6/var/log/www-slow.log, config tested and PHP reloaded. NGINX site backup /etc/nginx/rustpagespeed.com.before-fastcgi-fix-20261125; trial bufferingoff. Next: consume every HIT origin memory/file buffer, regressions, full NGINX tests, build ABI-compatible1.31.10 module using compiler configured tree, validate/deploy backed-up module and restore buffering setting, repeatedHTTPS cacheHIT+MISS+zstd tests and socket/pool checks. Do not claim resolved until repeated requests exceed PHP pool and upstream connections drain.

### Live FastCGI milestone — verified

- Fixed HIT body-filter skip to advance memory/file cursors and clear originlast_buf; cache output and all filters remain enabled. Added mixedmemory/file/null/idempotent chain regression. Baseline existing finish-chain test passed; `cargo test -p ps-nginx --lib -- --test-threads=1`:292passed in2.59s. `rustfmt --edition 2021 crates/ps-nginx/src/filters.rs`, `git diff --check` passed.
- `NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx`:passed20.51s. Remote backed-up module then atomic replacement, nginx-t passed, NGINX restarted to clear stalled connections, PHP reloaded. Trialfastcgi_bufferingoff removed; originalsite settings restored. Slowlog5s remains enabled with backup for diagnostics.
-40 sequential localhostTLS requests:20identityHIT median75.741ms,p9597.472ms;20zstdHIT median73.4725ms,p9581.268ms,all200anddecoded. Wire66778->12644bytes. SeparatequeryMISS83.347ms thenHIT83.077ms; publiczstdHIT410.030ms. No remainingFastCGIconnections inpost-testss, servicesactive, slowlogempty. No poolincrease/timeoutincrease/cacheorfilterdisablement. Outputs/rawlogs in docs/performance/fastcgi-cache-hit. Underlyingbug is our module buffer ownership, not establishedWordPresspluginfailure. No remote package rebuilt; avoid reverting byinstallingoldpackage.

## Installed WordPress benchmark retest — verified (2026-11-25 03:15 UTC)

User installed and requested benchmark. No code/deployment/config changes; clean tree6de635c, installed module SHA unchanged13bf7938... . Sequential60localTLS requests (30identity,30zstd) then10publiczstd:all70HTTP200, decoded, no timeouts. Perlocalmode1MISS/29HIT; identityHITmedian80.355ms,p95104.814;zstdHIT80.205ms,p9594.145; singleMISS107.462/82.426ms. Public10HITmedian385.539ms,p95415.917ms. No residualFastCGIconnections/newtimeout/poolwarnings. Rawdata and exactmethod appended docs/performance/fastcgi-cache-hit/RESULTS.md. No regression attribution or load-capacity claim. Nextaction: package source inclusion remains necessary for future builds; runtime currentlyfixed, optional deeperWordPressoriginprofiling separate from historicalPhase10work.

## Early HTML lookup — in progress

Plan before edits: add opt-in HtmlCacheEarlyLookup (defaultoff). Publish bounded binary origin-policy descriptor only alongside successful body publication. Descriptor key includes URL/options/request variant/filemtime; serving reuses existing eligibility/body validation/purge/beacon checks, failsclosed on missing/corrupt/expired metadata and falls through to origin. NGINX attempts only main GET with cookie-free, nonconditional/nonrange/noncache-control requests after effectiveoptions resolve. File-backed origins must resolve regularfile with currentmtime (directory not accepted). Replays safe originheaders with originalbody validators/encoding removed, no-store downstream, perrequestdiagnostic and negotiatedzstd. No filter initialization on earlyHIT. Existing ordinary lookup remains fallback. Opt-in means trusted TTL freshness without perrequest PHP/header revalidation; no automatic activation for personalized/dynamicpaths. Milestones: config+descriptor+unitregressions, NGINXintegration+tests/build, isolated/liveoptin validation withbackup and beforeafterbenchmarks, docs+commit. Baseline directPHP12median64.121ms; local12PageSpeedoff74.005ms/on74.330ms; earlierinstalled29HIT80.355/80.205ms. User authorizes implementation; maintain RUM/sampling gates, expiration/purge/requestseparation, no optimizationdisablement.

- Descriptor/earlyNGINX path implemented.18HTMLcachetests and171fullHTMLlibrarytests passed;292NGINXlibrarytests passed initial integration. Newproperty count expectation needed169vs168 (firstconfigsuite87pass1countfailure); corrected and rerunning. Matched1.31.10releasepassed28.24s. IsolatedsyntheticFastCGI25msdelay: firstMISS30.13ms,20earlyHITmedian0.640ms withZEROextraoriginrequests (counteroracle); Cookie/Auth/Range/Cache-Control requestbypasses eachinvokeorigin; Set-Cookieresponses nevercache. Actualzstdbeaconreloadwith earlyflag passed; preacceptedordering running. No liveearlydeploymentyet. Next: finalconfig/NGINXchecks, livebacked-upmodule+optin activation andpairedtimings, docs.

- Config88tests, HTML171tests, NGINX292tests nowpass; two early+zstd beaconreload orderings pass. IsolatedFastCGIcounteroracle passes. README documents optin TTLtrust, cookie/conditionalbypass, invalidation and RUM behavior. Earlyhandler runsNGINXPRECONTENT(afteraccess). No live deploymentyet. Nextaction applybackedupmodule and optin to rustpagespeed.com, verify20+HITs/MISS and originPHPnotcalled latency, persist evidence. Baselineunchanged. Runtime tested milestone readyforcommit.

- Live deployment validated: backedupmodule /usr/lib/nginx/modules/ngx_pagespeed_rust.so.before-early-html-20261125, backedupsite /etc/nginx/rustpagespeed.com.before-early-html-20261125; atomicmodule replacement/nginx-t/restart, optinenabled. Samebuild off/on controlled30freshTLSrequests/encoding plus30keepalive each: offidentityHIT71.883ms/p9587.065,offzstd73.097/p9579.681; onidentity14.8835/p9521.398,onzstd14.587/p9516.362; keepaliveidentity60.6835->0.9263ms. Public10zstdHITmedian339.041ms (network notcontrolled). NewqueryMISS77.667ms thenHIT22.770; fakeWPlogincookieBYPASS78.654ms; no-cache requestfallsorigin73.182ms. NoFastCGIconnections remain,servicesactive. Nootherfiltersdisabled. Allsampledproductiontimingsinlive-off/on.jsonl. TestsRUM-onlyfixture(browserclockinitretained/bodyhashsame/zeroorigincalls) andcorefixtureseparate, bothpass; fullconfig88/HTML171/NGINX292 plus4integrationbinariespass. EarlyRUMfixtureattemptwithcore correctlyBYPASSednonce-bearingHTML; separateRUM-onlyfixture isolatescacheableRUMwithoutchangingproductionfilters. Finalreview tightenedlookup to requireCacheKeyStateAvailableforbothhint/body; scopedHTMLtests/releasebuildinprogress, redeploysamefix afterpass, thenfinaldocs/checkpoint. Next exactaction poll88434 andfinaldeploy/health/report.

### Early HTML lookup milestone — completed and verified

- Finalsource includesAvailable-statechecks anddefaultoff/query-scope regression. Config89passed; HTML171passed+18cachefinaltests; NGINX292passed;34cache/RUM/criticalbeacon integrationtests passed. Finalrelease27.22s and4isolatedoracles(finalcore/RUMcounter+beaconreloadbothorderings)passed. RUMonlyfixturepreservesbrowserclockandnormalizedbodyhashwhilezerooriginrequests; core/browsernoncescorrectlybypass. Actualproductionfiltersunchanged.
- Finalinstalledmodule452a423e6e992f57d71c5bb3db48f2245d81151b5a20dc580362eaff0e0bcb70, nginx-t/restartpassed. Repeatedsamefinalmodulecontrolledoff/onbenchmarkwith3recordedwarmups then30assertedHITspermode/encoding +30assertedHITkeepaliverequests: identity73.8755->14.383ms;zstd72.363->14.4965ms;keepalive58.0261->0.8781ms. Onconfigrestored/leftactive. Earlierpreliminarytransitionrowsretained/labeled. No networkregressionattribution/no historical13msclaim.
- Report/commands/rawdata in docs/performance/early-html-cache/RESULTS.md. Tool tools/benchmark_wordpress_html_cache.py runsread-onlyonserver. Rollback: HtmlCacheEarlyLookupoff+nginx-t/reload; module/sitebackupsretained. No originpool/timeoutincrease, no filterdisablement/no push/noDebianpackagerebuild. Implementationcomplete; exactresumepointupdated.

## User-installed early-cache retest — verified 2026-11-25 04:02 UTC

Installedmodule452a423e... unchanged; header1.31.10+a1e2f12. Earlyon.30confirmedHITs/group:localfreshTLSidentity14.841ms,p9520.488;zstd14.8105ms,p9521.007;reusedTLS0.8629ms.10publiczstdHITsmedian302.184ms. All100measuredresponses200, decoded. CookieBYPASS85.655ms; noresidualFastCGIsockets/servicesactive. No serverchanges. Initialbenchmarkstopped on first PythonheadervariantMISS; fixedtoolwarmup3before30measuredHITs andreran; originalpartial60curlsamplesretained/labeled. Exactcommand `ssh -o BatchMode=yes root@157.245.148.68 'python3 -u -' < tools/benchmark_wordpress_html_cache.py`; reports/rawfiles underdocs/performance/early-html-cache. Runtimeimplementationremainscomplete; nextsessionresume unchanged.

## Same-site HTTPS image loading — in progress

User authorizes fix. Before: HTTPSheader.jpg returnsJPEG712226bytes despiteAVIFAccept; ordinaryFileFetcher.join_root rejectsHTTPS and chainednetworkleg HTTPonly. Same-siteHTTPS must map directly to root with identicalhost/traversal/NUL/symlink/sizechecks, noTLSneeded. Scope: fix filelegscheme andcapabilityrouting, regression HTTPSbytes/headers/foreignhost/jail/missingno-networkfallback, fullNGINXtests andmatchedrelease, deploybackedupmodule andverify actualAVIF/rewritewithoutdisablingfilters or synchronousHTMLresourcework. ExternalHTTPSnetwork support remainsseparate (not falselyclaimedfixed). ExistingHTMLcacheshouldnotpublishdetachedrenders; verifyprogressivefreshrequests toavoidstaleoriginalURL. No editsyet. Next editfile_fetch/origin_fetch andtests; runbaselinefiletests first.

- ImplementedHTTPSsamehostfileleg andchainedsupports_httpscapability (previousglobalfalsepreventedResourceDriverinputconstructionbeforefileleg). ExternalHTTPSstillunsupported/failsclosed. RegressionreadslocalJPEGbytes/headers, foreignhost/traversal/NUL/missing/directory/escaping-symlinkrejected; chainHTTPSlocal/missing/foreignwithdeadlinechecked. Baseline12filetests passed; full293NGINXtests passed2.60s. Matched1.31.10releasebuilding. Next: finishbuild, isolatedHTTPSimageconversion test, backup/deploymodule, testpublicimageAVIFandHTMLasyncprogression, targetedpurgeonlyifneeded. No synchronousHTMLfetch/encoderchanges.

- IsolatedNGINXendtoendpassed usingrealheader.jpg fixture: HTTPSsourceURL/noTLSorigin, firstHTML3.60msoriginalsrc, second2.84msoptimized.pagespeed.src; resource Content-Typeimage/avif withftypavifbytes712226->181199. InitialfixtureHTTPbase/HTTPSsourcewascross-originunauthorizedandfailedtorewrite; correctedtrustedXForwardedProto+matchinghostport toexerciseactualsame-originHTTPSwithoutlooseningauthorization. Full293NGINXtests+7asyncprogressivetests passed; matchedrelease28.99s. No runtimeencoder/deadline/filterchanges. Nexttestedmilestonecommitthenbacked-updeployment andlivechecks; externalHTTPSstilloutofscope.

### Same-site HTTPS file-loading milestone — verified/deployed

- Runtimecommit4efd9d1, installedmatched1.31.10moduleSHA703608b46284c4697652fb83c9b63c6ec762731110e41ab1bb0cb147d7d921f6. Backupmodule.before-https-file-20261125; atomicreplacement/nginx-t/restartpassed,no configchanges. Scope localHTTPS only, externalHTTPSnetworkstillunsupported. Filejailsecuritychecks preserved and293NGINX/7asyncprogressive tests pass.
- IsolatedrealNGINXHTTPSsourcefirstoriginalthenoptimizedURL, AVIFftypbytes712226->181199. InitialHTTP/HTTPSfixturecrossoriginauthorizationfailurecorrectedbytrustedXFPandhostportmatching, noauthorizationloosening. Live normalhello-worldsrc/srcsetnowrewrite; rewrittenresourceimage/webp80042bytes vs712226 (~88.8%saved). Directheader.jpg warms torecompressedJPEG127207bytes; browser-specificIPROfilter remainsdisabled (no forceconversion). Gravatar original dueexternalsupportlimit. FreshqueryHTMLBYPASS/BYPASS/MISS/HIT/HIT, warmHIT14.991/16.070ms. No targetpurge needed forcheckednormalvariant; otheroldHTMLvariants canawaitTTL/purge. Separateimage-freebeaconchurnbugremainsunfixed.
- Readonlyhomepage30HITs/mode: identity15.3285ms,zstd14.696ms,keepalive0.8577ms; no synchronousHTMLfetch/encodeadded. Report/commands/rawdata docs/performance/https-file-images/RESULTS.md. Toolverify_https_file_images.py testswithoutTLSorigin. Package not rebuilt; sourcefixmustbeincludedinfutureinstall. Next: finalhealth/gitdiffcheck+commitdocumentation. No furtherrequiredimplementation.

## Latest latency check — verified 2026-11-25 05:16 UTC

Userrequestedread-onlybenchmark after231832finstall.30localHITs/group:identity14.761ms,p9515.250;zstd14.8785ms,p9516.351;keepalive0.86264ms.90localrequestsall200/HIT. Public10all200:8HITmedian307.498ms,2MISS346.840/381.699ms,allmedian336.8545ms; notclaimallHITorregressioncause. RewrittenheaderstillWebP80042bytes,servicesactive,nolingeringsockets. No serverchanges/runtimeedits. Rawdata appendeddocs/performance/https-file-images, resumeimplementationstateunchanged.

## AVIF deployment dependency documentation — completed

User requested README advice after live diagnosis: avifenc missing onproduction (libavif-bin Installed:none, Candidate1.3.0-1ubuntu4). Added prominent AVIF runtime dependency section beforebuilding: Ubuntu/Debianaptinstall/versioncheck, everyproductionserver/workerPATH/execution requirements, module/filteraloneinsufficient, cachedWebP/JPEGpurge/expiry and backgroundwarmup, verifyContent-Type notextension, eligibility/fallbacklimitsretained. Documentationonly; no productioninstallation/configchanges. Verified referencedexternalencoder in crates/ps-image/src/avif.rs; gitdiffcheckpassed. No runtime tests needed for prose-only change.

## Srcset-only enhancement — Phase 0 in progress

User explicitly excludes background variant generation. Authorized phased work, bounded milestone per session. Before runtime edits persist plan/audit/baseline. Scope existing candidate URLs + accurate descriptors and explicitly configuredlayout sizes; never generate/resize images, guessWordPressfilenames, fetch/decode/probevariantbytes inHTML, relabel768imageas640, prunebyUA, or alterpictureartdirection. Existing imageoptimization stays unchanged. Existing responsive_images chain slots lack driver implementation; C++counterpart generatesdensities and skips existingSrcset, unsuitable to enable as a shortcut.

Phases/checklist:
- [x] P0 audit parser/slots/filter integration and C++ behavior; verifybuild with relevanttests; save srcset-only microbaseline and read-only livewarm/requestmetadata; document constraints andexactnexttask.
- [x] P1 define newoptin filter/config, defaultoff, targetedexplicit sizes policy, option-signature separation and limits; independentlytest/commit. Names are proposalsuntilimplemented.
- [x] P2 allocation-free bounded width eligibility, descriptor/URL preservation, malformed/density rejection and tests/benchmark. Existing optimizer unchanged; element-level picture/no-transform/equivalence tests explicitly moved to P3. No new resource jobs.
- [x] P3 targetedexplicit sizes driver filter, no CSSlayoutinference; exactclassmatching, preserveothermarkup, bounds/no-transform/picture/streaming/defaultoff/LCP regressions.
- [ ] P4 browserviewport/DPRselection+HTMLcache/RUM/LCPtests; filteroff/on cold/warmMISS/HITbenchmarks separately, no fabricatedbytesavings.
- [ ] P5 READMEconfig/results/rollback, finalreleasecheck; deploymentonlyafterfunctionalvalidation.

Dependencies: ps-html/src/srcset.rs, image_rewrite.rs, driver.rs/chain.rs; configfilteroptions/signatures; existingasyncrewrite/cachebehavior. Acceptance: defaultoffbyte-equivalence, no addedfetch/encode/queues, boundedparsing, correctdescriptors, no sourcequality/DPRregression, requestoptionscacheisolation, measuredlatencyandselectedbytes, cleanincrementalcommits/no push. Hero'sexisting300/768/800widthgapmeanssizescorrectionalone mayNOTreduceLighthouse'sselected768candidate. Additionalpreexistingcandidate mustbesuppliedexplicitly tofillgap. Knownpendingseparatework unchanged.

### Srcset Phase0 — verified checkpoint

Audit/planning persisted docs/performance/srcset/PLAN.md. Existing srcset URLrewrites/descriptors alreadywork; responsiveslotsnotwired, C++filtergeneratesvariants/skips existingsrcset. Parserallocatesfullcandidate list before256slotcap; newfiltermustboundbeforeparse. Liveexistingwidths300/768/800and sizes snapshot saved; no new640file/no fictitiousdescriptor.2existingintegrationtestspassed0.03s; release srcset_baseline built20.54s (9×5000nsbenchmark batches).3widthparse273.970ns/roundtrip335.594ns;24roundtrip2792.708ns;256roundtrip31244.482ns(noisyindependentcase). Live30HITs/group14.8445/15.378msfreshTLS,0.97218mskeepalive, no serverconfigchanges. Diagnostictool--path preservesdefaultrootbehavior; cold/HTMLMISS andcontrolledon/off remainPhase4,notclaimedmeasured. Phase0complete; stopatmilestoneperusagebudgetrequest. Phase1firsttaskrecorded; no incomplete runtimecode.

### Srcset Phase1 — in progress

Configuration-only milestone: dedicated default-off SrcsetSizesEnabled, exact class SrcsetSizesClass and explicit SrcsetSizes, Directory scope; bounded values, merge/signature regression tests. Full contract recorded in docs/performance/srcset/PLAN.md before source edits. Existing candidate rewriting retained; no new resources/queues, no deployment. Next: implement/test scalar policy, then checkpoint.

### Srcset Phase1 — verified checkpoint

Three table-backed Directory-scope scalars implemented: SrcsetSizesEnabled=false, SrcsetSizesClass="", SrcsetSizes="". Exact class token bounded128bytes; sizes1024bytes with control/delimiter checks (not full CSS semantic validation). Query rejected, invalid setters preserve effective signature, merge/signature/cache separation verified. Empty values clear inherited policy. Dedicated boolean gates future separate driver filter; no existing responsive_images behavior reused. No runtime HTML changes/resources/deployment. Baseline config89passed; finalconfig90passed; NGINX config7passed and directive dispatch1passed (rebuild verifies downstream crates). Exactcommands/results in PLAN.md; gitdiffcheck passed. Modified config options/table/count, NGINX directive regression, plan/ledger. Next Phase2 bounded candidate handling, then Phase3 actual filter. Existing benchmark baselines retained; no performance improvement claimed for inert configuration. Milestone stops here per usage-budget boundary.

### Srcset Phase2 — in progress

Phase1 baseline config90passed. Implement allocation-free safe-width-list predicate only, before future sizes mutation;16KiB/64candidate bounds, checked dimensions/duplicate validation, raw URL preservation. No replacement of existing WHATWG parser or optimization slots. Exact design in srcset PLAN. Phase3 element guards/equivalence remain pending until actual driver wiring. Next implement helper/tests, verify existing srcset integration, measure helper separately, checkpoint.

Phase2 helper implemented;3eligibility tests passed, existing2img_srcset tests passed, ps-html library171passed0.40s. No call site changed; existing parser/slots/URL conversion intact. Release validation diagnostic still building; next poll current build, run executable sequentially after all tests to avoid timing overlap, persist measurements, reconcile Phase2/3 checklist and commit.

### Srcset Phase2 — verified checkpoint

Allocation-free supports_width_sizes added to srcset.rs; bounded16KiB/64candidate stack table, checked positive unique widths, opaque URLs (data commas preserved), conservative malformed/density rejection. No parser/optimizer/driver call sites changed.3newhelper tests+2existingimg_srcset tests+171HTMLlibrary tests pass. Release diagnostic built31.03s; sequential final helper medians3/24/64candidates0.228/2.120/5.691microseconds,65reject5.701microseconds,overbyte3.192ns. Rawjson/logs and exactcommands in srcset PLAN. No total-request performance change claimed. Files:srcset.rs,tests/srcset_sizes.rs,examples/srcset_validation_latency.rs,diagnosticartifacts,plan/ledger. Phase3 actual driver filter, no-transform/picture and defaultoff/no-policy element tests remain unimplemented; exactresume updated. Working compilation verified; no deployment/push.

### Srcset Phase3 — in progress

Phase2 baseline3eligibility tests pass. Plan before edits saved in srcset PLAN: early-pre SrcsetSizes slot with immutable Rc policy cached in chain plan, request-local subtree state, exact class targeting, bounded decode, unique attrs, width-only eligibility, no-transform/picture/noscript/template/svg skips. Actual default-off/no-policy/HTML/LCP regressions next. No deployment/variants/fetches.

### Srcset Phase3 — verified checkpoint

Real SrcsetSizes early-pre filter implemented, enabled only with complete optin policy. Rc immutable config reused from cached ChainPlan, request-local blocked-subtree state; no stats/resource init, no added fetches/encoding. Class/srcset escaped text bounded before decoding;64attributes/1KiBclass/16KiBsrcset/64candidates; exact tokens/uniqueattrs/width eligibility. Picture/noscript/template/svg/no-transform/density/malformed input skipped; existing srcset/fallback/layout/priority unchanged, only explicit sizes add/replace. LCP-generated imagesizes uses correctedvalue.9srcset tests pass including0fetch with/without ImageRewrite/no-transform and policy Rc sharing/options isolation;171HTMLlibrary,29driver,26LCP,2existingimage,293NGINX pass. Initial nonexistentPicturekeyword compile error corrected with namecheck; trailing-whitespace fixture mismatch corrected, initial/final logs persisted. README documents behavior/limits/authorpreloads. No requesttiming improvement claimed, no deployment/push. Phase4 browser/cache/RUM+controlledlatency next; exactresume refreshed.

### Srcset Phase4 — in progress

Baseline9srcset tests pass. MatchedNGINX1.31.10release building. Isolated fixed25msFastCGI samebuild off/on + cold-key/MISS/HIT measurements, policy-enabled cache/RUM/reload oracles, Chrome viewport/DPR selection next. No live changes. Exact fixture plan in srcset PLAN.

Phase4 partialverification: matchedrelease37.26s; Chrome24cases pass widths352/412/1024×DPR1/1.75/2/3×off/on.3pairs use smaller suppliedfixturebytes,9unchanged;412×1.75still768w. RUMcacheonoracle20HIT+20MISS passes, samebodyclockretained,0extraoriginonHIT. Policy+earlycache+Zstdcorebeaconreloadpasses. Next sequential5pairs cachebenchmarks and persistentcacheoff/onkeyseparation; no runtimefixneeded. Browser uses installedChrome CDPpipe/Pillow; initialvenv unavailable ensurepip, no packages installed, no dependencyneeded.

Phase4 correctness now includes Chrome24checks (sameCSSwidth, no greaterbytes, sufficientlargestavailablephysicalwidth) and policy+Zstd+earlycachecorebeaconreload both acceptanceorderings. Initialtimingpasses excluded because driver diagnostic compilation overlapped some samples; preserved preliminaryfolder. First sequential pass also excluded from final because HTTPHost implicitly includedrandomport, invalidating exact-URLpersistentkeyoracle. Tool now pinsHostlocalhost and rerunsallgroups with no concurrentbuild. Next poll final benchmark, then build/run driverstage diagnostic sequentially, write honestresults/limitations andcheckpoint. BrowserProfileNGINXfixtures stopped; no production changes.

Phase4 beforeguardprofile: driverwarmoff/on filtersetup4.758/5.167us, HTMLprocessing511.349/552.756us,total516.985/558.760us,0fetches. Measured~42usaddedwork on1000paragraph fixture. Implementing keywordfastpath for blockednames, skipendchecksoutsideblockedcontexts, testImg beforeattributeaccess. Before artifacts preserved; must rerunrelease/browser/cachebeacon/latency afterchange beforecommit. Optionalmicrobenchmode/iterations to measure separatepeakRSS.

### Srcset Phase4 — saved for usage-budget interruption

Phase4 is IN PROGRESS, not complete. Current guard optimization compiles and passes171HTMLlibrary+29driver+26LCP+9srcset tests (phase4-tests.log). Matched NGINX1.31.10 release build29.53s and diagnostic release build37.40s pass. No commands running, no live deployment. First next action: run `target/release/examples/srcset_driver_latency > docs/performance/srcset/phase4-driver-after-guards.json` alone with no concurrent builds/tests. Compare with phase4-driver-before-guards.json; do not claim improvement before measuring. Then rerun browser, both beacon orderings and benchmark tools against current module, because stored browser/cache/beacon evidence predates guard changes. Commands below. Next peak RSS and final results/doc reconciliation; do not mark Phase4 done if unverified cases remain. Prior broader Phase10 latency attribution still separate.

Everything is saved in PLAN with exact commands and beforeguard evidence/limits. No new liveconfiguration, package or deployment. Tested runtimeguard refinement ready for incremental checkpoint; all final browser/cache/beacon/latency reruns are explicitly pending. Tools and diagnostics compile; existing policy and no-transform/picture/defaultoff/streaming/LCP regressions pass. Stage measurements and25mssyntheticorigin numbers must not be summed to infer NGINXoverhead or historical13ms. Keep firstincompletePhase4 rather than restarting audit.

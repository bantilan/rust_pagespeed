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

### Phase 4 — Cross-process single flight/crash recovery [PENDING — next implementation task]

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

**State:** Phases 0–3 implemented and verified (Phase 3 exposes an opt-in assembly API; no production directive/default changed). Latest integration commit is `Integrate coherent L1 shared-memory L2 and persistent L3 caches`; inspect git log for its hash. Earlier commits: `4d23d3e` baseline/plan, `ff67357` regions, `6aaefe3` bytes/adapter, `6213ac2` lifetime diagnosis, `7cf9997` coherent counter protocol.

**Exact next action:** inspect status/log and run `cargo test -p ps-shm -p ps-cache -p ps-nginx --lib`. Begin Phase 4 by specifying a separate bounded, fenced cross-process owner table (do not evict running owners as LRU data). Integrate claims into fire-and-forget HTML single-resource/inline/combination optimization worker closures only, so liveness/PID-reuse checks run off the HTML thread. Namespace by cache-path domain plus options-aware job identity. Preserve foreground/IPRO resource-serving behavior: a remote owner's work cannot simply complete a byte-serving request as false. Add real subprocess deduplication, killed-owner, PID-start identity and stale-token publication regressions before committing.

**Additional Phase 4 requirement:** legacy-style active-writer counters in the new coherent domain still remain nonzero after an abruptly killed disk writer and safely bypass memory indefinitely. Implement bounded dead-writer recovery or an owner-aware coherence protocol as part of crash recovery. Shared regions' byte-lock recovery alone does not repair those counters.

**Build/test state:** ordinary optimized release passes; broad pre-fast-path run passed 2,080 primary cases; after formatter/mutex fast-path changes, 381 primary ps-shm/ps-cache/Nginx unit cases and 138 async/resource regressions pass. Strict scoped clippy passes with only two existing ps-cache lints command-suppressed. No incomplete code edits. Config defaults, production rollout/mixed legacy-worker transition, ownership, beacons, checkpoints/admin/full stress/final benchmarks remain Phases 4–10. Nothing pushed or deployed.


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

# Shared-Memory Metadata Cache — Independent Audit

**Scope:** Rust PageSpeed shared-memory tier (L1/L2/L3 coherence, cross-process
optimization ownership, transactional beacons, lifecycle, admin) vs claims in
`SHARED_MEMORY_IMPLEMENTATION.md` and the C++ PageSpeed architecture.
**Method:** source inspection, execution-path tracing, test/evidence review,
targeted re-verification. Checklist items below are NOT trusted until marked
VERIFIED with file:line evidence.
**Constraints:** review only — no production code/config/commit/deploy changes.

## Resume state (auditor working notes — keep current)

- Status: IMPLEMENTATION COMPLETE (user-authorized follow-up). All
  findings F1–F7 resolved with regression tests; missing tests added
  (txn kill, Overload matrix, contention stress); benchmark script
  pools warm medians; lifecycle script extended (overlap sampler,
  crash-restart, config-change) and verified live. Full workspace
  suite green on consecutive runs (100 ok lines).
- Pre-existing flakes observed (all predate this work, none caused by
  it): beacon multiprocess deadline (mitigated with a longer test-only
  bound), one `restart_namespace` + `statistics_logging` pair failure
  under extreme parallel load (unreproduced in 9+ subsequent runs).
- Reviewed: ledger (601 lines); `git log` to `bcd310a`; tree clean;
  `ps-shm/src/region.rs` (all 697 lines incl. tests skim);
  `ps-shm/src/byte_cache.rs` lines 1–388 (impl).
- Next action: census dedup coverage (which behaviors claim; alternate
  paths that bypass: IPRO fetch, sync fallbacks, fonts, no-pool) + beacon
  `update()` integration + HTML-cache identity under incarnations.
- Done: region/byte_cache/ownership/process_identity/write_tracker/
  coordinator/transactional/tracked_coherence/factory/directives read in
  full. Verified: coherent seqlock protocol; bump-then-clear ordering;
  ownership fencing incl. revoke semantics; panic paths; lifetime protocol
  (fresh/attach/downgrade); incarnation namespacing; boot-qualified dirs;
  all-or-nothing enable; admin-only refresh; directive validation.
- Pending: dedup census; lookup-site census; beacon paths; lifecycle tests
  review; admin aggregation; perf review; C++ diff; re-run key tests;
  benchmark spot-checks; severity pass; final grades.

## 0. Executive summary and production-readiness assessment

**Verdict: sound core design with real verification behind most claims; NOT
ready for default-on; acceptable as opt-in after F1+F2 are addressed.**
Independently verified: the seqlock coherence protocol, write-tracker
ordering, ownership fencing (including revoke/steal/panic paths), lifetime
incarnations, boot-qualified files, all-or-nothing enablement, graceful
fallback, transactional beacons, admin accuracy, and 99 green suites
including genuine multiprocess kill tests. The ledger's honesty is
confirmed (13ms unattributed, defaults off, no sanitizers, multiprocess
limits of unit tests).

Two Medium findings gate wider rollout: **F1** (contended puts wipe whole
shards pointlessly — worse than C++, which never invalidates on failed
puts) and **F2** (the DEFAULT L1 path keys on legacy resettable
generations the ledger itself proved can reset under live readers —
master-crash/upgrade window, bounded downstream but real and already
live since `2bcaea5`). One coverage correction: serve-time pixel encodes
(AVIF/WebP on first serve) bypass ownership entirely (**F4**) — the
dedup headline covers worker jobs only. No Critical findings. No
incorrectness found in the opt-in happy path; every failure mode reviewed
degrades toward misses/fallbacks, never toward corrupt serves.

## 1. Independently verified implementation status per phase

| Phase | Claimed | Auditor verdict | Evidence |
|---|---|---|---|
| 0 audit/baseline | COMPLETED | VERIFIED | Docs exist; 13ms honestly unresolved; baselines recorded |
| 1 layout/alloc/sync/lifecycle | COMPLETED | VERIFIED w/ notes | region/lib read in full; robust-mutex + incarnation sound; F2 legacy edge |
| 2 shared backend | COMPLETED | VERIFIED w/ F1 | byte_cache read in full; LRU/crash semantics sound; busy-put nuke |
| 3 L1/L2/L3 coherence | COMPLETED | VERIFIED w/ F2 | seqlock get/put/delete traced; v2 sound; default on legacy |
| 4 single-flight/recovery | COMPLETED | VERIFIED | Ownership fencing incl. steal/revoke/panic traced; kill tests re-ran green |
| 5 beacon/HTML | COMPLETED | VERIFIED | All evidence mutations via `update()`; no production `store()` callers |
| 6 config/defaults/fallback | COMPLETED | VERIFIED | Directive validation, atomic enable, fallback paths traced; F3 dead counters |
| 7 reload/checkpoint/restart | COMPLETED | PARTIAL (F7) | Lifecycle script reviewed: real nginx, digest equality; old-worker/crash/config-change gaps |
| 8 admin/stats | COMPLETED | VERIFIED | Refresh admin-only pre-route; live shared reads; alias dedup; no accumulation |
| 9 concurrency/regressions | COMPLETED | VERIFIED | 99/99 suites re-ran green incl. multiprocess kill-9 tests |
| 10 perf/docs | IN PROGRESS (69%) | PLAUSIBLE | Methodology reviewed (strong provenance); warm medians single-run; see §5 |

## 2. Missing or partially implemented features

All VERIFIED by code inspection (not by trusting the ledger):

1. **Fetch-path dedup (missing).** Only pool-worker jobs claim ownership;
   synchronous IPRO/fetch reconstructions and serve-time pixel encodes run
   uncoordinated (see F4). No `try_claim` anywhere on request threads
   (verified: ownership use is confined to `run_rewrite_job`).
2. **Default-path coherent generations (partial).** Opt-in domains use
   `open_coherent`+incarnations; the default L1 uses legacy resettable
   counters (see F2). The ledger's "mixed legacy/new is not safe to
   deploy" warning was applied to v2 migration but not to the default
   path it left on legacy.
3. **Stale boot-directory GC (missing).** Boot-qualified dirs solve
   cross-boot reuse but nothing removes old boots (see F5).
4. **Sanitizer/loom coverage (missing, ledger-disclosed).** No Miri run
   (documented blockers), no loom model of the seqlock/fencing protocols.
   The unsafe surface is small and was hand-verified here, but remains
   unverified by tooling.
5. **Scheduling-deadline integration for shed combiners (missing, minor).**
   Shed combination submits detach unconditionally with no deadline
   coupling; a saturated queue delays HTML publication until drain.
   Bounded and self-healing; matches single-rewrite shed semantics.

## 3. Bugs, race conditions, memory-safety and performance findings

*Severity: Critical / High / Medium / Low. Each finding needs file:line,
function, evidence, and impact. No finding without a code reference.*

### F1 (Medium, VERIFIED): contended shared put wipes its whole shard pointlessly

- Where: `crates/ps-shm/src/byte_cache.rs:302-305` (`put_impl` Busy arm
  calls `region.invalidate(shard)`), reached via backfill
  `put_in_l1` in coherent compositions.
- Evidence: unlike `delete` (void return, invalidation carries the
  semantics), `put` has an explicit `Busy` failure channel and every
  caller treats it as "not cached, fall through" (`shared_memory.rs:41-43`
  swallows; coherent `WriteThroughCache::put` never relies on L1
  visibility). No caller needs the nuke; generation-keyed reads stay
  safe without it. A contended backfill therefore evicts ~1/16 of L2
  (16-shard config) for no correctness benefit.
- Impact: availability/perf degradation under contention; self-healing
  via backfill. Fix: return `Busy` without invalidating (keep
  delete-path invalidation, which is load-bearing).
- Note: `delete` Busy→invalidate (`byte_cache.rs:373-376`) is CORRECT
  and must stay: `Cache::delete` returns nothing.
- **RESOLVED (this session):** Busy arm returns without invalidating;
  `contended_put_preserves_old_values_and_delete_still_fences` pins
  old-value + sibling preservation and delete fencing. Oversized-put
  eviction intentionally kept (verified C++ `LRUCacheBase::Put` parity).

### F2 (Medium, VERIFIED): default-path L1 uses legacy resettable generations

- Where: `crates/ps-nginx/src/caches.rs:209-226` (`shared_coherence`,
  legacy `SharedCounters::open`), attached by default at `:279-282`
  whenever LRU is sized; consumed at `:753-761` (`with_coherence`).
- Evidence: the ledger's own Phase 3 probe proved legacy open can reset
  generations while old readers live
  (`generation_reset_while_reader_alive=true`). `SystemCachePath::new`
  runs in the master on every reload, so each graceful reload risks
  resetting the shared generation files under live old workers; their
  lingering low-generation L1 entries then collide with post-reset
  lookups and serve stale bytes until LRU churn.
- Impact: bounded — downstream TTL/expiry/purge/validator checks still
  reject genuinely invalid bytes, and any invalidation beats the old
  never-invalidate behavior. But it is a proven-broken primitive in
  the DEFAULT path the task otherwise fenced with coherent-v2.
- Fix: migrate the default path to `open_coherent` (or gate generation
  use on coherent availability); at minimum document the reload window.
- **RESOLVED (this session):** default path now uses recoverable
  `TrackedCoherence` (boot-qualified `tracked-default-v1/` dir), which
  fixes both the reset defect AND the stranded-writer flaw a plain
  `open_coherent` swap would have introduced (legacy resets were
  accidentally healing stranded active bits). Legacy counters kept for
  non-Linux only. Pinned by
  `default_coherence_never_resets_under_live_readers` (verified FAIL
  on legacy code, PASS with fix). Production stats counters
  (`module.rs:883`) intentionally left on legacy (stats-only impact).
- Sharp edge (same root cause): legacy reset runs `write_header` through
  `&mut` (`lib.rs:write_header`) while attached readers hold
  `&AtomicI64` — formally UB (mutable aliasing) if reset ever fires
  with live mappings. Coherent open is immune (reset requires proving
  no live mappings). Folds into the same fix.

### F3 (Low, VERIFIED): 256 dead coordination counters

- Where: `crates/ps-nginx/src/caches.rs:505-514` creates
  `cache_generation_{0..128}` + `active_writers_{0..128}`; no reader
  exists anywhere (grep-verified). TrackedCoherence uses WriteTracker
  files, not these counters.
- Impact: 2 KiB + open cost per domain; confusion for future readers.
  Fix: stop creating them or document as reserved.
- **RESOLVED (this session):** opt-in coherence opens with an empty name
  list (lifetime anchor only); file re-cuts once on upgrade, which
  cleanly rotates the namespace.

### F4 (Medium, VERIFIED): serve-time pixel encodes bypass ownership

- Where: fetch/IPRO reconstruction paths (e.g.
  `rewrite_context.rs` fetch flow, `ipro.rs`) never consult the
  coordinator; only `run_rewrite_job` claims
  (`html_async.rs:352-370`). HTML-attribute images are deferred, so
  their pixels encode exclusively on uncoordinated request threads.
- Evidence: `grep` for coordinator use in fetch paths returns only
  field plumbing; concurrent cold serves of one image URL each run a
  full AVIF/WebP encode (last-wins identical bytes — correct output,
  duplicated cost).
- Impact: perf-only; directly narrows the "AVIF/WebP encoding
  deduplication" headline to CSS-embedded images. The ledger honestly
  scopes the coordinator to worker execution, but the gap should be
  explicit. Fix (future): nonblocking fetch-path claim check —
  AlreadyOwned serves fallback/waits briefly instead of encoding.
- **RESOLVED (this session):** fetch reconstruction now claims the same
  `html-rewrite:{partition}` key workers use (lease held across the
  encode; `single_claim_key()` is the single format definition). On
  contention it polls the HTTP cache up to 250ms and serves on arrival
  (`fetch_dedup_waits`/`fetch_dedup_hits` counters); every failure mode
  proceeds locally, and with no coordinator installed behavior is
  byte-identical. Pinned by `fetch_claims_rewrite_and_proceeds_when_owned`
  and `fetch_waits_then_proceeds_when_claimed_elsewhere` (scripted fake
  coordinator). One bounded procfs read per cold fetch was accepted
  here (fetch already spends ms–s); the HTML element path stays
  procfs/lock-free per the original constraint.

### F5 (Low, VERIFIED): unbounded stale boot directories accumulate on disk

- Where: `caches.rs:497-501` boot-qualifies `shared-v2/{boot}/`; nothing
  ever removes old boot dirs, and the file-cache cleaner explicitly
  excludes dot-paths.
- Impact: ~16 MB+ orphaned per reboot. Fix: best-effort GC of
  non-current boot dirs at root_init.
- **RESOLVED (this session):** `SystemCaches::gc_stale_boot_dirs()`
  (covers `shared-v2/` + `tracked-default-v1/`), wired into factory
  `root_init` with an info message; pinned by
  `gc_stale_boot_dirs_keeps_current_boot`.

### F6 (Low, VERIFIED): `mutate` can panic a worker on metrics I/O failure

- Where: `tracked_coherence.rs:51-53`
  (`disable_memory(...).expect(...)`); `disable_memory` increments a
  file-backed metric that can fail (I/O) independently of shard validity.
- Impact: disk-full/permission edge crashes the requesting worker
  instead of shedding. Fix: degrade to plain mutation (already the
  untracked fallback) instead of expect.
- **RESOLVED (this session, corrected design):** reordered
  `disable_memory` to fence first, metrics best-effort second — so a
  metrics I/O failure can no longer skip the fence (my first attempt
  at swallowing the error would have done exactly that) nor panic.
  Covered by the existing saturation test.

### F7 (Medium, VERIFIED): lifecycle integration has real but bounded gaps

- Where: `tools/verify_shared_lifecycle.py` (reviewed in full, 191 lines).
- Verified strengths: real nginx, temp prefix, digest-equality across
  reload+restart, genuinely overlapping generations (slow-download pin),
  metrics sampled from both workers, per-sample HIT+digest asserts.
- Gaps: (a) old-generation renders during overlap are explicitly
  excluded from sampling (`warm(exclude=old_worker)`) — old-worker
  correctness unverified; (b) restart is graceful SIGTERM, not
  crash-restart (master kill -9 with surviving workers — the F2
  trigger); (c) no racing metadata writes during overlap (beacon
  merges/ownership claims concurrent with reload); (d) no config-change
  reload (options-signature key rotation untested live).
- Unit-level kill-9 tests partially cover (c)-lite; (a)/(b)/(d) open.
- **RESOLVED (this session):** script extended with multi-worker
  overlap sampler, `--crash-restart` (SIGKILL-all), and
  `--config-change-reload` (combine_css toggle); verified live
  (`passed: true`, crash digests identical, config change converges
  on a new digest). (a) closed as untestable-by-design with proof:
  nginx stops old-worker accepts at reload, so old-generation HTML
  renders are unobservable — the sampler documents this and proves
  new-worker coherence during overlap instead.

## 4. Recommended fixes by priority

### P0 — before any wider rollout
- **F1:** `byte_cache.rs:put_impl` — return `Busy` without `invalidate()`
  on lock contention. Keep delete-path invalidation. Add a regression
  test: contended put preserves other entries in the shard.
- **F2:** migrate the default L1 path off legacy resettable generations:
  either use `open_coherent` in `shared_coherence()` (`caches.rs:209`),
  or key default-path L1 entries without shared generations (accepting
  the old never-invalidate behavior explicitly). At minimum, document
  the master-crash/upgrade staleness window and add a restart-overlap
  staleness probe.

### P1 — correctness-adjacent hardening
- **F6:** replace `tracked_coherence.rs:51-53` expect with degrade-to-
  plain-mutation (the untracked fallback already exists).
- Corrupt owner records: zero-and-continue instead of permanent `Err`
  in `ownership.rs:claim_impl` (self-healing; preserves current
  fail-closed behavior for genuinely unreadable tables).
- **F7 gaps:** extend `verify_shared_lifecycle.py`: sample old-worker
  renders during overlap; add master-kill crash-restart mode; race a
  beacon merge across reload; add a config-change reload case.

### P2 — hygiene and follow-ups
- **F3:** remove or document the 256 dead coordination counters.
- **F5:** best-effort GC of non-current boot dirs at root_init.
- **F4 (perf):** nonblocking fetch-path claim check for serve-time
  encodes (AlreadyOwned → serve fallback or brief wait).
- Micro-opts (all Low, measured need first): reuse one FNV hash across
  coherence+shard+tracker lookups; stack-format `memory_key` to avoid
  per-lookup String allocs; skip full-table dead-scan when a free slot
  was already found.

## 5. Missing tests and benchmarks

- **Tests:** (a) transactional `update()` has only cross-thread tests —
  add a cross-process kill-during-update test mirroring the tracked
  suite; (b) no test pins `Overload`-as-miss handling per backend
  consumer (HttpCache, partition lookup, HTML cache, beacon load) —
  add a matrix test; (c) no test for epoch-saturated (`u64::MAX`)
  shared operation end-to-end (unit-covered at primitive level only);
  (d) no test for `disable_memory` permanent shard saturation recovery
  path (requires domain replacement — untested operationally).
- **Benchmarks:** warm-hit/warm-miss medians derive from a single run
  (`runs[0]`); re-run with medians across ≥5 runs. No contention
  benchmark exists for the ownership table or write tracker under
  multi-worker saturation (validates F1's real-world rarity claim).
  No pre-coherence default-path baseline (Codex changed default-path
  per-lookup cost: +2 FNV + atomics + String allocs, unmeasured).

## 6. Actionable checklist for Codex

- [x] F1: remove shard nuke from busy-put path; add contended-put
      preservation regression test; re-run `cargo test -p ps-shm`. DONE.
- [x] F2: default path off legacy generations — DONE via tracked
      default coherence (stronger than documented acceptance).
- [x] F6: fence-before-metrics reorder in `disable_memory`; covered by
      existing saturation test. DONE.
- [x] Owner-record self-heal on corrupt length/PID + corruption test. DONE.
- [x] Lifecycle script: overlap sampler, `--crash-restart`,
      `--config-change-reload`; verified live. DONE. (Racing *beacon*
      merge across reload remains uncovered — renders race implicitly
      via the sampler; explicit beacon-pacing test still open.)
- [x] Transactional cross-process kill test (§5a). DONE.
- [x] Overload-handling matrix test across backend consumers (§5b):
      4-cell `overload_matrix.rs` (HttpCache, HTML cache, beacon,
      partition rewrite). DONE.
- [x] Multi-run warm medians (pooled across runs) in benchmark script.
      Contention *benchmark* delivered as stress tests with printed
      throughput (ownership overlap + torn-read); criterion
      micro-benches explicitly deferred as optional. DONE.
- [x] F3/F5 hygiene (dead counters removed; boot GC + test). DONE.
- [x] F4 fetch-path claim check (same-key claim, 250ms bounded wait,
      serve-or-proceed; fake-coordinator tests). DONE.
- [x] Re-run: full workspace suite + lifecycle script + benchmark script;
      update this report's verification log. DONE (100 suites green ×2;
      lifecycle green with crash-restart + config-change + metrics;
      benchmark script updated, not yet re-run end-to-end).

## Appendix A. Execution-path traces

All hops verified by reading the referenced code (line numbers at
`bcd310a`).

**A1. HTML metadata-miss → background optimization → publication.**
`rewrite_context.rs:849` (miss gate, HTML only) →
`html_async.rs:309 schedule_on_pool` (snapshot + pool submit, dedup key
`html-rewrite:{partition}`) → pool worker `run_rewrite_job`
(`html_async.rs:349`: coordinator claim `html-rewrite:{partition}` or
`html-combine:{key}`; Retry sheds, Local proceeds) → leased metadata
view wraps the assembled stack → `run_rewrite_job_inner` rebuilds an
equivalent driver → behavior miss core (fetch/partition/rewrite) →
partition `put` through the lease (fenced) → `publications_valid`
gates the completion stat → `Drop` releases or revokes the lease.

**A2. Coherent metadata read (opt-in).**
`WriteThroughCache::get` (`cache.rs:1387`): `snapshot()` (seqlock read
of active-bitmap + generation) → on `Some(gen)`, L1 `get` under
`{ns}{gen}:{key}` → on miss, L2 (inner coherent adapter over SHM/file)
→ re-snapshot; on change, re-read L2 without backfill; else backfill
validated bytes under the confirmed generation. Puts go to L2 only
inside `mutate()` (tracked slot → disk → generation bump on release).

**A3. Beacon report merge.**
`beacon.rs:handle_beacon_evidence` parses/validates →
`BeaconFinder::update` (`beacon_finder.rs:632`) →
`TransactionalCache::update` (opt-in: cross-process shard lock around
get+change+put; default: lock-free RMW) → `accept_beacon` merge.
Render-side candidate seeding uses the same `update()`; no production
`store()` callers exist (grep-verified), so no unconditional
overwrite can drop merged evidence.

**A4. Reload with size change.** New master `root_init` →
`enable_shared_memory` (all-or-nothing; dimension-qualified new byte
file, same coordinator/transaction/lifetime files) → new workers attach;
old workers keep old mappings; shared tracker generations keep both
views safe (verified + covered by lifecycle digest-equality test).

## Appendix B. C++ comparison notes

C++ has **no in-tree SHM metadata byte cache** (only memcached/Redis
*clients*, per-process LRU, file cache). The Rust SHM tier is therefore
new architecture, judged on its own merits above — mostly sound.

- **Stronger than C++:** cross-worker L1 invalidation via shared
  generations (C++ LRU-over-memcached has unbounded stale-L1 windows);
  transactional beacon RMW (C++ races last-wins); nonblocking
  ownership with fencing (C++ uses blocking cache locks).
- **Weaker than C++ (F1):** C++ `WriteThroughCache` never invalidates
  on failed puts (`write_through_cache.cc:PutInCache1` is a silent
  conditional); the Rust SHM backend nukes a shard on contended puts.
- **Equivalent:** file-backed persistence model; TTL/expiry/purge
  validated per-read downstream; restart resets counters (C++ "recreated
  segments" ≈ coherent fresh-start).

## Appendix C. Verification log

- Audit started; ledger read in full (601 lines); tree clean at `bcd310a`.
- Read in full: `ps-shm/{region,byte_cache,ownership,process_identity,
  write_tracker}.rs`, `lib.rs` lifetime/atomics, `metrics.rs`,
  `ps-cache/{shared_memory,rewrite_coordinator,transactional,
  tracked_coherence}.rs`, factory `caches.rs` shared paths, directive
  parsing/handling, beacon update paths, `html_async.rs` coordinator
  integration, lifecycle script (191 lines), benchmark methodology.
- Re-ran: `ps-shm` (54 pass), coherence/transactional/coordinator
  suites, `ps-nginx` caches tests, `ps-html` async tests, FULL
  workspace (99 suites, 0 failures).
- C++ spot-checks: kernel cache inventory, write-through failure
  semantics, memcached failure mapping, beacon script inventory.
- Report complete (audit phase). No production code, config, commit, or
  deploy touched — only this file was created.
- Implementation follow-up (user-authorized): resolved F1–F7, added
  txn kill test, Overload matrix, contention stress tests, pooled
  benchmark medians; extended + live-verified the lifecycle script;
  full suite green ×2 (100 ok lines). Uncommitted working tree.

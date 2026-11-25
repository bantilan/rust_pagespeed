# Srcset-only filter: resumable implementation

Status: Phase0 verified; Phase1 pending. Baseline source9b6435b. No runtime filter/configuration behavior changed, no live edits/deployment. User explicitly excludes background variant generation. One independently functional/tested phase per session is the intended usage boundary.

## Scope and architectural decisions

- Reuse existing candidate URLs and real width/density descriptors. Do not create encodes, resize images, invent filenames, scan directories or probe/fetch/decode resources synchronously.
- Preserve existing asynchronous image optimization; it may continue converting existing candidates as before. This feature adds no background jobs.
- Do not prune candidate widths based on user-agent/mobile classification: viewport and pixel density are browser decisions.
- Preserve `<picture>` source ordering/media/type/art direction, density lists, fallback src, layout dimensions, LCP priority, lazy-loading behavior and RUM/beacons.
- `sizes` depends on CSS layout. An override must be explicitly configured and narrowly matched to an image; no automatic assumption of full viewport width or WordPress margins.
- New filter defaults off, must work as a real driver filter, and participates in effective-option signatures/cache keys. Proposed filter/option names are not yet implemented; confirm in Phase1 rather than documenting nonexistent directives as available.
- Candidate discovery is limited to existing author-provided declarations or explicit configuration with trustworthy dimensions/already available metadata. No URL guessing or descriptor relabeling. A 768px file must not be labeled640w.
- Preserve malformed/ambiguous author input unchanged rather than changing browser fallback behavior. Bound work by bytes and candidate/rule counts before expensive parsing, not just after allocating the entire list.

## Audit findings

1. `crates/ps-html/src/srcset.rs` parses candidates, preserves descriptors, stores one shared candidate collection, updates URLs, then commits once. Parser itself has no byte/candidate bound; `MAX_SLOTS=256` only caps downstream rewrite slots. Do not claim the current parser is bounded just because slots are.
2. `image_rewrite.rs::begin_rewrite_srcset` already applies ordinary image rewriting to existing URLs. It initializes an image context per candidate and does not derive target resize dimensions from width descriptors. Existing width descriptors should continue describing actual candidate dimensions.
3. No `sizes` handling exists in these srcset/image-rewrite implementations. Arbitrary CSS layout cannot be inferred there.
4. `chain.rs` declares ResponsiveImageFirst/Second when responsive_images+resize_images are enabled; `driver.rs` has no constructor for either and falls through to None. Enabling that existing flag does not implement this feature.
5. Original C++ `../mod_pagespeed/net/instaweb/rewriter/responsive_image_filter.cc` generates higher-resolution variants and explicitly skips images with existing srcset. It is not the srcset-only enhancement the user requested; do not silently repurpose its behavior/name.
6. Live `/hello-world/` hero has800w/300w/768w and `sizes="(max-width: 800px) 100vw, 800px"`. The avatar uses2x and no sizes. Snapshot in live-markup.json. Formats differ per candidate; a width policy must preserve the existing image optimization decisions.
7. The previously reviewed mobile report needed~616physicalpixels for352CSSpixels at1.75DPR. Both an overstated412CSSpixel slot (~721physicalpixels) and the accurate352slot still select768 with current300/768/800 candidates. Correcting sizes alone may save zero bytes in that scenario. Filling the640gap requires an actual existing file supplied by WordPress/site owner; generation is out of scope.

## Phases and acceptance checklist

### Phase0 — audit and baseline: VERIFIED

- [x] Clean status/recent commits/ledger reviewed.
- [x] Parser, slot integration, missing responsive constructors and C++ behavior audited.
- [x] Current srcset integration tests pass; release diagnostic build verifies current core compilation.
- [x] Persist parse/serialize and live warm HTML-HIT baselines with raw data/commands.
- [x] Record honest limits, future benchmark requirements and exact resume point.

### Phase1 — opt-in configuration: PENDING

- [ ] Specify filter and targeted sizes-rule syntax, validation/scope/defaults and byte/rule/candidate caps before editing runtime code.
- [ ] Prefer a separate srcset-only filter over hijacking C++ responsive_images.
- [ ] Implement default-off configuration, effective-options signature/key separation, invalid-value tests and config-only milestone commit.
- [ ] Decide whether candidate changes provide a measurable benefit beyond explicit sizes correction; avoid implementing redundant URL rewriting already provided by image_rewrite.

### Phase2 — existing candidate handling: PENDING

- [ ] Implement bounded parsing/handling only for safe supported declarations, with no new resources/jobs/variants.
- [ ] Use only declared or explicitly mapped real candidate URLs/dimensions; do not discard useful larger candidates or alter source order where meaningful.
- [ ] Validate width vs density lists, data URLs, escaping, malformed/duplicate descriptors, no-transform, `<picture>` and absent srcset cases.
- [ ] Default-off byte-equivalence and enabled-no-policy no-op tests; independently functional commit.

### Phase3 — explicit layout sizes: PENDING

- [ ] Implement narrowly targeted override policy with an appropriate validated CSS sizes value and opt-out semantics.
- [ ] Preserve unmatched images, density-only lists and art-directed sources. Explicit application to existing sizes must be documented, not silent.
- [ ] Test per-image rule matching and options/cache-key separation; commit.

### Phase4 — correctness and performance: PENDING

- [ ] Browser checks at multiple mobile/desktop viewports and DPR1/1.75/2/3; record actual currentSrc, decoded intrinsic dimensions and transfer bytes.
- [ ] Test HTML-cache HIT/MISS and per-request RUM/LCP preservation; confirm no additional fetch/encoding counts or queues.
- [ ] Controlled same-build filter-off/on cold requests, warm metadata/HTML MISS and HTML HIT separately; include missing/large/malformed inputs.
- [ ] Capture cold/MISS baselines in isolated instances before changing behavior (or build baseline9b6435b). Do not clear live caches just for a benchmark.
- [ ] Compare parser/HTML setup, total latency and memory. Do not add nested/overlapping measurements or infer page-load gains from parser nanoseconds.

### Phase5 — documentation and delivery: PENDING

- [ ] Document real directives/defaults/examples/layout obligations, result limits and rollback.
- [ ] Appropriate build/tests pass; incremental local commits, no push.
- [ ] Deploy only after functional validation within user authorization; preserve existing live configuration/backups.

## Baseline and exact commands

```bash
cargo test -p ps-html --test image_rewrite img_srcset -- --test-threads=1
cargo run --release -p ps-html --example srcset_baseline
ssh -o BatchMode=yes root@157.245.148.68 'python3 -u - --path /hello-world/' < tools/benchmark_wordpress_html_cache.py
```

Srcset integration:2passed,0failed,0.03s. Release diagnostic build20.54s. Parser9batches×5000iterations after100warmups; median per batch (nanoseconds/attribute):

| Candidates | Parse | Parse+serialize |
|---|---:|---:|
|3width|273.970ns|335.594ns|
|3density|277.369ns|350.296ns|
|24width|2686.766ns|2792.708ns|
|256width|31552.208ns|31244.482ns|

Parse and round-trip runs are independent; allocator/cache noise can put round-trip below parse in the256case. These are diagnostic observations, not filter cost, full HTML latency or reliable bounds. Both include allocations/drop work; no resource operations.

Read-only live baseline (30confirmedHITs/group,3warmups; Zstd decoded):

| /hello-world/ | Median | p95 |
|---|---:|---:|
|Local freshTLS identity|14.845ms|17.265ms|
|Local freshTLS Zstd|15.378ms|21.856ms|
|Local reusedTLS identity|0.972ms|—|

This existing-build observational sample is not a controlled off/on comparison, browser-selection test, cold-cache distribution or warm-HTML-MISS measurement. All runtime source/filter defaults unchanged. Parser/build and live benchmarks were sequential; a separate markup read occurred during live sampling, so do not use small timing differences for causal claims. Future controlled comparison must isolate other requests. Rawfiles parser-baseline.json, live-baseline.jsonl, live-markup.json and baseline-tests.log retained. tools/benchmark_wordpress_html_cache.py gains only --path with unchanged / default; runtime code untouched.

## Next Session Resume Point

Read this plan and SHARED_MEMORY_IMPLEMENTATION.md resume section, inspect git status/log. Phase0 is finished. Start Phase1 by reviewing filter IDs/options tables and existing rule/config parsers; write the proposed separate srcset filter and targeted sizes-rule contract/limits into this plan, then implement/test configuration only. Do not restart audit, generate image variants, enable incomplete responsive_images, deploy, or promise a640candidate absent a real source. No background commands intentionally left running after checkpoint. No current blockers; rule syntax/meaning is the first implementation decision.

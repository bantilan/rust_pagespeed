# Srcset-only filter: resumable implementation

Status: Phases0–3 verified; Phase4 IN PROGRESS (checkpoint, final reruns pending). Baseline source9b6435b. Phase3 implements opt-in output behavior; default-off remains unchanged, no live edits/deployment. User explicitly excludes background variant generation. One independently functional/tested phase per session is the intended usage boundary.

## Scope and architectural decisions

- Reuse existing candidate URLs and real width/density descriptors. Do not create encodes, resize images, invent filenames, scan directories or probe/fetch/decode resources synchronously.
- Preserve existing asynchronous image optimization; it may continue converting existing candidates as before. This feature adds no background jobs.
- Do not prune candidate widths based on user-agent/mobile classification: viewport and pixel density are browser decisions.
- Preserve `<picture>` source ordering/media/type/art direction, density lists, fallback src, layout dimensions, LCP priority, lazy-loading behavior and RUM/beacons.
- `sizes` depends on CSS layout. An override must be explicitly configured and narrowly matched to an image; no automatic assumption of full viewport width or WordPress margins.
- New SrcsetSizes driver filter defaults off and participates in effective-option signatures/cache keys via SrcsetSizesEnabled/Class/Sizes; it does not repurpose responsive_images.
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

### Phase1 — opt-in configuration: VERIFIED

- [x] Specify filter and targeted sizes-rule syntax, validation/scope/defaults and byte/rule/candidate caps before editing runtime code.
- [x] Prefer a separate srcset-only filter over hijacking C++ responsive_images.
- [x] Implement default-off configuration, effective-options signature/key separation, invalid-value tests and config-only milestone commit.
- [x] Decide whether candidate changes provide a measurable benefit beyond explicit sizes correction; avoid implementing redundant URL rewriting already provided by image_rewrite.

### Phase2 — bounded candidate eligibility: VERIFIED

- [x] Implement allocation-free safe width-list eligibility:16KiB/64candidate caps, no resources/jobs/variants or candidate allocations.
- [x] Retain existing URL/descriptor handling; no new mapping/discovery/rewriting machinery or order changes.
- [x] Test widths, density/mixed rejection, data URLs, escaped URLs, malformed/duplicate/overflow descriptors and exact bounds; existing URL-rewrite integration still passes.
- [x] Verify helper, HTML library and release diagnostic; persist cost and commit.

### Phase3 — explicit layout sizes: VERIFIED

- [x] Wire a separate real driver filter gated by SrcsetSizesEnabled and complete nonempty policy; call supports_width_sizes only after cheap class/element guards.
- [x] Element tests: default-off byte-equivalence, enabled-no-policy no-op, no-transform, picture/art direction, absent srcset, escaped attributes; these were intentionally deferred from helper-only Phase2.

- [x] Implement narrowly targeted override policy with an appropriate validated CSS sizes value and opt-out semantics.
- [x] Preserve unmatched images, density-only lists and art-directed sources. Explicit application to existing sizes must be documented, not silent.
- [x] Test per-image rule matching and options/cache-key separation; commit.

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

Phase4 post-guard checkpoint verified:9srcset tests,24Chrome cases, both policy+core+Zstd beacon reload orderings, canonicalHost/sharedcache separation and fivepaired NGINX benchmark rounds completed. Read docs/performance/srcset/RESULTS.md for measurements/limits, raw phase4-final files. No runtime edits this session, no deployment, no processes running. Next incomplete task: extend isolated profiling to realistic core/resourcewarmMISS and separately bounded large/malformed/no-policy enabled-filter latency; preserve current final evidence and avoid rerunning completed browser/reload checks unless source changes. Driver timing varied strongly across sessions; do not claim guard speedup. Phase4 remains IN PROGRESS, Phase5 pending. Prior production13ms attribution is unresolved and not explained by synthetic timings.

Copyable completed commands (rerun only after relevant changes):
```bash
cd /home/gamingph/Projects/Rust/pagespeed-rust
# Read ledger/plan, git status/log, verify prior tests first.
target/release/examples/srcset_driver_latency > docs/performance/srcset/phase4-driver-after-guards.json
# Run sequentially; no other benchmark/build load.
python3 tools/verify_srcset_browser.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/srcset/phase4-browser-after-guards.json
python3 tools/verify_beacon_reload.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/srcset/phase4-beacon-after-guards.json --early-html-cache --zstd --srcset-policy
python3 tools/verify_beacon_reload.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/srcset/phase4-beacon-preaccepted-after-guards.json --early-html-cache --zstd --srcset-policy --accept-before-reload
python3 tools/benchmark_srcset_latency.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output-dir docs/performance/srcset/phase4-final
/usr/bin/time -v target/release/examples/srcset_driver_latency off 100 > /tmp/srcset-off.json 2> /tmp/srcset-off-rss.log
/usr/bin/time -v target/release/examples/srcset_driver_latency on 100 > /tmp/srcset-on.json 2> /tmp/srcset-on-rss.log
```


## Phase1 contract (before implementation)

Configuration groundwork uses three Directory-scope scalar options: SrcsetSizesEnabled (false), SrcsetSizesClass (empty), SrcsetSizes (empty). Enablement is a dedicated boolean for a future separate driver filter, not an alias for responsive_images. One exact, case-sensitive HTML class token selects img elements; location inheritance can replace the scalar policy. Empty class/value means no policy. Class maximum128bytes, sizes maximum1024bytes. Values reject control characters, HTML delimiters, quotes, semicolons and unbalanced parentheses; CSS semantics remain the administrator's responsibility, not inferred or advertised as fully validated. No query overrides. All three table-backed values inherit existing merge/signature invalidation. Future candidate handling caps input at16KiB and64candidates before allocating candidate lists; reject oversize declarations unchanged. Existing URL rewriting is already sufficient: no new mapping/discovery machinery is needed. Phase1 adds no markup behavior; the driver implementation follows in later phases.

### Phase1 verification

- Baseline `cargo test -p ps-config --lib --quiet`:89passed. After config:90passed,0failed (0.01s); existing unused Scope import warning retained.
- `cargo test -p ps-nginx --lib config -- --test-threads=1`:7passed,0failed, compile5.64s.
- `cargo test -p ps-nginx --lib directive_lines_dispatch -- --test-threads=1`:1passed,0failed; compile4.64s, includes all three new directives. Existing file_fetch/send warnings unchanged.
- `rustfmt --edition 2021 crates/ps-config/src/options.rs`, `git diff --check`:passed. Tests rebuild downstream HTML/HTTP/NGINX crates; no release build or deployment claimed.
- Modified options.rs/properties_table.inc.rs/property.rs, module.rs directive regression and both progress documents. Existing baselines retained; no meaningful latency comparison yet because policy does not run on HTML. Full CSS semantic validation is intentionally not claimed; bounded delimiter validation only.

## Phase2 implementation decision (before source changes)

Add allocation-free width-list eligibility validation, rather than another candidate parser or rewriting URLs twice. Fixed16KiB/64candidate limits, fixed stack width table, checked positive u32 dimensions, unique widths, exactly one w descriptor per candidate. Density/mixed/implicit/height descriptors and malformed lists reject the policy without mutating input. URL tokens remain opaque (including commas inside data URLs); reject invalid UTF-8, controls other than HTML whitespace, raw HTML delimiters and trailing URL commas. Existing parser/image rewrite remains untouched for compatibility. Element-level no-transform/picture/absent srcset and default-off byte-equivalence tests belong with the actual Phase3 filter; do not mark those verified by helper tests.

### Phase2 verification and measured cost

Modified srcset.rs adds supports_width_sizes only; existing parse_srcset/slots unchanged. Added tests/srcset_sizes.rs and examples/srcset_validation_latency.rs. No image candidate bytes or dimensions probed; author widths are assertions, not intrinsic verification. Rejecting eligibility does not disable ordinary image optimization. No output behavior changes.

Commands/results:
- `cargo test -p ps-config --lib --quiet`:90passed (Phase1 baseline).
- `cargo test -p ps-html --test srcset_sizes --test image_rewrite img_srcset -- --test-threads=1`:2existingimg_srcset tests passed; helper tests filtered out, then explicitly run below.
- `cargo test -p ps-html --test srcset_sizes -- --test-threads=1`:3passed,0failed.
- `cargo test -p ps-html --lib --quiet`:171passed,0failed,0.40s; phase2-library-tests.log.
- `cargo run --release -p ps-html --example srcset_validation_latency`:release build31.03s. Reran sequentially after tests/build completion; saved final validation-phase2.json and build/run logs.9batches×5000iterations/100warmups.3width112bytes227.620ns;24width940bytes2120.226ns;64width2540bytes5691.030ns;65candidate reject5700.835ns; density reject51.647ns;16385byte reject3.192ns. No fair speed comparison to old parser (different work/inputs); this is additional future opt-in eligibility cost, not full request latency. Phase0 full-request baselines unchanged; controlled latency/browser tests pending Phase4.
- rustfmt for three added/modified Rust files and gitdiffcheck passed. No release NGINX build/deployment needed for an uncalled helper.

## Phase3 design before edits

Add SrcsetSizes chain slot in early-pre, ahead of image rewriting (which removes no-transform markers) and LCP preload generation (which copies sizes). Policy bytes owned once in cached ChainPlan via Rc; each filter has only request-local blocked-context depth. Gate slot on enabled plus both nonempty config strings; disabled/incomplete config constructs no filter. Exact HTML class-token matching, bounded raw class/srcset bytes before entity decoding, reject duplicate relevant attributes/decoding failures. Only safe width srcsets on img; preserve all other attributes and srcset verbatim. Skip picture/noscript/template/svg subtrees and both existing no-transform spellings. Replace existing sizes explicitly or add if absent. No statistics/resource driver/fetch/encoder required. Add driver-output, streaming/reset and LCP preload regressions. Cold/MISS comparison remains Phase4 using preserved baseline commit9b6435b in isolated instance; no live cache clearing.

### Phase3 verification and implementation

- ChainFilter::SrcsetSizes runs first in early-pre, before ImageRewrite strips no-transform and before LCP captures sizes. No image driver/resources/statistics registered by this filter. Immutable Rc policy retained in existing cached ChainPlan; per-document blocked depth isolated/reset. No policy -> no filter construction.
- Only exact class-token img with unique class/srcset/sizes attributes and supported width list can change sizes. Existing sizes explicitly overwritten; missing sizes appended. Raw escaped class/srcset bounds checked BEFORE decoding allocations; at most64attributes,1KiBclass,16KiBsrcset/64candidates. Other attrs/srcset unchanged. Picture/noscript/template/svg subtree guards retained across flush; density/unsupported/malformed/no-transform skips.
- New test module initially failed compilation because Keyword::Picture does not exist; changed to case-insensitive element-name check. First expanded suite then found a test expectation involving trailing tag whitespace normalized by the existing writer; fixture now has no trailing whitespace. No runtime workaround. Initial suite output retained as phase3-initial-tests.log, final helper/driver test rerun passed.
- `cargo test -p ps-html --lib --test driver --test srcset_sizes --test prioritize_lcp_images -- --test-threads=1`:171library,29driver,26LCP passed; initialsrcset8passed/1fixture failure corrected as above.
- Final `cargo test -p ps-html --test srcset_sizes -- --test-threads=1`:9passed0failed after additional no-transform+ImageRewrite check; phase3-tests.log. Tests verify zero fetches, generated LCP imagesizes/unchangedimagesrcset, cached policy Rc reuse/options separation, streaming/reuse, duplicate/oversizeattrs, encoded/case variants, defaultoff/incomplete byte-equivalence.
- `cargo test -p ps-html --test image_rewrite img_srcset -- --test-threads=1`:2passed; phase3-image-tests.log.
- `cargo test -p ps-nginx --lib -- --test-threads=1`:293passed0failed2.54s; phase3-nginx-tests.log. Existing warning output retained, no release build/deployment claimed.
- rustfmt for newfilter/test and gitdiffcheck passed. README documents actual syntax, execution order, explicit CSS responsibility, caps/skips, author preload alignment and no generated variants/byte-savings guarantee. Referenced install/common template/config-filter docs in chain header do not exist in this checkout; README is the actual config documentation updated.
- Modified chain.rs,driver.rs,filters/mod.rs,newfilters/srcset_sizes.rs,tests/srcset_sizes.rs,README,plan/ledger and test artifacts. No full-request performance claim: helper measurements from Phase2 unchanged; Phase4 isolates new enabled-filter overhead and checks browser/cache/RUM behavior.

## Phase4 execution plan before changes

Use isolated NGINX1.31.10+matched release module, no production changes. Synthetic FastCGI fixture with fixed25ms origin delay, source-provided real 300/768/800 JPEGs and explicit CSS slot, policyoff/on samebuild. Repeated unique paths measure first HTML MISS (process warm but resource/HTML keys cold); forced no-cache warm requests measure HTML MISS with resources/cache infrastructure warm; identical path measures early HTML HIT. Report markers/counts and all samples, avoid labeling process startup as cold latency. Add actual policy to existing RUM-only cache oracle and beacon-reload oracle. Browser via installed Chrome/CDP in disposable profile at multiple viewports/DPRs, record currentSrc, CSS/natural/file dimensions and transfer sizes; no generated deployment variants (synthetic fixture images only). Verify only actual supplied candidates and no promised640candidate. Preserve raw data, exact commands and build hash. Stop at Phase4 checkpoint; record any unverified item explicitly.

## Phase4 interruption checkpoint

User requested usage-budget wrapup. All code/tool/evidence saved; no deployment. Before-guard results are observations, not final current-module verification:
- Browser24cases pass,3off/onpairs reduce selected fixturebytes,9unchanged;412px×1.75DPR remains768w.
- Both beacon/reload orderings pass with sizes+earlyHTML+Zstd; RUM cache oracle verifies clock retained and normalized body same on HIT with0origin calls.
- CanonicalHostlocalhost pinned sameURL persistence test passes across policyoff/on, shared disk directory. Benchmark rawdata under phase4-before-guards (5cold+100warmMISS+100HIT per mode). Off/on medians30.527/31.075ms cold;28.990/29.062ms warmMISS;0.701/0.686ms HIT,25ms syntheticorigin. Batch ranges overlap; no established speedup or regression attribution. Corefullresourcecold/MISS behavior is NOT measured by PassThrough+RUM fixture.
- Driver beforeguard: resource setup0.741/0.812us;filter setup4.758/5.167us;HTML511.349/552.756us;total516.985/558.760us.0fetches.1000paragraph34KB fixture. Guard change avoids name comparisons for known tags and endchecks outside blockedcontexts; tests pass, performance afterchange NOT yet measured.
- Tool files: verify_early_html_cache.py adds optional policy/samples/cache-dir plus canonicalHost; verify_beacon_reload.py adds optional policy; new verify_srcset_browser.py uses ChromeCDPpipe/Pillow; new benchmark_srcset_latency.py sequentialalternating5pairs + sharedcacheoracle. New Rust diagnostic stages request setup/filter setup/HTML/drop separately; optionaloff/on100 enables RSS samples.
- phase4-preliminary files explicitly excluded (overlappingbuild or unpinnedHost); phase4-guard.patch retains exact runtime diff. Build attempt with nonexistent example was a command typo, failed immediately; correct matched build succeeded. Initial Pythonvenv failed missingensurepip; browser uses installedPillow andChrome directly; no packages installed.
- Required next validation: afterguard reruns, RSS, actual cache/RUM report, final RESULTS.md; targeted large/malformed enabled-filter latency and realistic core/resourcewarmMISS measurements remain unverified. Historical production13ms overhead not attributed by these synthetic tests. Do not deploy or claim comprehensive Phase4 completion.

### Phase4 post-guard verification checkpoint

Phase4 post-guard checkpoint verified:9srcset tests,24Chrome cases, both policy+core+Zstd beacon reload orderings, canonicalHost/sharedcache separation and fivepaired NGINX benchmark rounds completed. Read docs/performance/srcset/RESULTS.md for measurements/limits, raw phase4-final files. No runtime edits this session, no deployment, no processes running. Next incomplete task: extend isolated profiling to realistic core/resourcewarmMISS and separately bounded large/malformed/no-policy enabled-filter latency; preserve current final evidence and avoid rerunning completed browser/reload checks unless source changes. Driver timing varied strongly across sessions; do not claim guard speedup. Phase4 remains IN PROGRESS, Phase5 pending. Prior production13ms attribution is unresolved and not explained by synthetic timings.

FinalNGINXoff/on cold30.449/30.632ms,warmMISS28.918/28.920ms,HIT0.723/0.693ms;overlappingbatchranges,25msorigin,noresourceoptimizers. Driveroff/on total1042.508/1111.646us,priorbothmodesfaster;nointer-sessioncausalclaim. RSSlogs persisted. Exactcommandsexecutedfromsavedresume,sequentially:driverdiagnostic,browser,bothbeaconorderings,benchmarkscript,RSSoff/on;allcompletedexit0. All measurements scoped and reported in RESULTS.md. No source modifications beyond documentation/evidence, cleancheckpoint intended.

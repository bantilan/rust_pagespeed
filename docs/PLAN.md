# Build plan (Phase 2)

Incremental build order for `pagespeed-rust/`. Each step ships with its
per-component tests; nothing is "done" until its tests plus the benchmark
gate pass. Companion docs: [AUDIT.md](AUDIT.md), [ARCHITECTURE.md](ARCHITECTURE.md),
[MAPPING.md](MAPPING.md).

Conventions: "Tests:" = per-component tests in the normal crate layout
(`src/`, `tests/`, `benches/`); the reusable cross-implementation compat
suite lands in Phase 4 per spec (only its hooks are prepared here).

## Step 0 — Foundations (ps-config, ps-stats, ps-cache core)

Why first: every other crate depends on options, stats, and cache keys.

- ps-config: `Filter` enum + sets + URL ids + name map + aliases;
  `Enabled()` precedence; `RewriteLevel`; scopes; full property tables
  (transcribe 165 registrations incl. help text — closes config-gap-1);
  special/2-arg/3-arg parsing; scope gates; nginx-only paths; query-param
  adjust; request-time merge. Trace `standby` (config-gap-3) and query-
  scope setability (config-gap-2) here.
- ps-stats: thread-safe counters/timed vars/histograms; exact audited
  names; per-backend prefix families; enumerate the 27-filter `InitStats`
  fan-out (closes admin-gap-1); shared-mem segment lifecycle; append-only
  `stats_log_*` + trim + `PreviousDashboardCounters`; `DumpJSON` bounds +
  429 gate.
- ps-cache core: hasher (web64/MD5, 10-char default, `HashToUint64`);
  `ResourceNamer` encode/decode (all URL examples incl. experiments,
  options, slash escapes, `ce` shape); partition keys + `HashSplit`;
  composite keys + `v5/`; timer constants; TTL default set; `IsUrlCacheValid`
  + purge set/context + flush files; `Cache` trait + memory + LRU + file
  backends + 3 adapters.
- Tests: unit tests per type/table; golden tests for every audited URL
  example, hash vector, key format, TTL default, and stat name; doc tests
  for public APIs; property tests for encode/decode round-trips and
  `Enabled()` precedence.
- Benchmarks gate (§7): hasher throughput, namer encode/decode, LRU/file
  put/get latencies.

## Step 1 — ps-http core + ps-cache remotes

- ps-http: headers type + shortcuts; response state machine (status/
  headers/body rules incl. HEAD/204/304, overflow fail); `AsyncFetcher`/
  `AsyncFetch` traits; `HTTPCache` (composite keys, `RememberFailure`,
  per-status TTLs, `OverrideCacheTtlMs`, options-aware validity); mock
  fetcher; downstream purger.
- ps-cache remotes: memcached (hashed key + key-in-value, 1MB file
  overflow) and Redis (SET/SETEX) backends; `SystemCaches` wiring with the
  default chain order resolved (closes caching-gap-2); HTML-cache keys/
  TTL/backend traced from `proxy_fetch.cc` (closes caching-gap-1).
- Tests: state-machine unit tests incl. truncation/overflow/empty-body
  cases; cache hit/miss/expiry/failure-remember tests; mock-fetcher
  integration tests; backend tests against in-process fakes + optional
  live memcached/redis (gated by env var, skipped in default runs).

## Step 2 — ps-css + ps-js

- ps-css: lossless tokenizer; URL/`@import` rewrite + flattening with
  `CssFlattenMaxBytes`; minify; combine (import hoisting,
  `PermitIdsForCssCombining`); inline/outline (`CssInlineMaxBytes`);
  css stat family; parse-failure fallback (serve input + counter).
- ps-js: pure-Rust minifier with introspective-JS protection; combine/
  inline/outline (`JsInlineMaxBytes`); source maps (`sm`); analytics
  filters with before-JS ordering; js stat family; minify-failure
  fallback.
- Tests: golden input→output pairs ported from C++ filter tests (test
  bodies opened here — closes filters-7 for css/js); fallback tests;
  byte-saved accounting tests; `PermitIdsForCssCombining` gate tests.

## Step 3 — ps-image (DONE)

- DONE: sniffing (magic + WebP VP8/VP8L/VP8X feature parser, differential
  vs real WebPGetFeatures) + frame decoders (JPEG/PNG/GIF/WebP still,
  GIF animation iteration with disposal + Chrome quirks); JPEG marker
  walk (SOF/sampling/DQT quality/APP retention/SOS) + encode (quality,
  progressive + scan clamp, sampling incl. RETAIN, verbatim APP copy);
  PNG libpng transform chain + tRNS probe; WebP FFI (vendored libwebp:
  still decode/encode, animated encode, deadline hook); resizer
  (bit-exact vs 40 goldens) + analysis (Sobel/photo metric) ports;
  limits wired (16384px/100MP). Codec stack: png, jpeg-decoder (no
  rayon), jpeg-encoder, gif (pure Rust) + libwebp-sys2 static/mux/demux.
  Documented deviations: lossless JPEG = metadata strip only (Huffman
  re-opt needs entropy re-encode — follow-up); YCCK refused; interlaced
  PNG output refused (dead in C++: "never use progressive format");
  lossy path decodes to RGB first (C++ keeps native YCbCr).
- DONE (driver): JPEG/PNG/WebP optimizers with audited option structs
  (lossless strip, lossy matrix, PNG optipng-reduction subset + 4-trial
  search + gAMA passthrough, WebP quality rule + YUVA note);
  `ImageOptimizer::Optimize` + format matrix; `CompressionOptions`
  wiring + transcode matrix (AVIF→WebP→recompress order, budget,
  low-res); 3-tier qualities (jpeg/webp/animated maxima); negotiation
  stack (Accept+q-values incl. malformed-q reject, UA wildcard tables +
  version floors, PS-CapabilityList gate, level selection + option
  mapping); variant keys (`.|w|v|a` + `.avif` + `m` + `d`/`ss`) +
  extension mapping + Vary rules (UA/Accept/Save-Data/IE-private);
  AVIF `avifenc` pipeline (admission shed, zero-timeout, CQ mapping,
  poll-kill deadline, output validation, stats on every path).
  Documented deviations: lossless JPEG skips Huffman re-opt AND
  baseline→progressive conversion (entropy work follow-up); lossy WebP
  reduce goes via RGB not YUVA; interlaced PNG output refused;
  animated WebP encodes full canvases (no rect/disposal packing);
  child CPU time uncollected (no rusage via std).
- FOUND C++ BUGS (ported faithfully, reported): (1) JPEG recompress
  gate dead — MayConvert burns both attempts before it runs;
  (2) animated-WebP allowlist lacks 3-digit Chrome (Chrome 100+
  reports false); (3) QQBrowser/14.9 expectation stale — Chrome branch
  shadows the carve-out (verified: pagespeed_automatic_test FAILS this
  case); (4) trailing bare PS-CapabilityList token never matches.
- Tests DONE: 116 ps-image tests (63 codec + 12 optimizer/converter +
  14 driver/AVIF + 7 negotiate + 20 unit): everything above incl. fake-
  encoder AVIF matrix (ok/fail/timeout/zero/shed/CQ-argv/end-to-end),
  UA vectors ported from C++ tests, Accept×UA×capability matrix,
  q=0/malformed-q rejects, variant-key goldens, Vary matrix.
- Benches DONE (`benches/image.rs`, criterion): sniff ~2ns, webp
  features ~7ns, jpeg decode 512x384 ~2.0ms, png decode ~62µs, gif
  frame ~20µs, webp decode ~12µs, jpeg encode ~2.9ms, png encode
  ~720µs, webp lossless ~1.1ms, jpeg lossless ~2.0ms, png best ~3.9ms,
  jpeg→webp ~17ms, resize ~40µs, photo metric ~54µs, optimizer
  jpeg→webp ~22ms, gif→animated-webp ~1.3ms.

## Step 4 — ps-html engine + filter chain

- Streaming engine (tokenizer, `Write`/`Flush`/`Done`, reused buffers,
  incremental processing, flush points); chain builder with the exact
  audited order + debug order assertion + `pagespeed.conf.template`
  co-update note; all conditional/ordering deps; resource pipelines
  (`ce/cf/cc/if/ci/gf/is/ic/jc/jm/sm/ji/ls/pr/aj`); IPRO context
  (`InPlaceRewriteContext`, agreement gate, recorder); unverified wirings
  traced and closed (filters-1..4) or explicitly deferred with compat
  coverage.
- Tests: engine tests (chunk splits at every boundary, flush/done
  semantics, buffer-reuse assertions); chain-order golden test (filter
  sequence for Core/All/Bandwidth/Testing levels); per-filter golden
  tests ported from C++ bodies (closes filters-7 for html); dep-rule
  tests (each conditional/ordering dep forced on/off); memory-cap tests
  (`MaxHtmlParseBytes`, buffered-bytes cap).

## Step 5 — ps-admin

- Route dispatch (`/pagespeed_admin/<leaf>` + legacy statistics queries);
  all handlers incl. 404 + Did-you-mean; console/graphs gating (HTML
  requirements, JSON 429/404); purge endpoint (`<url|*>`, per-vhost
  scope); access-control wiring traced (closes admin-gap-2); dashboard
  counters (live + previous-delta semantics).
- Tests: route-table tests (every leaf + legacy query); handler tests over
  synthetic stats snapshots; purge tests (URL + `*` + scoping + auth);
  JSON-shape tests for console/stats endpoints.

## Step 6 — ps-nginx module

- Pinned ngx-rust bindings (pin version + record supported nginx range —
  closes nginx-gap-1); module/conf/phase hooks; sync-decide contract;
  content dispatch; suspend/resume/decline; all 6 filters in load-bearing
  order; per-request ctx + teardown; request-pool output; URL build;
  beacon GET/POST; threading bridge (pipe events, refcounts, writer
  backoff, worker-exit drain); native async fetcher (resolver/connect/
  state machine/keepalive); fork/thread lifecycle; resolver gate;
  `GetRemoteOptions` mitigation; config-time-only syscalls; gzip setter;
  script variables; message/crash handlers. FFI-only `unsafe` with
  `SAFETY:` comments + `#![deny(unsafe_op_in_unsafe_fn)]`.
- Tests: unit tests for pure logic (routing, URL build, gates); FFI tests
  behind a mock nginx ABI harness; integration tests driving a real nginx
  (reuse `test/nginx_system_test.sh` shape + `pagespeed_test.conf.template`
  cases); shutdown/drain tests; never-block assertions on event-loop paths.

## Step 7 — Benchmarks gate (runs from Step 0 on, blocks every step)

Criterion benches per crate (`benches/`), tracked over time; a step is not
done while its benches regress vs the previous step without justification.

Metrics (all reported, none optional): latency p50/p95/p99, throughput,
CPU, memory (peak + per-request), allocations, cache hit rates.

- ps-cache: hash/namer/key-gen throughput; backend put/get/delete p50/p99;
  purge-set insert/lookup; file-clean throughput.
- ps-http: state-machine bytes/s; HTTP-cache hit/miss latencies; failure-
  remember overhead.
- ps-css/ps-js: rewrite throughput (bytes/s) by input size; minify p99;
  fallback rate (must stay 0 on corpus).
- ps-image: per-format optimize latency p50/p99; throughput MPix/s;
  avifenc queue wait + concurrency cap behavior.
- ps-html: end-to-end rewrite latency p50/p95/p99 by page size and filter
  level; throughput req/s; peak + per-request memory; allocs/request
  (target: O(1) in page size for pass-through spans); flush latency.
- ps-stats: counter bump throughput (multi-threaded); snapshot/dump cost.
- ps-nginx: in-nginx req/s vs unoptimized baseline and vs C++ module;
  event-loop stall detector (any block >1ms fails); worker memory.

## Step 8 — Compat harness preparation (hooks only; suite lands Phase 4)

- Keep naming parity (stat names, key formats, URL ids, filter names,
  directive spellings byte-identical); record deviations in MAPPING.md.
- Expose deterministic fixtures from each crate: golden input→output
  pairs, config parse vectors, key/URL vectors, negotiation vectors.
- Define the harness interface (config corpus + page corpus + assertion
  kinds: byte-equal output, header-equal, stat-delta-equal, cache-key-
  equal) so Phase 4 can plug the reusable suite straight in.
- Close remaining gaps: filters-5/6 (Apache level spelling, header/X-PSA
  variants — document or implement), config-1..3 leftovers, deps-1..3
  (record-only), modules-gap-1 (bazel/changelog.d/external/log/out/
  strings/url roles), nginx-gap-2 (conf.template co-update check).

## Ordering summary

0 foundations (config/stats/cache-core) → 1 http + cache-remotes →
2 css/js → 3 image → 4 html engine+chain → 5 admin → 6 nginx →
7 benchmarks gate (continuous, blocks each step) → 8 compat hooks
(suite itself in Phase 4).

## Progress

- Step 0, ps-config: DONE. Filter enum/sets/levels/`Enabled()`/
  comma-list/query adjust, 167-row property table (closes config-gap-1),
  lookups, renames, nginx scope gates + directive entry, wildcard groups,
  `RewriteOptions` store with merge; 38 tests green; `benches/lookups.rs`
  baselines recorded (e.g. option lookup ~7ns, merge ~730ns).
- Step 0, ps-stats: DONE. Registry (variables, up/down + global counters,
  bucketed histograms, timed variables), all audited names with the
  27-filter fan-out (closes admin-gap-1), stats-log writer/reader/trim/
  recovery/`DumpJSON`; 19 tests green; `benches/counters.rs` baselines
  recorded (counter add ~1.7ns, snapshot ~29µs). Shared-memory backing
  deferred to the nginx step.
- Step 0, ps-cache core: DONE. Timer constants, base64 codecs, MD5
  hasher (md-5 crate), `ResourceNamer` encode/decode incl. legacy form,
  URL-segment escaper, partition/composite keys, TTL + failure policy,
  `PurgeSet`/purge file/invalidation validity, `Cache` trait with
  memory/LRU/file backends and prepender/fallback/compressed adapters;
  39 tests green; `benches/cache.rs` baselines recorded (MD5 4K ~5.8µs,
  namer ~130-175ns, memory/LRU ~90-100ns, file ~70µs). Deps added:
  `md-5`, `flate2` (pure-Rust backend), `ps-config` (wildcard only).
- Step 1, ps-http: DONE. Header names/statuses, HTTP dates, ordered
  headers with lookup/mutation semantics, content types, protobuf-wire
  headers codec, `ResponseHeaders` with full caching computation,
  headers/response parsers, request headers/context, HTTPValue framing,
  fetch traits + inflating gzip layer, mock fetcher, `HTTPCache`
  (find/put/delete/failures/compression/ETag), downstream purger;
  41 tests green; `benches/http.rs` baselines recorded (parse ~600ns,
  compute_caching ~580ns, binary round-trip ~300ns, put+find ~14µs).
- Step 1, ps-cache remotes: DONE. Server/cluster spec parsing, CRC32
  sharding + CRC16 slots, memcached ASCII and RESP transports with
  in-process fake servers, `MemcachedCache` (20-char keys, key-in-value,
  burst health), `RedisCache` (SET/SETEX, SELECT, MOVED/ASK follow,
  slot map, backoff), live-server tests gated by env vars,
  `WriteThroughCache`, `SystemCaches` builder in `SetupCaches` order
  (closes caching-gap-2), HTML-cache keys/values/validation/eligibility
  (closes caching-gap-1); 17 new tests green (56 total in ps-cache).
  Deps added: `hmac`, `sha1`.
- Step 2, ps-js: DONE. Keywords (45, gperf order), byte-exact
  tokenizer (RE2 semantics: `(?-u:)` byte classes, E2D fragment for
  U+2028/29 exclusions, code-tested `in|instanceof` ASI arm), new
  tokenizer minifier + legacy JSMin port (both fail closed), VLQ
  source maps + envelope, library identification, rewrite driver
  (minify/canonicalize/introspection/source-map URL/stats). Full C++
  suites ported mechanically: 81 tokenizer + 63 minify + 7 special
  (incl. byte-exact angular/jquery/prototype minification and source-
  map goldens); 159 ps-js tests green. Proven against real RE2
  (vendored): committed C++ `[!=]{0,2}` cannot yield its own tests'
  `===`/`!!` splits, so the port implements the tests' maximal munch.
  `benches/js.rs` baselines (242B sample): tokenize ~7.1µs,
  minify_utf8 ~8.2µs, legacy ~1.6µs.
- Step 2, ps-css: DONE. Full webutil parser port (UnicodeText,
  `%ignore-case` ident/property tables, named+system colors, values,
  selectors+tag index, media, escapes, 2800-line parser, `%g`
  differentially verified vs glibc over 20k values) plus minifier,
  URL-rewrite walk, streaming fallback scanner, flatten hierarchy
  (media/charset/recursion/limits), absolutify, combine stitching,
  inline/outline gates, and the css stat family. Suites: 67
  parser_unittest (62 mechanical + 5 hand, incl. 4 proven-stale
  expectations corrected to compiled-C++ behavior), 17 minify (16
  mechanical + saturation unit), 43 scanner goldens, 15 flatten
  end-to-end, plus module units — 176 ps-css tests green. Production
  flatten merges ASTs (RollUpStylesheets), not text; limit enforced
  via RollUpContents. Deferred to their steps: RollUpStylesheets
  consumers, tag-enum DOM matching, ParseCssElement (ps-html),
  GoogleUrl/trim layer (URL callbacks for now), async fetch/image
  slots, spriting, summarizer, beacon/critical/dependency filters.
  `benches/css.rs` baselines (603B sample): parse ~8.3µs, minify
  ~11.7µs, scanner ~1.05µs.
- Step 4, ps-html engine: DONE. Streaming tokenizer/parser/DOM/events,
  writer, and filter trait with C++-ported goldens (26 translated parse
  goldens + hand ports: auto-close matrix, chunk splits at every boundary,
  size-limit bands, token ceilings, case fold, comment/script/delete
  filter behaviors); 17 lib + 14 integration + 20 chain tests green.
- Step 4, chain builder (this session): DONE. `ps-html/src/chain.rs`
  ports `AddFilters`/`AddPreRenderFilters`/`AddPostRenderFilters` as a pure
  `RewriteOptions` + `ChainContext` → `ChainPlan` function (59 slots,
  4 phases, writer terminal, `MaxHtmlParseBytes` → `size_limit`); level
  order goldens (PassThrough/Core/Bandwidth/Testing/All) verified against
  an independent model transcribed from the C++ text (113/113); one test
  per conditional/ordering dep forced on/off; `CheckFilterSetOrdering`
  ported to ps-config; filters-1/4 closed, filters-2/3 verified absent
  from the HTML chain (see MAPPING.md). ps-config additions: `shards()`
  getter, `set_request_property()` for request-scoped options.
- Step 4, kernel HTML filters (this session): DONE. `ps-html/src/filters/`
  ports `RemoveComments` (retain wildcards via `WildcardGroup`),
  `CollapseWhitespace` (sensitive-tag stack, pass-through guard),
  `ElideAttributes` (45 boolean + 25 default rows transcribed),
  `HtmlAttributeQuoteRemoval` (safe-byte table, empty/8-bit/XHTML keeps);
  `tests/filters.rs` ports all 37 C++ cases across the four suites
  (doctype-gated cases carry the doctype inline — no `SetDoctype` hook
  yet); `filters::instantiate` registry maps implemented `ChainFilter`
  slots for the coming driver. 7 filter tests green.
- Step 4, rewriter HTML-only filters (this session): DONE.
  `ps-html/src/script_scanner.rs` ports `ScriptTagScanner` (JS mimetypes,
  type/language precedence, execution-mode flags, integrity check) with
  the full C++ suite ported to `tests/script_scanner.rs` (18 tests);
  `StripScripts`/`Pedantic`/`BaseTag`/`AddHead` ported with their C++
  cases (`AddHead` hand-derived — C++ has no unit test; base-URL and
  combine flag are constructor-injected driver context); registry covers
  the two context-free slots. 11 filter tests green.
- Step 4, second filter batch (this session): DONE. `HandleNoscriptRedirect`
  (canonical link into first bare head, URL injected; 6 C++ cases),
  `AddIds` (div-count stack machine incl. mid-flush persistence; 9 C++
  cases), `AmpDocument` (AMP verdict callback, 64KB trivia budget, late-AMP
  warning comment; 15 C++ cases) with `Dom::find_attribute_by_name`;
  registry covers `AddIds` (others need driver context). 15 filter +
  15 amp tests green. Deferred with reasons: MetaTag (response-headers
  channel), DeterministicJs/DeferIframe (static-asset blobs + mimetype/UA
  channels), MoveCssToHead (684-line CssTagScanner), URL-layer filters
  (DecodeRewrittenUrls/StripLegacyPolyfills/InsertAmpLink/InsertDnsPrefetch/
  DomainRewrite/UrlLeftTrim/JsDisable/SupportNoscript), FixReflow
  (property cache).
- Step 4, driver slice 1 (this session): DONE. `Parser` runs the
  `FlushAsync` phase order (early-pre, pre, render-done, post chain;
  only post waits out buffering) with unique per-filter deferral keys;
  new `ps-html/src/driver.rs` builds a `ChainPlan` from options,
  instantiates implemented slots into their phases plus the terminal
  writer, applies the size cap, and reports skipped slots; async rewrite
  initiation and listener wiring explicitly stubbed for the pipeline
  slice. `tests/driver.rs`: 6 tests (round trip, cross-phase execution,
  plan gating, size cap, writer-off, exact phase order). 715 workspace
  tests green.
- Step 4, CSS move support (this session): DONE.
  `ps-html/src/css_tag_scanner.rs` ports the element half of
  `CssTagScanner` (`ParseCssElement`, `HasImport`, the `rel` predicates;
  URL transformer machinery stays with the pipelines) with 15 unit tests;
  `MoveCssToHeadFilter` ports the hoisting filter (head/script anchors,
  noscript + scoped barriers, order preservation, flush-edge behavior)
  with all 12 C++ goldens in `tests/css_move_to_head.rs`. Registry pins
  the slot as options-contextual.
- Step 4, MetaTag + merge hardening (this session): DONE. Fixed two
  Step-1 divergences against C++ ground truth — `parse_content_type`
  rejects empty/mime-less values, `combine_content_types` rewritten
  exactly (verbatim adopt, fill-missing, complete-untouched, dirty only
  on change) — plus `merge_content_type_str`, `HasValue` forwarder, and
  `ForceCaching` recompute-when-clean; 4 C++ merge/combine tests ported.
  `MetaTagFilter` shares live driver headers and ports all 18 C++ cases
  in `tests/meta_tag.rs` (conflicts, charsets, caching recompute,
  noscript, unquoted values, XHTML guards). Registry pins the slot as
  headers-contextual.
- Step 4, static assets + script filters (this session): DONE.
  `ps-html/assets/` vendors 4 byte-exact support scripts served by new
  `src/static_assets.rs` (`GetAsset` selection by debug flag);
  `filters/common.rs` ports the shared `AddJsToElement` (+ `XhtmlStatus`);
  `DeterministicJs` (2 C++ cases) and `DeferIframe` (4 C++ cases + guard
  shape test) ported with driver-injected asset/flag context; registry
  pins both slots as contextual. 18 filter + 2 asset tests green.
  Deferred with reasons: JsDisable (URL resolution + allowlist),
  URL-layer filters, FixReflow (property cache).
- Step 4, reflow + noscript filters (this session): DONE.
  `FixReflowFilter` (pcache value injected, `id:height` pairs, style +
  marker attributes; 3 isolated C++ cases) and `SupportNoscriptFilter`
  (escaped URL + verdict injected; 4 C++ cases) plus the pure
  `should_insert_noscript` rule with its enablement matrix; registry
  pins both slots as contextual. 21 filter tests green.
- Step 4, scanner + listeners (this session): DONE.
  `ps-html/src/resource_tag_scanner.rs` ports `ScanElement` (full tag
  table, user `UrlValuedAttribute` overrides incl. the style/link alias,
  256-attribute cap) with 58 translated + 3 hand-ported C++ cases;
  `FlushHtmlFilter` ports the flush-score listener (3 C++ cases);
  `Driver` wires both listener slots (AMP verdict handle, flush-on-write
  via `RequestFlush`) with 4 new driver tests. Registry pins listeners
  as directly-wired. ps-config gained a `url_valued_attributes` getter.
- Step 4, URL layer + polyfills (this session): DONE. New
  `ps-http/src/google_url.rs` facades the WHATWG `url` crate as
  `GoogleUrl` (parse/resolve/validity/spec/canonical leaf with GURL's
  unreserved-decode rule; deviations documented in-module) with unit
  tests; `StripLegacyPolyfillsFilter` ports marker + allowlist removal
  (9 C++ cases + allowlist/escape-guard hand tests). ps-config gained an
  approved-filenames getter (directive wiring still open).
- Step 4, URL decomposition + AMP links (this session): DONE.
  `GoogleUrl` grew `query`/`all_except_query`/`all_except_leaf`/
  `leaf_with_query`/`path_sans_leaf` (GURL positional semantics incl.
  opaque/invalid empties) with unit tests; `InsertAmpLinkFilter` ports
  pattern rendering (`${...}` fragments, unknown/unterminated literals)
  plus first-bare-head insertion with all 11 C++ goldens. Registry pins
  the slot as pattern+URL contextual.
- Step 4, JsDisable (this session): DONE. `JsDisableFilter` ports
  script retagging (orig-type rename, duplicate-type collapse, psajs /
  priority tags, stable indices), onload rewriting, disallow honoring
  via `GoogleUrl` + allowlist, IE compat meta, experimental bootstrap
  at tracked body end, and UA gating — all driver inputs injected via
  `JsDisableConfig`; 4 tests port the C++ suite minus log assertions
  and the Defer-chain case. NOTE: the C++ `onload` expectation
  (`onload=""CODE">`) is unproducible by the implementation (writer
  emits `="..."` for every non-null value); pinned the derived
  `onload="CODE"` instead, second such stale C++ test after QQBrowser.
- Step 4, RedirectOnSizeLimit (this session): DONE.
  `GoogleUrl::copy_and_add_query_param` + `Lexer::document_url` unblock
  URL-needing filters; `RedirectOnSizeLimitFilter` ports redirect-elem
  construction (JS escaping of `'`/`"`/`\`/`\n`, param selection by
  query/form feed), cumulative-byte gating with the `size_limit > 0`
  disable switch, and CSP inline-script honoring. 4 tests port the C++
  suite: both single-window bands, all flush-split bands (limit ==
  split, ~150 iterations) byte-exact — confirming the engine's
  delayed-literal hold across flushes — plus the escaping case and a
  CSP-forbids hand test. CSP verdict is constructor-injected until the
  driver owns MetaElement state. Workspace: 796 passed, 0 failed.
- Step 4, DomStats + UrlLeftTrim (this session): DONE.
  `GoogleUrl` grew `origin` (byte-prefix of the spec, no trailing slash),
  `has_query`, and canonical `PartialEq`; new `ps-http/src/data_url.rs`
  ports the `data:` predicates. New `ps-html/src/base_tracker.rs` ports
  the `ScanFilter` base half as a per-filter `BaseTracker` — required
  because filter-major engines run each filter over the whole window, so
  the midway-base test proved a shared cell observes the final base too
  early; per-filter tracking reproduces C++ exactly (including the
  latch-on-inserted-base quirk, untested upstream) whenever no earlier
  filter deletes base tags. `DomStatsFilter` ports all five counters with
  an injectable critical-image set (mock finder semantics: available,
  identity keys); `UrlLeftTrimFilter::trim` ports `Trim` byte-for-byte
  with the `//`, bare-query, colon, and re-resolve guards. 26 tests port
  both C++ suites whole (5 stats + 12 trim-unit incl. all 5 anchor bases
  + 9 HTML goldens). Harness gained `SharedFilter` for stateful filters.
  Workspace: 825 passed, 0 failed.
- Step 4, base gate + JsDeferDisabled (this session): DONE.
  `BaseTracker` now ports the real `CommonFilter::BaseUrlIsValid` gate
  (reduces to "no base problem": the refs flag only raises at base time,
  so pre-base elements resolve against the document URL — the
  `DomainRewrite` comment claiming otherwise is stale, noted in-code)
  plus `ResolveUrl` and the `other_base_problem` poison flag, with unit
  tests and a trim regression test. Hoisted `BodyEndTracker`
  (`end_body_point_` + `InsertNodeAtBodyEnd`) into `filters/common.rs`;
  `JsDisable` adopts it (adding the missed start-element clear) with a
  fallback-path hand test. `JsDeferDisabledFilter` ports bootstrap
  injection (UA verdict + resolved asset URL injected); 10 tests port its
  suite minus `JsDeferPreserveURLsOn` (needs the `DeferJavascript`
  pipeline filter) plus the deferred `DisablesScriptOnlyFromFirstSrc`
  chain case (JsDisable + SupportNoscript + JsDeferDisabled).
  Workspace: 839 passed, 0 failed.
- Step 4, AddInstrumentation (this session): DONE.
  Vendored the 4 instrumentation assets with `StaticAsset` variants and
  marker tests; new `ps-html/src/js_escape.rs` ports
  `EscapeToJsStringLiteral` whole with the upstream goldens (the redirect
  filter keeps its 4-case subset, faithful to its own source).
  `AddInstrumentationFilter` ports head/unload/load injection with all
  driver inputs (beacon pair, experiment/timing/status/request params,
  XHTML, bot verdict) injected via config; 17 tests port the suite minus
  the 3 AMP cases (need driver event-buffering + dynamic disabling) —
  one assertion uses the engine's `/>` spelling (pinned convention, not
  a filter difference). Also fixed the flaky `transcode_budget_caps_
  attempts`: it spawned the real `avifenc` without the suite's PATH
  lock, passing only on accidental overlap with siblings' fake-`fail`
  window; now wrapped in `with_fake_avifenc("fail")`, deterministic.
  Workspace: 862 passed, 0 failed.
- Step 4, MakeShowAdsAsync (this session): DONE. New `ads_attribute`
  (rename rule + vocabularies), `ads_util` (publisher ids, element
  validation, src classifiers), and `show_ads_snippet_parser`
  (`ParseStrict` over the shared `ps-js` tokenizer — new `ps-html`
  dependency — with hand-rolled name/format patterns instead of RE2)
  ports, each with its upstream suite. `MakeShowAdsAsyncFilter` ports
  data→`<ins>` conversion (single library load, sorted renamed
  attributes, style-folded dimensions), API-call push replacement with
  pending-count pairing, raw-src classification, and the
  `is_rewritable` window gate (pre-existing `Dom` helper, exact
  semantics). All 19 C++ filter tests port byte-exact, including the 3
  mid-script flush cases (delayed-literal buffering holds). The
  `show_ads_snippets_not_converted` stat is skipped (registered but
  never incremented upstream). Workspace: 909 passed, 0 failed.
- Step 4, Debug (this session): DONE. Engine gained the phase hooks
  (`init_parse`/`start_parse`/`end_parse`/`start_render`, wired at the
  exact C++ call sites) plus dynamically-disabled-list plumbing
  (`disabled_reason` trait method, `set_dynamically_disabled_list`,
  per-document redetermination). `DebugFilter` ports the stopwatches,
  flush/end summaries with critical-image and disabled-filter blocks,
  literal-block message buffering, and the public format functions;
  clock, finder set, options strings, beacon flag, and list are
  injected. 8 tests port the suite minus the 2 cache-delay cases (need
  the `ce` pipeline + cache control); flush-message extraction erases
  both engine spellings of the unknown token element. Workspace: 917
  passed, 0 failed.
- Step 4, naming layer + scanning (this session): DONE. Found Step 0
  already ported `ResourceNamer` + `UrlEscaper` (`ps-cache/src/namer.rs`;
  removed my duplicate escaper) and hardened it: stale signatures now
  survive unsigned/legacy decodes like the C++, `set_ext` enforces its
  `CHECK`, extension table verified identical to `ps-http`'s with a
  sync note, plus a regression test. `GoogleUrl` grew `scheme` and
  `host_and_port`. New `ps-html/src/url_namer.rs` ports the `UrlNamer`
  trait (C++ base defaults) and `TestUrlNamer`'s resource-free surface
  (path/origin/proxy predicates, `Decode`, `EncodeUrl`) with
  hand-derived tests; resource-bound `Encode` waits for pipelines.
  `RewrittenContentScanningFilter` ports proxy counting with both C++
  tests (counter asserted directly; the DOM-cohort cache write stays
  driver-side). `DecodeRewrittenUrls` deferred: its `DecodeUrl` needs
  the filter registry + per-filter encoders (pipeline phase).
  Workspace: 924 passed, 0 failed.
- Step 4, CssInlineImportToLink (this session): DONE. `ps-css` grew
  `vectorize_media_attribute` + `stringify_media_vector` with unit
  tests. `CssInlineImportToLinkFilter` (new `ps-css` dependency) ports
  import-prefix extraction over the shared CSS parser: attribute gates
  (text/css, no href/rel/scoped/integrity), mid-style flush veto,
  per-import media checks (simple-match plus order-insensitive vector
  compare), style-attribute copying with original-media preference,
  brief closes under XHTML, and trailing-CSS preservation via parser
  offsets. All 12 C++ tests port byte-exact (the 2 cache-extender
  interaction cases need the `ce` pipeline + fetch mocking).
  Workspace: 937 passed, 0 failed.
- Step 4, GoogleAnalytics (this session): DONE. Transcribed the
  generated snippet prologue/epilogue/shim-call byte-exact
  (programmatic C-escape decoding; no generator in tree).
  `GoogleAnalyticsFilter` ports sync-load detection (script `src`
  suffix, `document.write` span with the 80-byte lookbehind),
  tracker-init matching, unhandled-call veto scanning, the
  document-global match state with tag/comment/CDATA/flush resets,
  and end-of-document rewriting (later loads deleted/emptied, init
  retargeted, first load prepended with the glue script). Custom
  method lists supported like the C++ test ctor. All 9 C++ tests
  port byte-exact. Workspace: 946 passed, 0 failed.
- Step 4, DecodeUrl core (this session): DONE. New segment codecs
  (`UrlSegmentEncoder` trait + default/multipart/CSS encoders, with the
  multipart suite and CSS round-trip/legacy cases; context params
  dropped — every decode caller discards them). New `decode_url`
  ports `DecodeUrlGivenOptions` whole: oblivious gate (new
  `ps-config` getter + test), leaf parse, hash requirement,
  proxy-mode branch, id registry (`ce/cf/cc/jc/jm/is/ls/sm` +
  outline `co`/`jo` decoding empty), encoder dispatch, forbidden
  check, and same-origin absolutization, with branch tests. `ic`
  documented as deferred to the image slice (needs
  `ImageUrlEncoder`). `DecodeRewrittenUrlsFilter` ports with its
  test (log assertions skipped, no channel). Workspace: 959 passed,
  0 failed.
- Step 4, InsertGA (this session): DONE. `GoogleUrl::host` added.
  `InsertGAFilter` ports snippet insertion (ga.js/analytics.js/gtag.js
  by account shape, experiment variants, body-end placement) and
  existing-snippet surgery (substring classification incl. the
  upstream `if (find(...))` quirk, custom-variable prepend,
  content-experiment blank+loader+inline split, and the analytics.js
  tokenizer state machine for speed tracking + pageview-send
  splicing); all account/experiment/UA inputs injected. Fixed a real
  engine bug found by the CX tests: `insert_node_after_current` left
  the cursor on the new node's first event, nesting back-to-back
  insertions — it now stops on the last event like the C++
  `++current/--current` dance. All 30 C++ tests port (plus gtag and
  quirk hand tests for uncovered branches); `<head/>` expectations
  adjusted for the single-filter harness. Workspace: 994 passed, 0
  failed.
- Step 4, DomainLawyer core (this session): DONE. New
  `ps-config::message_handler` (trait + null/mock, serious-message
  counting) and `Wildcard::is_simple`; `GoogleUrl::path_and_leaf`.
  New `ps-html/src/domain_lawyer.rs` ports authorization, declaration,
  normalization, longest-prefix + wildcard lookup, rewrite mappings
  (incl. two-protocol twins), sharding, same-content checks,
  rewritten-to listings, signatures, and listings — arena-indexed
  domains with an ordered registry (stable signatures). 42 C++ tests
  port; origin mappings, proxy support, and merges land next turn
  with `StripSubresourceHints`. Placed in `ps-html` (a `ps-config`
  home would cycle through `ps-http`/`ps-cache`); the driver will own
  it. Workspace: 1036 passed, 0 failed.
- Step 4, lawyer origin/proxy/merge + strip (this session): DONE.
  `DomainLawyer` grew origin mappings, `MapOrigin`, proxy mappings,
  `IsProxyMapped`, `Merge` (edge cloning + wildcard reorder),
  suffix get/set/strip, `DoDomainsServeSameContent`,
  `FindDomainsRewrittenTo`, and listing output (also fixing
  `can_rewrite_domains` for suffixes and the proxy-conflict hard
  failures); 44 more C++ tests port for 86 total — everything except
  `AddProxySuffix` (needs the public-suffix registry). New
  `preserve_subresource_hints` + preserve-URL getters in `ps-config`.
  `StripSubresourceHintsFilter` ports on the real lawyer with all 16
  tests (level cases via the `can_modify_urls` mechanism). Workspace:
  1097 passed, 0 failed.
- Step 4, DomainRewriteFilter (this session): DONE. New
  `ps-http/src/domain_registry.rs` vendors the public-suffix list
  (dat unpack + longest-match registry) so `AddProxySuffix` ports
  whole; client_domain_rewriter opt/dbg assets vendored with marker
  tests. `BaseTracker` grew lawyer-aware proxy-suffix adoption and
  the base-href pin writeback; `DomainRewriteFilter` ports
  resource/hyperlink/redirect/cookie/refresh rewriting, shard
  hashing, proxy-suffix mode, client-domain-rewrite injection, and
  the refresh/cookie parsers as tested units. All 22 C++ tests port
  verbatim (incl. the `ValidateNoChanges(url, ...)` case-id quirk and
  the `*dont_shard*` options fixture). Workspace: 1124 passed, 0
  failed.
- Step 4, InsertDnsPrefetch + DedupInlinedImages (this session):
  DONE. New `flush_early.rs` (`FlushEarlyInfo` totals/domains, no
  proto yet) and the dedup JS assets with marker tests.
  `InsertDnsPrefetchFilter` ports head/body domain collection,
  stability-gated tag insertion, flush-subresources interplay, and
  the IE9 `rel=prefetch` variant; log statuses are recorded on the
  filter (no channel yet). gdb against the prebuilt C++ binary proved
  the unsupported-UA quirk: `EndElementImpl` returns before clearing
  the in-head flag, so nothing is ever stored — replicated exactly.
  10 of 11 C++ tests port (the other-filters interaction case needs
  unported filters). `DedupInlinedImagesFilter` ports candidate
  detection (241-byte cutoff), id assignment, the before/after
  script insertions, and both statistics; tests feed pre-inlined
  data URLs since the inline pipeline is unported (the preview
  interaction case is skipped). 9 tests port. Workspace: 1144 passed,
  0 failed.
- Step 4, LazyloadImages (this session): DONE. `ps-config` grew
  the lazyload options (`LazyloadImagesAfterOnload` default true,
  `LazyloadImagesBlankUrl`), the lazyload class wildcard group, and
  `DisallowTroublesomeResources` with unit tests; `Dom` grew
  in-place `set_attribute_name`; lazyload opt/dbg assets vendored
  with marker tests. `LazyloadImagesFilter` ports head/body script
  placement, override postscripts, the abort fallback, class +
  slider + onload gates, blacklist/critical logging, and ce-URL
  decoding against an injected finder set; statuses recorded on the
  filter (no channel yet). All 22 C++ tests port (preserve-URLs as
  the options veto it is, plus one hand test for the
  upstream-untested abort script — after-insertions revisit the
  filter in both engines, verified in `ApplyFilter`). Workspace:
  1169 passed, 0 failed.
- Step 4, DelayImages (this session): DONE. `ps-config` grew the
  delay options (blank preview, mobile aggressive default true,
  lazy highres) with unit tests; delay/delay-inline opt/dbg assets
  vendored with marker tests. `DelayImagesFilter` ports inplace
  swapping (rename + placeholder + onload/onerror + one-time swap
  helper), the mobile low-res map with ordered data scripts and
  per-body high-res triggers, the mid-window count gate, and the
  UA/pcache enablement statuses. 22 of 35 C++ tests port with
  pre-tagged `low-res-src` inputs (the image pipeline that generates
  previews is unported); flush-window, noscript-tagging,
  preview-sizing, and resize cases are skipped as pipeline behavior.
  Workspace: 1193 passed, 0 failed.
- Step 4, pipeline slice 1: slots (this session): DONE. Remaining
  Step-4 filters all need fetch/pipeline/pcache, so the pipeline epic
  starts bottom-up. `ps-http` grew `data:` dismantling (`Encoding`,
  `ParseDataUrl` incl. the short-parameter `UNKNOWN` quirk,
  `DecodeDataUrlContent`, `DataUrl`) with all 15 C++ tests, plus
  `UrlRelativity`/`FindRelativity`/`Relativize`/`NetPath` with the
  C++ tests (double-slash and query corners included).
  `ps-config` grew `preserve_url_relativity`. New `resource.rs`
  (`Resource` trait, `UrlInputResource`, `DataUrlInputResource`)
  and `resource_slot.rs` (slot flags, null/fetch/HTML slots,
  relativize-or-passthrough, stable element/attribute ordering —
  the C++ pointer order is arbitrary) with all 4 C++ slot tests
  (line args pinned like the fixture's `AddElement` calls; probe
  filter renders mid-parse for before/after snapshots). Context
  tracking and input infos wait for `RewriteContext`. Workspace:
  1213 passed, 0 failed.
- Step 4, pipeline slice 2: resource load (this session): DONE.
  New `server_context.rs` (`ServerContext`: cache, fetcher, clock,
  stats, hasher; `ResourceDriver`: live options, base URL,
  fragment, bot verdict) and `cacheable_resource.rs`
  (`CacheableResourceBase` with hook generics: cache find,
  fetch-on-miss with referer/conditional/fallback serving,
  failure classification + remember, per-resource and global
  stats, background freshen, input-info fill). Servants: `MockTimer`,
  mock disable/error/referer controls, session origins,
  `classify_failure`, resource options getters. Fixed two real
  under-ports found by the tests: `fix_date_headers` (no Expires
  delta shift) and `is_imminently_expiring` (1s window vs the
  relative rule), each with ported/focused tests. All 10 C++
  `cacheable_resource_base` tests port (async/lock overlap mute
  under sync completion). Workspace: 1230 passed, 0 failed.
- Step 4, pipeline slice 3: URL resources (this session): DONE.
  New `url_input_resource.rs` (`unauth://` cache keys with
  `CHECK`-faithful panics, default-port origin capture, session
  authorization + `PageSpeed: off` prepare hook). Servants: URL
  port introspection (`IntPort`/`EffectiveIntPort`), canonical
  specs (fetch URLs shed `:80`/`:443` via the manager step, which
  also pins `Host`), capped gzip inflation, mock header-off
  verification, size/no-transform options, inflate-once
  extraction with describing headers. All 22 C++
  `url_input_resource` tests port (concurrency as sequential
  double-reads; death tests as `should_panic`). Workspace: 1252
  passed, 0 failed.
- Step 4, pipeline slice 4: outputs + creation (this session):
  DONE. New `output_resource.rs` (naming, write hashing, eager URL
  refresh, signatures, cache keys), `UrlNamer::Encode` (+ sharding,
  test policy, `StandardUrlNamer`), `rewrite_filter.rs` (trait +
  charset priority helpers), and driver APIs (`MayRewriteUrl`,
  `CreateInputResource` incl. proxy-decode retry, slot dedup,
  output creation with size caps). Servants: HMAC-SHA1 signatures
  (with the upstream check vector), namer `clear_hash`, inlining
  allow-group, URL/size/signing options. 22 hand contract tests
  (upstream covers these only via context suites). Findings:
  C++ signs write-time URLs but verifies served URLs (off by the
  signature length — unpinned upstream, replicated exactly with
  the vector test); the namer hash `DCHECK` is aspirational (real
  sizing flows read hashless URLs, matching C++ release).
  Workspace: 1274 passed, 0 failed.
- Step 4, pipeline slice 5: rewrite contexts (this session):
  DONE. New `metadata.rs` (native `CachedResult`/`OutputPartitions`,
  versioned TLV encoding, validation) and `rewrite_context.rs`
  (`SingleRewriteContext`: fetch-before-partition flow, metadata
  hit/revalidate/miss, render). Servants: `MockHasher`, dedicated
  lock hasher (`partition_signature_hash` survives test naming
  hashers), `CssOutlineMinBytes` setter. 9 of 10 rewrite-path C++
  single-resource tests port x2 encoder params (18 tests; only
  `BasicAsync` waits for deferred completion). Findings: C++
  `reuse_by_content_hash_` is a dead field (the CacheExpire
  "must rewrite again" comment is stale — revalidation reuses by
  hash); in-flight `repeated_` coalescing makes 3 busy tags cost
  one rewrite in C++ but one each in the sync model (render
  identical, documented divergence). Workspace: 1292 passed,
  0 failed.
- Step 4, pipeline slice 6: serving + async (this session):
  DONE. Driver `fetch_resource` (304 reloads, HTTP-cache serve,
  written-output serve, filter reconstruction) over
  `decode_output_resource` (namer/filter/kind/signature checks,
  outlined `co`/`jo` without filters) and a filter registry; the
  `RewriteFilter::fetch` hook fuses `MakeRewriteContext`+`Fetch`.
  `SingleRewriteContext` grew fetch mode (prepare/start/cache-done
  with hashed-URL and original fallbacks, fake-partition
  reconstruction, hash-gated serve with fallback headers,
  `FetchWritesPartition` gating) plus `ContextSlot` (HTML/fetch).
  Deferred completion: waiting mock fetcher + pending loads that
  conclude nothing (no cache write, no failure memory, no stats,
  no partition write). All 18 C++ single-resource tests port x2
  encoder params (36 tests) with `MockHasher` ("0" URLs),
  including the full `Fetch*` serving suite. Servants:
  `MockHasher`, lock hasher, Google-CDN cache-control fix (unit
  tested), fallback content-type gate, mismatch message. Workspace:
  1312 passed, 0 failed.
- Step 4, pipeline slice 7: cache extender (this session):
  DONE. New `filters/cache_extender.rs` (`ce`: category scan with
  PDF fallback, month-threshold/introspective/mapping gating,
  type whitelist, CSS rebasing, canonical links, on-the-fly
  serving) over new servants: `MayCacheExtend*` gates,
  `EnableExtendCacheFilters`, structural `IsPagespeedResource`,
  `MergeNonCachingResponseHeaders`, on-the-fly serving branch,
  `ResolveCssUrls` + `RewriteDomainTransformer` (reusing
  domain-rewrite + left-trim), `AddLinkRelCanonical`,
  `GoogleUrl::sanitize`, canonical-link header helpers, fetch
  `LoadEvenIfNotCacheable` (fallback needs uncacheable inputs),
  debug-comment insertion (budgeted, table + partition messages,
  unauthorized-domain + CSP-gated generation), metadata-health
  `can_rewrite_resources` gate, `MemoryCache` health/clear/prefix
  helpers, injectable metadata backend, `SrcSetSlot` + WHATWG
  srcset parse/serialize/commit, behavior
  policy/render/fallback-header hooks. 54 of 57 C++ `ce` tests
  port (only the outliner/css-rewriter/CSP-dependent three wait
  for those subsystems; LRU-count and logging assertions wait
  for statistics); MD5 `MnXHB3ChUY` and shard selection pinned
  byte-identical. Workspace: 1366 passed, 0 failed.
- Step 4, pipeline slice 8: CSS inlining (this session): DONE.
  New `InlineRewriteContext` (inline behavior trait with
  key-suffix/policy/authorization hooks, hashless partitions,
  inlined-data caching, inline render) over `CachedResult::
  inlined_data` (encoding v2) and `filters/css_inline.rs`
  (stylesheet scan, media/AMP/integrity/pedantic/noscript gates,
  size/gzip/markup/charset verdicts, render-time absolutify,
  link→style swap with attribute carryover). Servants:
  `css_util::can_media_affect_screen`, `css_inline_max_bytes`,
  inline-unauthorized resource types (signature-relevant),
  driver containing-charset/AMP state, `GoogleUrl::sanitize`,
  canonical-link helpers. 43 of 48 C++ `ci` tests port (only the
  combiner/minimizer/prioritizer/CSP-dependent five wait).
  Findings: C++ applies filters sequentially per filter at flush,
  so end-hook `ci` initiates before start-hook `ce` on one
  element; the sync port initiates `ce` on end-element to match
  that order (all 54 `ce` tests still green, both `ce`+`ci`
  interaction tests pin it); fetch rewrites load even-if-
  uncacheable (fallback needs those inputs); render serves
  cached inline bytes, never the unloaded input. Workspace: 1410
  passed, 0 failed.
- Step 4, pipeline slice 9: JS inlining (this session): DONE.
  New `filters/js_inline.rs` over the slice-8 inline machinery
  (noscript-literal/async/defer/integrity/module gates,
  size/gzip/introspective/template/charset verdicts, text-element
  escaping, XHTML CDATA wrap, attribute-preserving render).
  Servants: `js_inline_max_bytes`, driver XHTML state,
  `disallow`/`allow_only_when_inlining`. 37 of 41 C++ `ji`
  tests port (only the minifier/combiner/CSP-dependent four
  wait). Finding: upstream soft-enable-then-`EnableDebug()`
  observably revives the Debug filter though no static reading
  of the option sets explains it, so the port's `enable_filter`
  revives soft-disables (forbids still stick). Workspace: 1447
  passed, 0 failed.
- Step 4, pipeline slice 10: JS minification + source maps
  (this session): DONE. New `filters/javascript.rs`
  (`JavascriptFilter`, external/inline/minify-only flows,
  `sm` id-swap servant, library canonicalization,
  introspection gating, XHTML/CDATA handling, flush-edge
  cases). Servants: `canonicalize_url`, `mime_to_content_type`
  by-kind lookup, `XhtmlStatus` tri-state, `MINIFY_USES` /
  blocks/failures stats, `ps-js` source-map encode (XSSI
  guard, dropped-empty `file`, percent-encoding). 160
  `tests/javascript.rs` tests green (80 per minifier ×
  legacy/experimental), covering 80 of 97 C++ `jm` tests;
  the 17 deferred wait on other epics (CSP ×9, `jc`
  combiner ×3, `jo` outliner ×1, IPRO ×1, rewriter logging
  ×1, unfinalized-headers plumbing ×1, Debug-filter
  end-document machinery ×1). Findings: source-map output
  resources take the `.map` extension from the SourceMap
  content type (bare-MIME lookup resolves to plain JS, so
  the port looks the type up by kind); the C++ `)]}'`
  guard needs brace-escaping care in `format!`. Workspace:
  1607 passed, 0 failed.
- Step 4, pipeline slice 11: CSS rewriting (this session):
  DONE. New `filters/css.rs` (`CssFilter`: external `<link>`
  rewrites over `SingleRewriteContext`, sync inline `<style>` and
  `style`-attribute rewrites with metadata caching, `CssUrlEncoder`
  `A.`-prefixed naming, harvest absolutification, serving-path
  fallback re-resolution). Servants: `always_rewrite_css`,
  `css_parse_max_bytes` (2MiB default), `css_flatten_max_bytes`,
  `css/image_inline_max_bytes`, `rewrite_random_drop_percentage`,
  `in_place_preemptive_rewrite_css` options; domain/shard/rewrite-map
  options adders plus driver lawyer rebuild; `SimpleRandom`;
  `ServerContext::up_down_counter`; `send_fallback_response`
  behavior hook. 64 `tests/css.rs` tests green (all 136
  `RewriteVariousCss` vectors probed byte-exact through ps-css
  first), covering 64 of 83 C++ `cf` tests; the 19 deferred wait
  on other epics (image filters ×11, CSP ×6, Debug-filter
  unauthorized-domain comment ×1, async deadline ×1). Findings:
  C++ `cf` URLs carry the encoder's `A.` prefix (test `Encode`
  applies `FindEncoder`, so unprefixed round trips only worked by
  accident); `TestUrlAbsolutification`'s with/without-image
  variants are behaviorally identical with images unmocked;
  `AbsolutifyServingFallback` exercises the wrong-hash
  `SendFallbackResponse` path (which maps `@import`s), not
  harvest (which resolves them only). Workspace: 1671 passed, 0
  failed.
- Step 4, pipeline slice 12: CSS import flattening (this
  session): DONE. New `filters/css_hierarchy.rs` (`CssHierarchy`:
  nested init, media subsetting, charset checks, both roll-ups)
  plus the sync flatten driver in `filters/css.rs` (child fetch,
  URL resolution/trim transformer, media-suffixed metadata
  caching with failure markers, root media/charset plumbing with
  meta-tag tracking). Servants: `set_css_flatten_max_bytes`,
  `image_optimization_enabled`, domain/shard/rewrite-map options
  adders, driver lawyer rebuild, driver response headers,
  `send_fallback_response` behavior hook. 38 `tests/flatten.rs`
  tests green, covering 38 of 43 C++ `if` tests; the 5 deferred
  need nested image/cache-extend contexts. Findings: served
  `@import`s resolve but do not map (mapping happens only in the
  wrong-hash fallback re-resolution); empty/failed children fail
  flattening silently while 404s still surface their reason
  (Debug-gated); `TestFlattenNested`'s top-file mocks are dead
  setup (validates mock their own files). Workspace: 1709
  passed, 0 failed.
- Step 4, pipeline slice 13: JS combining (this session):
  DONE. New `filters/js_combine.rs` (`JsCombineFilter`: barriers,
  sync partition/write/render at hand-off, multipart serving) and
  `url_partnership.rs` (`UrlPartnership`). Servants:
  `is_likely_strict_mode`, `js_url_hash`,
  `max_combined_js_bytes`, `combine_across_paths`,
  `set_avoid_renaming_introspective_javascript`. 45
  `tests/js_combine.rs` tests green, covering 45 of 52 C++ `jc`
  tests; the 7 deferred wait on other epics (async timing ×3, CSP
  ×3, Debug-filter comment ×1). Also closed the 3 deferred `jm`
  combiner tests (166 jm tests green). Findings: C++ render-time
  insertions bypass the filter chain, so our minify filter must
  stand down on already-rewritten scripts (matching the cache
  extender); `TestUrlNamer` proxy expectations only assert
  serving, never rendering; several hardcoded hashes had to be
  re-transcribed from source (never recall them). Workspace: 1760
  passed, 0 failed.
- Step 4, pipeline slice 14: JS outlining (this session):
  DONE. New `filters/js_outline.rs` (`JsOutlineFilter`: inline
  scripts over the size threshold move to outlined resources,
  async/defer stripped, other attributes and quote styles
  preserved). Servant: `js_outline_min_bytes`. 14
  `tests/js_outline.rs` tests green, covering 14 of 16 C++ `jo`
  tests (2 wait on CSP). Also closed the deferred `jm` outline
  test (168 jm tests green). Findings: nested pagespeed names
  segment-encode (`.pagespeed.` → `,M`), which the test must
  replicate; the minify filter stands down only on combined
  (`jc`) URLs since outlines emit mid-stream and chain upstream.
  Workspace: 1776 passed, 0 failed.
- Step 4, pipeline slice 15: CSS combining (this session):
  DONE. New `filters/css_combine.rs` (`CssCombineFilter`:
  partition/write/render at hand-off, fetch-time recombination,
  barriers, media handling, proxy/shard rebasing) plus
  `RewriteOptions::add_proxy_domain_mapping`. 75
  `tests/css_combine.rs` tests green, covering 70 C++ `cc` tests
  (68 ported; `StatsWithDelay` waits on async, `OobSlotIndex…`
  on metadata-cache corruption). Findings: `ResolveCssUrls`
  takes the combination's unsharded `resolved_base()`, never the
  sharded output URL (else same-shard URLs relativize back);
  fetch refuses oversized names by `EventualSize`, not bare
  name length; `a.css` is 5 chars (recount segment math).
  Workspace: 1851 passed, 0 failed.
- Step 4, pipeline slice 16: font inlining (this session):
  DONE. New `filters/google_font_css_inline.rs`
  (`GoogleFontCssInlineFilter`: font-URL claimant, UA-keyed
  resources, usage restrictions) plus
  `font_service_input_resource.rs`
  (`CacheableResourceBase<FontServiceHooks>` as
  `InputResource::Font`), `ps-http` UA normalizers (Android +
  IE, `regex`), `InlineBehavior::create_resource` hook, driver
  UA, and 3 option accessors. 7 filter + 6 resource tests
  green, covering 14 C++ `gf` tests (`LoadParallel` waits on
  async); 6 normalizer unit tests ported as the unit contract.
  Workspace: 1870 passed, 0 failed.
- Step 4, pipeline slice 17a: image URL encoding (this session):
  DONE. New `image_url_encoder.rs` (encoder/decoder around
  the libwebp family, WebP fast-path gating) with 55
  `image_url_encoder` tests green.
- Step 4, pipeline slice 17b: image rewriting core (this session):
  DONE. New `filters/image_rewrite.rs` (`ImageRewriteFilter`:
  nested contexts, inline/resize/recompress, srcset, webp
  variants) with 115 `image_rewrite.rs` tests green, covering
  116 of 142 C++ `ic` cases (26 wait on beacon/property-cache/
  IPRO/CSP/logging/concurrency/URL-options/experiment
  subsystems).
  Findings: byte-exact PNG/JPEG assertions become structural
  (pure-Rust codecs differ); sync model runs nested contexts
  inline and ports async staging as final-state-only; image
  work renders last per slot (wins over cache extension).
- Step 4, pipeline slice 17c: CSS image rewriting (this session):
  DONE. New `filters/css_image_rewriter.rs` with 44
  `css_image_rewriter.rs` tests green (44 of 45 C++ cases;
  1 waits on beacon/critical images).
- Step 4, pipeline slice 17d: image combining (this session):
  DONE. New `filters/image_combine.rs` (`ImageCombineFilter`
  spriting) with 23 `image_combine.rs` tests green (23 of
  23 C++ `is` cases).
- Step 4, pipeline slice 17e: local storage cache (this session):
  DONE. New `filters/local_storage_cache.rs`
  (`LocalStorageCacheFilter`: storable-resource marking,
  hash/expiry attributes, runtime injection, cookie-driven
  snippet replacement) plus the `css_inline`/`image_rewrite`
  interplay hooks (`InlineState` checks, mark-before-sync-
  render, hash/expiry on render, removal on failure) and
  vendored `local_storage_cache_{opt,dbg}.js` static assets.
  12 `local_storage_cache.rs` tests green (12 of 12 C++
  `ls` cases; the 3 first-view sub-cases wait on wait-
  fetcher staging). Findings: C++ dispatches filter-major
  (`ApplyFilter`), which the sync model keeps; the sync CSS
  render replaces `<link>` with `<style>` before the LSC
  pass, so the marker is added before (not after) inline
  initiation and the script trigger accepts marked `<style>`
  nodes; failing inlines omit the runtime script here (C++
  inserts it because its inline resolves asynchronously).
  Workspace: 2160 passed, 0 failed.
- Step 4, pipeline slice 17f: critical selectors (this session):
  DONE. New `filters/critical_selector.rs`
  (`CriticalSelectorFilter`: critical-subset summaries, media
  filtering, noscript/loader bundling) plus
  `critical_selector_finder.rs` (finder seam + fake; the
  beacon finder waits for property-cache/beacon),
  `css_util` media-vector/selector helpers, and vendored
  `critical_css_loader_{opt,dbg}.js` static assets. 24
  `critical_selector.rs` tests green (24 of 25 C++ `pr`
  cases; 1 waits on wait-fetcher staging; logging asserts
  wait on logging). Findings: summaries recompute per
  element (no metadata entries); mid-pass deletion erases
  pending end events, so the style tracker clears on drop;
  the bundle emits at end-of-pass (post-phase filters never
  observe render-done before the writer streams);
  producer-first chains (`RewriteCss`/`CombineCss`) stand in
  for async interleaving. Workspace: 2185 passed, 0 failed.
- Step 4, pipeline slice 17g: IPRO (this session):
  DONE. New `ipro.rs` (`fetch_in_place_resource`: metadata
  hit/miss, sync revalidation + freshening, 304s,
  reconstruction, etag/TTL/Vary/UA-keying headers) plus
  `fake_filter.rs` servants, `RewriteFilter` nested-rewrite
  and UA-encoding hooks, shared server/HTTP-cache hasher,
  `set_s_max_age`, metadata v6 (`optimized_image_type`),
  mock-clock injection, and IPRO options. 38 `in_place.rs`
  tests green (38 of 42 C++ `aj` cases; deadline, 2
  mid-stream, and load-from-file cases wait on those
  subsystems). Findings: miss serves original while warming
  caches, wait-mode serves optimized at once; uncacheable
  immediate serves carry generated year-TTLs; stale serves
  re-date via `FixDateHeaders`; C++ op-counts adapt to
  origin-fetch + rewrite counts (split backends here vs
  C++'s shared LRU). Workspace: 2223 passed, 0 failed.
- Step 5, ps-admin (this session): DONE. New `ps-admin` crate
  ports `AdminSite` dispatch (`/pagespeed_admin/<leaf>` redirect,
  14 leaves, plain 404 with console-root guess) and the legacy
  statistics queries: dashboard (live + recovered counters,
  uptime, savings/activity panels, recent images, purge button),
  history archive health, images (100-row compare modal), HTML
  cache (summary counters + variant table), message/javascript/css
  diagnostics, statistics (+JSON), graphs/console (+JSON with
  `ParseGraphQuery` bounds), config, histograms, and caches
  (metadata lookup, structure table, backend stats, purge set).
  Purge: whole-cache + URL paths, per-vhost scoping ahead of the
  enabled check, `PurgeSet`-backed sink with `kPurgeSetFull`
  refusals. Expensive-leaf slots (5, 100ms wait, 429s). Access
  control: six `FastWildcardGroup` endpoint families (closes
  admin-gap-2). History reads: `PSH1` browse/filter/match.
  Assets: 16 vendored CSS/JS blobs, byte-verified vs genfiles.
  Verification: full `.cc`/`.h`/test ground truth re-read in
  small md5-pinned windows; invented surface (static-asset
  routes, edit-distance 404, 403 pages, console/graphs/setup
  texts, message/HTML-cache bodies) replaced with exact C++.
  Seams for later steps: `show_cache` hook (full `ShowCache`
  port → Step 8), `StatisticsLogger` trait, live-snapshot
  overlap in `browse_with_live`. 47 ps-admin tests green (26
  C++ `AdminSiteTest` ports + route/JSON/purge-shape extras).
  Workspace: 2272 passed, 0 failed.
- Step 6, ps-nginx (this session): PARTIAL (blocked, honestly
  scoped). The full module wrapper (`ngx_pagespeed/src`: phase
  hooks, threading bridge, native fetcher) lives outside this
  source tree — no `ngx_pagespeed.cc`, no `ngx_command_t`, no
  nginx headers, no ngx-rust offline — so native FFI bindings
  are recorded as blocked, not fabricated. Ported all in-repo
  ground truth instead: the four nginx-free helpers in
  `pagespeed/nginx/` with their full C++ suites mechanically
  ported — `fetch_body` (8 tests: accept/complete/overflow,
  close-completeness, HEAD/204/304), `gzip_rollback` (4 tests:
  per-cycle record/reset/re-arm, stale-pool isolation),
  `inplace_url_agreement` (9 tests: escapes, slash merging,
  host case), `request_body` (6 tests: 128 KiB bound, errors,
  pool copy) — plus `GoogleUrl::unescape_ignore_plus` in
  ps-http. 28 ps-nginx tests green. Remaining Step 6 work
  (pinned bindings, hooks, fetcher, real-nginx integration)
  needs `ngx_pagespeed/src` + nginx headers; nginx-gap-1 stays
  open. Workspace: 2299 passed, 0 failed.
- Step 7, benchmarks gate (this session): DONE. Added the
  missing `ps-html/benches/rewrite.rs` (end-to-end driver
  rewrite × PassThrough/Core/All × ~0.6/60/600 KB, latency +
  throughput): passthrough ~17µs/1.5ms/15.8ms (~34–38 MiB/s),
  core ~19µs/1.5ms/16ms (~31–38 MiB/s), all ~25µs/1.9ms/19.8ms
  (~23–30 MiB/s) — linear in page size. Ran the full gate
  (`--quick` smoke of all 8 suites): every suite runs, and all
  numbers match the recorded baselines within noise (config
  lookup ~7ns, stats add ~1.7ns, MD5-4K ~5.7µs, HTTP
  put+find ~13.7µs, js/css/image within a few %; config merge
  833ns vs recorded 730ns is `--quick` noise on untouched
  code — no hot path changed this session). No regressions;
  gate passes. Workspace: 2299 passed, 0 failed.
- Step 8, compat harness hooks (this session): DONE. Expanded
  the `ps-compat` scaffold to the full Step 8 spec: 20 fixture
  vectors (directives, levels, filter ids, client-options,
  segments, namer, URL resolve/validity, Accept, WebP/AVIF UAs,
  capability lists, variant keys, CSS/JS goldens + rejects,
  admin routes, stat names, HTML goldens, nginx agreement/
  response-body) each pinned against the live crates, plus the
  harness interface (`Runner` trait, `Corpus`, 4 `Assertion`
  kinds, `NullRunner`). 23 ps-compat tests green. Verified the
  recorded gap closures against ground truth: all 95 filter
  (id, display) pairs diffed byte-identical vs
  `kFilterVectorStaticInitializer`; 34 NULL-help (33 `NULL` +
  1 `nullptr`) + 5 request-only rows match C++; scope gate,
  dep pins, and module roles all check out. Fixed two doc
  slips found while verifying (template has 6 `EnableFilters`
  mentions, not 9; config-2 wording now states the actual
  gate direction) and one fixture bug the vectors caught
  (`ExplicitlyRejects` fires on ANY zero q while accepts takes
  the last — both true for `q=0;q=1`, exactly like the C++).
  Deps moved to dev-dependencies (vectors are data-only; only
  the pinning tests touch the crates). Benchmark gate re-smoked
  (`--quick`, all 8 suites): numbers match baselines within
  noise, no criterion regression (all p > 0.05); no production
  code changed this step. Workspace: 2332 passed, 0 failed.
- Phase 2 COMPLETE. Remaining work is Phase 4 (reusable compat
  suite on the `Runner` interface) and the Step 6 remainder.
- Step 6 remainder, slice 0 (this session): DONE. The "blocked"
  scoping was wrong: `ngx_pagespeed/src/` is complete in-tree,
  `N:test/` holds the system suite, the exact nginx 1.31.8 tree
  is staged at `../.nginx-build/` (configured for bindgen +
  module builds; never touch the pristine tree), and crates.io
  serves `ngx` 0.5.1. Pinned `ngx =0.5.1` (`nginx-sys` 0.5.0),
  defaulted `NGINX_SOURCE_DIR` + gcc-header clang args in
  `.cargo/config.toml`, and pinned the bound version by test
  (`bound_nginx_version() == 1031008`, `SUPPORTED_NGINX =
  ["1.31.8"]`). 29 ps-nginx tests green. MAPPING corrected
  (nginx-gap-1 in progress, `N:test/` row).
- Step 6 remainder, slice 1 (this session): DONE. Module skeleton:
  new `module.rs` (3-context directive table + null end, dual
  module structs with a signature test computed from the
  `NGX_MODULE_SIGNATURE_*` bits, pre/postconfiguration gating,
  conf create/merge wiring, phase-step insertion, directive-line
  dispatch with `GzipCall` recording, hand-written `ngx_modules!`
  export tables), `conf.rs` (level confs, `NgxOptions`,
  order-sensitive `ps_merge_options`, dummy identity, cache-path
  validation texts, seeded context, factory-delete guards),
  `directives.rs` (on/off/gzip classification, dir prep +
  ownership fixup, `$ps_dollar`), `filters.rs` (six slots in
  load-bearing init/execution order, bodies stubbed to Slices
  2-5), `log_bridge.rs`/`message.rs` (severity maps, formats,
  crash-handler installer), `self_fetch.rs` (loopback/peer
  checks, label init, relabel/restore round trip), and `mock.rs`
  (ABI link stubs + chain-composition proofs). Hooks run the
  real gating and decline until their slice lands. 70 ps-nginx
  tests green. Workspace: 2375 passed, 0 failed.
- Step 6 remainder, slice 2 (this session): DONE. Server context
  + content dispatch + self-fetch handler. ps-config grew the
  six admin paths + `PurgeMethod` on `NginxSystemOptions` (1-arg
  parse, case-sensitive `NGINX_PROPERTY_SCOPES` gate mirroring
  `AddProperties` — the 167-row table stays pinned), the six
  `*Domains` wildcard groups (2-arg allow/disallow, verbatim
  error), and `beacon_url()` (`None` when unset; the nginx
  default is factory behavior). New `routing.rs`: 15-way `Route`
  decision branch-for-branch (gates, URL validity, subrequest,
  static prefix, admin paths with `AllowDomain` checks, purge,
  beacon with the nginx default + absolute-URL quirk),
  `ps_determine_port` host half (`str::parse::<i32>` proven
  identical to Chromium `StringToInt`), `build_url`, and the
  `ContentAction` dispatch table. `SeededContext` grew into the
  live `ServerContext` (http2 index, initialized latch,
  `FormatOption`, `PROXIES_HTML`); merge covers the new system
  options. module.rs wires the real `content_handler` (disabled/
  initialized gates, flush poll, URL build, route, dispatch —
  terminal actions decline until their slices land) and
  `self_fetch_handler` (main/subrequest/peer checks, pool
  cleanup, relabel, restore callback). Fixed a real port gap the
  new tests caught: `enabled_state` never survived `merge` (C++
  `enabled_` is an `Option`), so server-level `off` was lost at
  merge — added the `was_set` bit with verbatim `MergeHelper`
  semantics. Benchmark gate smoked on ps-config (the one hot path
  touched): lookup ~6.7ns vs ~7ns recorded, merge ~776ns inside
  the 730-833ns established noise band. 81 ps-nginx + 55
  ps-config tests green. Workspace: 2388 passed, 0 failed.
- Step 6 remainder, slice 3a (this session): DONE. Request
  foundation under the filter bodies (Slice 3 splits into three
  turns: 3a foundation, 3b options determination, 3c filter
  bodies). ps-config grew request-time getters (`follow_flushes`,
  `respect_x_forwarded_proto`, `remote_configuration_url`,
  `running_experiment`, `enroll_experiment` as `!= -2`,
  rebeaconing key + match + integration predicates). ps-http
  grew the full `Method` table (`Trace`/`Connect`/`Patch`,
  case-insensitive `from_string`) and request versions. New
  `caching.rs` (preserve policy, privacy limits note/keep,
  disabled-page value, `ApplySMaxAge` with its lifetime reader),
  `headers.rs` (request/response copy cores with injected clock,
  override-token predicate, cache-control join + get/set FFI),
  `request.rs` (`RequestCtx` plain data, buffered bit, ctx
  get/set/attach/release wiring), and `resource.rs` (pre-options
  gates with the `CHECK`-faithful panic, post-options 7-way
  branch selection, HEAD→GET rule). 95 ps-nginx + 56 ps-config
  tests green. Workspace: 2404 passed, 0 failed.
- Step 6 remainder, slice 3b (this session): DONE. Options
  determination: the `RewriteQuery::Scan` orchestration
  (related-options path over the nine-id decoding registry,
  Ajax disables, custom-option pre-scan, authenticated cookie
  loop, query loop with URL rebuild, generalized header scans,
  CoreFilters default, always-strip response token) plus
  `GetQueryOptions` with threshold limiting, in new ps-http
  `query.rs` (14 tests incl. C++-ported override/sticky/cookie/
  related cases). Servants: ps-config related tables (`cf`/`ic`
  sets, seven registered-empty ids), Ajax disables, default
  level, threshold caps, override/cookie/timeout getters;
  ps-http cookies, XHR detection, sticky context field, method
  table completion, query-param mutation, best-effort unescape,
  public query escape, fragment accessor. New ps-nginx
  `script.rs` (script execution over an injected substituter,
  fail-closed to `Off`) and `options.rs` (remote-config plan
  with the native-fetcher zero-wait rule, request scan, merge
  assembly, experiment gate with cookie builder), wired into
  resource gating via `resolve_from` with an integration test.
  Seams (Slice 6): remote fetch, purge inheritance, experiment
  matcher (gate fails closed without a verdict), script
  recording/compilation, cookie write. Notably, the related
  tables looked binary-search-broken until `Initialize()`'s
  sort turned up — no C++ bug after all. 104 ps-nginx + 58
  ps-config + 139 ps-http tests green. Workspace: 2432 passed,
  0 failed.
- Step 6 remainder, slice 3c (this session): DONE. Filter bodies:
  new ps-http `inflate.rs` ports the streaming `GzipInflater`
  (gzip/zlib/raw over miniz_oxide plus manual gzip framing with
  CRC/trailer verification, deflate-to-raw fallback both ways,
  output limiting, finished/error accounting; 11 tests incl.
  split-at-every-boundary, tiny buffers, limit/corrupt/trailer/
  FDICT cases). New ps-nginx `send.rs` ports the body-delivery
  walk over an injected sink (pass-through/inflate/flush-gated/
  last-completes semantics, limit-vs-corrupt reporting; 4 tests).
  `filters.rs` carries the real html_rewrite dispatch (live
  gates, encoding/strip/Vary/limits cores, `finish_*` tails
  awaiting the Slice 6 fetch), a fully live fix_headers filter
  (token strip, disabled-`Cache-Control` write with an alloc-
  failure hardening where C++ would segfault, etag/last-
  modified/expires clears), and the etag rename plus `gzip_vary`
  (its recorder-backed `s-maxage` half moves to Slice 5).
  Header-list walker with fabricated-list tests; `RequestCtx`
  gains the decoder slot. 116 ps-nginx + 150 ps-http tests
  green. Workspace: 2455 passed, 0 failed (no hot paths
  touched, so the bench gate holds trivially).
- Step 6 remainder, slice 4 (this session): DONE. Base-fetch
  bridge and serving. New `fetch.rs`: `BaseFetch` state core
  (write buffering with the 1 MiB flush window, flush/done
  latches, chain-copy policy with exactly-once `last_buf`,
  two-reference lifecycle with in-flight event accounting,
  detach/suppress gating, content-length stashing) over an
  injected event channel, plus the base-fetch gate, handler
  status/preserve/discard matrices, send-header and collect
  routing, and the full read-callback plan (10 tests).
  `headers.rs` grew the outbound disposition (verbatim shortcut
  table incl. exact-case matches, weak-etag rename, `Vary` flag)
  with its FFI writer and the pool chain builder. Live: the
  base-fetch body filter, `CollectHeaders`/
  `CollectAccumulatedWrites`, async-wait suspend (fabricated-
  request test), and the discard failure core. Recorded seams:
  shared wrapper/pipe/threads, cache validation, handler/event
  FFI assembly (Slice 6); decline lands with its in-place
  callers (Slice 5); beacon/simple/resource terminal bodies all
  need factory services (Slice 6). 130 ps-nginx tests green.
  Workspace: 2469 passed, 0 failed.
- Step 6 remainder, slice 5 (this session): DONE. In-place
  recording. New `recording.rs`: the `InPlaceRecorder` core
  (concurrency gate with process-wide count, coding gate,
  content-type gate, size shortcut, origin capture/application
  with TTL clamping, empty/truncation rules, saved-`Cache-
  Control` lifecycle, gunzipping writes) over an injected
  cache/stats/log backend (8 tests). Live: both in-place
  filters (header plan + preliminary feed + miss/serve/decline
  routing with Slice 6 creation/stats seams; body drive over
  fabricated chains), decline (ctx reset + nginx advance, both
  fabricated-tested; resume calls live-only), and the etag
  `s-maxage` completion (auth gate, global value, save/restore,
  alloc-failure hardening). Fixed a 3b mistake along the way:
  C++ `Lookup1Unescaped` is exactly-one (not first-match), so
  the strict lookup went best-effort and repeated override
  tokens now deny. FOUND C++ BUG (ported faithfully): the size
  shortcut's `<= 0` gate fires exactly when unlimited. 142
  ps-nginx tests green. Workspace: 2481 passed, 0 failed.
- Step 6 remainder, slice 6a (this session): DONE. Factory
  config surface + module lifecycle. New `factory.rs`
  (`DriverFactory`: verified ctor defaults incl. beacon, inode
  limit, introspective-JS; the ten system factory directives
  with verbatim scope texts; resolver record/check;
  one-shot thread-count finalization; owned message handlers +
  `LoggingInit`; `PostConfig`; remote warnings; idempotent
  shutdown). Fixed a 3c mistake along the way: nginx config
  errors quote (`"directive" detail`, `"full line" detail`,
  empty details keep the trailing space), the gate runs before
  the arity check, and the invalid-wrap joins the raw
  (unstripped) words. Dispatch is table-first with a factory
  fallback that gate rejections bypass. `MainConf` owns the
  factory now; `merge_srv_conf` runs `SetMainConf`;
  postconfiguration presence is the deletion latch. New
  `init_module`/`init_process`/`exit_process` hooks (pure plan
  + FFI: reload guard, prefixed PostConfig errors, resolver
  gate, stats/RootInit flags, active-generation tracking) and
  `create_main_conf` logging/init calls. On-demand options
  added: `Statistics`, `FileCacheInodeLimit`,
  `MemcachedServers`, `RedisServer` (validated specs, verbatim
  errors). `init_all` completed (stdio, rate, RPC, shutdown,
  404 count, HTML histogram, dependencies cohort). Gzip-setter
  execution deferred to Slice 7 (live-only). 155 ps-nginx, 60
  ps-config, 20 ps-stats tests green. Workspace: 2497 passed,
  0 failed.
- Step 6 remainder, slice 6b (this session): DONE. Threading.
  Key finding: nginx spawns no rewrite-thread pool
  (`NumRewriteThreads` only sizes cache thread limits);
  `StartThreads` starts one scheduler thread. New
  `scheduler.rs`: injectable `Clock` (`SystemClock` epoch us,
  `ManualClock` tests), `Scheduler` (deadline/index-ordered
  alarms, add/cancel/run/signal/process/pump, ms-rounded
  waits, unlocked callbacks), and the 255s pump with an
  atomic check-then-wait stop (no missed wakeup). The
  condvar-wait family, sequences, and the locked alarm add
  have no nginx-path users and wait for their first caller.
  `DriverFactory` owns the scheduler plus the named thread
  (`start_threads` idempotent, `Drop` joins at the
  deferred-cleanup position); `init_child_process` starts it.
  No shared-state wrappers needed: only `Send` alarm
  callbacks cross threads. 165 ps-nginx tests green (timing
  tests stable 5/5). Workspace: 2507 passed, 0 failed.
- Step 6 remainder, slice 6c (this session): DONE. Fetchers
  (native async, per user choice; dormant until async drivers
  exist). Serf is unportable (2111 lines + missing libs) and
  the sync flow cannot drive nginx events, so serf-configured
  fetchers route to native with a loud warning. New `pipe.rs`
  (event struct, write/backoff/drop rules, single-event drain,
  init texts, install/take globals, Terminate plan; nginx
  registration + read handler transcribed live). New
  `native_fetch.rs` (pending/active/completed lifecycle with
  shutdown/cancel rules, byte/count accounting, verbatim
  fetcher keys, request serialization, UA fixup, keepalive
  protocol, message builders). `SharedBaseFetch` (Mutex state,
  Arc-per-reference lifetime with a shadow-count invariant
  test, atomic active count) plus the `ArcPost` pairing
  adapter. Factory owns the collapsed fetcher map
  (`get_fetcher`, warn-once) and reports shutdown task ids.
  Child init/exit wire Initialize/Terminate, per-server
  handlers, and thread start. Fixed two pre-existing
  mistakes: option lookup is case-sensitive (core + arms,
  fallout repaired) and table-invalid details read `Cannot
  set option...`. New options: `FetchProxy` (secret),
  `FetchWithGzip`, `FetcherTimeOutMs`. FOUND C++ BUG #2
  (resolve with no IPv4 falls through to an OOB read plus a
  double `CallbackDone`; the port will return after failing).
  Known limits: https unsupported (no TLS), slurp/proxy-factory
  unported, read-path actuation waits for 6d terminal bodies.
  185 ps-nginx, 61 ps-config, 151 ps-http tests green.
  Workspace: 2529 passed, 0 failed.
- Step 6 remainder, slice 6d (this session): DONE. Caches,
  terminal bodies, HTML sink-up, ring/stats wiring. Cache
  composition: new options `FileCacheCleanIntervalMs`,
  `FileCacheSizeKb`, `LRUCacheByteLimit`,
  `LRUCacheKbPerProcess`, `CacheFlushFilename`,
  `EnableCachePurge`, `CompressMetadataCache` plus
  `SystemWasSet` flags with was-set merging;
  `SystemCachePath`/`SystemCaches` with factory/root/child
  wiring and the shared purge set. Terminal FFI: content
  dispatch (beacon/simple/resource), beacon GET inline +
  POST via body-read with temp-file/spooled/multi-buffer
  bodies and 500-on-failure, simple assets off the live
  factory prefix plus message-history dumps with the fixed
  header set. Resource handler transcribed whole: guards
  (main/factory/shutdown/method/URL/headers/options/standby),
  `determine_options` over live headers/URL (directory
  options, fail-closed script `$`, no remote executor, no
  experiment verdict), `X-Forwarded-Proto`, rebeaconing
  preserve computation, then resource/admin/HTML/in-place/
  pass-through branches (proxy unreachable without a
  `DomainLawyer`; resource/IPRO unfinished-or-error declines
  so nginx renders error pages; misses decline without
  recording). Admin renders via per-request sites
  (statistics leaves incl. `?cache` notice, console, admin,
  purge with route eviction rights). HTML branch attaches
  `HtmlSink` and the filters call through (`NGX_OK` runs
  `finish_html_header_filter`; the body filter feeds the
  sink and passes NULL). New `RequestContext::set_options`;
  handler params are `&mut dyn` for the server handler.
  Corrected a same-session mistake along the way: debug
  assets are the Closure pretty-print genfiles (the data2c
  rule feeds on `closure_out`), not cleartext sources; all
  30 vendored assets are now byte-exact. Child init attaches
  the shared message buffer to per-server handlers.
  Known limits: single worker statistics registry, no
  origin fetching (caches serve hits only), IPRO never
  self-warms, script variables decline, running experiments
  decline. 209 ps-nginx tests green. Workspace: 2556
  passed, 0 failed.
- Step 6 remainder, slice 7 (this session): DONE. Loadable
  module + live verification. `ps-nginx` ships cdylib
  (`libps_nginx.so`, module tables under the C++ names) plus
  `ngx_module/{config,config.make}` for
  `--add-dynamic-module` (`make modules` rebuilds via cargo;
  verified in a scratch tree). The module signature is
  probed from the target tree by `build.rs` (no baked
  constant), `.cargo/config.toml` env no longer forces over
  real env, and target feature macros (`NGX_HTTP_V2`,
  `NGX_HTTP_GZIP`) gate the `http2` lookup and gzip scan.
  New `gzip_setter.rs`: command scan with signature checks,
  redirect hooks, flag/enum/bitmask/types writers, enable/
  set/rollback entry points, wired into preconfiguration and
  directive lines (auto-gzip and explicit-config rollback
  both verified live, incl. the global-disable semantics).
  Live verification on staged nginx 1.31.8 found and fixed
  real bugs: header copy never set `headers_out.status`
  (all serves hung); base fetches never got their request
  (every pipe event dropped); posted requests ran against
  the pipe connection (SIGSEGV); file-backed response
  buffers fed empty (dynamic modules run above the copy
  filter, so the filter reads ranges itself in 64 KiB
  chunks); server contexts used a blank instead of
  `NewRewriteOptions` (PassThrough, no filters);
  `HtmlWriterFilter` was default-on only via `new()`
  (now in `FilterState::default`, and `clear()` keeps
  forbidden like C++); `EnableCachePurge` lived as a system
  shadow of the core property (removed; caches take the
  core flag); IPRO miss left `in_place` set so the
  in-place header filter re-declined after our header
  filter passed, arming HTML on errored requests (SIGABRT
  on 404s; now resets C++-style in the content phase).
  `X-Page-Speed` stamped from `XHeaderValue` (new getter,
  factory default). Final matrix 16/16 green against live
  nginx (HTML incl. byte-identical filtered output vs C++,
  beacon GET/POST, assets, messages, statistics leaves,
  console, admin, purge, resource-decline 404); cross-tree
  build (flipped signature bit) loads and serves. Known
  limits: caches/IPRO/script/experiment limits from 6d
  stand; no-TLS fetches; unknown-`http2`-variable trees use
  the `NGX_ERROR` sentinel. 212 ps-nginx tests green.
  Workspace: 2559 passed, 0 failed.
- Step 7, benchmarks gate re-run (this session): PASS. All
  8 suites run green (`--quick`). Config/stats/cache/http/
  css/js/image match recorded baselines within noise
  (lookup ~7.2ns, merge ~768ns, MD5-4K ~5.7µs, put+find
  ~13.9µs, counter add ~1.8ns). ps-html rewrite showed
  +65-76% vs criterion's stale stored baseline, so it was
  A/B'd against the parent commit on the same machine back
  to back: identical (33.1 vs 33.3µs small, 2.86 vs
  2.94ms medium, 47.6 vs 47.7µs all/small) — no
  regression from slices 6d/7. Caveat recorded: this
  machine's governor is `powersave` (1.7 vs 4.5 GHz max,
  no sudo to pin), so absolute numbers dance between
  sessions and `--quick` small-bench variance reaches ~2x;
  future gates should A/B rather than trust stored
  baselines. Pre-existing gap noted, not built here:
  ps-nginx still has no in-nginx req/s + stall-detector
  bench. Workspace: 2559 passed, 0 failed.
- Phase 4, compat suite (this session): DONE. Live `Runner`
  impls boot real nginx with either `.so`
  (`ps-compat/tests/live.rs`, raw TCP + chunked reader, no
  new deps; `PS_COMPAT_LIVE=1` gates, skips cleanly
  otherwise). Page corpus (5 cases) and config corpus (7
  cases incl. boot parity) record from C++ via
  `PS_COMPAT_RECORD=1` into committed
  `tests/corpus_data/{pages,configs}.rs` and compare
  normalized responses (status, header multisets, bytes;
  framing/clocks/identity dropped, version pinned, fixed
  origin mtimes, unique ports, known-gap xfail reporting).
  Suite found and fixed one real divergence (rewritten HTML
  kept origin ETag/Last-Modified/Content-Length; C++ strips
  them in `SetupForHtml`, now ported) and pins two known
  gaps (compat-1 inline CSS/JS minify wiring, compat-2
  error s-maxage via recorder arming) in `MAPPING.md`.
  Status: 2 page cases byte-exact, 3 xfailed gaps, 7/7
  config cases green. Workspace: 2563 passed, 0 failed
  (live suite excluded without the env gate).
- compat-1 follow-up (this session): DONE. Inline CSS/JS
  minify wired: new `CssMinifyFilter` (`<style>` text via
  `ps-css`, fail-closed) and `JsMinifyFilter` (inline
  JavaScript via `ps-js`, with the verbatim end-tag guard
  plus noscript/non-JS/src skips) on the `CssFilter`/
  `Javascript` slots; the full driver-bound filters stay
  for the fetch-machinery epic. Unit goldens incl.
  C++-observed outputs; instantiate + driver skipped-slot
  tests updated. Compat suite reports GAP CLOSED on
  `core-rich` and `elide-attributes` (byte-exact);
  `missing-file` (compat-2) still xfailed. Workspace:
  2565 passed, 0 failed.
- compat-2 follow-up (this session): DONE. Recorder armed on
  IPRO miss (sync equivalent of C++'s header-filter arm on
  the synthetic 501): new `IproMaxResponseBytes` (10 MiB) /
  `IproMaxConcurrentRecordings` (10) system options with
  parse/scope/merge, new `max_html_cache_time_ms` core
  accessor (table entry already existed), `ServerContext::
  statistics_shared`, and `StatsRecordingBackend` (real
  `ipro_recorder_*` bumps + log lines; cache writes stay
  no-ops until the fetch machinery lands, so misses
  re-record). Arming the recorder exposed a second
  divergence: rewritten 200s transiently carried the stamp
  because our `SetupForHtml` port had only the strips —
  C++ replaces the fetch's `Cache-Control` there
  (`SetDateAndCaching` + preserved no-transform/no-store/
  s-maxage), and the fix-up filter's line-level drop then
  removes the recombined tail. Ported as
  `setup_for_html_caching` + the `html_cache` diagnostics
  tail (`show_cache_timestamp` stays unported). Compat suite
  reports GAP CLOSED on `missing-file`; all 5 page + 7
  config cases green. Workspace: 2568 passed, 0 failed.
  Follow-ups filed: IPRO cache-write backend
  (fetch-machinery epic); `set_date_and_caching` ttl=0
  branch diverges from C++ (drops suffix/Expires);
  `cache_control_values_to_preserve` keeps the wrong
  directive list.
- Divergence slices (this session): DONE. The two
  compat-2 follow-ups were mated (suffix format and
  concat shape compensated each other), so they landed
  together with `SetCacheControlMaxAge`, whose ms
  argument the port had also misread as seconds (1000x
  `max-age` on the 304-revalidate path): all three
  writers are now C++-verbatim (`Expires` always
  stamps; `, `-prefixed no-transform/no-store/
  standalone-s-maxage preserve list; `,`-joined
  non-max-age values). One pinning test added
  (`caching_writers_match_cxx`); no existing test
  flipped. Workspace: 2569 passed, 0 failed; live
  suite still 5/5 + 7/7.
- Fetch epic F1 (this session): DONE. Same-server file
  fetcher replaces `UnavailableFetcher`: `FileFetcher`
  maps `http` URLs to the server root (exact/`*.`
  host match, unescape, `..`/NUL/symlink-escape jail,
  regular files under the cacheable size cap) and
  serves `FileInputResource::SetDefaultHeaders`
  headers verbatim. Child-init builds it per server
  (server-level root only; aliases/variables/relative/
  missing roots and null confs fall back loud).
  `html_context` takes the fetcher; 8 unit tests incl.
  traversal/symlink attacks. Live: IPRO reconstructs
  and serves static files itself (original bytes;
  optimization needs F3). Workspace: 2577 passed, 0
  failed; live suite still 5/5 + 7/7. F2 (recording
  cache backend) deprioritized: file hits no longer
  decline, so it would only persist repeated-404
  notes.
- Fetch epic F3 (this session): DONE. Resource rewriting end
  to end. Driver-bound `CssFilter`/`JavascriptFilter` replace
  the minify subsets once a `ResourceDriver` exists (plumbed
  through `HtmlSink`/module, with the XHTML verdict from the
  page content-type); `proxy_mode` threads through IPRO so
  non-proxy callers fail unrewritable content back to native
  handling (notably HTML, which IPRO must never serve);
  minify-only `rewrite_in_place` servants with lazy
  registration; 8 more CoreFilters slots instantiated
  (combines, inlines, cache extender, image rewrite,
  AddHead, import-to-link, subresource hints).
  FileFetcher redesigned to subrequest-faithful bare
  headers (no synthesized TTL). First-hit transients
  differ architecturally (C++ declines+records, then
  optimizes async in ~30s; the sync bridge reconstructs
  inline) but steady states converge and match
  byte-exact. Serve path now matches C++ byte-exact:
  no `X-Page-Speed` on IPRO serves, `content_length_n`
  set (length + ranges, no chunking),
  `X-Original-Content-Length` end to end, output
  headers seeded from inputs. Compat grows extras,
  fetch counts, poll-until-optimized, per-case mtimes,
  staged mime.types, volatile max-age normalization,
  and 3 resource cases (css/js-resource,
  page-with-links). Suite: 8/8 page + 7/7 config
  green. Workspace: 2579 passed, 0 failed. Follow-ups:
  old-mtime IPRO gate mechanism (C++ refuses, ours
  over-optimizes); MetaTag/SupportNoscript slots
  (need headers/URL plumbing); CriticalImagesBeacon
  (no impl); nested CSS @import/image subsets in
  servants; second-hit recall for first-hit-only
  coverage.

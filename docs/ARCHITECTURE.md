# Architecture (Phase 2)

Rust port of PageSpeed (PSOL + ngx_pagespeed) as a Cargo workspace at
`pagespeed-rust/`. Companion docs: [AUDIT.md](AUDIT.md) (what the C++
does), [MAPPING.md](MAPPING.md) (which C++ owner feeds which crate),
[PLAN.md](PLAN.md) (build order).

Scaffold status (Phase 0, verified): workspace `Cargo.toml`
(`resolver="2"`, 10 members, `version 0.1.0`/`edition 2021`/`Apache-2.0`,
release `opt-level=3`/`lto=true`/`codegen-units=1`), `.gitignore`,
`README.md`, 10 crates × (`Cargo.toml` + `src/lib.rs`);
`#![forbid(unsafe_code)]` in all libs except `ps-nginx`; `cargo test
--workspace` 10/10 pass; zero commits (scaffold only). Toolchain: rustc
1.100.0 / cargo 1.100.0 / rustup 1.29.3 (stable-x86_64-unknown-linux-gnu).
Baseline: nginx 1.31.8-2b436b0 with dynamic ngx_pagespeed +
ModSecurity-nginx modules.

## 1. Workspace layout

```text
pagespeed-rust/
  Cargo.toml            # workspace, resolver = "2", 10 members
  docs/                 # AUDIT.md ARCHITECTURE.md MAPPING.md PLAN.md
  crates/
    ps-html/            # streaming HTML engine + filter chain
    ps-css/             # CSS parse / rewrite / combine / inline
    ps-js/              # JS minify / combine / inline / source maps
    ps-image/           # image decode / optimize / convert / resize
    ps-cache/           # resource naming, hashing, backends, TTL, purge
    ps-http/            # fetchers, HTTP cache, headers, downstream purge
    ps-stats/           # thread-safe stats + persistence
    ps-config/          # options, scopes, directive parsing
    ps-admin/           # admin pages, console JSON, purge endpoint
    ps-nginx/           # nginx module: hooks, filters, event bridge (FFI)
```

Dependency direction (acyclic): `ps-nginx` → `ps-admin` → {`ps-html`,
`ps-http`, `ps-stats`, `ps-config`} → `ps-cache`; `ps-html` → {`ps-css`,
`ps-js`, `ps-image`, `ps-cache`, `ps-http`, `ps-stats`, `ps-config`};
`ps-css`/`ps-js`/`ps-image` → {`ps-cache`, `ps-http`, `ps-stats`,
`ps-config`}; `ps-http` → {`ps-cache`, `ps-stats`, `ps-config`}; all →
`ps-stats` + `ps-config` (leaves, no intra-workspace deps except
`ps-stats` standalone and `ps-config` standalone).

Note (Step 0 decision): `ps-cache` → `ps-config` for the `Wildcard`
matcher (URL invalidation lists) and nothing else; `ps-config` and
`ps-stats` stay dependency-free leaves. Backend statistics live as
owned counters on the backends (mirrored into `ps-stats` by integrators
later) to avoid a `ps-cache` → `ps-stats` edge. `IsUrlCacheValid` is a
`ps-cache` free function over `PurgeSet` + invalidation lists for the
same acyclicity reason (it cannot live on `RewriteOptions` without a
dependency cycle).

## 2. Crate designs

### 2.1 ps-html — streaming HTML engine

Ports `net/instaweb/rewriter/` HTML path (`rewrite_driver.cc`,
`rewrite_context.cc`, `server_context.cc`, all `*_filter.{h,cc}`) and the
kernel HTML filters (CollapseWhitespace/ElideAttributes/RemoveComments/
RemoveQuotes/AmpDocument).

- Streaming engine: incremental tokenizer/parser over `&[u8]` chunks with
  explicit `Write`/`Flush`/`Done` (mirrors `RewriteDriver` + PSOL
  `ProxyFetch` feed from `ps_html_rewrite_body_filter`).
- Reused buffers: a per-rewrite bump/scratch arena plus a small set of
  reusable byte buffers (`BytesMut`-style) passed down the filter chain;
  filters append spans into the output buffer instead of allocating per
  node. No per-token `String` allocation on the hot path.
- Incremental processing: filters observe start-tag/end-tag/text events and
  may buffer only their declared window (e.g. head movers buffer until
  `</head>`); flush points (`FlushHtmlFilter`) stream completed prefixes
  downstream without waiting for `Done`.
- Chain builder reproduces §2.4 order exactly: event listeners →
  early pre-render → pre-render list (with `resource_filter_map_` ids
  `ce/cf/cc/if/ci/gf/is/ic/jc/jm/sm/ji/ls/pr/aj`) → post-render list →
  Debug last. Order table is a single `const` array with a debug assertion
  mirroring `CheckFilterSetOrdering`, plus a comment pointing at
  `install/common/pagespeed.conf.template` (nginx-gap-2) so order edits
  stay co-updated.
- Conditional/ordering deps from AUDIT §2.5 become chain-builder rules
  (e.g. `InlineImportToLink` forced by Prioritize/ComputeCriticalCss
  unless forbidden; `MoveCssToHead` before `CombineCss`; ResponsiveImages
  requires ResizeImages, two-phase First+Second).
- `Enabled()` precedence is implemented once in `ps-config`
  (forbidden/disabled > explicit > preserve-urls > RewriteLevel) and
  consumed here; `RequiresAddHead` gates the `AddHead` filter.
- Unverified wirings (filters-1..4: `cs/cu` style-attribute filters, 12
  enum-only filters, `io` IPRO path, `hw` writer) are implemented behind
  the same chain-builder API but marked `#[cfg(test)]`-covered TODO with
  conformance tests deferred to the compat harness (Phase 4).

### 2.2 ps-css — CSS pipeline

Ports `css_filter.cc`, `css_combine`/`css_inline`/`css_outline`/
`css_move_to_head`/`css_inline_import_to_link`/`google_font_css_inline`
filters, `CssFilter` resource pipeline (`cf`), and the in-tree gperf CSS
parser (`third_party/css_parser`).

- Parser: hand-written lossless CSS tokenizer (spans into the input) so
  rewritten output preserves unknown constructs byte-for-byte; no full AST
  allocation for pass-through rules.
- Rewrites: URL absolutization/rewrite inside `url(...)` and `@import`
  (with `FlattenCssImports` semantics and `CssFlattenMaxBytes` cap),
  minification (comment/whitespace elision mirroring RemoveComments for
  CSS), combine (concatenate with import hoisting, `PermitIdsForCssCombining`
  gate), inline/outline keyed off `CssInlineMaxBytes`.
- Emits `css_filter_blocks_rewritten` / `css_filter_total_bytes_saved`
  (+ parse/fallback/dropped variants) into `ps-stats` with identical names.
- Fallback: any parse failure serves the input unchanged and bumps the
  fallback counter (mirrors C++ fallback behavior).

### 2.3 ps-js — JavaScript pipeline

Ports `javascript_filter`/`js_combine`/`js_inline`/`js_outline`/
`javascript_source_map`/`google_analytics`/`insert_ga` filters, the `jm`
resource pipeline, and the Closure Compiler role (ADVANCED for opt builds,
SIMPLE for dbg).

- Minifier: pure-Rust JS minifier (no JVM at runtime; Closure parity is a
  compat-harness concern, not a build dependency). Must preserve
  `AvoidRenamingIntrospectiveJavascript` semantics for introspective code.
- Combine/inline/outline keyed off `JsInlineMaxBytes`; source-map filter
  (`sm`) chains minify→map; analytics filters run before JS rewrite per
  ordering deps.
- Emits `javascript_blocks_minified` / `javascript_total_bytes_saved` (+
  failures/uses variants) into `ps-stats` with identical names.
- Fallback mirrors ps-css: minify failure serves input unchanged + counter.

### 2.4 ps-image — image pipeline

Ports `pagespeed/kernel/image/*` codecs and `net/instaweb/rewriter/image*`
pipeline (see AUDIT §8).

- Formats: JPEG, PNG, GIF, WebP in-process; AVIF via external `avifenc`
  subprocess exactly like `ConvertToAvifExternal` (`posix_spawnp`,
  mkstemps temp files, watchdog deadline thread with kill,
  `AcquireAvifEncoderSlot` admission control, stats) — no in-process AVIF
  codec.
- Decode: scanline-reader trait (`ScanlineReaderInterface` equivalent) with
  per-format impls; sniffing via magic bytes (`ComputeImageType`
  equivalent: FFD8/PNG/GIF8/ftypavif+ftypavis/WebP features).
- Optimize: `JpegCompressionOptions` (progressive, retain profile/exif,
  lossy quality default 85, scans, subsampling), PNG params
  (filter/strategy/OptiPNG-equivalent best-compression/progressive), WebP
  config (lossless, quality default 75, method, target size, alpha,
  animated kmin/kmax); converter (png→jpeg, png→webp, opaque detection),
  shrink-only resizer with aspect preservation, alpha-strip pass,
  Sobel/histogram photo-vs-graphics analysis, `ImageOptimizer::Optimize`
  orchestrator with the `ComputeDesiredFormat` matrix (animated→WebP-
  animated; transparent→WebP-lossless/PNG; opaque photo→WebP-lossy/JPEG).
- Negotiation: Accept-header parser with q-values (q=0 = explicit reject),
  UA fallbacks (WebP: Firefox>=65, Chrome/Chromium>=32, OPR>=19, iOS/macOS
  Safari>=14; AVIF: Firefox>=93, OPR>=71, Edg/EdgA>=121, Samsung>=14,
  UC>=15.5, Baidu>=13.52, Safari>=16.4/iOS16, Chromium threshold;
  `IsKnownAvifUnsupported`: IE, legacy Edge, Opera Mini, KaiOS),
  downstream `PS-CapabilityList` AND-gate for WebP (AVIF passes through),
  `SupportsWebpInPlace` (Accept only) vs `SupportsWebpRewrittenUrls`
  (Accept OR UA unless rejected) distinction.
- Variants: `ResourceContext` cache-variant keys (dims, mobile UA, libwebp
  level, small-screen/save-data quality, supports_avif) with URL suffixes
  `./w/v/a` + `.avif` + `m` + `d/ss`, WxH dimension prefix, legacy w/v/m
  decode; `Vary: User-Agent` / `Vary: Accept` / `Vary: Save-Data` rules
  from `AddVaryIfRequired` incl. IE private override.
- Qualities: 3-tier selection (Save-Data > small-screen > desktop) with
  audited defaults (recompress 85; jpeg -1/70/50; webp 80/70/animated 70/
  save-data 50; avif timeout 30000ms, max concurrent 2; limits 32MB/100%/
  100%/95%; MaxRewritesAtOnce 8). `WebpTimeoutMs=-1` semantics and limit
  enforcement sites are open gaps (images-gap-1/2) — implement the
  documented defaults first, trace sites during porting.
- Emits `image_rewrites` / `image_rewrite_total_bytes_saved` (+
  dropped/no-rewrite/ongoing/latency variants) with identical names.

### 2.5 ps-cache — naming, hashing, backends, TTL, purge

Ports `resource_namer.cc`, `output_resource.cc`, `rewrite_context.cc`
(partition keys), `pagespeed/kernel/{base/cache/sharedmem}`,
`pagespeed/system/{apr_mem_cache,redis_cache,system_caches,
system_cache_path}` (see AUDIT §5).

- URL naming: `ORIGINAL_NAME.pagespeed[.EXPT|.options].ID.HASH[.SIG].EXT`
  with `Encode`/`Decode`/`EncodeIdName`/`EventualSize` parity, including
  slash-escaped options and the `ce` cache-extend example shape.
- Hashing: `Hasher::Hash` = web64(RawHash) truncated to size (MD5 default
  10 chars, parameterized); `HashToUint64` from first 64 bits; kHashBytes=20,
  kCachePurgeBytes=25000.
- Keys: `name_key` = resolved_base + id.name; metadata partition key
  `rname/<id>_<sig>/<url_key>@<suffix>` with `HashSplit` for multi-URL/
  data-URL keys; `CacheKeySuffix`/`UserAgentCacheKey` as overridable
  traits defaulting to empty; HTTP-cache composite key
  version/fragment/key with `v5/` prefix; `Resource::cache_key` defaults
  to URL, overridable.
- Backends behind one `Cache` trait (`Get`/`Put`/`Delete`/`MultiGet` +
  key-in-value support for size-limited backends): FileCache (filename
  encoder with `,` escapes, 128-char segments, hash fallback, async
  Clean by size/inode), LRU (per-process, explicitly `!Sync`-shared —
  wrap or shard for threads), SharedMem metadata cache (root/child
  Initialize/Attach model), memcached (hashed key + key-in-value, 1MB
  overflow to file fallback), Redis (SET/SETEX, default no TTL).
  Adapters: key-prepender (namespacing), fallback (size-split),
  compressed (gzip, 16MB inflation cap).
- TTL/validation: audited default set (implicit/load-from-file/input-
  errors 5min; generated 1yr + `W/0`; mismatch 5min; failure 300s,
  dropped-fetch 10s); `ComputeCaching`/`cache_ttl_ms`/
  `CacheExpirationTimeMs` API; `IsUrlCacheValid` = purge-set + legacy
  wildcards; input validation (`IsInputValid`) with expiry + staleness;
  cache-extender threshold (1 month) + long public output caching;
  `ApplyInputCacheControl` clamping.
- Purge: `PurgeSet` bounded LRU + `PurgeContext` file-backed
  (`cache.purge` vs legacy `cache.flush`, 5s poll), admin `cache?purge=
  <url|*>` (`*` = whole cache), downstream PURGE fetches.
- HTML-cache keys/TTL/backend and default backend-chain selection order
  are open gaps (caching-gap-1/2) — resolved during porting from
  `proxy_fetch.cc` + `SystemCaches` wiring.

### 2.6 ps-http — fetching and HTTP cache

Ports `net/instaweb/http/` (`http_cache.{h,cc}`, `http_cache_failure.cc`,
async fetch/fetcher APIs, `mock_url_fetcher.h`, `request_context.h`),
`ResponseHeadersParser`, serf fetcher role, and `DownstreamCachePurger`.

- `AsyncFetcher` trait (`Fetch(url, callback)`), `AsyncFetch` handle
  (`Write`/`HeadersComplete`/`Flush`/`Done`), `HTTPCache` with composite
  keys, `RememberFailure` + per-status TTLs, `OverrideCacheTtlMs` hook,
  `OptionsAwareHTTPCacheCallback` validity.
- Response state machine: status line → headers (chunk parser) → body
  (content-length bounded, overflow fails, no body for HEAD/204/304).
- Headers type with shortcut fields (content-length/etag/location/…),
  `SetDateAndCaching`, `ApplySMaxAge`, `@psol-etag` internal convention.
- Native async fetcher (event-loop driven, resolver + timers + keepalive
  pool with 60s idle) lives in `ps-nginx`; `ps-http` holds the portable
  core (state machine, cache, headers, mock fetcher for tests).
- Emits the `HTTPCache::InitStats` family (`cache_time_us`, `cache_hits`,
  …) with identical names.

### 2.7 ps-stats — thread-safe statistics with persistence

Ports `pagespeed/kernel/{base/sharedmem}/statistics*`,
`pagespeed/kernel/util/statistics_logger.cc`,
`pagespeed/kernel/cache/cache_stats.cc`, `rewrite_stats.cc`, per-filter
`InitStats` families (see AUDIT §7.2).

- Thread-safe: all variables `Send + Sync` (atomics for counters,
  sharded/RwLock histograms); safe to bump from rewrite threads and read
  from the event loop without the C++ shared-mem segment dance inside one
  process. Cross-process sharing (multi-worker) uses a shared-mem segment
  `<filename_prefix>statistics` with root-Create/child-Attach semantics
  and per-restart reset, matching C++ lifecycle.
- Variables: counters, timed variables, histograms, with the exact audited
  names (`cache_*`, `rewrite_*`, `html_cache_*`, `image_*`, `css_filter_*`,
  `javascript_*`, per-backend `<prefix>_{hits,misses,inserts,deletes}` +
  latency/size histograms). The 27-filter `InitStats` fan-out
  (admin-gap-1) is enumerated during porting — every name preserved.
- Persistence: append-only `<log_dir>/stats_log_<global|local>` sampled on
  interval (timestamp + statistics_instance id; whole-file trim over max
  size — preserved semantics, documented wart); `PreviousDashboardCounters`
  pre-restart deltas for the dashboard set (bounded 16MiB scan, per-
  instance reset detection); dashboard counters = saved-samples-plus-
  current, not a ledger.
- Console JSON: `DumpJSON` with graph-query bounds (1s..1day, max 64 vars)
  and the 429-busy expensive-leaf gate, served by `ps-admin`.

### 2.8 ps-config — options, scopes, parsing

Ports `net/instaweb/rewriter/rewrite_options.{h,cc}`,
`rewrite_filter_names.gperf`, `rewrite_query.cc`, `system_rewrite_options.cc`,
`system_rewrite_driver_factory.cc` option parsing,
`ngx_rewrite_options.cc`, `rewrite_options_manager` (request-time).

- Types: `Filter` enum (95 + EndOfFilters), `FilterSet` bitset in enum
  order (debug order assertion), `RewriteLevel` (PassThrough/Core/
  TestingCore/All/OptimizeForBandwidth), scope enum
  Query < Directory < Server < LegacyProcess < ProcessStrict.
- Tables: 93-name gperf-equivalent map (`phf` or match) with the audited
  aliases; per-filter URL ids; Core/Bandwidth/Test/Dangerous sets;
  dependency sets (`kRequiresScriptExecution`, `kAddHeadFilters`,
  `kMobilize`, preserve-urls off-sets); compound aliases
  (`rewrite_images` 14, `recompress_images` 11, `extend_cache` 3 sans pdfs,
  `rewrite_javascript` 2, `testing`, `core`); `kResizeMobileImages` →
  `kDelayImages` auto-insert.
- Properties: all ~160 core typed properties + ~50 system options + 9
  factory globals + 6 nginx options with audited defaults, contexts, and
  secret flags (FetchProxy). Full per-directive table (config-gap-1) is
  transcribed during porting from `AddProperties` help strings — the table
  is the spec.
- Parsing: `Enable/Disable/ForbidFilters` + `AdjustFiltersByCommaSeparated
  List` (`+x`/`-x`/bare/empty semantics); 13 special 1-arg, 2-arg, 3-arg
  directives; scope gates (20 server-only + 2 main-only for nginx;
  process-only factory group with hard-error vs warn+ignore split);
  `ModPagespeed` prefix strip; case-insensitive names; `pagespeed
  on|off|gzip` / `ClearInheritedScripts` / `UseNativeFetcher` /
  `NativeFetcherMaxKeepaliveRequests` / `ProcessScriptVariables` nginx-only
  paths; `?PageSpeedFilters=` per-request adjust; `RewriteOptionsManager`
  request-time merge (query scope setability is config-gap-2).
- Defaults layering: core PassThrough → nginx CoreFilters + BeaconUrl
  `/ngx_pagespeed_beacon` + FileCacheInodeLimit 500000 +
  AvoidRenamingIntrospectiveJavascript; `standby` vs FileCachePath-empty
  PostConfig error is config-gap-3 (trace before implementing).

### 2.9 ps-admin — admin site

Ports `pagespeed/system/admin_site.{h,cc}` dispatch + handlers, wired to
nginx paths (statistics/global_statistics/console/messages/admin-prefix/
global_admin-prefix/purge) and Apache handler names (kept as route aliases
for compat).

- Routes: `/pagespeed_admin/<leaf>` leaves (dashboard|empty, history,
  images, html_cache, statistics, stats_json, graphs, config, console,
  message_history|javascript|css, cache, histograms; 404 + Did-you-mean);
  legacy `/*_pagespeed_statistics` query dispatch (`?json→ConsoleJson`,
  `?config`, `?histograms`, `?graphs`, `?cache`, else Statistics).
- Console/graphs: `?json` delegation; HTML requires Statistics+
  StatisticsLogging+LogDir else setup notice; JSON rate-limited (429
  busy), 404 without logger.
- Purge endpoint `cache?purge=<url|*>` (`*` = whole cache; per-vhost scope).
  Access-control wiring (Allow/Disallow/Domain vs page/purge enforcement)
  is admin-gap-2 — traced and implemented here.
- All rendering takes `&ps-stats` snapshots + `&ps-config`; no global
  state, so the same handlers serve nginx today and any future server
  binding.

### 2.10 ps-nginx — nginx module (only `unsafe` crate)

Ports `ngx_pagespeed/src/*` (17 `.cc` + 17 `.h`) against pinned ngx-rust
bindings. This is the ONLY crate allowed `unsafe`, and only for FFI.

- Bindings: pinned `ngx-rust` (exact version pinned in `Cargo.toml` at
  implementation time; supported nginx range is nginx-gap-1 — pin to the
  tested release, currently test pin release-1.29.5-2, and record the
  range once verified).
- FFI-only `unsafe`: every `unsafe` block touches only the raw nginx ABI
  (`ngx_*` structs/functions); all logic above the boundary is safe Rust.
  `#![deny(unsafe_op_in_unsafe_fn)]` + a `SAFETY:` comment on every
  `unsafe` block. The FFI surface mirrors the audited boundary files
  (`ngx_pagespeed`, `ngx_base_fetch`, `ngx_event_connection`, `ngx_fetch`,
  `ngx_url_async_fetcher`, `ngx_rewrite_driver_factory`,
  `ngx_rewrite_options`, `ngx_server_context`, `ngx_message_handler`,
  `log_message_handler`, `ngx_gzip_setter`, `ngx_caching_headers`,
  `ngx_list_iterator`).
- Hooks: `ngx_module_t` (init_master/init_process/exit_process) + etag
  filter module; conf hooks (pre/postconfiguration, create/merge ×
  main/srv/loc); phase hooks (POST_READ self-fetch relabel; PREACCESS/
  PRECONTENT ≤/≥1.13.4 with in-place phase-engine rewrite and the
  sync-decide contract); content dispatch (beacon/static/messages/
  statistics/admin/purge/resource); suspend/resume (`r->count++`,
  buffered flag, `NGX_DONE`); decline path (phase-engine resume).
- Filters in load-bearing init order (in_place → fix_headers → base_fetch
  → html_rewrite, html_rewrite outermost): base-fetch completion pump,
  html-rewrite header/body pair (non-HTML skip, Content-Length/Accept-
  Ranges/Vary strip, gzip/deflate inflater, `filter_need_in_memory` +
  buffered, NULL-down-chain body forward), IPRO check/body pair
  (hit/miss-`kNotInCacheStatus`/error, recorder, `ngx_buf_in_memory`
  requirement), fix_headers (override-token strip, no-cache forcing),
  etag (internal `@psol-etag`→ETag rename, gzip_vary clear, conditional
  s-maxage).
- Event-loop-safe offload: rewrite work runs on worker threads; completion
  crosses to the event loop via a nonblocking pipe + `ngx_connection_t`
  read event (port of `NgxEventConnection`); refcount 2 (Done + Detach)
  +1 per queued event, self-delete at 0; writer-side EAGAIN backoff
  confined to background threads; 30s drain only at worker exit; native
  fetcher fully async (resolver, connect peer, read/write handlers,
  timers, 60s keepalive pool); threads start only after fork
  (`StartThreads` from init_process); blocking syscalls (stat/chown/
  mkdir) only at config time; the one request-path block
  (`GetRemoteOptions`) keeps its timeout-0 mitigation for native+blocking
  cache.
- Request plumbing: per-request ctx (base_fetch/proxy_fetch/inflater/
  driver/recorder/ipro headers/url/caching flags) with deterministic
  teardown; request-pool-only output allocation; Host/scheme/uri URL
  build with X-Forwarded-Proto + IPv6/merge_slashes handling; IPRO
  agreement gate; `$ps_dollar` script-variables path; gzip setter
  (config-time scan + rollback on explicit user config); message/crash
  handlers (`ngx_log_error`, signal + alarm(2) + stack trace).

## 3. Cross-cutting rules

Zero-copy: parse over borrowed input spans wherever the data flows
straight through (HTML tokens, CSS rules, headers, URLs); copy only at
transform boundaries (rewritten bytes, encoded images, cache values that
outlive the request). Every `Vec<u8>`/`String` allocation on a request
path must name its owner and lifetime in a comment.

Bounded memory: every buffer, queue, and cache entry has a cap sourced
from config or a named constant — `MaxHtmlParseBytes`,
`MaxCacheableResponseContentLength` (16MB), `IproMaxResponseBytes` (10MB),
`IproMaxConcurrentRecordings` (10), `kMaxBufferedOutputBytes` (1MB),
`MessageBufferSize`, `AvifMaxConcurrent` (2), `ImageMaxRewritesAtOnce`
(8), `ResolutionLimitBytes` (32MB), purge-set capacity (25000),
`PurgeMethod`/flush poll, stats-log trim, graph-query bounds (1s..1day,
64 vars). Unbounded growth is a bug; tests assert the caps.

Threading: event loop never blocks (§6.5/§6.6 non-blocking summary is
normative); rewrite threads never touch nginx handles; shared stats are
`Send + Sync`; LRU/shm caches keep their documented sharing models.

Naming parity: stat names, cache key formats, URL ids, filter names, and
directive spellings are byte-identical to C++ so existing configs,
caches, and dashboards keep working; any intentional deviation is recorded
in MAPPING.md with rationale.

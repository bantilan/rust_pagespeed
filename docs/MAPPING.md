# C++ → Rust mapping (Phase 2)

Every C++ owner path from [AUDIT.md](AUDIT.md) mapped to its Rust crate.
Prefixes: `M:` = mod_pagespeed, `N:` = ngx_pagespeed, `P:` = ngx_psol
(snapshot/bundle, no sources of its own), `C:` = ngx_compiler
(shell-only). Anchor = primary audited symbol for disambiguation.

## ps-html (streaming engine + filter chain)

| C++ owner | Anchor |
|---|---|
| `M:net/instaweb/rewriter/rewrite_driver.cc` + `public/rewrite_driver.h` | chain order, `SetServerContext`, `PopulateResourceNamer`, `InitStats` |
| `M:net/instaweb/rewriter/rewrite_context.cc` + `public/rewrite_context.h` | `SetPartitionKey`, metadata TTL, `CacheKeySuffix`/`UserAgentCacheKey` |
| `M:net/instaweb/rewriter/server_context.cc` | `rname/`, generated max-age, `ApplyInputCacheControl` |
| `M:net/instaweb/rewriter/*_filter.cc` + `public/*_filter.h` (all HTML/resource filters) | filter classes in §2 owner list |
| `M:pagespeed/kernel/` HTML filters (CollapseWhitespace/ElideAttributes/RemoveComments/RemoveQuotes/AmpDocument) | kernel-owned HTML filters |
| `M:net/instaweb/rewriter/input_info_utils.cc` | `IsInputValid` |
| `M:net/instaweb/rewriter/rewrite_stats.cc` | `RewriteStats::InitStats` (names feed ps-stats) |
| `M:net/instaweb/rewriter/cache_extender.cc` | `ce` pipeline, threshold |
| `M:net/instaweb/rewriter/output_resource.cc` | `EndWrite` hash, `name_key`, `HttpCacheKey` |
| `M:net/instaweb/rewriter/resource_namer.cc` | URL format (impl shared with ps-cache) |
| `M:net/instaweb/rewriter/collect_dependencies*` etc. (chain members) | CollectDependencies position |
| `M:net/instaweb/htmlparse/` | HTML tokenizer/parser role |
| `M:net/instaweb/rewriter/cached_result.proto` (`ResourceContext`) | variant-key fields (shared with ps-image) |
| `M:net/instaweb/rewriter/image_url_encoder.cc` | variant suffixes, `CacheKeyFromResourceContext` (shared with ps-image) |
| `M:net/instaweb/rewriter/in_place_rewrite_context.cc` | `AddVaryIfRequired`, `EncodeUserAgentIntoResourceContext` |
| `M:net/instaweb/rewriter/device_properties.cc` + `request_properties.cc` + `public/downstream_caching_directives.h` | negotiation inputs (shared with ps-image) |
| `M:pagespeed/automatic/proxy_fetch.cc` | `ProxyFetch` HTML path, `HtmlCacheLookup` (gaps caching-gap-1) |
| `M:pagespeed/automatic/proxy_interface.h` + `static_rewriter.h` | `ProxyInterface::Fetch`, `FileRewriter` API shape |
| `M:pagespeed/nginx/` (`ngx_fetch_body`, `ngx_gzip_rollback`, `ngx_inplace_url_agreement`, `ngx_request_body`) | nginx-only helper roles (impl split with ps-nginx) |
| `M:net/instaweb/spriter/` | sprite filter role |
| `M:net/instaweb/util/` + `net/instaweb/js/` (rewriter support) | filter support code |

## ps-css

| C++ owner | Anchor |
|---|---|
| `M:net/instaweb/rewriter/css_filter.cc` (+ `public/css_filter.h`) | `cf` pipeline, blocks/saved stats |
| `M:net/instaweb/rewriter/` css_combine/css_inline/css_outline/css_move_to_head/css_inline_import_to_link/google_font_css_inline + `public/*.h` | `cc/ci/gf` pipelines, head moving |
| `M:third_party/css_parser/` (in-tree, gperf) | CSS parser role |
| `M:net/instaweb/rewriter/rewrite_options.cc` CSS defaults | `CssInlineMaxBytes`, `CssFlattenMaxBytes` |

## ps-js

| C++ owner | Anchor |
|---|---|
| `M:net/instaweb/rewriter/javascript_filter.cc`, `js_combine.cc`, `js_inline.cc`, `js_outline.cc`, `javascript_source_map.cc`, `javascript_code_block.cc` (+ `public/*.h`) | `jm/jc/ji/sm` pipelines, minified/saved stats |
| `M:net/instaweb/rewriter/google_analytics*.cc`, `insert_ga*.cc` | analytics-before-JS ordering |
| `M:third_party/closure/` + `closure_library/` + `closure.gypi` | Closure Compiler role (ADVANCED/SIMPLE); replaced by pure-Rust minifier |
| `M:net/instaweb/rewriter/rewrite_options.cc` JS defaults | `JsInlineMaxBytes` |

## ps-image

| C++ owner | Anchor |
|---|---|
| `M:pagespeed/kernel/image/image_util.{h,cc}` | `ImageFormat`, `ComputeImageType` |
| `M:pagespeed/kernel/image/jpeg_optimizer.h` + `jpeg_reader.h` + `jpeg_utils.h` | JPEG optimize/decode/quality |
| `M:pagespeed/kernel/image/png_optimizer.h` | PNG params, OptiPNG role |
| `M:pagespeed/kernel/image/gif_reader.h` | GIF decode |
| `M:pagespeed/kernel/image/webp_optimizer.h` | WebP config, frame writer |
| `M:pagespeed/kernel/image/read_image.{h,cc}` | scanline factories (AVIF: none) |
| `M:pagespeed/kernel/image/image_converter.h` + `image_resizer.h` + `pixel_format_optimizer.h` + `image_analysis.h` | convert/resize/alpha/analysis |
| `M:pagespeed/kernel/image/image_optimizer.{h,cc}` + `image_optimizer.proto` | `Optimize`, `ComputeDesiredFormat` |
| `M:net/instaweb/rewriter/image.cc` + `public/image.h` | `ConvertToAvifExternal` (avifenc), `CompressionOptions` |
| `M:net/instaweb/rewriter/webp_optimizer.{h,cc}` | rewriter `OptimizeWebp`, `ReduceWebpImageQuality` |
| `M:net/instaweb/rewriter/image_rewrite_filter.cc` | `DetermineQualities`, `ImageOptionsForLoadedResource`, `ResizeImageIfNecessary` |
| `M:net/instaweb/rewriter/image_url_encoder.cc` | levels, suffixes, Vary allow (shared with ps-html) |
| `M:net/instaweb/rewriter/cached_result.proto` (`ResourceContext`) | variant-key fields (shared with ps-html) |
| `M:net/instaweb/rewriter/device_properties.cc` + `request_properties.cc` + `public/downstream_caching_directives.h` | negotiation (shared with ps-html) |
| `M:pagespeed/kernel/http/user_agent_matcher.cc` | UA fallbacks |
| `M:pagespeed/kernel/http/content_type.cc` | image mime map |
| `M:net/instaweb/rewriter/rewrite_options.cc` image defaults + `rewrite_gflags.cc` | qualities/timeouts/limits |
| `M:pagespeed/kernel/image/testdata/` | test images (reused by port tests) |
| `M:third_party/libpng/src`, `libjpeg_turbo/src`, `libwebp`, `giflib/src`, `optipng` | codec roles (Rust equivalents) |

## ps-cache

| C++ owner | Anchor |
|---|---|
| `M:net/instaweb/rewriter/resource_namer.cc` | `Encode`/`Decode`/`EncodeIdName` (shared with ps-html) |
| `M:net/instaweb/rewriter/output_resource.cc` | hash/key roles (shared with ps-html) |
| `M:pagespeed/kernel/base/hasher.cc` + `md5_hasher.h` | `Hash`, `HashToUint64` |
| `M:pagespeed/kernel/base/cache_interface.h` | `CacheInterface` API |
| `M:pagespeed/kernel/cache/file_cache.{h,cc}` | `FileCache`, `EncodeFilename`, `Clean` |
| `M:pagespeed/kernel/util/url_to_filename_encoder.h` | filename escaping |
| `M:pagespeed/kernel/cache/lru_cache.h` | `LRUCache` |
| `M:pagespeed/kernel/sharedmem/shared_mem_cache.h` | shm metadata cache |
| `M:pagespeed/system/apr_mem_cache.{h,cc}` | memcached backend, key-in-value |
| `M:pagespeed/kernel/cache/key_value_codec.h` | `Encode`/`Decode` |
| `M:pagespeed/system/redis_cache.{h,cc}` | Redis backend |
| `M:pagespeed/system/system_caches.{h,cc}` | backend wiring (chain order = caching-gap-2) |
| `M:pagespeed/kernel/cache/cache_key_prepender.h` + `fallback_cache.h` + `compressed_cache.h` | adapters |
| `M:pagespeed/kernel/cache/purge_context.h` + `purge_set.h` | purge LRU + file poll |
| `M:pagespeed/system/system_cache_path.cc` | `cache.purge`/`cache.flush`, `FlushCacheIfNecessary` |
| `M:pagespeed/kernel/base/timer.h` | time constants |
| `M:third_party/google-sparsehash/src` | hash-table role (Rust maps) |
| `M:third_party/hiredis/src` | Redis client role |
| `M:third_party/aprutil/src` (memcache client) | memcached client role |

## ps-http

| C++ owner | Anchor |
|---|---|
| `M:net/instaweb/http/http_cache.cc` + `public/http_cache.h` | `HTTPCache`, `CompositeKey` `v5/`, `RememberFailure` |
| `M:net/instaweb/http/http_cache_failure.cc` | failure TTL table |
| `M:net/instaweb/http/public/url_async_fetcher.h` + `async_fetch.h` + `mock_url_fetcher.h` + `request_context.h` | fetcher APIs |
| `M:pagespeed/kernel/http/response_headers.{h,cc}` | TTL API, shortcut fields |
| `M:net/instaweb/http/` response/request header parsing | `ResponseHeadersParser::ParseChunk` |
| `M:net/instaweb/rewriter/public/downstream_cache_purger.h` | downstream PURGE |
| `M:pagespeed/system/` serf fetcher | serf role (async fetch impl) |
| `M:third_party/serf/src` | HTTP client role |
| `M:third_party/re2/src` | regex role (Rust regex) |
| `M:third_party/brotli/src` + `third_party/zlib/src` | compression roles |
| `M:third_party/domain_registry_provider` | public-suffix role |
| `M:third_party/chromium/src/googleurl` + `M:url/` + `M:base/` URL utils | URL parsing role |
| `M:third_party/modp_b64` + `M:third_party/base64` | base64 role |

## ps-stats

| C++ owner | Anchor |
|---|---|
| `M:pagespeed/kernel/{base,sharedmem}/statistics*` | shared-mem segment lifecycle |
| `M:pagespeed/kernel/util/statistics_logger.cc` | `stats_log_*`, `DumpJSON`, `PreviousDashboardCounters` |
| `M:pagespeed/kernel/cache/cache_stats.cc` | `CacheStats::InitStats(prefix)` |
| `M:net/instaweb/rewriter/rewrite_stats.cc` | `RewriteStats::InitStats` |
| `M:net/instaweb/http/http_cache.cc` (`InitStats`) | `cache_*` family |
| `M:net/instaweb/rewriter/rewrite_driver.cc` (`InitStats` `:880-912`) | `html_cache_*` + 27-filter fan-out (admin-gap-1) |
| `M:net/instaweb/rewriter/image_rewrite_filter.cc` (stats) | `image_*` family |
| `M:net/instaweb/rewriter/css_filter.cc` (stats) | `css_filter_*` family |
| `M:net/instaweb/rewriter/javascript_code_block.cc` (stats) | `javascript_*` family |
| `M:pagespeed/system/system_caches.cc` (`InitStats`) | 7 backend prefixes |

## ps-config

| C++ owner | Anchor |
|---|---|
| `M:net/instaweb/rewriter/public/rewrite_options.h` (`enum Filter`, scopes) | 95 filters, scope enum |
| `M:net/instaweb/rewriter/rewrite_options.cc` | sets, ids, `Enabled()`, properties, special/2-arg/3-arg directives |
| `M:net/instaweb/rewriter/rewrite_filter_names.gperf` | 93 names + aliases |
| `M:net/instaweb/rewriter/rewrite_query.cc` | `?PageSpeedFilters=` |
| `M:net/instaweb/config/rewrite_options_manager.{h,cc}` | request-time merge |
| `M:pagespeed/system/system_rewrite_options.cc` | ~50 system options |
| `M:pagespeed/system/system_rewrite_driver_factory.cc` (option parsing) | 9 factory globals, shm-cache directive |
| `N:src/ngx_rewrite_options.cc` + `ngx_rewrite_options.h` | 6 nginx options, scope gates, `pagespeed on|off|gzip` |
| `N:src/ngx_rewrite_driver_factory.cc` (defaults) | CoreFilters, beacon URL, inode limit |
| `M:pagespeed/apache/mod_instaweb.cc` (directives only) | Apache `ModPagespeed*Filters`, Apache-only renames |
| `M:net/instaweb/rewriter/rewrite_gflags.cc` | gflags mirrors |
| `M:third_party/gflags/` | flags role |
| `M:third_party/protobuf/src` + `image_optimizer.proto` + `cached_result.proto` + `controller.proto` | proto roles (Rust serde/structs) |
| `M:third_party/jsoncpp/src` | JSON role (beacon/config) |
| `M:third_party/icu` | Unicode role |

## ps-admin

| C++ owner | Anchor |
|---|---|
| `M:pagespeed/system/admin_site.{h,cc}` | handlers, `kTabs`, `AdminPage`, `StatisticsPage`, purge |
| `M:pagespeed/apache/instaweb_handler.cc` (admin routes only) | Apache admin handler names (route aliases) |
| `M:third_party/closure_library/` | admin/beacon JS sources role |

## ps-nginx

| C++ owner | Anchor |
|---|---|
| `N:src/ngx_pagespeed.cc` + `ngx_pagespeed.h` | modules, conf/phase hooks, filters, ctx, beacon, URL build |
| `N:src/ngx_base_fetch.cc` + `ngx_base_fetch.h` | `NgxBaseFetch` refcount/pipe bridge |
| `N:src/ngx_event_connection.cc` + `ngx_event_connection.h` | pipe + `ngx_connection_t` events |
| `N:src/ngx_fetch.cc` + `ngx_fetch.h` | native fetch, state machine, keepalive pool |
| `N:src/ngx_url_async_fetcher.cc` + `ngx_url_async_fetcher.h` | native fetcher, rewrite/event split |
| `N:src/ngx_server_context.cc` + `ngx_server_context.h` | `NgxServerContext`, `NewRequestContext` |
| `N:src/ngx_rewrite_driver_factory.cc` + `.h` (lifecycle only; options→ps-config) | `StartThreads`, `AllocateFetcher`, fork hooks |
| `N:src/ngx_rewrite_options.cc` + `.h` (plumbing only; tables→ps-config) | `ParseAndSetOptions` routing |
| `N:src/ngx_message_handler.cc` + `.h` | `ngx_log_error` routing, crash handler |
| `N:src/log_message_handler.cc` + `.h` | logging install |
| `N:src/ngx_gzip_setter.cc` + `.h` | config-time gzip scan/rollback |
| `N:src/ngx_caching_headers.cc` + `.h` | caching header policy |
| `N:src/cache_control_merge.cc` + `.h`, `caching_header_policy.cc` + `.h` | header merge/policy |
| `N:src/ngx_list_iterator.cc` + `.h` | header iteration |
| `N:src/ngx_self_fetch_label.cc` + `.h` | 224.0.0.0 relabel |
| `N:src/ngx_caching_headers.h` et al. | FFI boundary set (§6.6) |
| `M:pagespeed/nginx/` (helper impls) | nginx-only helpers (split with ps-html) |
| `N:config` + `config.make` + `cpp_feature` | addon/configure role → cargo build + nginx `config` shim |
| `N:scripts/` + `N:docker/` | build-script roles (documented, not ported as code) |
| `N:test/nginx_system_test.sh` + `pagespeed_test.conf.template` | system-test shape + conf cases reused by Slice 7 |

## Explicitly not ported as code

| C++ owner | Disposition |
|---|---|
| `M:pagespeed/apache/` (except directive/admin-route names) | Apache binding out of scope; names kept as aliases |
| `M:pagespeed/controller/` | experimental gRPC controller; out of scope |
| `M:pagespeed/envoy/` | Envoy filter; out of scope |
| `M:pagespeed/opt/ads`, `pagespeed/opt/logging` | ads/property-cache passes; out of scope unless compat harness demands |
| `M:third_party/grpc/src`, `third_party/boringssl/src` | vendored RPC/TLS; Rust TLS via platform crates if needed |
| `M:third_party/httpd/src`, `httpd24/src`, `apr/src`, `chromium/src/base,build`, `nghttp2`, `mod_fcgid` | Apache/APR/Chromium build substrate; not needed in Rust |
| `M:build/`, `tools/gyp`, `Makefile`, `WORKSPACE`, `.bazelrc`, `pagespeed.bazelrc` | gyp/Bazel build substrate; replaced by Cargo |
| `M:install/`, `M:devel/`, `M:benchmark/` (harness), `M:test/main.cc` runner | build/packaging/dev substrate; Rust has own harness (PLAN.md) |
| `M:testing/`, `testing/gtest`, `testing/gmock` | C++ test frameworks; Rust `#[test]` + compat harness |
| `M:third_party/closure/download.sh` compiler binary | replaced by pure-Rust minifier (parity via compat) |
| `P:` everything (`include/`, `lib/*/pagespeed_automatic.a`, `psol.tar.gz`, `tools/`, `python2/`, `gperf/`, nested `mod_pagespeed/`) | prebuilt snapshot + vendored toolchain; reference only |
| `C:` everything (`bootstrap.sh`, `build_from_source_to_deb.sh`, hardening script, admin snippet) | build driver; hardening flags re-expressed in Cargo/nginx build, snippet kept as deploy config |
| `N:psol/`, `N:testing-dependencies/`, `N:PSOL_BINARY_URL` | snapshot/test-matrix pointers; reference only |

## Naming parity + known deviations (Step 8 audit)

- Directive spellings: 162/162 non-empty property-table rows resolve to
  their referenced C++ `Add*Property` registration (const value or
  literal at the cited line); 167 rows total (5 empty-name synthetic
  entries). Filter ids/names/levels, stat names, cache-key formats,
  URL vectors, negotiation vectors, and admin route leaves are pinned
  by `ps-compat` fixture vectors (23 tests).
- Filter identities: all 95 `(id, display)` pairs diffed
  byte-identical against `kFilterVectorStaticInitializer`
  (`rewrite_options.cc:766-896`, resolving the 15 `k*Id`
  constants); the 11 sampled public names verified in
  `rewrite_filter_names.gperf`. Pinned by
  `filter_identity_vectors` plus the exhaustive `Filter::ALL`
  sweep (`all_filter_ids_are_unique_and_round_trip`).
- Naming pins: 167-row table shape (34 NULL-help + 5 synthetic)
  in `property_table_shape_pins_config_1`; scope gate in
  `scope_gate_pins_config_2`; 14 admin leaves + redirect/404/
  purge-auth in `admin_route_vectors`; 20 stat names across all
  families in `stat_name_vectors`; client-options grammar in
  `client_options_vectors`.
- Engine-level deviations (behavioral, documented at the source):
  URL canonicalization rides WHATWG `url` instead of Chromium GURL
  (exotic inputs may differ; `ps-http/src/google_url.rs` header);
  HTTP-date parsing accepts the 3 standard formats instead of the
  broader NSPR range (`ps-http/src/time.rs` header).
- Deferred epics keep their tests (beacon/CSP/logging/concurrency/
  URL-options/experiments/property-cache/file-resources/deadlines);
  the suites assert current behavior and the owners are named in
  `PLAN.md` progress entries.

## ps-compat (harness hooks, no suite)

| C++ owner | Anchor |
|---|---|
| `M:net/instaweb/rewriter/rewrite_query.{h,cc}` (`X-PSA-Client-Options`, `ScanHeader`) | filters-6: client-options parse/apply + header scan |
| per-crate naming tables | fixture vectors (directives, levels, filters, client-options, keys, namer, URLs, negotiation, goldens, routes, stats, nginx) |
| `ps-compat/src/harness.rs` (Step 8) | `Runner` trait + `Corpus` + 4 `Assertion` kinds the Phase 4 suite must satisfy |

## Gap watch (from AUDIT §9)

Unverified wirings that may add rows here once traced:
(none remain — see closures below).
- caching-gap-1 DOCUMENTED/BLOCKED: the HTML page cache
  (`HtmlCacheLookup` in `proxy_fetch.cc`, `HtmlCache`
  option default off) serves the proxy flow only; the
  sync bridge rewrites every hit. Needs the proxy epic.
- admin-gap-1 CLOSED: 185-name console roster ported
  (`ps-html/src/rewrite_stats.rs::init_stats`,
  transcribed verbatim from every C++ `InitStats`
  across rewriter/http/automatic/kernel-http/system),
  pre-created in `ServerContext::new_with_metadata`.

Traced during Step 8 (compat hooks):
- filters-5 CLOSED: `RewriteLevel` is a regular base property
  (`kRewriteLevel`, id `"l"`, `DirectoryScope`,
  `rewrite_options.cc:1253-1256`) set through the generic
  `ParseAndSetOptionFromName` path every `ModPagespeed*`
  directive uses (`ApacheConfig` strips the prefix and calls
  through). The port carries the identical row
  (`properties_table.inc.rs`, id `"l"`, `Scope::Directory`).
- filters-6 CLOSED (implemented): request headers run the same
  `ScanNameValue` as query params (`ScanHeader`,
  `rewrite_query.cc:108-171`); `X-PSA-Client-Options`
  (`v=1,m=2,iqp=1` grammar, version-gated, unknown pairs
  ignored) parses via `ParseClientOptions` and applies via
  `UpdateRewriteOptionsWithClientOptions` (NoTransform →
  `DisableAllFilters`, NoImageTransform → image-related
  disables, Default → ignore). Ported to
  `ps-config/src/client_options.rs` (8 C++ `ClientOptions*`
  cases) plus `ps-http` `scan_headers_for_options`
  (override-token gate, header consumption) with 3 tests.
- config-1 CLOSED (verified, nothing to transcribe): the
  port's 34 help-less properties are exactly C++'s 34
  NULL-help registrations (name-by-name diff); the other 5
  `help: None` rows are empty-name synthetic entries. All 128
  C++ help strings are already transcribed.
- config-2 CLOSED (rule documented): the query path sets only
  `kQueryScope` options (`SetOptionFromQuery` passes `kQueryScope`
  as max_scope); file/config directives pass the context scope
  through the same single gate (`option->scope() > max_scope` →
  `kOptionNameUnknown`), ported 1:1 with no per-directive
  exceptions in C++. Pinned by `scope_gate_pins_config_2`.
- config-3 DOCUMENTED/BLOCKED: `pagespeed standby` +
  `PostConfig` interaction lives in the nginx module
  (`N:src/`, outside this tree); no in-repo behavior to port.
- deps-1/2/3 RECORDED: `bazel/repositories.bzl` pins —
  envoy 9ae348c, brotli d6d9895, hiredis/jsoncpp/libpng/
  libwebp/giflib/optipng by release number, sparsehash
  6ff8809, glog 0a2e593, gflags f7388c6, drp 21a7a0f,
  libjpeg-turbo ab7cd97, apr 901ece0, aprutil 13ed779,
  serf 3a37fc1, closure 20191111. Vendored
  `third_party/grpc`, `third_party/boringssl` carry no
  version files in-tree (SHA-only upstream); test-only
  nginx trees are gitfile references; ngx_compiler nginx
  stays intentionally unpinned (apt source at build).
- modules-gap-1 CLOSED (roles recorded): `bazel/` =
  third-party fetch/build rules (`.bzl`); `changelog.d/` =
  release-note fragments; `external/` = upstream patch files
  (apr, brotli); `log/` = local build logs (not source);
  `out/` = build output trees (not source); `strings/` =
  one stringpiece helper header; `url/` = vendored
  Chromium URL library + gyp files.
- nginx-gap-2 CHECKED: `install/common/pagespeed.conf.template`
  lists filters illustratively (6 `EnableFilters` mentions),
  and `MAPPING.md` already records the co-update mandate
  (`AddPreRenderFilters` comment → template + config-filters
  docs). The Rust chain authority is `ps-html/src/chain.rs`;
  order changes must review the template comment block.
- nginx-gap-1 IN PROGRESS (Step 6 remainder): the earlier
  "no `N:src/`" scoping was wrong — `ngx_pagespeed/src/` is
  complete in-tree (28 files incl. `ngx_pagespeed.cc`), the
  exact nginx 1.31.8 sources are staged at
  `../.nginx-build/nginx-1.31.8`, and `ngx` 0.5.1 /
  `nginx-sys` 0.5.0 bind against them (`SUPPORTED_NGINX =
  ["1.31.8"]`, `bound_nginx_version() == 1031008` pinned by
  test). Supported range stays 1.31.8-only until a second
  version is staged and matrix-tested.

Traced during Step 4 (chain builder):
- filters-1 CLOSED: `cs/cu` (`kRewriteStyleAttributes[IWithUrl]`) owned by
  `CssFilter` (`css_filter.cc:1015-1017`); no chain slot. Stays on the
  ps-css `CssFilter` row.
- filters-2 VERIFIED ABSENT: the 12 enum-only filters (`kDivStructure`,
  `kExplicitCloseTags`, …) have no instantiation outside enum/registration
  (`rewrite_options.cc` only); C++ wires no chain slots for them either.
  Record-only unless C++ gains owners.
- filters-3 VERIFIED ABSENT from the HTML chain (full read of
  `AddPreRenderFilters`/`AddPostRenderFilters`): `kInPlaceOptimizeForBrowser`
  rides the separate `InPlaceRewriteContext` path. Stays open until the
  IPRO context lands (ps-html `ipro.rs`).
- filters-4 CLOSED: writer attached in `RewriteDriver::SetWriter`, gated by
  `kHtmlWriterFilter` (default-enabled in the `RewriteOptions` ctor).
  Ported as the terminal `HtmlWriter` post slot in `ps-html/src/chain.rs`.

Chain-order authority: `ps-html/src/chain.rs` (mirrors C++
`AddPreRenderFilters`/`AddPostRenderFilters`). Order changes must also
review `M:install/common/pagespeed.conf.template` + the config-filters docs,
which mention filters in application order (illustratively, not exhaustively).

## Phase 4 compat suite (`ps-compat/tests/`)

Live runners boot real nginx with either `.so` (`Runner`
impls), serve each corpus case fresh, and compare normalized
responses (framing/clock/identity headers dropped,
`X-Page-Speed` pinned, multiset header compare). Record mode
(`PS_COMPAT_RECORD=1`) captures C++ expectations into
`tests/corpus_data/{pages,configs}.rs`; compare mode asserts
Rust equality. Page corpus: 5 cases; config corpus: 7 cases
(boot parity included).

Fixed by the suite (were divergences, now match):
- Rewritten HTML kept the origin ETag/Last-Modified/
  Content-Length; C++ strips them in
  `ProxyFetch::SetupForHtml` (`proxy_fetch.cc:1113-1119`).
  Ported into `finish_html_header_filter`.

Known gaps (suite reports, never fails):
- compat-1 CLOSED: inline CSS/JS minify wired as
  `CssMinifyFilter`/`JsMinifyFilter` on the `CssFilter`/
  `Javascript` slots (inline `<style>`/`<script>` only;
  external resources still need the driver-bound filters
  and fetch machinery). `core-rich` and `elide-attributes`
  now match C++ byte-exact.
- compat-2 CLOSED: recorder armed on IPRO miss with
  `StatsRecordingBackend` (stats + logs real; cache writes
  no-ops until the fetch machinery lands), plus the
  `SetupForHtml` caching half (fresh rewritten-HTML
  lifetime; the fix-up filter's line-level drop removes
  the recombined stamp tail). `missing-file` now matches
  C++ byte-exact. Remaining: recording cache writes
  (fetch-machinery epic). (`set_date_and_caching`,
  `cache_control_values_to_preserve`, and
  `set_cache_control_max_age` divergences fixed this
  session; all three writers now C++-verbatim.)
- Fetch epic F1 DONE: `FileFetcher`
  (`ps-nginx/src/file_fetch.rs`) serves same-server
  static inputs (server root + names from child-init;
  `http`-only, jailed, capped, subrequest-faithful
  bare headers). Gaps: per-location roots/aliases/
  `try_files`/index, regex server names, proxy/TLS/
  mapped origins (fail closed); recording cache
  writes (only repeated-404 notes now).
- Fetch epic F3 DONE: end-to-end resource rewriting
  (driver-bound CSS/JS slots, `proxy_mode` IPRO
  fallback, minify servants + lazy registration, 8
  more CoreFilters slots, C++-exact serve headers).
  Compat: 8 page + 7 config cases green (resource
  cases poll to steady state). Gaps: old-mtime IPRO
  gate (C++ refuses, mechanism unidentified — ours
  over-optimizes); MetaTag/SupportNoscript slots;
  CriticalImagesBeacon (no impl); nested CSS subsets
  in servants.
- Fetch epic F4 DONE: image servant (`rewrite_in_place`
  via the shared transcode core; pixel-identical to
  C++ on the corpus PNG). Compat: 9 page + 7 config
  green with pixel comparison. Gaps: webp/avif
  servants (need request Accept); lossy JPEG corpus
  coverage (needs tolerance).

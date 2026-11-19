# PageSpeed Audit (Phase 1)

Complete inventory of the read-only originals (`mod_pagespeed`, `ngx_pagespeed`,
`ngx_psol`, `ngx_compiler`) as input to the Rust port. All paths below are
relative to those checkouts unless prefixed with `pagespeed-rust/`.

Sources: Phase 0 prerequisite result plus the audit children for
modules, filters, config, dependencies, caching, nginx hooks, admin/stats,
and images. Known gaps are collected in §9.

> Rule: the four original checkouts are READ-ONLY. This document lives under
> `pagespeed-rust/docs/` and never modifies them.

## 1. Module inventory

### 1.1 mod_pagespeed (HEAD 0fdb3545f, VERSION 1.15.0.0)

Top level (L1): `base/ bazel/ benchmark/ build/ changelog.d/ devel/ doc/
external/ html/ install/ log/ net/ out/ pagespeed/ strings/ test/ testing/
third_party/ tools/ url/` plus `BUILD WORKSPACE .bazelrc pagespeed.bazelrc
Makefile`.

`pagespeed/` (L2): `apache/ automatic/ controller/ envoy/ kernel/ nginx/
opt/ system/` (+ `.gyp`/`.target.mk` build artifacts).

| Directory | Purpose |
|---|---|
| `base/` | Chromium-derived base utilities (`logging.h`, `process_util`); has BUILD |
| `pagespeed/kernel/` | Platform-independent core; subdirs `base cache html http image js sharedmem thread util` (string/http/image/js/shared-mem/thread primitives) |
| `pagespeed/opt/` | Optimization passes; subdirs `ads http logging` (incl. property cache, ads attribute) |
| `pagespeed/system/` | Server glue shared by Apache/Nginx (`system_server_context.h`, `system_rewrite_driver_factory.h`, `system_caches.h`, admin site, redis/memcache, serf fetcher, in-place resource recorder) |
| `pagespeed/apache/` | Apache module; `mod_instaweb.cc/h` entry, instaweb handler/context, apache config/fetch/server context/rewrite driver factory/writer |
| `pagespeed/automatic/` | PSOL proxy/static-rewriter API; `proxy_interface.h` `ProxyInterface::Fetch`, `static_rewriter.h` `FileRewriter`, proxy fetch; builds `pagespeed_automatic.a` |
| `pagespeed/nginx/` | nginx-only helpers (`ngx_fetch_body`, `ngx_gzip_rollback`, `ngx_inplace_url_agreement`, `ngx_request_body` + tests) |
| `pagespeed/controller/` | Central/schedule-rewrite controller incl. gRPC (`controller.proto`, central controller, queued expensive operation controller) |
| `pagespeed/envoy/` | Envoy filter integration (envoy rewrite driver factory/server context, http filter config); root BUILD `envoy_cc_binary` target |
| `net/instaweb/` | Rewriter engine; subdirs `config htmlparse http js public rewriter spriter util`; `public/` has `VERSION version.h global_constants.h` |
| `net/instaweb/rewriter/public/` | Core rewrite API headers (`rewrite_driver.h`, `rewrite_options.h`, `server_context.h`, resource/output resource, ~100 filter headers) |
| `net/instaweb/http/public/` | Fetcher/cache API (`url_async_fetcher.h`, `http_cache.h`, `async_fetch.h`, `mock_url_fetcher.h`, `request_context.h`) |
| `test/` | `test/main.cc` gtest runner via `PageSpeed::TestRunner` + `integration/*_nginx_test.py` + net/pagespeed mirrors |
| `testing/` | Bundled gtest/gmock (+ `.gyp`/`.mk`); `benchmark/`: `benchmark.h` + `history_archive_benchmark.py` + `results/` |
| `tools/` | gyp, clang plugins, closure compiler jar |
| `install/` | Packaging scripts (`build_mps.sh build_psol.sh debian/ rpm/ centos/ nginx.service`) |
| `devel/` | Dev/test helper scripts; `build/`: gyp `.gypi` + generated `.target.mk` |
| `html/` | Static site assets; `doc/`: history_archive/html_cache/polyfill_removal `.md` |

L1 entries present on disk but without an inventoried purpose (gap
modules-gap-1): `bazel/` (`.bzl` files), `changelog.d/`, `external/`, `log/`
(build logs), `out/` (build output), `strings/` (`stringpiece_utils.h`),
`url/` (`url_canon_icu.cc`, `base/`).

Build files: root `BUILD` (`cc_binary mod_pagespeed`,
`libmod_pagespeed.so` linkshared, envoy), `WORKSPACE` (workspace
mod_pagespeed + envoy deps), `.bazelrc` (envoy flags, C++17 linux),
`pagespeed.bazelrc` (APR/serf includes, C++17), `Makefile` (gyp-generated,
BUILDTYPES, `out/$(BUILDTYPE)`), per-dir `BUILD` + `*.gyp`/`*.gypi` +
`*.target.mk`.

Entry points: `pagespeed/apache/mod_instaweb.cc`
`mod_pagespeed_register_hooks` (`ap_hook_handler` instaweb handler,
`ap_register_output_filter` `MOD_PAGESPEED_OUTPUT_FILTER`, …);
`mod_instaweb.h` declares `pagespeed_module` + filter names;
`test/main.cc` gtest main.

Key headers: `net/instaweb/rewriter/public/rewrite_driver.h`,
`rewrite_options.h`, `server_context.h`;
`net/instaweb/http/public/url_async_fetcher.h`, `http_cache.h`;
`pagespeed/system/system_server_context.h`,
`system_rewrite_driver_factory.h`;
`pagespeed/automatic/proxy_interface.h`, `static_rewriter.h`;
`pagespeed/apache/mod_instaweb.h`.

### 1.2 ngx_pagespeed (HEAD c6eef0f)

L1: `src/ psol/ scripts/ docker/ test/ testing-dependencies/` +
`config config.make cpp_feature PSOL_BINARY_URL`.
`src/` = 17 `.cc` + 17 `.h`: `ngx_pagespeed`, `server_context`,
`rewrite_driver_factory`, `rewrite_options`, `base_fetch`, `fetch`,
`url_async_fetcher`, `caching_headers`, `cache_control_merge`,
`caching_header_policy`, `event_connection`, `gzip_setter`,
`list_iterator`, `message_handler`, `log_message_handler`,
`self_fetch_label`.

| Directory | Purpose |
|---|---|
| `src/` | nginx HTTP-filter module; `ngx_pagespeed.cc/h` entry + directives, `NgxServerContext` (extends `SystemServerContext`), `NgxRewriteDriverFactory` (extends `SystemRewriteDriverFactory`), fetch/caching/gzip/event-loop adapters |
| `psol/` | Prebuilt PSOL snapshot (`include/` headers mirror + `lib/Release/.../pagespeed_automatic.a` + `include_history.txt`) |
| `scripts/` | `build_from_scratch_with_fixes.sh build_ngx_pagespeed.sh format_binary_url.sh pagespeed_libraries_generator.sh rebuild.sh` |
| `docker/` | Alpine Dockerfiles; `test/`: empty; `testing-dependencies/`: nginx, mod_pagespeed, headers-more, ngx_brotli, ModSecurity-nginx submodules |

Build: `config` (nginx auto/configure addon: PSOL detection, include
paths, `pagespeed_automatic.a` link, filter ordering, gcc>=4.8/clang>=3.3
check), `config.make` (per-source `--std=c++11` CFLAGS), `cpp_feature` (C++
autoconf probe); `PSOL_BINARY_URL` is a dev-branch stub pointing to the
Building-PSOL-From-Source wiki (must build from source).

Entry: `src/ngx_pagespeed.cc` defines `ngx_module_t ngx_pagespeed`
(`NGX_HTTP_MODULE`, `ps_commands`, `ps_init_module`/`init_child`/
`exit_child`) + `ngx_pagespeed_etag_filter`; `ngx_pagespeed.h` declares
ctx structs and string-piece helpers.

Key headers: `src/ngx_pagespeed.h`, `ngx_server_context.h`
(`NgxServerContext`), `ngx_rewrite_driver_factory.h`
(`NgxRewriteDriverFactory`), `ngx_rewrite_options.h`,
`ngx_base_fetch.h`, `ngx_url_async_fetcher.h`.

### 1.3 ngx_psol (HEAD 1d17cb9)

L1: `include/ lib/ local-lib/ gperf/ tools/ python2/ mod_pagespeed/` +
`psol.tar.gz include_history.txt`.

| Directory | Purpose |
|---|---|
| `include/` | PSOL public header snapshot (`net pagespeed strings testing third_party tools url out`), copied 2026-02-13 from mod_pagespeed@a863f1d per `include_history.txt` |
| `lib/Release/linux/x64/` | `pagespeed_automatic.a` + `pagespeed_js_minify` binary |
| `local-lib/` | `libuuid.so` symlink; `gperf/`: gperf tool install |
| `tools/` | `gperf-3.1.tar.gz` + Python-2.7.18 source tree; `python2/`: python2 build env |
| `mod_pagespeed/` | Nested full mod_pagespeed source checkout |

No `BUILD`/`WORKSPACE`/`Makefile`/`config` of its own; ships prebuilt
artifacts + header snapshot; build tooling vendored under `tools/` +
`gperf/`. Public surface = `include/` snapshot
(`pagespeed/automatic proxy_interface.h static_rewriter.h`,
`pagespeed/apache mod_instaweb.h`, kernel/http/rewriter headers) consumed
via `pagespeed_automatic.a`.

### 1.4 ngx_compiler (HEAD 273c2d3)

Tree: `bootstrap.sh build_from_source_to_deb.sh README.md
snippets/pagespeed_admin_localhost.conf tools/assert-binary-hardening.sh`
(no C++ sources). Shell-only portable Ubuntu 24.04 build driver:
`bootstrap.sh` sets up system/apt/nginx repo + clones
ngx_pagespeed+mod_pagespeed+ModSecurity refs with resume support;
`build_from_source_to_deb.sh` builds nginx `.deb`s + PSOL tarball into
`artifacts/`; `tools/assert-binary-hardening.sh` checks
RELRO/BIND_NOW/canary/FORTIFY via readelf; the snippet restricts
`pagespeed_admin`/`global_admin` to localhost. Entry =
`bash bootstrap.sh` (delegates to `build_from_source_to_deb.sh`);
config = `*_REF`/`*_REPO`/`RESUME_BUILD` env vars per README.

## 2. Filters

Owner: `mod_pagespeed/net/instaweb/rewriter/public/rewrite_options.h:108-204`
(`enum Filter`, 95 filters `kAddBaseTag..kStripScripts` + `kEndOfFilters`;
`kFirstFilter=kAddBaseTag`; `FilterSet=EnumSet<Filter,kEndOfFilters>`).
Every HTML/resource filter class instantiated in `rewrite_driver.cc` is
declared in `net/instaweb/rewriter/public/<snake>_filter.h` (includes at
`rewrite_driver.cc:43-120`) with impl in
`net/instaweb/rewriter/<snake>_filter.cc`; kernel-owned HTML filters
(CollapseWhitespace/ElideAttributes/RemoveComments/RemoveQuotes/AmpDocument)
are instantiated in the same file. `ngx_pagespeed/src` holds no filters
(only plumbing); `ngx_psol` holds no filter sources; `ngx_compiler`
holds none.

### 2.1 Public names and resource-URL ids

- `rewrite_filter_names.gperf:65-160`: gperf `FilterMapper` maps 93 public
  names to enum values (e.g. `add_head`, `collapse_whitespace`,
  `rewrite_css`); aliases: `inline_preview_images`→`kDelayImages`,
  `insert_img_dimensions`/`insert_image_dimensions`→`kInsertImageDimensions`,
  `trim_urls`→`kLeftTrimUrls`; internal filters banned here.
- `rewrite_options.cc:764-896`: each enum gets a short resource-URL id +
  display name (`ab/ah/ad/ai/ca/ct/ij/cw/cc/ch/jc/bc/bp/gp/jp/jw/av/mc/pj/
  wa/ws/db/du/dd/df/dj/di/mj/jd/ds/ea/xi/x2/xc/ec/ei/ep/es/fc/fr/if/fs/hn/
  hpsr/hw/sm/ci/gf/ii/il/ji/io/ial/idp/ig/id/js/ll/tu/ls/ga/gaa/mob/
  mob_precompute/cj/cm/co/jo/pc/pli/pr/rj/rp/rw/rc/rq/ri/rm/ir/rx/rz/cf/
  rd/jm/jj/cs/cu/sd/sh/se/is/cp/md/slp/ss`).
- `rewrite_options.cc:377-391`: resource-pipeline ids: `ce`=CacheExtender
  `cf`=CssFilter `cc`=CssCombine `if`=CssImportFlattener `ci`=CssInline
  `gf`=GoogleFontCssInline `is`=ImageCombine `ic`=ImageCompression
  `jc`=JsCombine `jm`=JsMin `sm`=JsSourceMap `ji`=JsInline
  `ls`=LocalStorageCache `pr`=PrioritizeCriticalCss `aj`=InPlaceRewrite.

### 2.2 Filter sets

- Core (`rewrite_options.cc:605-636`, 30 filters): AddHead, CombineCss/Js,
  ConvertGifToPng/JpegProgressive/JpegToWebp/ToAvif/PngToJpeg/
  ToWebpLossless, ConvertMetaTags, ExtendCache Css/Images/Scripts,
  FallbackRewriteCssUrls, FlattenCssImports, InlineCss/Images/
  ImportToLink/Javascript, JpegSubsampling, Recompress Jpeg/Png/Webp,
  ResizeImages, RewriteCss, RewriteJs External/Inline,
  RewriteStyleAttributesWithUrl, StripImageColorProfile/MetaData.
- Optimize-for-bandwidth (`:650-666`, 15): image convert/recompress/strip
  set + InPlaceOptimizeForBrowser + RewriteCss + RewriteJs
  External/Inline; excludes URL-mutating combine/extend/inline/outline.
- Test (`:670-688`, 17): ConvertJpegToWebp Debug DeferIframe
  DeferJavascript DelayImages IncludeJsSourceMaps InsertAmpLink InsertGA
  InsertImageDimensions LazyloadImages LeftTrimUrls MakeGoogleAnalyticsAsync
  PrioritizeCriticalCss ResizeToRenderedImageDimensions ResponsiveImages
  RewriteDomains SpriteImages; Core auto-included via switch.
- Dangerous (`:691-706`, excluded even from All):
  CachePartialHtmlDeprecated CanonicalizeJavascriptLibraries
  ComputeVisibleTextDeprecated DeterministicJs DisableJavascript
  DivStructure ExperimentCollectMobImageInfo ExplicitCloseTags FixReflows
  Mobilize MobilizePrecompute ServeDeprecationNotice StripLegacyPolyfills
  StripScripts.
- Dependency sets (`:712-754,898-929`): `kRequiresScriptExecutionFilterSet`
  (9, drives SupportNoscript), `kAddHeadFilters` (10 filters requiring
  head: AddBaseTag AddHead AddInstrumentation CombineHeads DeterministicJs
  HandleNoscriptRedirect MakeGoogleAnalyticsAsync Mobilize MoveCssAboveScripts
  MoveCssToHead), `kMobilizeFilterSet` (Mobilize+RewriteCss+RewriteDomains),
  preserve-urls off-sets for image/js/css.
- Compound aliases (`:2918-2971`, `AddByNameToFilterSet`): `rewrite_images`
  (14 filters), `recompress_images` (11), `extend_cache` (3: css/images/
  scripts, NOT pdfs), `rewrite_javascript` (2), `testing` (Test+Core),
  `core` (Core).
- `kResizeMobileImages` auto-inserts `kDelayImages` (`:2984-2986`, hard
  dependency).

### 2.3 Enablement precedence and flags API

`Enabled()` (`:3596-3658`): forbidden/disabled (always lose) > explicitly
enabled (always win) > css/js/image preserve-urls > RewriteLevel switch
(TestingCoreFilters falls through to Core; All enables all non-dangerous;
PassThrough enables none). `RequiresAddHead()` (`:3819-3826`) is true if
any `kAddHeadFilters` member is enabled.

Flags API (`:296-307`, `:3268-3272`, `public/rewrite_options.h:1411-1420`):
option names `EnableFilters`/`DisableFilters`/`ForbidFilters` parsed by
`SetOptionFromName1` into enabled/disabled/forbidden sets; C++
`Enable`/`ForceEnable`/`Disable`/`ForbidFilter` (+FilterSet,
+ByCommaSeparatedList), `ClearFilters`,
`DisableAllFilters(NotExplicitlyEnabled)`.
`AdjustFiltersByCommaSeparatedList` (`:2860-2906`): `+x` enable, `-x`
disable, bare `x` resets level to PassThrough then enables listed; empty
list disables all. `?PageSpeedFilters=` / `?ModPagespeedFilters=`
(`rewrite_query.cc:567-574`) adjust options per request (only listed
filters stay enabled). Apache directives
`ModPagespeedEnableFilters/DisableFilters/ForbidFilters` (comma-separated,
dir scope; `mod_instaweb.cc:123-132,1718-1733`); Apache default level
`SetDefaultRewriteLevel(kCoreFilters)` (`:1876`). nginx routes the same
names through `ParseAndSetOptionFromName1`/factory parser
(`ngx_rewrite_options.cc:265-278,419-427`); nginx default
`SetRewriteLevel(kCoreFilters)` (`ngx_rewrite_driver_factory.cc:146-155`).

### 2.4 Chain order (`rewrite_driver.cc`)

Start (`:1053-1086`): event listeners `FlushHtmlFilter` (if flush_html) +
`AmpDocumentFilter` (always); early pre-render: `DomStatsFilter`
(kComputeStatistics), `StripSubresourceHintsFilter` (unless preserve),
`DecodeRewrittenUrlsFilter`, `ResponsiveImageFirstFilter`, `AddHead`
(kRequiresAddHead), `BaseTag` (kAddBaseTag), `AddIds` (kAddIds).

Pre-render (`:1111-1284`): StripScripts StripLegacyPolyfills
CriticalImagesBeacon MakeShowAdsAsync CssInlineImportToLink CssOutline
GoogleFontCssInline CssMoveToHead CssCombine(cc) CssFilter(cf)
CriticalCssBeacon CriticalSelector(CriticalCss) CssInline JsOutline
GoogleAnalyticsFilter InsertGA JsCombine(jc) JavascriptFilter(jm) JsInline
ImageRewrite(ic) RemoveComments ElideAttributes CacheExtender(ce)
ImageCombine(is) LocalStorageCache(ls) CollectDependencies.

Post-render (`:1287-1412`): RewrittenContentScanning(flush+preconnect)
InsertDnsPrefetch PrioritizeLcpImages InsertAmpLink AddInstrumentation
Defer-chain (DeferIframe+JsDisable+JsDeferDisabled) FixReflow
DeterministicJs MetaTag DisableJs-chain DelayImages DedupInlinedImages
LazyloadImages (after DelayImages) SupportNoscript HandleNoscriptRedirect
RedirectOnSizeLimit Pedantic DomainRewrite (after all URL-adding filters)
UrlLeftTrim (before quote removal) RemoveQuotes CollapseWhitespace Debug
(last); `PushPreload` is oddly appended to the PRE-render list.

Order changes must also update `install/common/pagespeed.conf.template`
and config-filters docs (`:1054-1061`); debug `CheckFilterSetOrdering`
requires set arrays in enum order
(`rewrite_options.cc:932-934,1175-1184`).

### 2.5 Registration and conditional/ordering dependencies

`SetServerContext` (`:950-968`) unconditionally constructs+registers
resource filters (CssCombine, CssFilter [holds cache_extender/
image_rewriter/image_combiner ptrs], JavascriptFilter, JsCombine,
ImageRewrite, CacheExtender, ImageCombine, LocalStorageCache,
JavascriptSourceMap) + always-built DomainRewriteFilter/UrlLeftTrimFilter;
`EnableRewriteFilter(id)` (`:1460-1464`) looks up `resource_filter_map_`
and appends to the pre-render chain.

Conditional deps (`:1128-1136,1163-1171,1173-1181,1225-1231,1253-1255`):
InlineImportToLink forced by Prioritize/ComputeCriticalCss unless
forbidden; CssFilter(cf) forced by FlattenCssImports unless forbidden;
CriticalCssBeacon by PrioritizeCriticalCss+beacon-results or
ComputeCriticalCss; cf/jm/ic skipped in the HTML path under preserve-urls
unless `in_place_preemptive_rewrite_*` (inline-JS always allowed for jm).
Ordering deps (`:1137-1156,1194-1210,1234-1239`): OutlineCss skipped when
PrioritizeCriticalCss; GoogleFontCssInline before MoveCssToHead;
MoveCssToHead before CombineCss (combine needs links in head);
MakeGoogleAnalyticsAsync+InsertGA before JS rewrite; CombineJs after
minify/analytics; JsInline/CssInline after minify. ResponsiveImages
requires BOTH kResponsiveImages+kResizeImages; two-phase First (early
pre-render) + Second (post-render) (`:1088-1097`).

Verified filter→owner instantiations: AddHead/AddIds/AddInstrumentation/
BaseTag/CacheExtender/CollectDependencies/CriticalCssBeacon/
CriticalImagesBeacon/CriticalSelector/CssCombine/CssFilter/CssInline/
CssInlineImportToLink/CssMoveToHead/CssOutline/Debug/DecodeRewrittenUrls/
DedupInlinedImages/DeferIframe/DelayImages/DeterministicJs/DomainRewrite/
FixReflow/GoogleAnalytics/GoogleFontCssInline/HandleNoscriptRedirect/
ImageCombine/ImageRewrite/InsertAmpLink/InsertDnsPrefetch/InsertGA/
JavascriptFilter/JavascriptSourceMap/JsCombine/JsDeferDisabled/JsDisable/
JsInline/JsOutline/LazyloadImages/LocalStorageCache/MakeShowAdsAsync/
MetaTag/Pedantic/PrioritizeLcpImages/PushPreload/RedirectOnSizeLimit/
ResponsiveImageFirst+Second/StripScripts/StripLegacyPolyfills +
DomStats/FlushHtml/RewrittenContentScanning/SupportNoscript/UrlLeftTrim.

## 3. Directives

### 3.1 nginx: single `pagespeed` directive

`ngx_pagespeed/src/ngx_pagespeed.cc` (`ps_commands` L844-869): one nginx
directive `pagespeed`, registered 3x for http(main)/server/location+
limit_except contexts; args `NGX_CONF_TAKE1..TAKE5` (directive + up to 5
sub-args); handlers `ps_main/srv/loc_configure` map to
kProcessScopeStrict/kServerScope/kDirectoryScope; all three use
`NGX_HTTP_SRV_CONF_OFFSET`.

`ps_configure` (L951-1033): 1-arg `pagespeed on|off` toggles per-location
gzip via `NgxGzipSetter` AND falls through to options parsing; 2-arg
`pagespeed gzip on|off` is nginx-only (handled inline, never reaches
`RewriteOptions`); 2-arg `LogDir`/`FileCachePath` pre-create/chown the dir
via `ps_init_dir` then parse normally; `n_args>4` yields "not recognized
or too many arguments". No other `ngx_command` entries exist.

`NGX_PAGESPEED_MAX_ARGS=10` (`ngx_rewrite_options.h:43`) caps ScriptLine
arg arrays (`CHECK n_args<=10`); effective arity is smaller (<=5 sub-args
by `ps_commands`, <=4 parsed by `ParseAndSetOptions`).

`NgxRewriteOptions::ParseAndSetOptions0` (`ngx_rewrite_options.cc:196-211`):
bare `pagespeed <word>` accepts `on/off/true/false/unplugged/standby`;
nginx maps `off`→Unplugged; sets `enabled_` (default kEnabledOn,
kDirectoryScope).

### 3.2 nginx-only options (`ngx_rewrite_options.cc`)

`AddProperties` (L110-143): `StatisticsPath` — context server
(kServerScope), 1 arg (path), default `''`, id `nsp`.
`GlobalStatisticsPath` — http-only (kProcessScopeStrict), 1 arg, default
`''`, id `ngsp`. `ConsolePath`/`MessagesPath`/`AdminPath` — server, 1 arg,
default `''` (ids ncp/nmp/nap); `GlobalAdminPath` — http-only, 1 arg,
default `''` (id `ngap`).

`ParseAndSetOptions` (L265-465): nginx-only 1-arg `ClearInheritedScripts`
(any scope, no value); 2-arg `UseNativeFetcher on|off` (http-only,
default false) and `NativeFetcherMaxKeepaliveRequests N>0` (http-only,
default 100) set `NgxRewriteDriverFactory` fields; 2-arg
`ProcessScriptVariables off|on|all` (http-only, set-once, default kOff).
Owners: `ngx_rewrite_options.cc` + `ngx_rewrite_driver_factory.{h,cc}`.

Scope gate (L58-86, L163-194, L274-283): 20 `server_only_options`
(FetcherTimeoutMs, FetchProxy, ForceCaching, GeneratedFilePrefix,
ImgMaxRewritesAtOnce, InheritVHostConfig, InstallCrashHandler,
MessageBufferSize, NumRewriteThreads, NumExpensiveRewriteThreads,
StaticAssetPrefix, TrackOriginalContentLength, UsePerVHostStatistics,
BlockingRewriteRefererUrls, CreateSharedMemoryMetadataCache, LoadFromFile/
LoadFromFileMatch/LoadFromFileRule/LoadFromFileRuleMatch, UseNativeFetcher,
NativeFetcherMaxKeepaliveRequests) rejected at location scope; 2 main_only
(UseNativeFetcher, NativeFetcherMaxKeepaliveRequests) rejected below http
scope; leading `ModPagespeed` prefix stripped; names matched
case-insensitively.

nginx default overrides (`ngx_rewrite_driver_factory.cc:70-99,146-155`):
BeaconUrl `/ngx_pagespeed_beacon` (core: `/mod_pagespeed_beacon`),
FileCacheInodeLimit 500000 (core: 0), AvoidRenamingIntrospectiveJavascript
true, `NewRewriteOptions()` sets RewriteLevel CoreFilters (core property
default PassThrough); `default_x_header_value=kModPagespeedVersion` set in
`NgxRewriteOptions::AddProperties`.

### 3.3 Process-global factory directives

`system_rewrite_driver_factory.cc ParseAndSetOption1` (L444-538): 9
one-arg process-global directives — `StaticAssetPrefix` (string, default
`/pagespeed_static/`), `NumRewriteThreads`/`NumExpensiveRewriteThreads`
(int, default -1=autodetect), `MessageBufferSize` (int bytes, default 0=off,
range 0..kMaxCapacity), `UsePerVHostStatistics` (bool, default true),
`InstallCrashHandler` (bool false), `ForceCaching` (bool),
`ListOutstandingUrlsOnError` (bool false), `TrackOriginalContentLength`
(bool false); first group hard-errors below process scope, second group
warn+ignore. Constructor defaults L103-129.

`ParseAndSetOption2` (L540-562): `CreateSharedMemoryMetadataCache <name>
<size_kb>` (2 args, process-only else warn+ignore); `PostConfig` (L564-599)
errors if FileCachePath empty while enabled.

### 3.4 System options (`system_rewrite_options.cc AddProperties L94-354`)

~50 options, default context server (kServerScope) per `AddSystemProperty`
overload (`system_rewrite_options.h:408-415`). 1-arg strings: FileCachePath
`''`, LogDir `''`, FetchProxy `''` (secret), MemcachedServers (empty
cluster), SslCertDirectory/File `''`, SlurpDirectory `''`, TestProxySlurp
`''`, CacheFlushFilename `''`, StatisticsLoggingChartsCSS/JS `''`,
PurgeMethod `''` (explicit server). Server-scope ints/bools (1 arg):
MemcachedThreads 1, MemcachedTimeoutUs 500000, RedisServer (empty, port
6379), RedisReconnectionDelayMs 1000, RedisTimeoutUs 50000,
RedisDatabaseIndex 0, RedisTTLSec -1, SlowFileLatencyUs 50000, Statistics
true, StatisticsLogging false, StatisticsLoggingIntervalMs 600000,
StatisticsLoggingMaxFileSizeKb 1024, UseSharedMemLocking true,
FileCacheCleanIntervalMs 3600000, FileCacheSizeKb 102400,
FileCacheInodeLimit 0, LRUCacheByteLimit 0, LRUCacheKbPerProcess 0,
CacheFlushPollIntervalSec 5, CompressMetadataCache true, FetchHttps
`enable`, TestProxy false, SlurpReadOnly false, RateLimitBackgroundFetches
true, SlurpFlushLimit 0. Process-strict, http-only (1 arg): HistoryArchive
false, HistoryArchiveInterval 300, HistoryArchiveRetentionDays 7,
HistoryArchiveMaxSizeMb 100, ExperimentalCentralControllerPort `''`,
ExperimentalPopularityContestMaxInFlight 10,
ExperimentalPopularityContestMaxQueueSize 1000,
DangerPermitFetchFromUnknownHosts false, StaticAssetCDN `''`,
ShmMetadataCacheCheckpointIntervalSec 300; legacy-process: FetchWithGzip
false, IproMaxResponseBytes 10MB, IproMaxConcurrentRecordings 10,
DefaultSharedMemoryCacheKB 50MB.

`ParseAndSetOptionFromName2` (L503-538): 6 two-arg directives
`StatisticsDomains/GlobalStatisticsDomains/MessagesDomains/ConsoleDomains/
AdminDomains/GlobalAdminDomains`, args `allow|disallow <wildcard>`
(repeatable, merged).

### 3.5 Core options (`rewrite_options.cc AddProperties L1220-2360`)

~160 `AddBaseProperty` registrations (grep counts 165 incl.
`AddProperty`) = core 1-arg typed-property directives (bool
on/off/true/false, int/int64, string, enums via `SetFromString`); scope
enum `public/rewrite_options.h:649-661` ordered
kQueryScope<kDirectoryScope<kServerScope<kLegacyProcessScope<
kProcessScopeStrict; `PropertyBase` ctor default scope kDirectoryScope
(L717-725). Full per-directive name/type/default/scope table is a gap
(config-gap-1); help text lives inline in `AddProperties` (config-1).

`ParseAndSetOptionFromNameWithScope` (L3223-3321): 13 special 1-arg
directives bypassing the property table — Allow, Disallow, Domain,
ProxySuffix, EnableFilters/DisableFilters/ForbidFilters (comma-separated
filter names), ExperimentVariable (int 1-5), ExperimentSpec, RetainComment,
BlockingRewriteRefererUrls, PermitIdsForCssCombining,
DownstreamCachePurgeLocationPrefix (must be valid URL).
`ParseAndSetOptionFromName2` (L3323-3377): 2-arg AddResourceHeader,
CustomFetchHeader, LoadFromFile, LoadFromFileMatch, LoadFromFileRule/
LoadFromFileRuleMatch (`Allow|Disallow <rule>`), MapOriginDomain,
MapProxyDomain, MapRewriteDomain, ShardDomain; `...FromName3`
(L3379-3417): 3-arg UrlValuedAttribute `<tag> <attr> <Category>`, Library
`<bytes> <md5> <url>`, MapOriginDomain/MapProxyDomain with 3rd host-header
arg. Name constants L295-369.

Core key defaults (`:417-549` + AddProperties): RewriteLevel PassThrough;
BeaconUrl `/mod_pagespeed_beacon`; CssInlineMaxBytes 2048,
CssFlattenMaxBytes 1024000, ImageInlineMaxBytes 3072, JsInlineMaxBytes
2048, ImageRecompressionQuality 85, JpegQuality -1 (fallback), WebpQuality
80, RewriteDeadlineMs 10(NDEBUG)/20, ImplicitCacheTtlMs 5min,
ImageMaxRewritesAtOnce 8 (kLegacyProcessScope), MaxHtmlParseBytes -1,
MaxCacheableResponseContentLength 16MB, InPlaceResourceOptimization true,
ModifyCachingHeaders true, XHeaderValue `''`.

Deprecated-but-accepted (`:2302-2310`, `:988-993`): MaxPrefetchJsElements,
DistributeFetches, DistributedRewriteKey, DistributedRewriteServers,
DistributedRewriteTimeoutMs; rename
ImageWebpRecompressionQuality→WebpRecompressionQuality (+ForSmallScreens).
Apache-side GeneratedFilePrefix/InheritVHostConfig are ApacheConfig
deprecated and ImgMaxRewritesAtOnce is Apache-only (`mod_instaweb.cc:181-182,
1441`) — in nginx they pass the scope gate but resolve to "not recognized"
(inferred from factory known-name lists + rename map).

### 3.6 Negative results and fixtures

`AddDirective` has zero matches in `ngx_pagespeed/src`,
`ngx_pagespeed/test`, `mod_pagespeed/net`, `mod_pagespeed/pagespeed`;
`config_manager`/`ConfigManager` likewise zero. Real APIs:
`AddProperty`/`AddBaseProperty`/`AddSystemProperty`/`add_ngx_option`/
`AddDeprecatedProperty`; `ParseAndSetOptionFromName{1,2,3}` + factory
`ParseAndSetOption{1,2}`. Closest "manager" is
`net/instaweb/config/rewrite_options_manager.{h,cc}` (request-time
`GetRewriteOptions`/`PrepareRequest`, not config-file parsing).

Fixtures: `net/instaweb/rewriter/rewrite_options_test.cc`
(`RewriteOptionsTest` gtest fixture), `pagespeed/system/
system_rewrite_options_test.cc`,
`net/instaweb/config/rewrite_options_manager_test.cc`;
`ngx_pagespeed/test/pagespeed_test.conf.template` exercises ~738
`pagespeed` lines (ProcessScriptVariables all, *Path, StaticAssetPrefix,
MessageBufferSize, FetcherTimeoutMs, NativeFetcherMaxKeepaliveRequests,
CreateSharedMemoryMetadataCache, …) driven by
`test/nginx_system_test.sh`.

## 4. Dependencies

### 4.1 mod_pagespeed submodules (`.gitmodules`, 30+ pins)

| Submodule | Pin | Version | Purpose |
|---|---|---|---|
| `third_party/apr/src` | e461da5 | trunk | pools, threads, file IO, DSO; portable runtime under PSOL |
| `third_party/aprutil/src` | c9a9a77 | 1.7.x | buckets/brigades, memcache client; HTTP buffering + memcached backend |
| `third_party/httpd/src` + `httpd24/src` | 493b437 + f24c7c6 | Apache 2.2/2.4 | Apache module build headers |
| `third_party/serf/src` | 95cf754 | 1.3.9 | async HTTP client; serf URL fetcher, `-DSERF_HTTPS_FETCHING=1` |
| `third_party/brotli/src` | 882f418 | v0.4.0-89 | brotli encoder/decoder; compression filter |
| `third_party/zlib/src` | cacf7f1 | v1.2.11 | deflate/gzip; HTTP compression + gzip filter |
| `third_party/libpng/src` | b78804f | 1.6.31-master-169 | PNG codec; image rewriting |
| `third_party/libjpeg_turbo/src` + patched-yasm | 7260e4d + 7da28c6 | — | SIMD JPEG codec; image rewriting |
| `third_party/libwebp` | 6b7a95f | v0.6.1 | WebP codec; convert_jpeg_to_webp / image filters |
| `third_party/giflib/src` | c9a54af | 5.1.4 | GIF codec; image rewriting |
| `third_party/optipng` | e9a5bd6 | — | PNG optimizer; recompress_png filter |
| `third_party/re2/src` | e2deff3 | 2017-03-01 | regex engine; URL/pattern matching |
| `third_party/protobuf/src` | e8ae137 | v3.0.0-beta-4-74 | serialization; logging proto, config protos |
| `third_party/grpc/src` | b45b83c | SHA only | gRPC C core (vendored); experimental RPC |
| `third_party/boringssl/src` | 07e7806 | SHA only | TLS stack (vendored); crypto/TLS where enabled |
| `third_party/hiredis/src` | 0107560 | v0.13.3 | Redis client; redis cache backend |
| `third_party/jsoncpp/src` | 7165f6a | 1.0.0 | JSON parser; beacon/config JSON |
| `third_party/google-sparsehash/src` | 6ff8809 | sparsehash-1.7 | dense_hash_map/set; in-memory hash tables |
| `third_party/chromium/src/{base,build,googleurl}` | ccf3c2f/06b7bd9/405b6e1 | — | base primitives, build flags, URL parsing |
| `third_party/icu` | 46a834e | fork | Unicode; i18n/charset handling |
| `third_party/gflags/{arch,src}` | cc7e9a4/e7390f9 | SHA only | CLI flags; test binaries + tools |
| `third_party/domain_registry_provider` | f353346 | — | C++ public-suffix-list interface; domain checks |
| `third_party/modp_b64` + in-tree `third_party/base64` | aae6075 | — | base64 codecs; data-URI/inline encoding |
| `third_party/css_parser` | in-tree | gperf-generated | CSS parser; CSS rewriting filters |
| `third_party/redis-crc` + `rdestl` | in-tree | — | redis-cluster CRC + RDESTL containers |
| `third_party/nghttp2` + `mod_fcgid` | f4474d5 + c63f176 | v1.16.0-24 | dev-Apache HTTP/2 + FastCGI (dev/test only) |
| `testing/gtest` + `testing/gmock` | c994585 + c440c8f | release-1.7.0 | unit-test frameworks |

Closure: `third_party/closure/download.sh` pins Closure Compiler
VERSION=20191111 (override `CLOSURE_COMPILER_VERSION`, dl.google.com +
Maven fallback); JS minification via `closure.gypi` (ADVANCED opt / SIMPLE
dbg); `third_party/closure_library` @cd0e794 (v20161024) holds beacon/admin
JS sources (must match compiler version per download.sh).

### 4.2 Bazel/Envoy track (`bazel/repositories.bzl`)

envoy@9ae348c (2022-11-05), brotli@d6d9895, hiredis@0.14.1, jsoncpp@1.9.3,
libpng@1.6.37, libwebp@1.1.0, sparsehash@6ff8809, glog@0a2e593,
gflags@f7388c6, drp@21a7a0f, giflib@5.2.1, optipng@0.7.7,
libjpeg_turbo@ab7cd97 (sha empty), apr@901ece0, aprutil@13ed779,
serf@3a37fc1, closure-library@20191111; hermetic Bazel builds + Envoy
integration. Toolchain: `.bazelversion=6.0.0rc1`, Bazel via bazelisk,
C++17 on linux, `-fPIC`, `-DPAGESPEED_SUPPORT_POSIX_SHARED_MEM=1`,
ASan/TSan/MSan configs. gyp+make track is the PSOL production build.

### 4.3 Toolchains and build scripts

- `build/common.gypi`: `-std=gnu++0x`, `-D_GLIBCXX_USE_CXX11_ABI=0`,
  `-fexceptions`, `-fasynchronous-unwind-tables`, `-D_FORTIFY_SOURCE=2`,
  `-frtti`, `-static-libstdc++`/`-static-libgcc` when
  `use_system_libs=0`, gcc>=4.8 required, `linux_fpic=1`.
- `build/gyp_chromium` + `tools/gyp` @0fb3129: Python-2 GYP generator
  (Chromium rev-256281 wrapper); generates the top-level `Makefile`
  (`BUILDTYPE`=Release/Debug).
- `install/build_psol.sh`: runs gyp, `make mod_pagespeed_test` +
  `pagespeed_automatic_test`, `make pagespeed/automatic` with
  `CXXFLAGS=-DSERF_HTTPS_FETCHING=1` → `pagespeed_automatic.a` +
  `js_minify` → `psol/lib/{Release,Debug}/linux/{x64,ia32}` + rsynced
  `psol/include` headers → psol tarball.
- `install/build_mps.sh`: submodule update, gyp, `make mod_pagespeed`
  (+tests, `redis_cache_cluster_setup`) → optional
  `linux_package_{deb,rpm}_{beta,stable}` packaging targets.
- Ubuntu prereqs (`install/ubuntu/install_required_packages.sh`):
  subversion apache2 g++ gperf devscripts fakeroot git-core zlib1g-dev wget
  curl net-tools rsync ssl-cert psmisc libexpat1-dev; dev adds memcached
  autoconf valgrind libev-dev libssl-dev libpcre3-dev uuid-dev pkg-config
  openjdk-8-jre redis-server libtool php.
- CentOS prereqs: subversion httpd gcc-c++ gperf make rpm-build glibc-devel
  curl-devel expat-devel openssl-devel zlib-devel libevent-devel rsync; dev
  adds php autoconf libtool valgrind pcre-devel libuuid-devel memcached;
  devtoolset-2 gcc on EL5/6.
- `install/shell_utils.sh` from-source pins: git 2.0.4, wget 1.12,
  memcached 1.4.20, python 2.7.8, redis 3.2.4 (all with sha256).
- `.travis.yml`: clang++-14, apt pkgs (pkg-config zip g++ zlib1g-dev unzip
  python3 ninja-build cmake gperf memcached apache2-dev python2 npm),
  bazelisk + LLVM 14, memcached+redis services; runs
  `bazel test //test/...`.
- Alpine Dockerfiles: apk pkgs apache2-dev apr-dev apr-util-dev build-base
  curl gettext-dev git gperf icu-dev libjpeg-turbo-dev libpng-dev
  libressl-dev pcre-dev zlib-dev; PSOL built with `-Duse_system_libs=1`,
  `CFLAGS`/`CXXFLAGS` `-I/usr/include/apr-1 -DUCHAR_TYPE=uint16_t`;
  nginx 1.15.6 link line `-lapr-1 -laprutil-1 -licudata -licuuc -lpng
  -lturbojpeg -ljpeg`.

### 4.4 ngx_pagespeed / ngx_psol / ngx_compiler deps

- `ngx_pagespeed/config` (nginx `./configure` addon): requires gcc>=4.8 or
  clang>=3.3, injects `-D_GLIBCXX_USE_CXX11_ABI=0 --std=c++11`, links
  `pagespeed_automatic.a -lstdc++ -lrt -pthread -lm -luuid`; `config.make`
  appends `--std=c++11` to each module object; `cpp_feature` = C++ autoconf
  probe.
- `scripts/build_ngx_pagespeed.sh`: installer handling nginx download/
  PGP-verify, deps check (gcc>=4.8), PSOL download-vs-from-source, `--devel`
  mode (Debug PSOL + git checkouts).
- `scripts/build_from_scratch_with_fixes.sh`: inits 7 submodules, rewrites
  legacy `git://` URLs to https, patches avifenc `--quiet`/gRPC
  gettid/APR sys_siglist/BoringSSL+serf GNU-stack asm, bootstraps local
  Python 2.7.18 (OpenSSL-3/_hashlib/sigaction patches) + gperf 3.1 from
  ftp.gnu.org, then `install/build_psol.sh --skip_deps --skip_tests`; env
  `JOBS`/`BUILD_JS`/`CLOSURE_COMPILER_VERSION`/`CLOSURE_DOWNLOAD_BASE`.
- `.gitmodules` + submodule pins: mod_pagespeed@a863f1d (v1.14.36.1~31),
  nginx@f8e1bc5 (release-1.29.5-2), ngx_cache_purge@331fe43 (2.3),
  set-misc@fdad936 (v0.33-10), ngx_devel_kit@bd44d16 (v0.3.2-5),
  headers-more@21066f7 (v0.41), ngx_psol@301dad9, ngx_brotli@53e0fb5
  (v1.0.0rc-10), ModSecurity-nginx@28bfa42 (v1.0.4-27); test/devel matrix
  (nginx, cache-purge, devel-kit, set-misc trees not checked out in this
  workspace — deps-2).
- `test/run_tests.sh`: system-test driver over serf fetcher (default) and
  native fetcher (opt-in), valgrind opt-in, needs mod_pagespeed checkout +
  nginx binary + PHP.
- `psol/` mirror: prebuilt-style `include/` (`net/instaweb`, `pagespeed`,
  `third_party`, `url`, …) + `lib/` tree consumed by `config` when
  `MOD_PAGESPEED_DIR` unset; `PSOL_BINARY_URL` is dev-branch stub.
- ngx_psol: mod_pagespeed submodule @0fdb3545f (master);
  `include_history.txt`: headers copied from incubator-pagespeed-mod@
  a863f1d on 2026-02-13. `lib/Release/linux/x64/pagespeed_automatic.a`
  (ar archive) + `pagespeed_js_minify` (ELF x86-64 pie, not stripped):
  the linked PSOL artifacts; `local-lib/libuuid.so` →
  `/usr/lib/x86_64-linux-gnu/libuuid.so.1` satisfies `-luuid`. Vendored
  toolchain: `python2/bin/python2.7` verified 2.7.18 (needs
  `LD_LIBRARY_PATH=python2/lib`), `gperf/bin/gperf` verified 3.1;
  `tools/` holds Python-2.7.18.tgz + gperf-3.1.tar.gz + plucky fixes;
  `psol.tar.gz` = 5884-entry 20MB header/lib bundle.
- ngx_compiler `bootstrap.sh`: gates Ubuntu 24.04 noble amd64,
  apt-installs build-essential g++ git apache2 autoconf
  default-jre-headless debhelper devscripts gperf libev-dev libexpat1-dev
  libmodsecurity-dev libpcre3-dev libssl-dev libtool memcached pkg-config
  uuid-dev valgrind zlib1g-dev etc, pins nginx.org signing key fingerprint
  `573BFD6B3D8FBC641079A6ABABF5BD827BD9BF62`, adds nginx-mainline deb-src,
  clones mod_pagespeed @`NGX_MPS_REF`.
- `build_from_source_to_deb.sh`: `apt-get build-dep nginx` + `apt-get
  source nginx` (unpinned mainline — deps-3), builds ModSecurity-nginx
  connector + PSOL (@`NGX_PAGESPEED_REF`/`NGX_MPS_REF`/
  `NGX_MODSECURITY_NGINX_REF`, default master), injects
  `--add-dynamic-module` for both + `--build=pagespeed-mps-<sha>-ngx-<sha>`
  into `debian/rules`, asserts `ngx_pagespeed.so` +
  `ngx_http_modsecurity_module.so` ship in version-stamped debs, writes
  `artifacts/nginx_version`.
- `tools/assert-binary-hardening.sh` + snippet: requires readelf-verified
  full RELRO (`-Wl,-z,relro`), BIND_NOW (`-Wl,-z,now`), stack canary
  (`-fstack-protector-strong`), FORTIFY (`_FORTIFY_SOURCE=2`) on the
  unstripped `.so`; admin snippet restricts `/pagespeed_admin*` to
  localhost.

## 5. Caching

### 5.1 Resource URLs and key generation (owners)

- `net/instaweb/rewriter/resource_namer.cc:37-56,199-239`: resource URL
  format `ORIGINAL_NAME.pagespeed[.EXPT|.options].ID.HASH[.SIG].EXT`;
  key-gen/parse functions `ResourceNamer::Encode()`, `::Decode()`,
  `::EncodeIdName()` (id.name), `::EventualSize()`.
- URL examples (`resource_namer_test.cc:58-78`):
  `name.ext.as.many.as.I.like.pagespeed.id.hash.ext`,
  `...pagespeed.q.id.hash.ext` (experiment),
  `...pagespeed.a=b.id.hash.ext` (options),
  `name.ext.pagespeed.options,_with,_slashes.id.hash.ext` (slash-escaped
  options). Cache-extend example (`cache_extender.cc:346-349`):
  `a.com/,hb.com,_evil.html.pagespeed.ce.html` (`ce` = cache-extender
  filter id).
- `pagespeed/kernel/base/hasher.cc:37-53`: key-gen `Hasher::Hash()` =
  Web64Encode(RawHash) truncated to `HashSizeInChars()`;
  `HashToUint64()` from first 64 bits. `md5_hasher.h:32-35`: `MD5Hasher`
  default hash 10 chars; ctor takes hash size.
- `net/instaweb/rewriter/public/rewrite_options.h:637-641`: kHashBytes=20
  (signature hashing), kCachePurgeBytes=25000 (URL invalidation set
  capacity).
- `output_resource.cc:137-145`: `OutputResource::EndWrite` sets URL hash
  via `server_context_->hasher()->Hash(contents)` + `ComputeSignature()`;
  `:159-165` `name_key()`=resolved_base+`EncodeIdName()`; `:180-197`
  `HttpCacheKey()`=unsharded `UrlNamer::Encode` + domain-lawyer mapping.
- `rewrite_driver.cc:2736-2754`: `RewriteDriver::PopulateResourceNamer
  (filter_id,name)` fills id/name/experiment/options (never options for
  in-place id).
- `public/resource.h:207-209`: `Resource::cache_key()` defaults to
  `url()`, overridable (e.g. UA-dependent).
- `rewrite_context.cc:1271-1275,1308-1379`: metadata partition key
  `rname/<id>_<sig>/<url_key>@<suffix>` via
  `RewriteContext::SetPartitionKey()`; multi-URL/data-URL keys via
  `HashSplit()`=hash[0]/hash[1:]; single-slot suffix =
  `encoding@UserAgentCacheKey_suffix`. `public/rewrite_context.h:478-490`:
  `CacheKeySuffix()` and `UserAgentCacheKey()` extension points, both
  default empty.
- `net/instaweb/http/public/http_cache.h:362-367`: `HTTPCache::CompositeKey`
  = version/fragment/key (fragment=Host header anti-poisoning shard, may be
  shared explicitly). `http_cache.cc:53,97-99`: HTTP cache version prefix
  `v5/` (`SetVersion` bumps invalidate all).
- `server_context.cc:218`: metadata name→hash mapping prefix `rname/`
  (cache-bust by changing prefix); `:198-200`
  kGeneratedMaxAgeMs=kYearMs, kCacheTtlForMismatchedContentMs=5min.
- `server_context.cc:366-380`: generated resources served
  `SetDateAndCaching(now,1yr)`+ETag `W/0`; `:411-471`
  `ApplyInputCacheControl` clamps max-age to min(input cache_ttl),
  downgrades to private/no-cache.

### 5.2 Cache backends and adapters (owners)

- `pagespeed/kernel/base/cache_interface.h:132-153`: `CacheInterface` API
  Get/Put/Delete/MultiGet + `MustEncodeKeyInValueOnPut`/
  `PutWithKeyInValue` for key-size-limited backends.
- `pagespeed/kernel/cache/file_cache.h:42-66` + `file_cache.cc:209-225`:
  `FileCache` backend; `FileCache::EncodeFilename` via
  `UrlToFilenameEncoder::EncodeSegment`, hash-named fallback when over
  `path_length_limit_`; async `Clean()` by size/inode targets.
- `pagespeed/kernel/util/url_to_filename_encoder.h:43-71`: filename
  escaping (`,` escape char, trailing `,`, 128-char segment split);
  example `/u?foo=bar` → `/u,3Ffoo=bar,`.
- `pagespeed/kernel/cache/lru_cache.h:46-49`: `LRUCache` in-memory backend
  (not thread-safe, shadow for memcached).
- `pagespeed/kernel/sharedmem/shared_mem_cache.h:46-63`:
  `SharedMemCache<kBlockSize>` shm metadata cache, `Initialize()`/`Attach()`
  root/child model.
- `pagespeed/system/apr_mem_cache.h:46-61`: `AprMemCache` memcached backend;
  kValueSizeThreshold=1MB (overflow to FallbackCache file cache).
  `apr_mem_cache.cc:264-311`: memcached key = `hasher_->Hash(key)`, value =
  `key_value_codec::Encode(key,value)` (key embedded in value).
- `pagespeed/kernel/cache/key_value_codec.h:32-50`:
  `key_value_codec::Encode/Decode` (65535 max key).
- `pagespeed/system/redis_cache.h:77-85` + `redis_cache.cc:184-197`:
  `RedisCache` backend; Put uses SET or SETEX when ttl_sec set (default -1
  = no TTL).
- `pagespeed/system/system_caches.h:49-62,172-202`: `SystemCaches` wires
  LRU/File/Memcached/Redis/shm; external caches wrapped
  AsyncCache+CacheBatcher+CacheStats (async + blocking interfaces).
- Adapters: `CacheKeyPrepender` (`cache_key_prepender.h:31-56`,
  prefix-namespaces keys, test isolation); `FallbackCache`
  (`fallback_cache.h:35-50`, splits small/large objects across two caches by
  threshold); `CompressedCache` (`compressed_cache.h:35-48`, gzip adapter,
  kDefaultMaxInflatedValueSize=16MB corruption cap).
- Default backend-chain selection order (which backs metadata vs HTTP cache
  vs HTML cache when memcached/redis absent) is a gap (caching-gap-2).
- HTML cache (`ProxyFetch::HtmlCacheLookupComplete`,
  `pagespeed/automatic/proxy_fetch.cc:1610` + `html_cache_*` stats) has no
  inventoried key/TTL/backend/invalidation entries (caching-gap-1).

### 5.3 TTLs, validation, purge

- `pagespeed/kernel/base/timer.h:35-45`: kSecondMs=1000, kMinuteMs, kHourMs,
  kDayMs, kMonthMs=31d, kYearMs=365d.
- `rewrite_options.cc:437-441`: TTL defaults
  kDefaultImplicitCacheTtlMs=5min, kDefaultLoadFromFileCacheTtlMs=5min,
  kDefaultMetadataInputErrorsCacheTtlMs=5min.
- `rewrite_context.cc:1382-1398`: metadata recheck TTL =
  max(implicit_ttl, input expiry−now); 4xx uses
  metadata_input_errors_cache_ttl; else failure_caching_ttl.
- `pagespeed/kernel/http/response_headers.h:110,161-187,222-233` +
  `response_headers.cc:273-277`: TTL API `ComputeCaching()`/
  `cache_ttl_ms()`/`CacheExpirationTimeMs()`/`SetCacheControlMaxAge()`/
  `implicit_cache_ttl_ms()`.
- `http_cache_failure.cc:40-70`: failure-remember TTLs: default 300s,
  dropped-fetch 10s, per-FetchResponseStatus table.
  `public/http_cache.h:287-332`: `HTTPCache::RememberFailure` +
  `failure_caching_ttl_sec`/`set_failure_caching_ttl_sec`;
  `HTTPCacheCallback::OverrideCacheTtlMs` hook.
- `rewrite_driver.cc:3060-3082`:
  `OptionsAwareHTTPCacheCallback::IsCacheValid` = has_date &&
  `IsUrlCacheValid(url,date,wildcards)`; `OverrideCacheTtlMs` via
  `IsCacheTtlOverridden`.
- `cache_extender.cc:124-170`: cache extension: rewrite iff
  (expiry−now)<kMinThresholdMs(=kMonthMs, `:65`) or proxy/domain change;
  output served with long public caching.
- `pagespeed/kernel/cache/purge_context.h:99-137`: `PurgeContext`
  file-backed purge: `AddPurgeUrl` / `AddPurgeUrlWithoutEviction` /
  `SetCachePurgeGlobalTimestampMs` + `PollFileSystem` (5s stat).
- `pagespeed/kernel/cache/purge_set.h:75-105`: `PurgeSet` bounded LRU of
  (url,timestamp); eviction bumps global timestamp; `IsValid(key,timestamp)`.
- `pagespeed/system/system_cache_path.cc:73-100` + `system_cache_path.h:95-99`:
  purge file `cache.purge` (EnableCachePurge) else `cache.flush`; legacy
  touch-to-flush vs purge; `FlushCacheIfNecessary()`→`PollFileSystem`.
- `pagespeed/system/admin_site.h:219-230` + `admin_site.cc:1636-1716,
  2116-2132`: admin `cache?purge=<url|*>` endpoint; url ending `*` =
  whole-cache purge; per-vhost console scoped to own host; `Purge()`→
  `SetCachePurgeGlobalTimestampMs` or `AddPurgeUrl[WithoutEviction]`.
- `rewrite_options.cc:5277-5312`: `RewriteOptions::IsUrlCacheValid
  (url,time,wildcards)` = `PurgeSet::IsValid` + legacy wildcard
  invalidation entries.
- `input_info_utils.cc:125-156`: metadata validation `IsInputValid`:
  CACHED inputs checked via `IsUrlCacheValid` (no wildcards) + expiry +
  metadata staleness threshold.
- `public/downstream_cache_purger.h:30-56`: `DownstreamCachePurger` issues
  downstream purge fetch (default method PURGE per `rewrite_options.cc:550`).
- nginx: `ngx_pagespeed.cc:1988-1992,2558,3217`: PURGE-method routing
  (kCachePurge) when enable_cache_purge+purge_method match;
  `FlushCacheIfNecessary` per request cycle; FileCachePath mandatory
  (`:1281-1294`).

Rust scaffold state: `pagespeed-rust/crates/ps-cache/src/lib.rs:1-42` has
only a `MemoryCache` HashMap put/get (no TTL, hashing, URL naming, purge,
or disk/remote backends yet).

## 6. nginx hooks

File owner for all hooks below: `ngx_pagespeed/src/ngx_pagespeed.cc`
unless noted.

### 6.1 Module and conf hooks

- `ngx_module_t ngx_pagespeed` (`:3696-3708`, `NGX_HTTP_MODULE`):
  init_master=`ps_init_module:3479`, init_process=`ps_init_child_process:
  3635`, exit_process=`ps_exit_child_process:3624`; no init_thread/
  exit_thread/master hooks.
- `ngx_http_module_t ps_module` (`:3464-3475`): preconfiguration=
  `ps_pre_init:3390`, postconfiguration=`ps_init:3397`, create_main/srv/
  loc_conf=`ps_create_main_conf:1145`/`ps_create_srv_conf:1172`/
  `ps_create_loc_conf:1183`, merge_srv/loc_conf=`ps_merge_srv_conf:1235`/
  `ps_merge_loc_conf:1301`.
- Second `ngx_module_t ngx_pagespeed_etag_filter` (`:3681-3694,3453-3462`)
  with postconfiguration=`ps_etag_filter_init:3379` only; installs
  `ps_etag_header_filter:2483` at top of header-filter chain.
- `ps_commands` registers `pagespeed` at MAIN+SRV+LOC(+LIF) scope, up to 5
  args, handlers `ps_main_configure:1035` (kProcessScopeStrict) /
  `ps_srv_configure:1042` (kServerScope) / `ps_loc_configure:1049`
  (kDirectoryScope) — see §3.1.

### 6.2 Phase hooks

- `NGX_HTTP_POST_READ_PHASE` → `ps_self_fetch_post_read_handler`
  (`:3441-3447,1833-1857`): main-request + ModPagespeedSubrequest UA +
  peer-is-this-server check, relabels connection sockaddr to 224.0.0.0
  with `ngx_pool_cleanup_add` restore (`ps_self_fetch_restore`); returns
  `NGX_DECLINED` always.
- PREACCESS_PHASE (nginx<1.13.4) or PRECONTENT_PHASE (>=1.13.4) →
  `ps_preaccess_handler` (`:3423-3437,3346-3377`); rewrites
  `cmcf->phase_engine.handlers` in place to insert checker=
  `ps_phase_handler:3252` before try_files/content phase; runs on the
  nginx event-loop thread.
- `ps_phase_handler` (`:3252-3272`, phase checker, not handler slot) calls
  `ps_content_handler:3200` synchronously; `NGX_DECLINED` advances phase
  (phase_handler++, run_phases, `NGX_AGAIN`), else
  `ngx_http_finalize_request(r,rc)`; contract: content handler must decide
  synchronously, never async `NGX_DONE` here.
- `ps_content_handler` (`:3200-3250`) dispatches `ps_route_request:1927`
  categories: kBeacon→`ps_beacon_handler:3172`,
  kStaticContent/kMessages→`ps_simple_handler:2914`,
  kStatistics/kAdmin/kCachePurge/kResource→`ps_resource_handler:2009`;
  declines when disabled/uninitialized/error/subrequest; calls
  `FlushCacheIfNecessary` on event loop.
- `ps_resource_handler` (`:2009-2344`, resource/ipro/proxy/admin/
  statistics path) creates `ps_request_ctx_t` + `NgxBaseFetch` via
  `ps_create_base_fetch:1872`, registers `ngx_http_cleanup_add`→
  `ps_release_request_context:1893`, starts PSOL ResourceFetch/AdminPage/
  ProxyFetch, then suspends with `ps_async_wait_response:1514` (`r->count++`,
  read=`ngx_http_test_reading`, write=empty, buffered=1, `NGX_DONE`).
- `ps_async_wait_response` (`:1514-1530`) suspends the request without
  adding its own timer (relies on nginx timer); connection-close/RST aborts
  via `ngx_http_test_reading` and runs cleanup handlers instead of
  resuming.
- `ps_decline_request` (`:1488-1512`) tears down IPRO lookup (driver
  Cleanup, release base fetch, buffered=0) then resumes the phase engine
  via `ngx_http_core_run_phases` + `ngx_http_run_posted_requests`,
  returns `NGX_DONE`.

### 6.3 Filters (load-bearing init order)

Init order in `ps_init` (`:3413-3418`): in_place → fix_headers →
base_fetch → html_rewrite (each prepends to `ngx_http_top_*_filter`, so
html_rewrite ends outermost).

- `ps_base_fetch_filter` (top body filter, `:261-285`) +
  `ps_base_fetch_handler` (`:290-411,413-417`, event-loop completion pump:
  CollectHeaders→`ngx_http_next_header_filter`, CollectAccumulatedWrites→
  next body filter); init saves top filters and installs itself; handler
  runs on event loop via `NgxEventConnection` byte.
- `ps_html_rewrite_header_filter` (`:2544-2664`, header filter,
  html_rewrite path): skips subrequests/err_status/non-HTML, calls
  `ps_resource_handler(html_rewrite=true)`, strips Content-Length/
  Accept-Ranges/Vary, handles gzip/deflate inflater setup, sets
  `filter_need_in_memory=1` + buffered=1, returns `NGX_AGAIN`.
- `ps_html_rewrite_body_filter` (`:2667-2709`) forwards buffers to PSOL
  via `ps_send_to_pagespeed:2348` (Write/Flush/Done on proxy_fetch) and
  passes NULL down chain; init chains top header+body filters.
- IPRO filters (`:2723-2877`): `ps_in_place_check_header_filter` (hit→pass
  through; miss kNotInCacheStatus→create `InPlaceResourceRecorder` +
  `filter_need_in_memory=1` + `ps_decline_request` to backend;
  error→decline) and `ps_in_place_body_filter` (Write/Flush/
  `DoneAndSetHeaders` on recorder, requires `ngx_buf_in_memory`).
- fix_headers filter (`:3295-3338`): `ps_html_rewrite_fix_headers_filter`
  strips RequestOptionOverride token on every main response, forces
  no-cache Cache-Control + clears etag/last-modified/expires for rewritten
  HTML unless kPreserveAllCachingHeaders.
- `ps_etag_header_filter` (`:2483-2536,3379-3387`) renames internal
  `@psol-etag` header to ETag, clears gzip_vary when psol_vary_accept_only,
  appends s-maxage via `ApplySMaxAge` only when no Authorization header;
  installed by the separate etag-filter module postconfiguration.

### 6.4 Per-request state and output path

- `ps_request_ctx_t` (`ngx_pagespeed.h:83-116`): per-request context
  (base_fetch, proxy_fetch, inflater_, driver, recorder,
  ipro_response_headers, url_string, caching flags); stored via
  `ngx_http_set_ctx`/`ngx_http_get_module_ctx` (`ngx_pagespeed.cc:220,
  2160`); freed by `ps_release_request_context:1893` (proxy_fetch
  Done(false), driver Cleanup, recorder DoneAndSetHeaders, base fetch
  Detach).
- `ps_set_buffered` (`:225-233,89`) toggles `r->buffered` with
  `NGX_HTTP_PAGESPEED_BUFFERED(0x08)` so nginx installs writer while PSOL
  work is outstanding; cleared on completion/error/decline.
- `ps_discard_request_body` (`:245-252,353-358/3001`):
  `ngx_http_discard_request_body`; on failure keepalive=0 +
  lingering_close=1; called before module-generated responses; skipped for
  IPRO-lookup miss and HTML transform (upstream owns body).
- Beacon path (`:3172-3192,3150-3166,3039-3134`): GET reads query string
  synchronously (`NGX_HTTP_NO_CONTENT`); POST calls
  `ngx_http_read_client_request_body(r,ps_beacon_body_handler)` async,
  then `ps_beacon_body_handler` finalizes `NGX_HTTP_NO_CONTENT`/ERROR;
  temp-file bodies read via `ngx_read_file` through `NgxReadRequestBody`
  with size cap.
- Output path (`:132-216,155/186,296-310`): allocates only from request
  pool (`ngx_calloc_buf`/`ngx_alloc_chain_link`/`ngx_palloc`/
  `ngx_pagesize` chunks, temporary=1, last_buf/flush flags) via
  `string_piece_to_buffer_chain`; `NgxBaseFetch::CopyBufferToNginx:286`
  runs on event loop under mutex; header copy via
  `copy_response_headers_to_ngx` sets nginx shortcut pointers
  (content_length/etag/location/etc.).
- `NewRequestContext` (`ngx_server_context.cc:51-94`,
  `ngx_pagespeed.cc:1260-1268`): reads `r->connection->local_sockaddr`
  (IPv4/IPv6 port), `ngx_connection_local_sockaddr` IP, and indexed `http2`
  variable (`ngx_http_get_variable_index` at merge,
  `ngx_http_get_indexed_variable` per request) to set using_http2;
  event-loop, non-blocking.
- Request URL built on event loop (`:1365-1461`) from Host header
  (IPv6-aware port parse) or local sockaddr fallback + scheme from
  `r->connection->ssl` (`NGX_HTTP_SSL`) + `r->unparsed_uri`;
  X-Forwarded-Proto override in `ps_apply_x_forwarded_proto:1775` when
  respect_x_forwarded_proto.
- IPRO agreement gate (`:1472-1485,2302-2305`): `ps_in_place_url_agrees`
  compares normalized `r->uri` + host vs parsed URL path/host with
  `cscf->merge_slashes`; disagreement leaves the request fully to nginx
  (no lookup, no recording).
- Self-fetch relabel (`ngx_self_fetch_label.h:50-72`,
  `ngx_self_fetch_label.cc:49-85`): swaps connection→sockaddr/addr_text to
  224.0.0.0 label (`NgxRelabelSelfFetch`) so loopback-originated internal
  fetches get no localhost allow-rule privileges; peer match = loopback OR
  peer==local; restored by pool cleanup.
- Script-variables path (`:939-947,1011-1029`,
  `ngx_rewrite_options.cc:469-528`): `ps_configure` registers `$ps_dollar`
  var (`ngx_http_add_variable` + get_handler=`ps_dollar`) when enabled;
  per-request `ExecuteScriptVariables` calls `ngx_http_script_run(r,...)`
  on event loop then `ParseAndSetOptions`; on script error forces
  enabled=off.

### 6.5 Threading bridge (event-loop-safe offload)

- `NgxBaseFetch` (`ngx_base_fetch.h:48-192`,
  `ngx_base_fetch.cc:51-78`): `AsyncFetch`, refcount starts at 2 (PSOL
  Done + nginx Detach), +1 per queued pipe event via
  `__sync_add_and_fetch`; self-deletes at 0; `buffer_` guarded by
  `pthread_mutex_t mutex_`; 1MB `kMaxBufferedOutputBytes` flush window.
- `NgxBaseFetch::Initialize` (`:80-85,87-132`) creates a static atomic
  `NgxEventConnection` in the worker (called from
  `ps_init_child_process:3642`); `Terminate` drains the pipe with
  `usleep(100)` up to 30s, then NULLs the pointer and `Shutdown()` without
  deleting the object (leaks one object, avoids use-after-free from late
  rewrite-thread writes); event-loop thread only.
- `NgxBaseFetch::ReadCallback` (`:153-258`) runs ON the nginx event loop:
  `DecrementRefCount` per event, skips detached/destroyed/stale ctx, calls
  `ps_base_fetch_handler`, then `ngx_http_finalize_request` +
  `ngx_http_run_posted_requests`; upstream-active case holds
  `r->main->count++` before finalize and skips finalize on `NGX_AGAIN`.
- PSOL threads never touch nginx directly (`:346-432`):
  `HandleHeadersComplete`/`HandleFlush`/`HandleDone` only
  `RequestCollection(type)` → atomic load event_connection +
  `IncrementRefCount` + `WriteEvent`; NULL connection after Terminate
  drops the event.
- `NgxEventConnection` (`ngx_event_connection.h:56-99`,
  `ngx_event_connection.cc:48-114`): nonblocking pipe + `ngx_connection_t`
  from `ngx_get_connection` registered with `ngx_add_event(NGX_READ_EVENT)`
  or `ngx_add_conn` (non-epoll); read side owned by event loop, write side
  by PSOL threads.
- `ReadEventHandler` (`:116-169`, `ngx_event_t` handler on event loop) →
  `ngx_handle_read_event` → `ReadAndNotify` `read(fd)` one `ps_event_data`
  per call (EAGAIN=done, EINTR=retry) → `connection->event_handler_(data)`;
  read errors left for `Shutdown()` to avoid double-close of recycled
  connection.
- `WriteEvent` (`:175-240`, any PSOL thread) serializes with
  `pthread_mutex_t write_mutex_`, drops event if `closed_`, retries EAGAIN
  with exponential backoff `usleep` 100us→100ms x50 (~6.5s worst case)
  releasing mutex during sleep; blocking risk confined to background
  threads, never event loop.
- `Shutdown` (`:247-267`, event-loop thread at worker exit) marks
  `closed_` under mutex, `close(write fd)`, then `ngx_close_connection`
  (read side) so event-loop bookkeeping is released; raw `close()` only if
  `connection_==NULL`.
- `NgxUrlAsyncFetcher` (`ngx_url_async_fetcher.h:57-150`,
  `ngx_url_async_fetcher.cc:56-111`): native fetcher, one
  `NgxEventConnection` each, constructed per ServerContext when
  use_native_fetcher_; `Fetch:225` runs on REWRITE thread (queues
  `pending_fetches_` under CondvarCapableMutex + `WriteEvent` wakeup);
  `ReadCallback:254` runs on EVENT loop (moves pending→to_start,
  `StartFetch` each); `ShutDown:187` tears down channel under lock then
  `CallbackDone(false)` outside lock.
- `NgxFetch::Start/Init` (`ngx_fetch.cc:300-418`) runs on event loop:
  `ngx_create_pool`, `ngx_add_timer(timeout_event_, fetch_timeout_)` with
  `TimeoutHandler:935`, IP-literal fast path else `ngx_resolve_start`/
  `ngx_resolve_name` with `ResolveDoneHandler:525` + resolver_timeout_;
  all async, no blocking DNS.
- `NgxFetch::Connect` (`:696-726,730-810`) uses `ngx_event_connect_peer`
  then installs `ConnectionWriteHandler:730` (`c->send` loop +
  `ngx_handle_read/write_event`) and `ConnectionReadHandler:768`
  (`c->recv` loop → response_handler state machine → `CallbackDone`);
  timers from Init stay armed; all non-blocking event-driven.
- Response state machine (`:813-933`): HandleStatusLine
  (`ngx_http_parse_status_line`) → HandleHeader
  (`ResponseHeadersParser::ParseChunk`) → HandleBody (content-length
  bounded, overflow fails, no-body for HEAD/204/304); writes go to
  `AsyncFetch::Write`.
- `NgxConnection` keepalive pool (`ngx_fetch.h:61-104`,
  `ngx_fetch.cc:108-260`): static Pool + PthreadMutex
  connection_pool_mutex; `Connect` reuses idle same-sockaddr connection
  (del idle timer) else `ngx_event_connect_peer`; `Close` returns to pool
  with 60s `ngx_add_timer` + IdleRead/IdleWrite handlers, or
  `ngx_close_connection`; `Terminate` closes pooled idle connections at
  shutdown.

### 6.6 Lifecycle, blocking-call inventory, FFI boundary

- `StartThreads` (`ngx_rewrite_driver_factory.cc:204-215`, called once
  from `ps_init_child_process:3673` AFTER fork) starts SchedulerThread;
  TODO notes nginx native timer not used; never start threads before fork.
- `PrepareForkedProcess` (`:267-284`) resets `ngx_pid=ngx_getpid` for
  logging; `NameProcess` sets argv[0] via `ngx_setproctitle('pagespeed
  <name>')`.
- `AllocateFetcher` (`:111-128,161-166`, `ngx_pagespeed.cc:3553-3561`)
  returns `NgxUrlAsyncFetcher` (native, event-loop) when use_native_fetcher_
  else serf-based `SystemRewriteDriverFactory` fetcher; master-config
  `CheckResolver` fails config (`NGX_ERROR`) when native fetcher has no
  resolver (resolver+resolver_timeout copied from core loc conf).
- BLOCKING CALL `ps_determine_remote_options` (`:1543-1570,3579-3619`):
  `ServerContext::GetRemoteOptions` blocks up to
  remote_configuration_timeout_ms on request path; native-fetcher +
  blocking-cache case forces timeout 0 (serve local config, refresh
  completes on free loop); non-blocking external cache keeps configured
  wait; `ps_init_module` logs one warning per affected server.
- `ps_init_module` (`:3479-3622`, master, post-config, pre-fork): refuses
  single-process reload cycles (`NGX_PROCESS_SINGLE` + !initial_cycle →
  stay unoptimized), PostConfig server contexts, InitStats/RootInit
  (skipped when `ngx_test_config`), set_resolver/timeout,
  `LoggingInit(cycle->log,true)`, publishes active_driver_factory;
  heavy/blocking init allowed here, not on request path.
- `ps_init_child_process` (`:3635-3675`, worker, post-fork):
  `NgxBaseFetch::Initialize` (pipe+event registration), LoggingInit,
  ChildInit server contexts, per-server ProxyFetchFactory +
  `SetServerContextMessageHandler(error_log)`, StartThreads; cfg NULL/NULL
  factory ⇒ `NGX_OK` no-op.
- `ps_exit_child_process` (`:3624-3631`, worker exit):
  `NgxBaseFetch::Terminate` (30s drain) + driver_factory→ShutDown; single
  place where `usleep`/blocking drain is permitted.
- Blocking filesystem syscalls (`:887-933,994-1003,1291-1295`, stat/chown/
  StdioFileSystem IsDir/RecursivelyMakeDir) confined to CONFIG time:
  `ps_init_dir` for LogDir/FileCachePath (root chown to worker user) and
  FileCachePath IsDir validation in `ps_merge_srv_conf`.
- `NgxMessageHandler` (`ngx_message_handler.h:48-72`,
  `ngx_message_handler.cc:97-120`) routes PSOL messages to
  `ngx_log_error([ngx_pagespeed <ver>] msg)` when `log_` set else
  `GoogleMessageHandler`; per-factory + per-server-context instances
  (`SetServerContextMessageHandler:243`); `InstallCrashHandler:72`
  signal(SIGTRAP/SIGABRT/SIGFPE/SIGSEGV) with alarm(2)+StackTraceString,
  installed via `LoggingInit:225` (may_install_crash_handler=true in
  master+worker). `log_message_handler::Install`
  (`log_message_handler.cc:104-113`) wires base logging→`ngx_log_error`
  and lowers min level for DEBUG; called from LoggingInit (master+worker).
- `ps_pre_init` (`:3390-3395,969-988`, `ngx_gzip_setter.cc:40-73,107-113`)
  calls `g_gzip_setter.Init(cf)` (scans cycle modules for gzip directives,
  BeginCycle, installs redirect setters that RollBackAndDisable on
  explicit user gzip config); `ps_configure` auto-enables gzip on
  `pagespeed on` / disables on `off`; config-time only.
- FFI boundary files (all carry `extern "C" { #include <ngx_...> }` and
  are the only nginx-ABI touchpoints): `ngx_pagespeed.h:35`,
  `ngx_base_fetch.h:56`, `ngx_event_connection.h:30`
  (+`ngx_event_connection.cc:22`), `ngx_fetch.h:37` (+`ngx_fetch.cc:38`),
  `ngx_url_async_fetcher.h:33` (+`.cc:22`),
  `ngx_rewrite_driver_factory.h:25`, `ngx_rewrite_options.h:27` (+`.cc:24`),
  `ngx_server_context.h:30` (+`.cc:24`), `ngx_message_handler.h:26`
  (+`.cc:46`), `log_message_handler.h:25`, `ngx_gzip_setter.h:56`
  (+`.cc:30`), `ngx_caching_headers.h:25`, `ngx_list_iterator.h:29`.
- nginx timer/event APIs used (`ngx_fetch.cc:145,194,340,179-184,286-288`):
  `ngx_event_connect_peer`, `ngx_add_timer` (fetch timeout + 60s
  keepalive), `ngx_del_timer` (reuse/close/done/timeout),
  `ngx_handle_read_event`/`ngx_handle_write_event`, `ngx_event_get_peer`,
  `ngx_close_connection`/`ngx_free_connection`/`ngx_get_connection`.

NON-BLOCKING REQUIREMENTS SUMMARY: the event loop must never block — all
PSOL completion crosses via pipe events; only sanctioned waits are
(a) `GetRemoteOptions` remote-config fetch (mitigated to timeout-0 for
native+blocking cache), (b) EAGAIN backoff `usleep` confined to writer
(background) threads, (c) 30s Terminate drain confined to worker-exit path,
(d) config-time filesystem/chown/RootInit in master; native fetcher
DNS/connect/read/write are fully async via resolver + event handlers +
timers.

Supported nginx version range is a gap (nginx-gap-1): only the 1.13.4
phase-hook branch (`ngx_pagespeed.cc:3359,3425`), the test pin
release-1.29.5-2, and the unpinned mainline build are inventoried.
`install/common/pagespeed.conf.template` integration is a gap
(nginx-gap-2) though `rewrite_driver.cc:1055-1059` mandates co-updating it
on filter-order changes.

## 7. Admin pages and statistics

### 7.1 Admin pages (owner `pagespeed/system/admin_site.{h,cc}`)

`AdminSite` owns all admin pages (`admin_site.h`): DashboardHandler,
ImagesHandler, StatisticsHandler, PrintConfig, PrintHistograms, PrintCaches,
ConsoleHandler, MessageHistoryHandler, GraphsHandler, ConsoleJsonHandler,
StatisticsJsonHandler, AdminPage, StatisticsPage.

- `kTabs` (`admin_site.cc`): `/pagespeed_admin` leaves dashboard, history,
  images, html_cache, javascript, css, statistics, config, histograms,
  cache, console, message_history, graphs; legacy statistics links only
  for statistics/config/histograms/cache.
- `AdminSite::AdminPage`: `/pagespeed_admin/<leaf>` dispatch
  (dashboard|empty, history, images, html_cache, statistics, stats_json,
  graphs, config, console, message_history|javascript|css, cache,
  histograms); unknown leaf → 404 + Did-you-mean.
- `AdminSite::StatisticsPage`: legacy `/*_pagespeed_statistics` dispatch by
  query (`?json` → ConsoleJsonHandler, `?config`, `?histograms`, `?graphs`,
  `?cache`, else StatisticsHandler).
- Apache (`pagespeed/apache/instaweb_handler.cc`): handlers
  `pagespeed_admin`, `pagespeed_global_admin`, `mod_pagespeed_statistics`,
  `mod_pagespeed_global_statistics` + console/messages/purge route to
  `ServerContext::AdminPage/StatisticsPage/ConsoleHandler/
  MessageHistoryHandler` and `AdminSite::PurgeHandler`.
- nginx (`ngx_pagespeed.cc` `ps_route_request` + `ps_resource_handler`):
  nginx paths statistics_path, global_statistics_path, console_path,
  messages_path, admin_path (prefix), global_admin_path (prefix), purge
  method route to ServerContext AdminPage/StatisticsPage/ConsoleHandler
  and `AdminSite::PurgeHandler`.
- `ConsoleHandler`/`GraphsHandler`: `?json` delegates to
  ConsoleJsonHandler; HTML requires Statistics+StatisticsLogging+LogDir
  else setup notice.
- `ConsoleJsonHandler`: rate-limited by `ExpensiveLeaf::kGraphs` slot (429
  busy), 404 without console_logger, else `StatisticsLogger::DumpJSON`
  (`ParseGraphQuery` bounds 1s..1day, max 64 vars).
- Admin access-control wiring (Allow/Disallow/Domain directives vs
  AdminPage/StatisticsPage/PurgeHandler enforcement, including `*`
  whole-cache purge auth) is a gap (admin-gap-2).

### 7.2 Statistics variables and persistence

- `net/instaweb/http/http_cache.cc HTTPCache::InitStats`: cache_time_us,
  cache_hits, cache_misses, cache_backend_hits, cache_backend_misses,
  cache_fallbacks, cache_expirations, cache_inserts, cache_deletes.
- `net/instaweb/rewriter/rewrite_stats.cc RewriteStats::InitStats`:
  rewrite_cached_output_hits, rewrite_cached_output_misses,
  rewrite_cached_output_missed_deadline,
  total_rewrite_count/total_fetch_count/num_rewrites_executed/
  num_rewrites_dropped (TimedVariable), histograms Beacon/Resource/Rewrite/
  Backend latency, ipro_served/ipro_not_in_cache/ipro_not_rewritable,
  resource_404/slurp_404, flushes, fallbacks, domain accept/reject,
  downstream purge counters.
- `net/instaweb/rewriter/rewrite_driver.cc RewriteDriver::InitStats`:
  html_cache_hits, html_cache_misses, html_cache_bypasses,
  html_cache_inserts, html_cache_bytes_served + per-filter InitStats
  fan-out (27 filters fan out at `:880-912`; only image/css/js families
  are named here — admin-gap-1).
- `pagespeed/automatic/proxy_fetch.cc HtmlCacheLookup`: html_cache_bypasses
  on ineligible, html_cache_hits/misses in Done(), html_cache_inserts +
  html_cache_bytes_served on publish/serve.
- `image_rewrite_filter.cc`: image_rewrites, image_rewrite_total_bytes_saved
  (+ dropped/no-rewrite/ongoing/latency variants).
- `css_filter.cc`: css_filter_blocks_rewritten,
  css_filter_total_bytes_saved (+ parse/fallback/dropped variants).
- `javascript_code_block.cc JavascriptRewriteConfig::InitStats`:
  javascript_blocks_minified, javascript_total_bytes_saved (+
  failures/uses variants).
- `pagespeed/kernel/cache/cache_stats.cc CacheStats::InitStats(prefix)`:
  `<prefix>_hits`, `<prefix>_misses`, `<prefix>_inserts`,
  `<prefix>_deletes` + get_count/hit_latency/insert_latency/insert_size/
  lookup_size histograms.
- `pagespeed/system/system_caches.cc SystemCaches::InitStats` + prefixes:
  file_cache, lru_cache (`system_cache_path.cc`), shm_cache,
  memcached_async, memcached_blocking, redis_async, redis_blocking each get
  the `CacheStats` `_hits`/`_misses`/`_inserts`/`_deletes` family.
- Live counters live in the SharedMemStatistics segment
  `<filename_prefix>statistics` (`pagespeed/kernel/base`+`sharedmem`):
  CreateSegment in root Init(true), Attach in children, DestroySegment on
  GlobalCleanup; new segment per restart so live Variable/Histogram values
  reset, histograms re-Init() in parent.
- Persistence across restarts is the append-only file
  `<log_dir>/stats_log_<global|local>` (`AllocateAndInitSharedMemStatistics`
  in `system_rewrite_driver_factory.cc`;
  `pagespeed/kernel/util/statistics_logger.cc`):
  `UpdateAndDumpIfRequired` samples kConsoleVars+kOtherLoggedVars+
  kGraphsVars with timestamp + statistics_instance id;
  `TrimLogfileIfNeeded` deletes the whole file over max size.
- `PreviousDashboardCounters`: dashboard reads pre-restart deltas for
  javascript/css/image bytes_saved+blocks, image_rewrites,
  html_cache_hits/misses/bypasses/inserts/bytes_served (bounded 16MiB
  scan, per-instance reset detection); `admin_site.cc`
  `DashboardCounter(live+previous)` and HtmlCache summary are documented
  as saved-samples-plus-current, not a ledger.

Rust scaffold state: `pagespeed-rust/crates/ps-admin/src/lib.rs` is
scaffold only (`health()`→ok, no admin routes yet);
`pagespeed-rust/crates/ps-stats/src/lib.rs` is scaffold only (in-process
saturating Counter, no shared-mem/file persistence yet).

## 8. Images

### 8.1 Formats and codecs (owner `pagespeed/kernel/image/`)

- `image_util.h`: `ImageFormat` enum {JPEG,PNG,GIF,WEBP,AVIF} +
  `PreferredLibwebpLevel` {NONE,LOSSY,LOSSLESS,ANIMATED} + `PixelFormat`;
  canonical format vocabulary.
- `image_util.cc`: `ComputeImageType` magic-sniffs JPEG(FFD8)/PNG
  header/GIF8/ftypavif+ftypavis/WebP (via `WebPGetFeatures` incl.
  animated/alpha/lossy split); `ImageFormatToMimeTypeString`.
- `jpeg_optimizer.h`: `OptimizeJpeg`/`OptimizeJpegWithOptions`;
  `JpegCompressionOptions` {progressive, retain_color_profile,
  retain_exif_data, lossy+`JpegLossyOptions` (quality default 85,
  num_scans, YUV420/422/444)}.
- `jpeg_reader.h`: `JpegScanlineReader` decode (GRAY_8 or RGB_888); thin
  `JpegReader` wrapper over libjpeg-turbo.
- `jpeg_utils.h`: `JpegUtils::GetImageQualityFromImage` reads source JPEG
  quality from quantization tables (-1 unknown).
- `png_optimizer.h`: `PngCompressParams` {filter_level,
  compression_strategy, try_best_compression (OptiPNG), is_progressive};
  `ScopedPngStruct`; libpng+optipng.
- `gif_reader.h`: `GifReader` + `GifFrameReader` (single+animated GIF
  decode to RGB_888/RGBA_8888); must instantiate via
  `CreateImageFrameReader`.
- `webp_optimizer.h`: `WebpConfiguration` {lossless, quality default 75,
  method, target_size, alpha_compression/filtering/quality, kmin/kmax
  animated}; `WebpFrameWriter`; libwebp.
- `read_image.h` + `read_image.cc`: decoder/encoder factories;
  `InstantiateScanlineReader` maps PNG→PngScanlineReaderRaw,
  JPEG→JpegScanlineReader, WEBP→WebpScanlineReader,
  GIF→FrameToScanlineReaderAdapter(GifFrameReader); IMAGE_AVIF has NO
  reader (unsupported).
- `image_converter.h`: `ConvertPngToJpeg`, `ConvertPngToWebp` (+opaque
  detection), `OptimizePngOrConvertToJpeg`, scanline + multi-frame
  `ConvertImage` paths.
- `image_resizer.h`: `ScanlineResizer` shrink-only resizer with
  kPreserveAspectRatio; wraps any `ScanlineReaderInterface`.
- `pixel_format_optimizer.h`: strips opaque alpha channel
  (RGBA_8888→RGB_888) as `ScanlineReaderInterface`.
- `image_analysis.h`: SobelGradient/Histogram photo-vs-graphics metric
  driving lossy-vs-lossless choice.
- `image_optimizer.h` + `image_optimizer.cc` + `image_optimizer.proto`:
  `ImageOptimizer::Optimize` one-shot orchestrator; `ImageOptions` proto
  {allow_png/jpeg/webp_lossy/lossless_or_alpha/animated,
  allow_convert_lossless_to_lossy, must_reduce_bytes, max_jpeg/webp/
  animated quality default 85, max_timeout_ms,
  try_best_compression_for_png}; `ComputeDesiredFormat` matrix
  (animated→WebP-animated; transparent→WebP-lossless/PNG; opaque
  photo→WebP-lossy/JPEG).
- `pagespeed/kernel/http/content_type.cc`: canonical image mime map
  image/png/gif/jpeg/webp(.webp)/avif(.avif) + jpeg synonyms.

### 8.2 Rewriter-level image pipeline (owner `net/instaweb/rewriter/`)

- `image.cc`: AVIF has NO in-process codec; `ConvertToAvifExternal` shells
  to avifenc via `posix_spawnp` (mkstemps temp files, AvifWatchdog
  deadline thread w/ pidfd kill, `AcquireAvifEncoderSlot` admission
  control, `UpdateAvifStats`); JPEG branch order AVIF→WebP
  (`ConvertJpegToWebp`)→JPEG recompress; PNG/GIF via
  `ComputeOutputContentsFromGifOrPng`; `TypeToContentType` maps
  IMAGE_AVIF→kContentTypeAvif; `AvifBytesLookComplete` ftyp/avif smoke
  check.
- `public/image.h`: `Image::CompressionOptions` {preferred_webp,
  allow_webp_alpha/animated, webp/webp_animated/jpeg quality,
  progressive_jpeg(+min_bytes), convert_gif_to_png/png_to_jpeg/
  jpeg_to_webp/to_avif, recompress_jpeg/png/webp,
  retain_color_profile/sampling/exif, jpeg_num_progressive_scans,
  webp/avif_conversion_timeout_ms, avif_max_concurrent_conversions} +
  ConversionVariables/AvifConversionVariables stats.
- `webp_optimizer.h` + `webp_optimizer.cc`: rewriter-level `OptimizeWebp`
  (libjpeg decode → libwebp encode, kNoQualityGiven=-1) and
  `ReduceWebpImageQuality`; JPEG→WebP path used by `image.cc`.
- `image_rewrite_filter.cc DetermineQualities`: 3-tier quality selection —
  Save-Data qualities > small-screen qualities > desktop qualities (webp,
  webp_animated, jpeg + progressive scans).
- `image_rewrite_filter.cc ImageOptionsForLoadedResource` +
  `SetWebpCompressionOptions`: wires RewriteOptions+ResourceContext into
  CompressionOptions; maps `ResourceContext::LibWebpLevel` to
  `PreferredLibwebpLevel`; convert_to_avif = kConvertToAvif &&
  supports_avif; `ResizeImageIfNecessary` handles resize.

### 8.3 Negotiation and variants

- `device_properties.cc AcceptsImageFormat`/`ExplicitlyRejectsImageFormat`:
  Accept-header parser with q-value support (q=0 counts as explicit
  reject); `ParseRequestHeaders` sets accepts_webp_/accepts_avif_.
- `device_properties.cc+h`: SupportsWebpInPlace = Accept:image/webp only;
  SupportsWebpRewrittenUrls = Accept OR UA-fallback unless explicitly
  rejected; SupportsAvifRewrittenUrls = (Accept:image/avif OR UA-fallback)
  unless rejected/IsKnownAvifUnsupported; LosslessAlpha/Animated require
  Accept AND UA match.
- `pagespeed/kernel/http/user_agent_matcher.cc+h`: UA fallbacks WebP
  (Firefox>=65, Chrome/Chromium>=32, OPR>=19, iOS Safari>=14, macOS
  Safari>=14) and AVIF (Firefox>=93, OPR>=71, Edg/EdgA>=121, Samsung>=14,
  UC>=15.5, Baidu>=13.52, Safari>=16.4/iOS16, Chromium threshold);
  `IsKnownAvifUnsupported` (IE, legacy Edge, Opera Mini, KaiOS).
- `request_properties.cc+h`: ANDs DeviceProperties with
  DownstreamCachingDirectives (PS-CapabilityList) for WebP variants; AVIF
  passes device result through (no downstream gate).
- `public/downstream_caching_directives.h`: PS-CapabilityList parsing;
  SupportsWebp/LosslessAlpha/Animated gates (no AVIF gate exists).
- Image filter flags (`rewrite_options.h`+`rewrite_options.cc`):
  kConvertJpegToWebp, kConvertToAvif, kConvertPngToJpeg, kConvertGifToPng,
  kConvertToWebpLossless, kConvertToWebpAnimated, kRecompressJpeg/Png/Webp,
  kResizeImages, kResizeMobileImages, kResizeToRenderedImageDimensions,
  kStripImageColorProfile/MetaData, kJpegSubsampling,
  kConvertJpegToProgressive.
- Defaults (`rewrite_options.cc`): ImageRecompressQuality=85;
  JpegRecompress=-1(inherit)/small-screen 70/save-data 50;
  WebpRecompress=80/small-screen 70/animated 70/save-data 50;
  WebpTimeoutMs=-1 (semantics unexplained — images-gap-2);
  AvifTimeoutMs=30000; AvifMaxConcurrent=2; JpegNumProgressiveScans=-1;
  LimitOptimizedPercent=100; LimitResizeAreaPercent=100;
  LimitRenderedAreaPercent=95; ResolutionLimitBytes=32MB;
  MaxRewritesAtOnce=8. Limit enforcement sites untraced (images-gap-1).
- `rewrite_gflags.cc`: gflags mirrors image_recompress_quality,
  image_jpeg/webp_recompress_quality (+_for_small_screens),
  image_jpeg_num_progressive_scans(+_small_screens),
  image_webp_timeout_ms, image_limit_optimized/rendered_area/resize_area_
  percent, image_max_rewrites_at_once.
- `cached_result.proto ResourceContext`: per-format cache-variant key
  fields {desired_image_dims, mobile_user_agent, libwebp_level
  NONE/LOSSY_ONLY/LOSSY_LOSSLESS_ALPHA/ANIMATED,
  may_use_small_screen_quality, may_use_save_data_quality, supports_avif};
  `CachedResult.optimized_image_type` + inlined/low_resolution_inlined_data.
- `image_url_encoder.cc`: `SetLibWebpLevel` (capability+enabled-filters →
  level); `SetWebpAndMobileUserAgent` (supports_avif = kConvertToAvif &&
  SupportsAvifRewrittenUrls); `CacheKeyFromResourceContext` variant
  suffixes `./w/v/a` + `.avif` + `m` + `d/ss`; WxH dimension URL prefix +
  legacy w/v/m decode; `AllowVaryOnUserAgent`/`AllowVaryOnAccept`
  (VaryOnAuto: no-Via→UA, Via→Accept).
- `in_place_rewrite_context.cc AddVaryIfRequired`: Vary:User-Agent when
  UA-negotiated (webp/avif/small-screen qualities), Vary:Accept for
  JPEG/WebP/AVIF lossy IPRO, Vary:Save-Data append, Cache-Control:private
  override for IE; `UserAgentCacheKey` delegates to
  `CacheKeyFromResourceContext`; `EncodeUserAgentIntoResourceContext`
  downgrades libwebp level when Vary UA/Accept disallowed.

Tests: `net/instaweb/rewriter/device_properties_test.cc`
(Accept:image/webp × UA matrix), `webp_optimizer_test.cc`
(ReduceWebpImageQualityPreservesAlpha), kernel `image_*_test.cc` per codec
(jpeg/png/webp/gif_reader/image_converter/image_resizer/image_optimizer/
image_util/jpeg_utils), rewriter `image_test.cc`/
`image_rewrite_filter_test.cc`/`image_url_encoder_test.cc`/
`request_properties_test.cc`/`in_place_rewrite_context_test.cc`; testdata
under `pagespeed/kernel/image/testdata/{gif,jpeg,png,pngsuite,resized,
webp}`.

Rust scaffold state: `pagespeed-rust/crates/ps-image` exists
(`src/lib.rs` + `Cargo.toml` only); no image logic ported yet.

## 9. Known gaps

Propagated from the audit children plus the synthesis pass (labels kept
stable for Phase 3+ follow-up):

- filters-1: owner/wiring of kRewriteStyleAttributes +
  kRewriteStyleAttributesWithUrl (cs/cu) unverified; no branch in
  AddPreRenderFilters/AddPostRenderFilters, presumed inside CssFilter.
- filters-2: owner/wiring unverified for kDivStructure kExplicitCloseTags
  kExperimentCollectMobImageInfo kExperimentHttp2 kMobilize
  kResponsiveImagesZoom kFallbackRewriteCssUrls kSplitHtml kSplitHtmlHelper
  kServeDeprecationNotice kCachePartialHtmlDeprecated
  kComputeVisibleTextDeprecated (in enum/sets, no instantiation found).
- filters-3: kInPlaceOptimizeForBrowser (io) HTML-chain wiring absent;
  IPRO/single-resource path via InPlaceRewriteContext presumed but not
  verified.
- filters-4: kHtmlWriterFilter (hw) always-on writer construction site not
  verified.
- filters-5: Apache ModPagespeedRewriteLevel setability unverified; only
  SetDefaultRewriteLevel(kCoreFilters) default verified.
- filters-6: request-header PageSpeedFilters variant and X-PSA client
  options not traced; only `?PageSpeedFilters=`/`?ModPagespeedFilters=`
  query-param path verified.
- filters-7: per-filter unit/system test inventory by listing only
  (`*_filter_test.cc`, `system_tests/*.sh`); no test bodies opened.
- config-1: help/doc text for all ~160 core options not transcribed
  (inline in AddProperties help strings).
- config-2: effective nginx-conf setability of core kQueryScope options
  not traced (query-param/cookie path).
- config-3: interaction of `pagespeed standby` with PostConfig
  FileCachePath-empty error not traced (unplugged skipped; standby path
  unknown).
- deps-1: numeric upstream versions for vendored grpc@b45b83c,
  boringssl@07e7806, gflags@cc7e9a4/e7390f9 unavailable; SHA pins only.
- deps-2: testing-dependencies/nginx, ngx_cache_purge, ngx_devel_kit,
  set-misc-nginx-module trees not checked out (gitfile-only); file
  contents uninspected, pins from git only.
- deps-3: ngx_compiler nginx mainline version intentionally unpinned
  (apt-get source at build time); no static version reportable.
- modules-gap-1: no purpose/build-role for L1 entries bazel/,
  changelog.d/, external/, log/, out/, strings/, url/ (§1.1 lists names).
- modules-gap-2: no original-module → pagespeed-rust crate mapping in the
  modules evidence; resolved by `docs/MAPPING.md` in this deliverable.
- config-gap-1: full core directive table missing: 165
  AddBaseProperty/AddProperty registrations verified by grep, but only
  counts + ~25 key defaults inventoried; per-directive
  name/type/default/scope/rename-status not transcribed.
- caching-gap-1: HTML cache not inventoried:
  ProxyFetch::HtmlCacheLookupComplete (`pagespeed/automatic/proxy_fetch.cc:
  1610`) + html_cache_* stats exist but no HtmlCache key/TTL/backend/
  invalidation entries.
- caching-gap-2: default backend-chain selection order not stated (when
  memcached/redis absent: FileCache vs LRU vs shm; which backs metadata vs
  HTTP cache vs HTML cache).
- nginx-gap-1: supported nginx version range not stated; only 1.13.4
  phase-hook branch + test pin release-1.29.5-2 + unpinned mainline noted.
- nginx-gap-2: `install/common/pagespeed.conf.template` integration not
  inventoried though `rewrite_driver.cc:1055-1059` mandates co-updating it
  on filter-order changes.
- admin-gap-1: per-filter stat variables not enumerated:
  RewriteDriver::InitStats (`rewrite_driver.cc:880-912`) fans out to 27
  filters, but only image/css/js families + HTTPCache/RewriteStats/
  CacheStats were named.
- admin-gap-2: admin access-control wiring not inventoried: Allow/Disallow/
  Domain directives vs AdminPage/StatisticsPage/PurgeHandler enforcement
  (including `*` whole-cache purge auth) not traced.
- images-gap-1: image limit enforcement sites not traced:
  ResolutionLimitBytes=32MB, LimitOptimizedPercent=100,
  LimitResizeAreaPercent=100, LimitRenderedAreaPercent=95 listed as
  defaults only.
- images-gap-2: WebpTimeoutMs=-1 semantics unexplained (vs
  AvifTimeoutMs=30000 + AvifMaxConcurrent=2 wired to the avifenc pipeline).

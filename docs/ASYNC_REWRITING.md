# Asynchronous HTML resource rewriting

Production HTML requests read optimization metadata. A missing partition schedules
work on the bounded worker pool and immediately preserves the original URL. The
worker runs the existing fetch, validation, optimization, and output-writing code;
a later render uses its completed partition. HTML requests do not wait for these
jobs, including when the queue is full or shutting down.

This applies to CSS, JavaScript, images, cache extension, CSS/JS/font inlining, and
CSS/JS combinations. Combination metadata records ordered candidates, option and
UA identity, applicable charsets/base URLs, and input validity watermarks. Warm
combination hits neither load input bytes nor repeat CSS parsing or JS minification.
Failed candidates retain their original URLs without discarding successful
neighboring partitions. Negative dependencies retry after one second on a later
metadata lookup; incomplete inputs also prevent publishing fallback HTML.

Background snapshots preserve effective options, request headers (including explicit
image format rejection), request context, font-service hooks, and authorization.
Request/parser/DOM state stays on its owning thread. Jobs own their snapshots and
share only thread-safe cache, fetcher, statistics, and hashing handles.

## Pool and deadlines

Nginx installs one pool per worker. `NumRewriteThreads` selects its thread count;
the pending queue holds at most 1024 jobs. Keys deduplicate concurrent work within
a pool. Separate Nginx workers can still perform the same optimization; output
writes and cache coherence remain safe. Queue saturation sheds work to original
URLs and allows later requests to retry. A panicking job releases its waiters and
deduplication entry, and the worker remains available.

`RewriteDeadlinePerFlushMs` bounds remaining fetch waits. Elapsed work as well as
waited time counts against the flush window, which rearms on flush.
`InPlaceRewriteDeadlineMs` applies to in-place serving when
`InPlaceWaitForOptimized` is enabled. Explicitly disabling serving deadlines keeps
the existing unbounded serving behavior; HTML never interprets that setting as an
unbounded wait. Resource serving owes the caller bytes and remains separate from
HTML URL rewriting.

Contexts without an installed pool retain the synchronous embedding/test API.
Custom behaviors without a background snapshot also retain that API. The Nginx
factory installs the pool for the production HTML path.

## Reuse and HTML caching

The server keeps a bounded, 16-entry LRU of immutable filter plans keyed by effective
signature, Debug state, and chain context. UA wildcard/regex configurations and
normalizer expressions are shared. Request capabilities cache on the resource
driver and invalidate when headers or UA change; LCP evidence snapshots once per
document. Mutable filters and parser state remain per document. Statistics names
and registrations remain in the shared worker registry.

Options signatures are shared across configuration clones and invalidate on
mutation. Request option cloning no longer merges the source into an already
complete clone. Signatures include directive collections such as custom headers,
libraries, and URL-valued attributes; unordered collections serialize deterministically.
Partition signature hashes reuse the current signature generation. Debug shares
resource signatures intentionally but has separate filter-plan and whole-HTML keys.

HTML cache lookup precedes filter construction. A HIT serves stored HTML without
constructing the parser/filter chain. A detached render does not publish fallback
HTML while its optimizations remain incomplete.

`AddInstrumentation` is now wired into the driver. Its cached browser bootstrap
executes `new Date()` on every page load; it does not cache a server-generated
request timestamp or identifier. Critical-image/LCP sampling retains its existing
request-time gate: sampled nonce initializers never enter the HTML cache, and
accepted evidence versions the cached variant. Regression tests check RUM script
retention on HITs without constructing filters.

## Memory and persistent caches

The existing `LRUCacheKbPerProcess` default remains **8192 KiB per worker**;
`LRUCacheByteLimit` controls per-entry admission. Setting the size to zero keeps
the disk-only path. This bound is shared by HTTP and metadata entries.

Persistent disk writes remain authoritative. Process-local LRU keys include a
shared write generation. 128 generation/writer-count pairs in a mapped file
coordinate replacements and deletions across workers without filesystem reads on
memory hits. The reserved `.pagespeed-lru` directory is excluded from cleaning.
Reads overlapping writes fall through to disk; TTL, HTTP validation, and metadata
validity checks still run above the byte cache. Oversized replacements invalidate
previous smaller L1 entries, and local read/backfill operations serialize with
writes and deletes.

If the coordination mapping cannot initialize, the path uses disk caching. A
worker killed during a write can leave its coordination shard bypassing memory
until a full restart; disk access continues safely. Direct edits to cache files
outside this protocol are not cross-worker invalidation notifications.

## Profiling and compatibility

Build `ps-nginx` with `--features pipeline-profile` for per-request filter-init,
HTML-cache lookup/store, and HTML-processing timings, plus foreground/background
resource fetch timings. Ordinary builds compile out these clocks and logs.
`tools/profile_html.py` launches an isolated foreground Nginx from an explicit
configuration and measures cold delivery, stable HTML HITs, and warm HTML MISSes
separately. MISS probes vary only a request header used in the whole-HTML key.
It also supports strace and Callgrind. See [the measured report](PERFORMANCE.md).

The C++ reference is `RewriteContext::OutputCacheMiss` and
`RewriteDriver`'s bounded wait/detach path in `mod_pagespeed/net/instaweb/rewriter`.
C++ may spend its configured rewrite budget before detaching; this HTML path takes
the requested immediate fallback policy. Optimizers, combination partition rules,
resource reconstruction, and content-addressed output naming are reused rather
than replaced with cheaper transformations. Existing compatibility limitations
such as CSP and external purge-file polling are not implemented by this change.

# Shared cache configuration (Linux, explicit opt-in)

At `http` scope, name the cache with the same `FileCachePath`:

```nginx
pagespeed FileCachePath /var/cache/pagespeed;
pagespeed CreateSharedMemoryMetadataCache /var/cache/pagespeed 16384;
```

The budget is KiB. Enabled range: 1024–262144 KiB; zero explicitly disables
that named cache. Omitting the directive preserves the existing local-LRU/disk
stack. The default local LRU remains enabled. Shared memory is not automatically
enabled: measurements show lower shared-L2 lookup cost but additional hot-L1
coherence overhead; workload/reload validation is required before choosing an
automatic default.

The byte mapping fits within this budget using 16 shards and fixed slots with a
16KiB combined key/value limit. Larger values remain on disk. The fixed-slot
allocator trades internal slack for predictable allocation; it does not remove
resource optimization, image conversions, HTML caching or RUM. Each cache domain
also allocates bounded ownership, writer-tracking, transaction and counter
mappings (outside the byte budget). Exact totals/aggregation diagnostics remain
Phase 8 work; this is not a claim that total process RSS equals the byte budget.

Initial migration from a binary/coherence stack predating this protocol requires
a **full stop/start**, with every writer to the same disk path enrolled. Do not
activate it by graceful reload over legacy workers. Mixed protocol writers can
invalidate each other's assumptions. Subsequent same-protocol reloads and size
changes must retain the same boot-qualified domain; Phase 7 live overlap tests
are still pending. Do not remove/truncate live mappings.

Startup mapping errors retain the disk/LRU stack and log the reason once during
configuration initialization. Unsupported platforms also retain that stack.
The mapping subtree `.pagespeed-lru` is excluded from file-cache cleanup. Linux
boot-qualified paths prevent reuse of pthread state after reboot. Disk cache
entries retain their existing TTL, purge, options/UA and codec checks.

Worker optimization claims and metadata transactions never wait on HTML
requests. A contended beacon transaction skips nonce issuance or report merging
for that attempt. Cached HTML retains the browser-side per-load RUM timestamp
and instrumentation bootstrap; sampled nonce-bearing output retains its existing
HTML-cache bypass.

Disk mutations use 64 robust writer slots. Killed writers are recovered without
procfs reads or waiting for a live writer. Exceptional exhaustion preserves disk
writes/deletes and permanently bypasses memory for the affected hash shard to
avoid stale data; restoring it requires an offline replacement/new tracker
domain. Do not delete the domain while any worker is alive. Optimization owner
cleanup may conservatively revoke earlier claims under contention, causing later
requests to retry without publishing stale metadata.

Run an isolated comparison (does not alter the installed service):

```sh
python3 tools/benchmark_shared_memory.py \
  --nginx ../.nginx-build/nginx-1.31.8/objs/nginx \
  --module target/release/libps_nginx.so \
  --output docs/performance/shared-memory/shared-enabled.json \
  --shared-kb 16384 --samples 100 --cold-runs 5
```

Ordinary release builds omit pipeline instrumentation. They cannot attribute the
historical 13ms residual; a profiling build and matched instrumented run are
required. Baseline/raw measurements and their build provenance live beside this
file. No module has been installed or live Nginx configuration changed.

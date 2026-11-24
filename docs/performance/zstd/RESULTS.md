# compress_zstd latency refinement

Source baseline `7875327`; context-reuse milestone `641b633`, followed by bounded complete-body reuse. All measurements are isolated local release builds, not production deployment. Module hashes/temporary paths are in modules.json; rebuilding may be necessary after temporary files expire.

## Changes and preserved behavior

- Reuse at most one completed bulk and one completed streaming codec context per thread. No global locks, worker spawning or waits; active streams own independent contexts. Session reset prevents prior frame history from affecting the next response. Abandoned/error paths drop their context. Responses above 1 MiB still encode, but their contexts are not pooled afterward.
- Complete-body result LRU: maximum eight entries and 512 KiB of combined input+encoded payload per thread, plus bounded metadata. Admit after repeated fingerprint/length observation, avoiding input/output copies for one-off responses. CRC32 is only candidate selection: exact input-byte comparison is required before returning cached frames. Changed validated resource bodies miss naturally; TTL/header/options validation still happens in the caller. No headers are cached.
- Streaming HTML frames are never memoized. Per-request nonce/RUM state and explicit streaming flushes remain independent.
- MIME eligibility and split Accept-Encoding parsing avoid temporary normalization/join allocations. Disabled/non-negotiating paths do not initialize compression contexts or memoize bodies.

Compression level remains **3**, identical to baseline; no filters, conversions or resource work were disabled. Cache misses and over-budget inputs still compress normally. Negotiation, non-HEAD/body-carrying status gates, existing encoding checks, Vary/length handling and incompressible complete-body fallback retain their behavior.

## Codec work

Nine batches of 200 operations/case, 4 KiB chunks for streaming. Level and encoded sizes are unchanged. Warm bulk figures below include exact-body result-cache hits; they are not measurements of fresh compression CPU.

| Workload | Original | Refined | Encoded bytes (both) |
|---|---:|---:|---:|
| html_bulk | 132.023 µs | 0.642 µs | 3012 |
| html_stream | 36.362 µs | 27.851 µs | 3000 |
| css_bulk | 13.768 µs | 3.182 µs | 91 |
| css_stream | 21.228 µs | 10.585 µs | 88 |

Unique-body misses (first eight input bytes changed per iteration, same level; nine alternating batches of 200):

| Body | Original compression | Refined MISS path |
|---|---:|---:|
| html | 128.785 µs | 25.463 µs |
| css | 14.122 µs | 11.357 µs |

The codec files' `cold_ns` fields are individual first calls per case within one process, not statistical cold-process benchmarks; later cases may reuse prior state. Do not use those values to claim cold-request speedups. Real first encodes/cache misses still pay compression cost.

## Real NGINX response latency

Three alternating original/refined runs per build; ordinary single worker; each run starts fresh application caches and warms before measuring. 300 samples per phase/build/configuration. Client wire timing ends before decoding; the harness then decompresses and checks output. Filter-off and identity-client controls remain visible to account for drift. Full-body CSS sample:106,153 bytes identity /19,649 zstd, unchanged between builds.

| Build | Filter | HTML HIT identity | HTML HIT zstd client | HTML MISS identity | HTML MISS zstd client | Resource identity | Resource zstd client |
|---|---|---:|---:|---:|---:|---:|---:|
| original | off | 0.445 ms | 0.413 ms | 1.829 ms | 1.887 ms | 0.247 ms | 0.229 ms |
| original | on | 0.479 ms | 0.544 ms | 1.876 ms | 1.971 ms | 0.236 ms | 0.471 ms |
| refined | off | 0.442 ms | 0.405 ms | 1.844 ms | 1.826 ms | 0.225 ms | 0.222 ms |
| refined | on | 0.414 ms | 0.451 ms | 1.831 ms | 1.990 ms | 0.238 ms | 0.230 ms |

Observed enabled zstd resource latency fell about51% (0.471->0.230ms) and HTML HIT about17% (0.544->0.451ms). HTML MISS was essentially unchanged/slightly higher (1.971->1.990ms). Disabled-filter identity HIT remained0.445->0.442ms. Some control values drift, so the HTTP differences are local observations rather than universal or zero-overhead guarantees. The same-level codec/unique-input comparisons show where work was removed. No claim that these measurements resolve the earlier production13ms tails.

Post-workload single-process PSS medians were~25.5MiB original/~25.0MiB refined with compression enabled. This is not a peak/idle-memory bound. Codec workspace retention trades memory for speed and follows zstd's default-level workspace sizes; the512KiB body-payload budget is additional and per thread (normally per NGINX worker). Counts are bounded, but total RSS includes allocator overhead, active responses, other caches and codec memory. No persisted compressed-body cache or new NGINX directive was added.

## Verification

- **172 ps-http tests passed** (negotiation, MIME/headers, fresh/reused exact bulk frames, interleaved/abandoned/empty streams, threads, large-response non-retention, collisions, bounded LRU/admission and changed bodies).
- **291 NGINX library tests passed serially**. The first parallel attempt hit the previously recorded restart_namespace test flake (290pass/1fail); the failed log is retained, not suppressed. No unrelated cache implementation changes were made.
- Original/refined real HTTP samples decode to identical optimized HTML; CSS identity/zstd bytes match. Compression remains off when disabled and identity when not negotiated.
- Both actual two-worker zstd beacon reload orderings passed: HTML really carried Content-Encoding:zstd, old nonce accepted by new worker, prior evidence preserved and replay rejected. Cached RUM regression is included in the NGINX suite.
- Ordinary release builds, scoped rustfmt, Python syntax and diff checks pass. No deployment, live configuration change, permissions change or push.

## Reproduce

```sh
cargo test -p ps-http
cargo test -p ps-nginx --lib -- --test-threads=1
cargo run --release -p ps-http --example zstd_latency
cargo run --release -p ps-http --example zstd_miss_latency
cargo build -p ps-nginx --release
python3 tools/benchmark_zstd.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module MODULE.so --output RESULT.json
python3 tools/verify_beacon_reload.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module MODULE.so --output BEACON.json --zstd
# Repeat with --accept-before-reload for the alternate acceptance ordering.
```

`original-http-{1,2,3}.json` and `refined-http-{1,2,3}.json` are the final HTTP comparison (admitted-http-pairs.log). Earlier context-only/pre-admission experiments and the unique CSS miss regression that motivated admission are retained separately. Result-cache warm hits are identified explicitly, not reported as fresh compression.

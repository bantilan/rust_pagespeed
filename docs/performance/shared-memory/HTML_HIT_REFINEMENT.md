# HTML cache-hit setup allocation refinement

Runtime change: NGINX's HtmlSink now reuses the request ResourceDriver options snapshot, borrowing it only to construct a rewrite driver on MISS. It no longer eagerly copies the full RewriteOptions for eventual HITs. Header snapshots needed for cache publication are also copied only on non-HIT paths. The legacy constructor retains its independent-options snapshot semantics; the no-resource-driver path is unchanged.

All filter creation/processing, cache validation, option separation, resource processing and RUM behavior remain enabled. The driver-backed constructor is an explicit API so callers using different driver/sink options keep their previous semantics. The borrowed options reference ends before parsing begins. HITs construct no parser/filter chain and hold neither a second options snapshot nor a publication-header snapshot.

## Focused measurement

Seven alternating batches of 10,000 setup/drop iterations in the same ordinary release binary, request driver already allocated: original constructor median **1710.889 ns**, driver-backed median **1362.918 ns**. This removes ~348 ns (20.3%) of this setup workload. It does not measure total request latency, header-copy savings, or 12–13 ms production tails. See html-sink-setup.json and the html_sink_setup example.

## End-to-end evidence and limit

Initial before/after three-run tests drifted markedly: OFF medians moved from ~0.16 ms early to ~0.55 ms later. These data are retained in html-hit-before/after.json and are not a causal speedup estimate.

Saved old/new ordinary modules were then compared in three alternating pairs, 100 samples per warm phase/page/run. Pooled medians below are milliseconds; output hashes remained exactly equal. The baseline source was restored temporarily for its build, then new source and final module restored. Provenance is in html-hit-ab-state.json; temporary module paths may expire. The current patch/committed source is authoritative.

| Version | Page | HTML HIT | HTML MISS | OFF |
|---|---|---:|---:|---:|
| before | Moto | 1.295 | 4.191 | 0.508 |
| before | Aroma | 1.289 | 6.465 | 0.594 |
| after | Moto | 1.362 | 4.605 | 0.541 |
| after | Aroma | 1.449 | 6.716 | 0.564 |

The revised build's pooled request medians are higher in these trials. Run-level timing and OFF-baseline drift prevent isolating causality; these results do **not** demonstrate a net request-latency improvement or rule out a regression. The proven benefit is reduced setup allocation/work in the same-binary probe. The change is not deployed, and the README latency headline is unchanged. Repeat matched end-to-end checks under stable load before claiming a user-visible gain or recommending rollout for latency.

## Interleaved HIT-only follow-up

A further paired test ran old/new single-worker instances simultaneously on
separate loopback addresses with the **same virtual host and port**, alternating
request order for 500 pairs/page. Both returned only HITs with identical
normalized output. Moto medians: old1.366 ms/new1.395 ms; Aroma:
old1.433 ms/new1.416 ms. Median paired deltas were +0.027 ms and -0.014 ms.
These mixed results reinforce that there is no consistent demonstrated
end-to-end gain; the setup saving is much smaller than observed request jitter.
See html-hit-paired.json/log and tools/compare_html_hit_modules.py.

The initial draft used different ports, which changed rewritten CSS content
hashes; the equality assertion rejected that comparison. The final harness
keeps the origin port identical rather than masking URL/hash differences.

## Verification

- Nine send tests passed, including cached per-load RUM script/bootstrap, MISS/HIT publication, cookies, and request isolation. Driver-backed cached-RUM HIT test asserts no options/header snapshot and no parser initialization.
- Full NGINX library suite: **284 passed**, zero failures.
- Ordinary release build passed (10.97 s); final rebuilt current source passed (5.80 s).
- Real two-worker NGINX beacon reload test passed with the rebuilt module: pending nonce survives reload, valid evidence is accepted once and replay does not increment it.
- All before/after optimized Moto/Aroma hashes match previous benchmarks (15/23 optimized references). No filters disabled.
- Scoped rustfmt, git diff --check and raw benchmark marker/hash checks passed. Full workspace rerun was not needed for this NGINX-only change after the complete NGINX suite passed.

Commands:

```sh
cargo test -p ps-nginx --lib send::tests
cargo test -p ps-nginx --lib
cargo build -p ps-nginx --release
cargo run --release -p ps-nginx --example html_sink_setup
python3 tools/benchmark_shared_memory.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output RESULT.json --samples 100 --cold-runs 3
python3 tools/verify_beacon_reload.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --module target/release/libps_nginx.so --output docs/performance/shared-memory/html-hit-beacon-reload.json
```

The 12–13 ms transitional BYPASS and slow HIT observations remain unattributed. This small setup refinement does not replace production stage profiling or direct duplicate-fetch/encode measurement. No live configuration/module changes, purge, or remote push.

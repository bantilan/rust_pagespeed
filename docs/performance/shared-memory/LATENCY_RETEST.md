# Latency retest after 9929fc2

Three sequential paired comparisons, 500 old/new request pairs per page/run (1,500 pairs/page total), followed by a separate installed-server check. Both phases used ordinary modules. Initial overlapping checks are retained under exploratory-* and excluded from this summary; they introduced competing test traffic. No purge, reload, deployment or source change.

## Paired isolated modules

| Page | Before median | After median | Median paired after-minus-before |
|---|---:|---:|---:|
| Moto | 1.409 ms | 1.384 ms | -0.022 ms |
| Aroma | 1.401 ms | 1.408 ms | +0.004 ms |

All requests were confirmed HITs and normalized output matched old/new. Moto shows a small gain; Aroma is nearly unchanged/slightly higher. This does not establish a broad end-to-end latency improvement. Earlier same-binary setup probe remains ~20% lower setup time, saving only ~0.35 microseconds of that workload.

## Installed server

On-disk module SHA256 `03b71dcaff4a3bc4125688dc45e397bd376a6159e62cacf0be2cdf09e68c7987` changed since the previous review. It differs from the staged benchmark module; loaded-image/source identity is not inferred from this file hash. 100 samples per normal/MISS/OFF phase per page, interleaved; keepalive sampled separately.

| Page | Normal median | Normal p95 | Forced MISS median | OFF median | Keepalive median |
|---|---:|---:|---:|---:|---:|
| Moto | 1.506 ms | 1.823 ms | 4.526 ms | 0.666 ms | 1.108 ms |
| Aroma | 1.596 ms | 1.859 ms | 6.335 ms | 0.692 ms | 1.301 ms |
| Aerosky | 1.588 ms | 1.923 ms | 4.896 ms | 0.690 ms | 1.137 ms |

All normal samples were HIT (100/100 per page), all measured page phases returned HTTP200. This run is slower than the earlier installed observation, including the OFF baseline (~0.67–0.69 ms now vs~0.23–0.24 ms previously). Different load/environment and installed module identity prevent attributing that change solely to the update; OFF drift does not rule out a regression either. No new server-side CPU/IO/stage attribution was obtained. Historical and transitional tail causes remain open.

## Commands and evidence

```sh
python3 tools/compare_html_hit_modules.py --nginx ../.nginx-build/nginx-1.31.8/objs/nginx --before BEFORE.so --after AFTER.so --samples 500 --output RESULT.json
# Three repetitions; exact saved module paths and hashes in html-hit-ab-state.json.
python3 tools/check_installed_latency.py --output docs/performance/shared-memory/installed-update-retest.json --samples 100
```

Sequential command log: html-hit-retest-clean.log. Raw pairs: html-hit-retest-paired-{1,2,3}.json. Installed samples/markers: installed-update-retest.json. No code changed, so no rebuild/test suite rerun was necessary; runtime benchmark assertions/status checks passed.

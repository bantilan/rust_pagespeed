# Installed update latency review

Read-only localhost measurements on 2026-11-24. On-disk installed module SHA256: `fe5ea6a6ae3d4f798bcc20811050c11e42b38b4e562171e573541ce169a1aef0`; this differs from the staged module. A file hash alone does not establish loaded-image/source equivalence. One installed NGINX master has 12 workers. Admin reports zero shared-cache domains. No cache purge, configuration change, reload or deployment was performed.

## Sequential request measurements

100 samples per page/phase; enabled, unique-header forced MISS and PageSpeed-off phases interleaved with seeded random order. Keepalive sampled separately afterward, so phase/load drift is a confounder. Existing application/OS caches retained. Python HTTP client and total response time; no browser rendering/LCP claim.

| Page | Normal median | Normal p95 | Forced MISS median | OFF median | Keepalive normal median |
|---|---:|---:|---:|---:|---:|
| Moto | 0.552 ms | 0.906 ms | 2.011 ms | 0.232 ms | 0.391 ms |
| Aroma | 0.638 ms | 1.083 ms | 2.836 ms | 0.241 ms | 0.430 ms |
| Aerosky | 0.554 ms | 1.134 ms | 1.985 ms | 0.235 ms | 0.417 ms |

Moto/Aroma normal samples were 100/100 HIT, all HTTP 200. Aerosky was 95 HIT, one MISS, four BYPASS. The four BYPASS responses occurred at the beginning of the normal phase (13.176, 11.991, 13.309 and 12.992 ms), followed by a 2.164 ms MISS. This is a newly observed transitional ~13 ms interval, not proof of the cause of the historical production report. First observations were not controlled cold-cache tests.

The header-forced phases confirmed MISS for Moto/Aroma and MISS/BYPASS for Aerosky; zero HITs. Keepalive normal samples were all HIT. The server supports connection reuse; the data does not justify an NGINX keepalive configuration change. Real clients may already reuse connections.

Sampled existing optimized resources all returned 200: 13 URLs total, five fetches per URL. Per-resource medians ranged from 0.206 to 0.988 ms. These are already-cached resource requests, not reconstruction/encode benchmarks. Normalized HTML references in the initial page snapshots were Moto15, Aroma23 and Aerosky3; this is observational evidence, not comparison with a matched historical binary.

## Follow-up Aerosky measurements

24 normal requests paced 250 ms apart: all HIT, median 1.257 ms, maximum 2.289 ms. Eight concurrent clients /100 requests: all HIT, median 1.679 ms, maximum 11.423 ms. Raw bodies were normalized/hashed per sample; latency tails can occur on confirmed cache hits too. Timing alone does not identify CPU scheduling, disk cache access, socket/client overhead or any one cause.

## Evidence-supported priorities

1. Instrument or profile both early BYPASS and slow HITs with per-stage timings and worker identity. Those are distinct paths. Existing pipeline-profile support can attribute isolated runs; the installed ordinary module provides no equivalent stage breakdown here. Previous production ptrace attach was denied, so no server CPU/IO attribution is claimed.
2. Measure cold resource reconstruction and actual duplicate fetch/encode counts. Earlier source tracing confirmed synchronous reconstruction can block the NGINX event loop even after coordination sleeps were removed. The cached-resource samples cannot evaluate that cost.
3. Focus HTML MISS work on measured processing/cache costs before micro-optimizing filter initialization. Earlier isolated stage profiling found ~9 microseconds initialization versus ~1.1 ms processing; that is historical isolated evidence, not a stage breakdown for this installed binary.
4. Keep shared caching opt-in. It is not active in this deployment; earlier matched worker tests showed mixed latency and did not prove redundant-work savings. Activating it solely from these live timings is unjustified.

No new correctness failure established; all sampled responses were 200. No production source/config change is proposed without attribution. Remaining open work: slow-path/slow-HIT stage attribution, direct duplicate fetch/encode measurement, and fully asynchronous resource reconstruction.

## Reproduce

```sh
python3 tools/check_installed_latency.py --output docs/performance/shared-memory/installed-update-latency.json --samples 100
```

Raw data: installed-update-latency.json/log, installed-update-aerosky-settled.json, installed-update-processes.json. This harness warms with ten requests but does not assume they guarantee HIT: markers are recorded and reported. It does not clear caches or hide transitional slow responses. A small later warm sample cannot disprove a production tail-latency report.

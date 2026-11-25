# Same-site HTTPS resource loading fix

Fixed and deployed to rustpagespeed.com, 2026-11-25. Local HTTPS image sources now reach the jailed file fetcher; no network TLS connection is needed to read an authorized same-site file. External HTTPS fetching is still unsupported by the HTTP-only network leg; Gravatar remains unchanged.

## Cause and change

`FileFetcher::join_root` previously rejected every scheme except HTTP. Separately, `ChainedFetcher::supports_https` always returned false, so ResourceDriver rejected HTTPS before scheduling resource work. Both gates now recognize the local HTTPS capability. Existing hostname matching, traversal/NUL rejection, canonical root confinement, regular-file check, symlink jail and size limit remain intact. Mapped missing files remain file-leg failures; foreign HTTPS never acquires a local mapping and still fails closed in the network leg.

No encoder, quality, rewrite deadline, background queue, HTML scheduling, cache eligibility or filter settings changed. HTML still returns original URLs until asynchronous metadata is ready; all7asyncprogressive regressions passed. This fixes the local fetch path used by images and other same-site CSS/JS inputs, not general external HTTPS support.

## Verification

Baseline12filetests passed. Final293NGINXlibrarytests passed in2.60s, including HTTPS bytes/headers, foreignhost/traversal/NUL/missing/directory/escaping-symlink rejection and chain fetch_with_timeout routing. Matched1.31.10release build28.99s. OriginalHTTPbehavior and externalnetworkfailure tests remain intact.

Isolated realNGINX, actual712226-byteheader.jpg fixture, CoreFilters unchanged:

- HTML base and image source both HTTPS via trusted X-Forwarded-Proto simulation on a private HTTP listener, with matching host/port. NoTLSorigin exists.
- FirstHTML3.596ms returned originalimageURL; second2.842ms returned a.pagespeed.URL.
- OptimizedURL response image/avif, AVIFftyp signature confirmed,181199bytes (~74.6% reduction). Source/optimizedhash and actualURLs in isolated.json. URL extension need not identify negotiated MIME.
- InitialfixtureHTTPbase/HTTPSsource failed same-origin authorization, as expected; correctedfixtureorigin matching rather than loosening authorization. This unsuccessful fixture run is not a production failure or a performance baseline.

## Live results

ModuleSHA256 `703608b46284c4697652fb83c9b63c6ec762731110e41ab1bb0cb147d7d921f6`. Backed up previous module to `/usr/lib/nginx/modules/ngx_pagespeed_rust.so.before-https-file-20261125`, installed via new inode/rename, nginx-t passed and service restarted. No configuration changes, no cache purge, no filters disabled, no package rebuild or push.

`https://rustpagespeed.com/hello-world/` now emits rewritten local image URLs in src and srcset. Actual normal-pageattributes retained in live-page.json; externalGravatarURL remains original. Freshqueryprogression: firstoriginalURL/BYPASS97.847ms; nextoptimizedURL/BYPASS99.082ms; MISS98.752ms; subsequentHIT14.991/16.070ms. Existing beacon sampling/BYPASS behavior is unchanged and its image-free-page scheduling issue remains separate work.

The live rewritten header image returned **image/webp,80042bytes**, versus originalimage/jpeg712226bytes (~88.8% reduction); optimizedheaders saved. Live does not always choose AVIF; the existing conversion/fallback policy is preserved. Isolatedfixture separately verifies AVIF capability.

The original header.jpg URL now returns a recompressedJPEG127207bytes after warming (firstresponseoriginal712226bytes). Browser-dependent in-place conversion requires the existing in_place_optimize_for_browser filter, which is not enabled here; HTML-rewrittenURLs negotiate modern formats already. No additional directive was enabled just to force an AVIF result.

Read-only warm homepage sanity benchmark,30confirmedHITs each plus3warmups:

| Mode | Median | p95 |
|---|---:|---:|
| Local freshTLS identity |15.329ms|21.974ms|
| Local freshTLS Zstd |14.696ms|15.619ms|
| ReusedTLS identity |0.858ms|—|

These remain consistent with previous~15msfresh/~0.86mskeepalive observations; not a randomized causal latency comparison. Source JPEG fetch/decode/encode is absent from warmHTMLHITs. ColdHTML/resource requests and synthetic timings are separate from this warmcache table.

## Commands

From repositoryroot:

```bash
cargo test -p ps-nginx --lib file_fetch::tests -- --test-threads=1
cargo test -p ps-nginx --lib -- --test-threads=1
cargo test -p ps-html --test async_progressive
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
```

Retrieve originalfixture with PageSpeed disabled for that request, then run the isolated oracle with a configured, ABI-matched NGINX:

```bash
curl -fsS 'https://rustpagespeed.com/wp-content/uploads/2026/11/header.jpg?PageSpeed=off' -o /tmp/header-original.jpg
python3 tools/verify_https_file_images.py \
 --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx \
 --module target/release/libps_nginx.so --image /tmp/header-original.jpg \
 --output /tmp/https-image-result.json
ssh -o BatchMode=yes root@157.245.148.68 'python3 -u -' < tools/benchmark_wordpress_html_cache.py
```

Original712226-bytefixtureSHA is recorded in isolated.json; future image edits may change sizes. Existing cachedHTML variants can retain older originalURLs until expiry/purge, so test a freshrequestvariant when checking asyncprogression. The checked normal browservariant now rewrites without a purge. Rebuild Debian package from current source before reinstalling to retain this fix. Rollback: restore savedmodule through new inode/rename, validate nginx-t and restart; doing so restores the known localHTTPS limitation.

## Installed231832f retest, 2026-11-25 05:16 UTC

Read-only benchmark,3warmups then30confirmedlocalHITs pergroup: freshTLSidentitymedian14.761ms/p9515.250;freshTLSzstd14.8785ms/p9516.351;reusedTLSidentity0.86264ms. All90localmeasuredrequests200/HIT; zstddecoded. Public10requestsall200:8HITs median307.498ms and2MISS346.840/381.699ms; mixedall-requestmedian336.8545ms. Publicnetworksample is not uniformlywarm and is not a causal codecomparison. Rewrittenheader remainsimage/webp80042bytes; bothservicesactive,noresidualFastCGIconnections. Headerreportsnginx1.31.10+231832f. No serverchanges. Raw latest-latency.jsonl/latest-public.json retained. Exactlocalcommand remains ssh stdin benchmark_wordpress_html_cache.py as above.

# Actual Chrome/Firefox HTML cache groups — 2026-11-26

User requested that the attached Chrome158/Firefox147 desktop navigation pair share public entries. Existing default grouping normalized UA/Accept/client hints but retained transport, refresh and GPC inputs; it also keyed irrelevant in-place WebP support. Actual Firefox navigation Accept omits image MIME advertisements and revealed lossless/alpha support incorrectly requiring explicit WebP despite known UA decoding support.

Changes:

- In public grouping only, normalize Connection/Keep-Alive/TE/Sec-GPC/DNT and plain request max-age=0 unless origin explicitly names that header in Vary. Original headers are forwarded unchanged; this only changes cache-key copies.
- Permit early lookup for plain max-age=0 in public mode. Additional no-cache/no-store directives and Pragma continue through origin; exact grouping-off mode retains old behavior.
- Infer lossless/alpha WebP for known capable navigation UAs via existing rewritten-support logic; explicit image/webp;q=0 and unsupported UAs remain excluded. In-place resource Accept negotiation remains unchanged.
- Remove the unused in-place-only capability from HTML identity; account for RecompressWebp as well as ConvertToWebpAnimated when animated capability changes HTML output.
- Version grouped identity to public-browser-v2 so previous hints/bodies are not accidentally reused. Default grouping stays on; mobile/tablet/desktop and incompatible effective capabilities stay separate. Explicit Vary on normalized additional headers still separates entries.

Baseline: adding captured GPC/transport/refresh differences to the grouping regression failed. Key normalization passed that regression, but using actual Firefox navigation Accept exposed lossless/in-place differences; actual-accept-before.log preserves the failing capability vectors. Both defects are repaired and the final exact-UA/Accept regression passes.

Commands (repository root):

```bash
cargo test --locked -p ps-html --lib public_browser_grouping_defaults_on
cargo test --locked -p ps-html --lib --test html_cache_cookies --test html_cache_vary --test image_url_encoder
cargo test --locked -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
python3 tools/verify_early_html_cache.py --nginx ../rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module "$PWD/target/release/libps_nginx.so" --rum-only --analytics-early fixed --browser-groups --output docs/performance/browser-group-normalization/oracle.json
```

Results: HTML210 + cookie4 + Vary8 + image URL55 tests passed; NGINX294 passed; matched release build passed. Final exact Chrome158/Firefox147 test passes (last edit was test-only). New table tests verify early/normal sharing for each normalized header, explicit origin Vary separation, original header preservation, exact mode, and revalidation controls. Existing private/auth/cookie and incompatible format/device assertions retained.

Real isolated NGINX/FastCGI oracle: one Chrome warm-up on /browser-groups, then ten alternating Firefox/Chrome header profiles all HIT with identical HTML and exactly one origin request total. Uses captured navigation differences with both encoding values set to identity for the uncompressed fixture. Includes Firefox GPC/TE/Connection, Chrome max-age=0/client hints, actual UA and Accept strings. Separate RUM/analytics/private-origin controls pass. This is a header replay, not interactive browser execution.

Synthetic origin delay25ms; alternating cross-browser HIT median 0.276 ms, ordinary warm HIT median 0.369 ms. No production before/after latency claim. No live deployment or purge. Installation requires matching NGINX; new grouped namespace needs initial warming.

Privacy/content contract: GPC/DNT reach the origin unchanged. Applications varying public HTML on them must declare Vary accordingly or turn public browser grouping off. Authorization, private cookies, Set-Cookie, private/no-store origin policy and sampling still gate reuse. This does not force every request to HIT.

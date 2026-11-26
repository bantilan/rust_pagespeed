# Empty font beacon POST fix — 2026-11-26

The Firefox HAR contained `navigator.sendBeacon(url)` with font fields in the query and Content-Length0; endpoint returned500. Receiver expected body buffers even when empty. NGINX may use no buffers or a sentinel with nullpos/last. Completed zero-length requests now return empty bytes to the existing query/body merge and unchanged evidence validation. Missing buffers for an announced positive-length body and existing nonempty read failures/limits still fail. No filter/sampling/cache/RUM disabled.

Before: isolated matched NGINX reproduced500 (`before.log/json`, expected regression failure against820da4b module). First guard-only attempt still failed on the sentinel representation; failures retained as guard-only-failed-*; final explicit-zero-body path fixes both cases. Final module passes real issued-nonce font reporting with204, persisted confirmedfont evidence, unissuednonce rejection, replay refusal before/afterreload, and evidence preservation across overlappingworkers. Existing critical-image POST-body acceptance/replay/reload path also passes. Plain and zstd/earlyHTMLcache variants both pass.294NGINXlibrary tests pass. Matched1.31.10releasebuild passes9.80s. Module/config hashes/prefixes in after*.json. No production deployment, reload or purge in this task; isolated processes shut down by harness.

Commands:

```sh
cargo test --locked -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
python3 tools/verify_beacon_reload.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module "$PWD/target/release/libps_nginx.so" --font-query-post --early-html-cache --output docs/performance/font-empty-beacon/after.json
# Repeat with --zstd and output after-zstd.json
```

The new test obtains the nonce from actual generated font-collector HTML, not synthetic registry injection. Disk evidence oracle verifies learning rather than assuming204meansaccepted. Invalid nonce requests still return204 by existing protocol but cannot update evidence. Existing instrumentation executes unchanged. No browser-runtime JS test newly claimed: wire transport is reproduced through HTTPclient. No latency improvement claim: this fixes a500 and enablesfontlearning. Intentional BYPASS for fresh critical-beacon sampling remains; HAR contained only one HTMLnavigation so repeatedsampling cause is still unproven. Do not claim everyHIT→BYPASS transition fixed.

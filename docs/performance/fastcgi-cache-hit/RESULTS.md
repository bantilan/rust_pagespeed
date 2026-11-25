# FastCGI HTML cache HIT stall: 2026-11-25

Fixed and deployed to https://rustpagespeed.com/ with user authorization. No filters, HTML cache, or Zstd disabled.

## Cause and evidence

Our HTML HIT body-filter branch discarded the origin chain without consuming its buffers. NGINX upstream buffers remained busy. PHP-FPM workers finished output, then waited in recvfrom for NGINX to close the connection; NGINX sockets stayed CLOSE-WAIT with 32745 unread bytes (65513 during the unbuffered trial). All five PHP workers became occupied, and further requests stalled. NGINX workers were idle in epoll_wait, not consuming CPU. The PHP log reported max_children reached. PHP reload only temporarily restored responses; exhaustion recurred after roughly five requests.

Before: public Zstd TTFB9454.977ms; later local/public requests timed out after12s. These are incident observations, not a clean baseline distribution. Trial fastcgi_bufferingoff failed and was reverted. Raising pool size or upstream timeout would mask this ownership bug.

## Change and checks

`crates/ps-nginx/src/filters.rs`: HIT branch now advances every origin buffer's memory/file cursors and clears last_buf before passing NULL downstream. Cached output has already completed through the base fetch. Origin bytes are drained without rewriting them or altering cached bytes. New regression covers a64KiB memory buffer, file buffer, final flag, NULL and repeat consumption.

Commands (repository root):

```bash
cargo test -p ps-nginx --lib filters::html_rewrite::tests::finish_consumes_fabricated_chain -- --test-threads=1
rustfmt --edition 2021 crates/ps-nginx/src/filters.rs
cargo test -p ps-nginx --lib -- --test-threads=1
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
git diff --check
```

Existing baseline test passed. Full292NGINX tests passed in2.59s; release build20.51s. Two existing unused-import/mut warnings remain. Build and test logs retained alongside this report.

## Live verification

Sequential fresh curl TLS connections on the server with `--resolve rustpagespeed.com:443:127.0.0.1`, five-second bound, `--compressed`, explicit identity/zstd headers. Each encoding20requests, body parsed for cache comment, successful curl decoding checked. No excluded warmups: identity first sample588ms is included. Results measure full PHP/WordPress origin and TLS/request processing, not isolated PageSpeed overhead. HTML cache lookup still occurs after origin headers.

| Check | Requests | Median TTFB | p95 TTFB | Result |
|---|---:|---:|---:|---|
| Identity |20|75.741ms|97.472ms|All200/HIT|
| Zstd |20|73.473ms|81.268ms|All200/HIT, decoded|
| Unique query MISS |1|83.347ms|—|200/MISS|
| Same query HIT |1|83.077ms|—|200/HIT|
| Public network Zstd |1|410.030ms|—|200/HIT, decoded|

Identity66778wirebytes; Zstd12644bytes. `ss -tnp | grep :9000` showed no lingering FastCGI connections after verification. Both services active; no slowlog traces. Short sequential check, not a sustained concurrency/load test. Raw rows in live-verification.jsonl; MISS/HIT and health in post-fix-health.log.

Reproduce one request:

```bash
curl -sS --compressed --max-time 5 \
  --resolve rustpagespeed.com:443:127.0.0.1 \
  -H 'Accept-Encoding: zstd' \
  -w '\nhttp=%{http_code} ttfb=%{time_starttransfer} total=%{time_total}\n' \
  https://rustpagespeed.com/
```

## Deployment and rollback records

Matched NGINX1.31.10 module SHA256 `13bf793865008b277dfac916a05ff167fb72538741394ebc476d6512764b36c2`. NGINX version header retains package label7f7e686; it does not identify the patched module. Existing Debian package has NOT been rebuilt and reinstalling it would revert this fix.

Remote changes:

- `/usr/lib/nginx/modules/ngx_pagespeed_rust.so`: backed up to `.so.before-fastcgi-fix-20261125`, replaced using a new inode/rename; `nginx -t` passed. Restarted NGINX to release already-stalled connections and reloaded php-8.6-fpm.
- `/opt/php-8.6/etc/php-fpm.d/www.conf`: backed up to `.before-slowlog-20261125`; added request_slowlog_timeout5s and slowlog `/opt/php-8.6/var/log/www-slow.log`. PHP config test passed before reload. Diagnostic retained.
- Site backed up to `/etc/nginx/rustpagespeed.com.before-fastcgi-fix-20261125`; trial bufferingoff removed. No final site-setting changes.

Emergency module rollback (would restore the known bug): copy the backup to a new file, rename it over the active module, validate `nginx -t`, then restart NGINX and reload PHP-FPM. Do not overwrite the inode of a mapped active module in place.

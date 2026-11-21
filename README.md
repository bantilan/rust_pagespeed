# Rust Pagespeed

A Rust port of PageSpeed (HTML/CSS/JS/image optimization, caching, HTTP
helpers, stats, configuration, admin pages, and an NGINX module backend).

## Status: working, under active development

The workspace test suite passes, live compatibility checks against the C++
module pass, and the module serves rewritten traffic (HTML, images with
WebP/AVIF negotiation, proxy mode). It is still maturing: keep backups of
your C++ configuration when switching over.

## Original sources are preserved

The existing checkouts are untouched and remain the source of truth:

- `../mod_pagespeed` (read-only)
- `../ngx_pagespeed` (read-only)
- `../ngx_psol` (read-only)
- `../ngx_compiler` (read-only)

Nothing in this repository modifies those directories. Port work here must
be verified against them.

## Layout

Cargo workspace (`resolver = "2"`):

| Crate      | Purpose                                  |
|------------|------------------------------------------|
| `ps-html`  | HTML parsing / rewriting helpers         |
| `ps-css`   | CSS parsing / minification helpers       |
| `ps-js`    | JavaScript handling helpers              |
| `ps-image` | Image optimization helpers               |
| `ps-cache` | Cache backends                           |
| `ps-http`  | HTTP fetch / header helpers              |
| `ps-stats` | Statistics / instrumentation             |
| `ps-config`| Configuration / rewrite options         |
| `ps-admin` | Admin pages / status endpoints           |
| `ps-nginx` | NGINX FFI bindings (see safety note)     |
| `ps-compat` | Compatibility test harness              |

## Safety policy

Every crate carries `#![forbid(unsafe_code)]` **except** `ps-nginx`, which is
the documented FFI-only exception: it alone may contain `unsafe` code, and
only for `extern "C"` declarations and minimal wrappers around the NGINX C
API. No optimization logic may live behind `unsafe` in `ps-nginx`.

Release builds use `opt-level = 3`, full LTO, and `codegen-units = 1`
(see the workspace `Cargo.toml`).

## Building and testing

Requires a Rust stable toolchain (scaffolded against stable):

```sh
cargo metadata --format-version 1 > /dev/null
cargo test --workspace
cargo build --workspace --release
```

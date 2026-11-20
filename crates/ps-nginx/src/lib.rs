//! NGINX module backend.
//!
//! Ports `ngx_pagespeed/src` (module/conf/phase hooks, threading
//! bridge, native fetcher) plus the nginx-free helpers from
//! `pagespeed/nginx/` (body accounting, gzip-config rollback,
//! in-place URL agreement, spooled request bodies). FFI goes
//! through the pinned `ngx`/`nginx-sys` crates, bound against
//! the staged nginx 1.31.8 tree (`NGINX_SOURCE_DIR`, defaulted
//! in the workspace `.cargo/config.toml`): this crate ships no
//! hand-written ABI declarations.
//!
//! # Safety policy: FFI-only exception
//!
//! This is the only crate in the workspace allowed to contain
//! `unsafe` code, and that allowance is strictly limited to
//! `extern "C"` declarations and minimal wrappers around the
//! NGINX C API (plus test-only simulations of nginx-owned
//! memory, each with a `SAFETY:` comment). All optimization
//! logic lives in the safe sibling crates, each of which
//! carries `#![forbid(unsafe_code)]`. No new `unsafe` may be
//! added here for anything other than NGINX FFI.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod caching;
pub mod conf;
pub mod directives;
pub mod factory;
pub mod fetch;
pub mod fetch_body;
pub mod filters;
pub mod gzip_rollback;
pub mod headers;
pub mod inplace_url_agreement;
pub mod log_bridge;
pub mod message;
pub mod mock;
pub mod module;
pub mod options;
pub mod request;
pub mod recording;
pub mod request_body;
pub mod resource;
pub mod send;
pub mod routing;
pub mod scheduler;
pub mod script;
pub mod self_fetch;

/// Name under which this module registers itself with NGINX.
#[must_use]
pub fn module_name() -> &'static str {
    "ngx_pagespeed_rust"
}

/// Supported nginx range, as dotted versions (`nginx-gap-1`).
pub const SUPPORTED_NGINX: &[&str] = &["1.31.8"];

/// Numeric nginx version these bindings were generated against
/// (`nginx_version`, e.g. 1031008 for 1.31.8).
#[must_use]
pub fn bound_nginx_version() -> u32 {
    ngx::ffi::nginx_version
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_name_is_stable() {
        assert_eq!(module_name(), "ngx_pagespeed_rust");
    }

    #[test]
    fn bindings_target_pinned_nginx() {
        // `ngx`/`nginx-sys` versions are pinned in Cargo.toml; the
        // generated bindings must match the staged 1.31.8 tree.
        assert_eq!(1031008, bound_nginx_version());
        assert_eq!(["1.31.8"], SUPPORTED_NGINX);
    }
}

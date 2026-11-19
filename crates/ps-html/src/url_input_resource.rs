//! URL-backed input resources (`UrlInputResource`).
//!
//! Fetches a URL through the cacheable load flow. Unauthorized
//! domains cache under `unauth://` keys (kept apart from the
//! authorized namespace) and authorize their origin for the session
//! just before fetching; every fetch carries `PageSpeed: off` so
//! origin-side rewriting stands down.

use std::rc::Rc;

use ps_http::content_type::ContentType;
use ps_http::google_url::{GoogleUrl, PORT_UNSPECIFIED};
use ps_http::request::{RequestContext, RequestHeaders};

use super::cacheable_resource::{CacheableResourceBase, ResourceHooks};
use super::resource::Resource;
use super::server_context::ResourceDriver;

/// URL fetch hooks: session authorization plus the rewriting opt-out.
#[derive(Debug, Clone, Default)]
pub struct UrlInputHooks {
    origin: String,
}

impl ResourceHooks for UrlInputHooks {
    fn prepare_request(&self, ctx: &mut RequestContext, headers: &mut RequestHeaders) {
        if !self.origin.is_empty() {
            ctx.add_session_authorized_fetch_origin(&self.origin);
        }
        // Origin-side rewriting must not run for rewriter fetches.
        headers.headers_mut().add("PageSpeed", "off");
    }
}

/// A URL-backed input resource (`UrlInputResource`).
pub type UrlInputResource = CacheableResourceBase<UrlInputHooks>;

/// Computes the cache key (`GetCacheKey`): the URL itself when
/// authorized, else an `unauth://` (or `unauths://`) alias. Non-web
/// URLs cannot key the unauthorized namespace and panic, like the
/// C++ `CHECK(false)`.
#[must_use]
pub fn url_cache_key(url: &str, authorized: bool) -> String {
    if authorized {
        return url.to_string();
    }
    let gurl = GoogleUrl::new(url);
    let prefix = if gurl.scheme() == "https" {
        "unauths://"
    } else if gurl.scheme() == "http" {
        "unauth://"
    } else {
        panic!("unauthorized non-web URL has no cache key: {url}");
    };
    format!("{prefix}{}{}", gurl.host_and_port(), gurl.path_and_leaf())
}

/// Builds a URL input resource (`UrlInputResource::UrlInputResource`).
#[must_use]
pub fn make_url_input_resource(
    driver: Rc<ResourceDriver>,
    content_type: Option<ContentType>,
    url: &str,
    authorized: bool,
) -> UrlInputResource {
    let cache_key = url_cache_key(url, authorized);
    let mut origin = String::new();
    if !authorized {
        let gurl = GoogleUrl::new(url);
        if gurl.is_web_valid() && gurl.int_port() == PORT_UNSPECIFIED {
            // Default ports read as unspecified, so `:80`/`:443`
            // spellings still authorize; anything explicit does not.
            // The origin is canonical (default ports stripped), like
            // GURL's `Origin()`.
            origin = GoogleUrl::new(gurl.canonical_spec()).origin().to_string();
        }
    }
    let mut resource = CacheableResourceBase::new(
        driver,
        "url_input_resource",
        url,
        &cache_key,
        content_type,
        UrlInputHooks { origin },
    );
    resource.set_is_authorized_domain(authorized);
    resource
}

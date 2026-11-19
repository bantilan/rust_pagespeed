//! Google Font Service input resources
//! (`GoogleFontServiceInputResource`).
//!
//! UA-keyed loader-CSS fetches: the cache key carries the normalized
//! user agent, the fetch forwards the raw one, and the font
//! service's `private` responses are swizzled into cacheability.

use std::rc::Rc;

use ps_http::content_type::mime_to_content_type;
use ps_http::google_url::GoogleUrl;
use ps_http::names::header;
use ps_http::request::{RequestContext, RequestHeaders};
use ps_http::response::ResponseHeaders;
use ps_http::user_agent_normalizer::normalize_with_all;

use super::cacheable_resource::{CacheableResourceBase, ResourceHooks};
use super::server_context::ResourceDriver;

/// Font-service host (`kFontApiHost`).
const FONT_API_HOST: &str = "fonts.googleapis.com";

/// Stat prefix (`"font_service_input_resource"`).
pub const STAT_PREFIX: &str = "font_service_input_resource";

/// A font-service input resource (`GoogleFontServiceInputResource`).
pub type FontServiceInputResource = CacheableResourceBase<FontServiceHooks>;

/// Per-fetch font hooks: UA forwarding plus response swizzling.
#[derive(Debug, Clone, Default)]
pub struct FontServiceHooks {
    user_agent: String,
    is_https: bool,
}

impl ResourceHooks for FontServiceHooks {
    fn prepare_request(&self, ctx: &mut RequestContext, headers: &mut RequestHeaders) {
        // We want to give the font service the UA the client used, so
        // that it can optimize for the visitor's browser, and not
        // something like Serf/1.1 mod_pagespeed/x.y
        headers
            .headers_mut()
            .replace(header::USER_AGENT, &self.user_agent);
        ctx.add_session_authorized_fetch_origin(if self.is_https {
            "https://fonts.googleapis.com"
        } else {
            "http://fonts.googleapis.com"
        });
    }

    fn prepare_response_headers(&self, headers: &mut ResponseHeaders) {
        // Refuse to deal with anything but CSS.
        if !headers
            .determine_content_type()
            .is_some_and(|content_type| content_type.is_css())
        {
            headers.set_status_code(406);
        }
        // The resource is served with cache-control: private; we need
        // to swizzle that in order to save it in the cache.
        headers
            .headers_mut()
            .remove(header::CACHE_CONTROL, "private");
        // Remove cookies just in case.
        headers.sanitize();
    }
}

impl CacheableResourceBase<FontServiceHooks> {
    /// Whether the URL names the font service (`IsFontServiceUrl`).
    #[must_use]
    pub fn is_font_service_url(url: &GoogleUrl) -> bool {
        url.is_web_valid() && url.host() == FONT_API_HOST
    }

    /// Builds a font-service resource (`Make`), `None` for other URLs.
    #[must_use]
    pub fn make_font_service(
        parsed_url: &GoogleUrl,
        driver: &Rc<ResourceDriver>,
    ) -> Option<FontServiceInputResource> {
        if !Self::is_font_service_url(parsed_url) {
            return None;
        }
        // Compute cache key, incorporating the UA string --- but
        // normalize it first, to cut down on irrelevant noise.
        let ua = normalize_with_all(
            driver.server_context().user_agent_normalizers(),
            &driver.user_agent(),
        );
        let with_ua = parsed_url.copy_and_add_query_param("X-PS-UA", &ua);
        let spec = with_ua.spec().to_string();
        let (cache_key, is_https) = if let Some(rest) = spec.strip_prefix("http://") {
            (format!("gfnt://{rest}"), false)
        } else if let Some(rest) = spec.strip_prefix("https://") {
            (format!("gfnts://{rest}"), true)
        } else {
            // Huh?
            return None;
        };
        Some(FontServiceInputResource::new(
            driver.clone(),
            STAT_PREFIX,
            parsed_url.spec(),
            &cache_key,
            mime_to_content_type("text/css"),
            FontServiceHooks {
                user_agent: driver.user_agent(),
                is_https,
            },
        ))
    }
}

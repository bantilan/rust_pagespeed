//! Local-storage cache (`LocalStorageCacheFilter`, id `ls`).
//!
//! Marks inlined CSS/images for client-side caching, injects the
//! local-storage runtime once per page, and swaps marked elements
//! for restore snippets when the client's cookie already holds them.
use std::rc::Rc;

use ps_config::filter::Filter;
use ps_http::google_url::GoogleUrl;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::server_context::ResourceDriver;

/// Filter id (`kLocalStorageCacheId`).
pub const FILTER_ID: &str = "ls";
/// Candidates stat (`kNumLocalStorageCacheCandidatesFound`).
pub const CANDIDATES_FOUND: &str = "num_local_storage_cache_candidates_found";
/// Stored-total stat (`kNumLocalStorageCacheStoredTotal`).
pub const STORED_TOTAL: &str = "num_local_storage_cache_stored_total";
/// Stored-images stat (`kNumLocalStorageCacheStoredImages`).
pub const STORED_IMAGES: &str = "num_local_storage_cache_stored_images";
/// Stored-CSS stat (`kNumLocalStorageCacheStoredCss`).
pub const STORED_CSS: &str = "num_local_storage_cache_stored_css";
/// Candidates added (`kCandidatesAdded`).
pub const CANDIDATES_ADDED: &str = "num_local_storage_cache_candidates_added";
/// Candidates removed (`kCandidatesRemoved`).
pub const CANDIDATES_REMOVED: &str = "num_local_storage_cache_candidates_removed";

/// Cookie name (`kLscCookieName`).
pub const LSC_COOKIE_NAME: &str = "_GPSLSC";
/// Runtime initializer (`kLscInitializer`).
const LSC_INITIALIZER: &[u8] = b"pagespeed.localStorageCacheInit();";

/// Per-resource inline state (`InlineState`).
#[derive(Debug, Default)]
pub struct InlineState {
    initialized: bool,
    enabled: bool,
    url: String,
}

impl InlineState {
    /// Fresh state.
    #[must_use]
    pub fn new() -> Self {
        InlineState::default()
    }
}

/// Local-storage cache filter.
pub struct LocalStorageCacheFilter {
    driver: Rc<ResourceDriver>,
    script_needs_inserting: bool,
    script_inserted: bool,
}

impl LocalStorageCacheFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        LocalStorageCacheFilter {
            driver,
            script_needs_inserting: false,
            script_inserted: false,
        }
    }

    /// Inserts the runtime before an element (`InsertOurScriptElement`).
    fn insert_script_element(&mut self, dom: &mut Dom, lexer: &Lexer, before: NodeId) {
        let debug = self
            .driver
            .options()
            .enabled(ps_config::filter::Filter::Debug);
        let js = crate::static_assets::get_asset(
            crate::static_assets::StaticAsset::LocalStorageCacheJs,
            debug,
        );
        let mut initialized = js.to_vec();
        initialized.extend_from_slice(LSC_INITIALIZER);
        let parent = dom.arena_node(before).parent();
        let script = dom.new_element(parent, b"script");
        dom.insert_node_before_node(before, script);
        crate::filters::common::add_js_to_element(
            dom,
            lexer,
            script,
            &initialized,
            false,
            crate::filters::common::XhtmlStatus::IsNotXhtml,
        );
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        self.script_inserted = true;
    }

    /// Marks a resource storable (`AddStorableResource`): records the
    /// absolute URL on the element unless LSC is off (or, without
    /// `skip_cookie_check`, its hash already sits in the cookie, in
    /// which case the caller must skip inlining).
    pub fn add_storable_resource(
        url: &[u8],
        driver: &Rc<ResourceDriver>,
        skip_cookie_check: bool,
        dom: &mut Dom,
        element: crate::element::NodeId,
        state: &mut InlineState,
    ) -> bool {
        if !state.initialized {
            state.enabled = driver
                .options()
                .enabled(ps_config::filter::Filter::LocalStorageCache);
            if state.enabled {
                let base = driver.base_url().clone();
                let resolved = GoogleUrl::resolve(&base, &String::from_utf8_lossy(url));
                state.url = if resolved.is_web_valid() {
                    resolved.spec().to_string()
                } else {
                    String::from_utf8_lossy(url).into_owned()
                };
            }
            state.initialized = true;
        }
        if !state.enabled {
            return false;
        }
        let mut add_the_attr = skip_cookie_check;
        if !skip_cookie_check {
            let hash = Self::generate_hash_from_url_and_element(
                driver,
                state.url.as_bytes(),
                dom,
                element,
            );
            if Self::is_hash_in_cookie(driver, LSC_COOKIE_NAME, &hash) {
                add_the_attr = true;
            }
        }
        if add_the_attr {
            dom.add_attribute(element, b"data-pagespeed-lsc-url", state.url.as_bytes());
        }
        add_the_attr
    }

    /// Adds the hash/expiry attributes after inlining
    /// (`AddLscAttributes`). Must run before dimensions are stripped:
    /// the hash covers them.
    pub fn add_lsc_attributes(
        url: &[u8],
        cached: &crate::metadata::CachedResult,
        driver: &Rc<ResourceDriver>,
        dom: &mut Dom,
        element: crate::element::NodeId,
    ) -> bool {
        if !driver.options().enabled(Filter::LocalStorageCache) {
            return false;
        }
        // Don't add the other attributes if we don't have a
        // data-pagespeed-lsc-url.
        if dom
            .find_attribute(element, Keyword::DataPagespeedLscUrl)
            .is_none()
        {
            return false;
        }
        driver.server_context().variable(CANDIDATES_ADDED).add(1);
        let base = driver.base_url().clone();
        let resolved = GoogleUrl::resolve(&base, &String::from_utf8_lossy(url));
        let lsc_url = if resolved.is_web_valid() {
            resolved.spec().to_string()
        } else {
            String::from_utf8_lossy(url).into_owned()
        };
        let hash =
            Self::generate_hash_from_url_and_element(driver, lsc_url.as_bytes(), dom, element);
        dom.add_attribute(element, b"data-pagespeed-lsc-hash", hash.as_bytes());
        if let Some(input) = cached.input.first() {
            if let Some(expiry_ms) = input.expiration_time_ms {
                let expiry = ps_http::time::format_http_date(expiry_ms);
                dom.add_attribute(element, b"data-pagespeed-lsc-expiry", expiry.as_bytes());
            }
        }
        true
    }

    /// Removes the local-storage attributes (`RemoveLscAttributes`).
    pub fn remove_lsc_attributes(
        driver: &Rc<ResourceDriver>,
        dom: &mut Dom,
        element: crate::element::NodeId,
    ) {
        if !driver.options().enabled(Filter::LocalStorageCache) {
            return;
        }
        dom.delete_attribute(element, Keyword::DataPagespeedLscUrl);
        dom.delete_attribute(element, Keyword::DataPagespeedLscHash);
        dom.delete_attribute(element, Keyword::DataPagespeedLscExpiry);
        driver.server_context().variable(CANDIDATES_REMOVED).add(1);
    }

    /// Hashes a URL plus img dimensions (`GenerateHashFromUrlAndElement`).
    fn generate_hash_from_url_and_element(
        driver: &Rc<ResourceDriver>,
        url: &[u8],
        dom: &mut Dom,
        element: crate::element::NodeId,
    ) -> String {
        let raw = |keyword: Keyword| -> Option<Vec<u8>> {
            dom.find_attribute(element, keyword).and_then(|index| {
                dom.element_attributes(element)[index]
                    .escaped_value()
                    .map(<[u8]>::to_vec)
            })
        };
        let width = raw(Keyword::Width);
        let height = raw(Keyword::Height);
        let mut hash_string = url.to_vec();
        if let Some(width) = width {
            hash_string.extend_from_slice(b"!w=");
            hash_string.extend_from_slice(&width);
        }
        if let Some(height) = height {
            hash_string.extend_from_slice(b"!h=");
            hash_string.extend_from_slice(&height);
        }
        driver.server_context().hasher().hash(&hash_string)
    }

    /// Whether a hash sits in the named cookie (`IsHashInCookie`).
    /// Cookies re-parse per call (C++ caches them per filter; the
    /// pure function reads identically).
    fn is_hash_in_cookie(driver: &Rc<ResourceDriver>, cookie_name: &str, hash: &str) -> bool {
        let headers = driver.request_headers();
        let Some(values) = headers.headers().lookup("Cookie") else {
            return false;
        };
        let mut prefix = cookie_name.as_bytes().to_vec();
        prefix.push(b'=');
        for value in values {
            for cookie in value.split(';') {
                let mut cookie = cookie.trim();
                // TrimQuote: strip one layer of surrounding quotes.
                if cookie.len() >= 2 && cookie.starts_with('"') && cookie.ends_with('"') {
                    cookie = &cookie[1..cookie.len() - 1];
                }
                if cookie.len() < prefix.len()
                    || !cookie[..prefix.len()]
                        .as_bytes()
                        .eq_ignore_ascii_case(&prefix)
                {
                    continue;
                }
                let hashes = &cookie[prefix.len()..];
                for hash_value in hashes.split('!') {
                    if !hash_value.is_empty() && hash_value == hash {
                        return true;
                    }
                }
                break;
            }
        }
        false
    }

    /// Serializes the remaining img attributes for the snippet
    /// (`ExtractOtherImgAttributes`).
    fn extract_other_img_attributes(dom: &mut Dom, element: crate::element::NodeId) -> Vec<u8> {
        let mut out = Vec::new();
        let attributes = dom.element_attributes(element).clone();
        for (index, attribute) in attributes.iter().enumerate() {
            match attribute.keyword() {
                Keyword::DataPagespeedLscUrl
                | Keyword::DataPagespeedLscHash
                | Keyword::DataPagespeedLscExpiry
                | Keyword::DataPagespeedNoDefer
                | Keyword::PagespeedNoDefer
                | Keyword::Src => continue,
                _ => {}
            }
            let (decoded, _) = dom.decoded_attribute(element, index);
            out.extend_from_slice(b", \"");
            out.extend_from_slice(&crate::js_escape::escape_to_js_string_literal(
                attribute.name().value(),
                false,
            ));
            out.extend_from_slice(b"=");
            if let Some(value) = decoded {
                out.extend_from_slice(&crate::js_escape::escape_to_js_string_literal(
                    &value, false,
                ));
            }
            out.extend_from_slice(b"\"");
        }
        out
    }
}

impl HtmlFilter for LocalStorageCacheFilter {
    fn name(&self) -> &'static str {
        "LocalStorageCache"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.script_inserted = false;
        self.script_needs_inserting = false;
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        // The css_inline_filter and image_rewrite_filter can add the LSC URL
        // to the inlined resource, indicating that we have to insert our JS
        // for them.
        match dom.element_keyword(id) {
            Keyword::Img | Keyword::Link | Keyword::Style => {
                // C++ only checks img/link: there the CSS render
                // lands after this pass, so the marked node is
                // still the live <link>. Here the sync inline
                // already replaced it with a marked <style>, which
                // is the inlined resource carrying the marker.
                if dom
                    .find_attribute(id, Keyword::DataPagespeedLscUrl)
                    .is_some()
                {
                    // Note that we might end up not needing the inserted
                    // script because the img/link might not be able to be
                    // inlined. So be it.
                    self.script_needs_inserting = true;
                }
            }
            _ => {}
        }

        // We need to insert our javascript before the first element that
        // uses it.
        if self.script_needs_inserting && !self.script_inserted {
            self.insert_script_element(dom, lexer, id);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        // An <img> or <link> that has a data-pagespeed-lsc-url attribute, and
        // whose URL's hash is in the LSC cookie, needs to be replaced by a JS
        // snippet.
        let is_img = dom.element_keyword(id) == Keyword::Img;
        let is_link = dom.element_keyword(id) == Keyword::Link;
        if !(is_img || is_link) {
            return;
        }
        let Some(url_index) = dom.find_attribute(id, Keyword::DataPagespeedLscUrl) else {
            return;
        };
        let (Some(url), _) = dom.decoded_attribute(id, url_index) else {
            return;
        };
        let server = self.driver.server_context().clone();
        server.variable(CANDIDATES_FOUND).add(1);
        let hash = Self::generate_hash_from_url_and_element(&self.driver, &url, dom, id);
        if !Self::is_hash_in_cookie(&self.driver, LSC_COOKIE_NAME, &hash) {
            return;
        }
        server.variable(STORED_TOTAL).add(1);
        let base = self.driver.base_url().clone();
        let resolved = GoogleUrl::resolve(&base, &String::from_utf8_lossy(&url));
        let lsc_url = if resolved.is_web_valid() {
            resolved.spec().to_string()
        } else {
            String::from_utf8_lossy(&url).into_owned()
        };
        // The URL comes from a page-authored attribute and is spliced into
        // a double-quoted JS string, so escape it first.
        let escaped_lsc_url =
            crate::js_escape::escape_to_js_string_literal(lsc_url.as_bytes(), false);
        let mut snippet = b"pagespeed.localStorageCache.".to_vec();
        if is_img {
            server.variable(STORED_IMAGES).add(1);
            snippet.extend_from_slice(b"inlineImg(\"");
            snippet.extend_from_slice(&escaped_lsc_url);
            snippet.extend_from_slice(b"\", \"");
            snippet.extend_from_slice(hash.as_bytes());
            snippet.extend_from_slice(b"\"");
            snippet.extend_from_slice(&Self::extract_other_img_attributes(dom, id));
            snippet.extend_from_slice(b");");
        } else {
            server.variable(STORED_CSS).add(1);
            snippet.extend_from_slice(b"inlineCss(\"");
            snippet.extend_from_slice(&escaped_lsc_url);
            snippet.extend_from_slice(b"\");");
        }
        let parent = dom.arena_node(id).parent();
        let script_element = dom.new_element(parent, b"script");
        dom.add_escaped_attribute(
            script_element,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        if dom.replace_node(id, script_element) {
            let text = dom.new_leaf(
                Some(script_element),
                crate::element::LeafKind::Characters,
                snippet,
            );
            dom.append_child(Some(script_element), text);
        }
    }
}

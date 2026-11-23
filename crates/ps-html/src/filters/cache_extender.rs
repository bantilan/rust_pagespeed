//! Cache extension (`CacheExtender`, `ce`).
//!
//! Rewrites cacheable image/CSS/JS (and optionally PDF) URLs onto
//! long-lived hashed names, serving the original bytes (CSS rebased
//! to the output base) under them. Outputs are on-the-fly: serving
//! reconstructs instead of consulting the HTTP cache.
//!
//! NOTE: collection sharing across filters waits for multi-filter
//! chains (each scan builds its own srcset table).

use std::cell::RefCell;
use std::rc::Rc;

use ps_cache::timer::MONTH_MS;
use ps_config::options::Category;
use ps_http::content_type::ContentTypeKind;
use ps_http::fetch::AsyncFetch;
use ps_http::google_url::GoogleUrl;
use ps_http::response::{RequestProperties, ValidatorOption, VaryOption};

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::metadata::{CachedResult, OutputPartitions};
use crate::name::Keyword;
use crate::output_resource::{OutputResource, OutputResourceKind};
use crate::parser::Dom;
use crate::resource::{InputResource, Resource};
use crate::resource_tag_scanner::scan_element;
use crate::rewrite_context::{
    add_link_rel_canonical, RewriteResult, SingleRewriteBehavior, SingleRewriteContext,
};
use crate::rewrite_filter::RewriteFilter;
use crate::server_context::{
    CssResolutionStatus, InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver,
};
use crate::srcset::{SrcSetCollection, SrcSetSlot, MAX_SLOTS};
use crate::url_namer::ProxyExtent;

/// Filter id (`CacheExtender::kFilterId`).
pub const FILTER_ID: &str = "ce";

/// Extension counter (`CacheExtender::kCacheExtensions`).
pub const CACHE_EXTENSIONS: &str = "cache_extensions";

/// Uncacheable-input counter (`CacheExtender::kNotCacheable`).
pub const NOT_CACHEABLE: &str = "not_cacheable";

/// Whether an input earns a canonical link (images and PDFs only;
/// nobody searches for CSS/JS).
fn should_add_canonical(input: &InputResource) -> bool {
    input.content_type().is_some_and(|content_type| {
        content_type.is_image() || content_type.kind() == ContentTypeKind::Pdf
    })
}

/// Per-context behavior (role varies by tag category).
pub(crate) struct CacheExtenderBehavior {
    driver: Rc<ResourceDriver>,
    role: InputRole,
}

impl CacheExtenderBehavior {
    /// Builds a nested behavior (`MakeNestedContext` behavior half).
    pub(crate) fn for_nested(driver: Rc<ResourceDriver>, role: InputRole) -> Self {
        CacheExtenderBehavior { driver, role }
    }

    /// Rebuilds the behavior on a background worker from a snapshot.
    pub(crate) fn for_background(driver: Rc<ResourceDriver>, role: InputRole) -> Self {
        CacheExtenderBehavior { driver, role }
    }
}

impl SingleRewriteBehavior for CacheExtenderBehavior {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn kind(&self) -> OutputResourceKind {
        OutputResourceKind::OnTheFly
    }

    fn policy_permits_rendering(
        &self,
        driver: &Rc<ResourceDriver>,
        outputs: &[Option<OutputResource>],
    ) -> bool {
        if outputs.len() == 1 && outputs[0].as_ref().is_some_and(|output| output.has_hash()) {
            let url = GoogleUrl::new(&outputs[0].as_ref().expect("checked output").url());
            return driver.is_load_permitted_by_csp(&url, self.role);
        }
        // Failure cases still permit error rendering.
        true
    }

    fn note_render(
        &self,
        _driver: &Rc<ResourceDriver>,
        _dom: &mut Dom,
        partitions: &OutputPartitions,
        _slots: &[crate::rewrite_context::ContextSlot],
    ) {
        if partitions.partition.len() == 1 && partitions.partition[0].optimizable {
            self.driver
                .server_context()
                .variable(CACHE_EXTENSIONS)
                .add(1);
        }
    }

    fn extra_fetch_fallback_headers(
        &self,
        driver: &Rc<ResourceDriver>,
        inputs: &[InputResource],
        headers: &mut ps_http::response::ResponseHeaders,
    ) {
        if inputs.len() == 1 && inputs[0].loaded() && should_add_canonical(&inputs[0]) {
            add_link_rel_canonical(driver, &inputs[0], headers);
        }
    }

    fn rewrite_single(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        output: &mut OutputResource,
        partition: &mut CachedResult,
    ) -> RewriteResult {
        if should_add_canonical(input) {
            add_link_rel_canonical(driver, input, output.response_headers_mut());
        }
        rewrite_loaded_resource(driver, input, output, partition)
    }

    fn background_snapshot(&self) -> Option<crate::html_async::BackgroundBehavior> {
        Some(crate::html_async::BackgroundBehavior::CacheExtender { role: self.role })
    }
}

/// Whether an input merits extension (`ShouldRewriteResource`).
fn should_rewrite_resource(
    driver: &ResourceDriver,
    input: &InputResource,
    partition: &mut CachedResult,
) -> bool {
    let Some(input_type) = input.content_type() else {
        return false;
    };
    if input_type.kind() == ContentTypeKind::Javascript
        && driver.options().avoid_renaming_introspective_javascript()
        && ps_js::rewrite::unsafe_to_rename(&input.contents())
    {
        partition
            .debug_message
            .push(ps_js::rewrite::INTROSPECTION_COMMENT.to_string());
        return false;
    }
    let headers = input.response_headers();
    if headers.cache_expiration_time_ms() - driver.server_context().now_ms() < MONTH_MS {
        // Short-lived (or previously rewritten): extend.
        return true;
    }
    let url = GoogleUrl::new(input.url());
    // An upstream filter may have rewritten the URL after this
    // context queued; recheck now that it is final.
    if driver.server_context().is_pagespeed_resource(&url) {
        return false;
    }
    if driver.server_context().url_namer().proxy_mode() == ProxyExtent::Full {
        return !driver.server_context().url_namer().is_proxy_encoded(&url);
    }
    let lawyer = driver.lawyer();
    // Proxy-mapped origins must reconstruct (serving them long and
    // public), even though the domain will not change.
    lawyer.is_proxy_mapped(&url) || lawyer.will_domain_change(&url)
}

/// Extends one loaded input (`RewriteLoadedResource`).
fn rewrite_loaded_resource(
    driver: &Rc<ResourceDriver>,
    input: &InputResource,
    output: &mut OutputResource,
    partition: &mut CachedResult,
) -> RewriteResult {
    assert!(input.loaded(), "extending an unloaded resource");
    let headers = input.response_headers();
    let vary = if driver.options().respect_vary() {
        VaryOption::RespectVaryOnResources
    } else {
        VaryOption::IgnoreVaryOnResources
    };
    if !driver.server_context().http_cache().force_caching()
        && !headers.is_proxy_cacheable(
            RequestProperties::default(),
            vary,
            ValidatorOption::NoValidator,
        )
    {
        // NOTE: `RewriteContextTest.PreserveNoCacheWithFailedRewrites`
        // relies on failing here (security: never extend no-cache).
        driver.server_context().variable(NOT_CACHEABLE).add(1);
        return RewriteResult::Failed;
    }
    if !should_rewrite_resource(driver, input, partition) {
        return RewriteResult::Failed;
    }
    // Whitelist against script-executing types (XSS: a misconfigured
    // lawyer must not serve HTML under another domain).
    let input_type = input.content_type().expect("checked type");
    let whitelisted = input_type.is_image()
        || (input_type.kind() == ContentTypeKind::Pdf && driver.may_cache_extend_pdfs())
        || input_type.kind() == ContentTypeKind::Css
        || input_type.kind() == ContentTypeKind::Javascript;
    if !whitelisted {
        return RewriteResult::Failed;
    }
    let contents = input.contents();
    let mut contents = contents.as_slice();
    let transformed;
    if input_type.kind() == ContentTypeKind::Css {
        let input_base = GoogleUrl::new(input.url());
        match driver.resolve_css_urls(&input_base, output.resolved_base(), contents) {
            (CssResolutionStatus::NoResolutionNeeded, _) => {}
            (CssResolutionStatus::WriteFailed, _) => return RewriteResult::Failed,
            (CssResolutionStatus::Success, rewritten) => {
                transformed = rewritten;
                contents = &transformed;
            }
        }
    }
    driver.server_context().merge_non_caching_response_headers(
        input.response_headers(),
        output.response_headers_mut(),
    );
    let inputs = [input.clone()];
    let wrote = driver.write_output(
        &inputs,
        contents,
        Some(input_type),
        input.charset(),
        output,
        partition,
    );
    if wrote {
        RewriteResult::Ok
    } else {
        RewriteResult::Failed
    }
}

/// Cache-extending filter (`CacheExtender`).
pub struct CacheExtenderFilter {
    driver: Rc<ResourceDriver>,
}

impl CacheExtenderFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        CacheExtenderFilter { driver }
    }

    /// Input role for a scanned category, if extendable.
    fn role_for(
        &self,
        dom: &mut Dom,
        id: NodeId,
        index: usize,
        category: Category,
    ) -> Option<InputRole> {
        match category {
            Category::Stylesheet if self.driver.may_cache_extend_css() => Some(InputRole::Style),
            Category::Image if self.driver.may_cache_extend_images() => Some(InputRole::Img),
            Category::Script if self.driver.may_cache_extend_scripts() => Some(InputRole::Script),
            _ => {
                // PDFs extend by leaf extension, ignoring queries.
                if !self.driver.may_cache_extend_pdfs() {
                    return None;
                }
                let (decoded, _) = dom.decoded_attribute(id, index);
                let value = decoded?;
                let url =
                    GoogleUrl::resolve(self.driver.base_url(), &String::from_utf8_lossy(&value));
                if url.is_web_valid() && url.leaf_sans_query().to_lowercase().ends_with(".pdf") {
                    Some(InputRole::Unknown)
                } else {
                    None
                }
            }
        }
    }
}

impl HtmlFilter for CacheExtenderFilter {
    fn name(&self) -> &'static str {
        "CacheExtender"
    }

    // NOTE: C++ scans on start-element, but C++ also applies each
    // filter to the whole stream before the next filter sees any
    // event, so an end-hook filter earlier in chain order (notably
    // CSS inlining) initiates first. The sync port initiates inline
    // per event, so scanning here on end-element reproduces C++
    // initiation order for same-element interactions (an inlined
    // element arrives dead or disabled, and extension stands down).
    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if !dom.is_rewritable(id) {
            return;
        }
        let url_valued = self.driver.options().url_valued_attributes().to_vec();
        for (index, category) in scan_element(dom, id, &url_valued) {
            let Some(role) = self.role_for(dom, id, index, category) else {
                continue;
            };
            if !dom.is_rewritable(id) {
                continue;
            }
            let (decoded, _) = dom.decoded_attribute(id, index);
            let Some(value) = decoded else {
                continue;
            };
            let url = GoogleUrl::resolve(self.driver.base_url(), &String::from_utf8_lossy(&value));
            if !url.is_web_valid() {
                continue;
            }
            let (resource, authorized) = self.driver.create_input_resource(
                &url,
                InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
                IntendedFor::General,
                role,
            );
            let Some(input) = resource else {
                if !authorized {
                    self.driver.insert_unauthorized_domain_debug_comment(
                        dom,
                        &String::from_utf8_lossy(&value),
                        role,
                        id,
                    );
                }
                continue;
            };
            if self
                .driver
                .server_context()
                .is_pagespeed_resource(&GoogleUrl::new(input.url()))
            {
                continue;
            }
            let handle: Rc<dyn crate::resource::Resource> = Rc::new(input.clone());
            let slot = self.driver.get_slot(Some(handle), dom, id, index, "ce");
            let behavior = CacheExtenderBehavior {
                driver: self.driver.clone(),
                role,
            };
            let mut context = SingleRewriteContext::new(self.driver.clone(), behavior);
            context.add_slot(slot, input);
            context.start(dom);
        }
        if dom.element_keyword(id) == Keyword::Img && self.driver.may_cache_extend_images() {
            if let Some(srcset) = dom.find_attribute(id, Keyword::Srcset) {
                self.extend_srcset(dom, id, srcset);
            }
        }
    }
}

impl CacheExtenderFilter {
    /// Extends one image's srcset candidates inline.
    fn extend_srcset(&self, dom: &mut Dom, id: NodeId, srcset: usize) {
        let (decoded, _) = dom.decoded_attribute(id, srcset);
        let Some(value) = decoded else {
            return;
        };
        let collection = Rc::new(RefCell::new(SrcSetCollection::new(id, srcset, &value)));
        let count = collection.borrow().candidates().len().min(MAX_SLOTS);
        for index in 0..count {
            let url_text = collection.borrow().candidates()[index].url.clone();
            if url_text.is_empty() {
                continue;
            }
            let url = GoogleUrl::resolve(self.driver.base_url(), &url_text);
            if !url.is_web_valid() {
                continue;
            }
            let (resource, _authorized) = self.driver.create_input_resource(
                &url,
                InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
                IntendedFor::General,
                InputRole::Img,
            );
            // Null slots (unparseable or disallowed URLs) are skipped.
            let Some(input) = resource else {
                continue;
            };
            let handle: Rc<dyn crate::resource::Resource> = Rc::new(input.clone());
            let slot = Rc::new(RefCell::new(SrcSetSlot::new(
                handle,
                collection.clone(),
                index,
                dom,
                id,
                "ce",
            )));
            let behavior = CacheExtenderBehavior {
                driver: self.driver.clone(),
                role: InputRole::Img,
            };
            let mut context = SingleRewriteContext::new(self.driver.clone(), behavior);
            context.add_srcset_slot(slot, input);
            context.start(dom);
        }
        collection.borrow_mut().commit(dom);
    }
}

impl RewriteFilter for CacheExtenderFilter {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn compute_on_the_fly(&self) -> bool {
        true
    }

    fn fetch(
        &self,
        driver: &Rc<ResourceDriver>,
        output: OutputResource,
        fetch: &mut dyn AsyncFetch,
    ) -> bool {
        let behavior = CacheExtenderBehavior {
            driver: driver.clone(),
            role: InputRole::Reconstruction,
        };
        let mut context = SingleRewriteContext::new(driver.clone(), behavior);
        context.fetch(output, fetch)
    }
}

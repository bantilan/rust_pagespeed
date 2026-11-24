//! CSS combining (`CssCombineFilter`, `cc`).
//!
//! Concatenates adjacent stylesheets into one multipart resource,
//! keeping the first link (retargeted) and deleting the rest.
//! Partitions form per combinability (parseable, no mid-combination
//! `@import`, URL/size rules); barriers (inline styles, unknown or
//! nonstandard links, integrity pins, media mismatches, noscript,
//! IE directives, flushes) split combinations. The sync port
//! partitions, writes, and renders at hand-off instead of across
//! threads.
//!
//! NOTE: CSP gating waits for the CSP subsystem. Combination
//! metadata is validated before rendering when a worker pool is installed (fetch
//! tests rebuild from inputs).

use std::rc::Rc;

use ps_config::filter::Filter;
use ps_http::google_url::{GoogleUrl, UrlRelativity};

use crate::css_tag_scanner::parse_css_element;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::metadata::CachedResult;
use crate::name::Keyword;
use crate::output_resource::{OutputResource, OutputResourceKind};
use crate::parser::Dom;
use crate::resource::{InputResource, Resource};
use crate::rewrite_filter::RewriteFilter;
use crate::script_scanner::ScriptTagScanner;
use crate::server_context::{InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver};
use crate::url_partnership::UrlPartnership;
use crate::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};

/// Filter id (`RewriteOptions::kCssCombinerId`).
pub const FILTER_ID: &str = "cc";

/// Opportunity counter (`kCssCombineOpportunities`).
pub const COMBINE_OPPORTUNITIES: &str = "css_combine_opportunities";
/// File-count reduction counter (`kCssFileCountReduction`).
pub const FILE_COUNT_REDUCTION: &str = "css_file_count_reduction";

/// URL slack for later filters (`ResourceCombiner::kUrlSlack`).
const URL_SLACK: usize = 100;

/// One candidate stylesheet (resource created up front, like
/// `Context::AddElement`).
struct PendingLink {
    element: NodeId,
    href_attr: usize,
    url: String,
    relativity: UrlRelativity,
    input: InputResource,
}

/// Per-partition combiner state (sync form).
struct CombinerState {
    combined_css_size: i64,
    prev_num_components: usize,
    accumulated_leaf_size: usize,
    url_overhead: usize,
    encoder_urls: Vec<String>,
}

impl CombinerState {
    fn new() -> Self {
        CombinerState {
            combined_css_size: 0,
            prev_num_components: 0,
            accumulated_leaf_size: 0,
            url_overhead: FILTER_ID.len() + ps_cache::namer::OVERHEAD + "css".len(),
            encoder_urls: Vec::new(),
        }
    }

    fn reset_sizes(&mut self) {
        self.prev_num_components = 0;
        self.accumulated_leaf_size = 0;
        self.encoder_urls.clear();
        self.combined_css_size = 0;
    }
}

/// CSS combining filter (`CssCombineFilter`).
pub struct CssCombineFilter {
    driver: Rc<ResourceDriver>,
    pending: Vec<PendingLink>,
    media: Vec<u8>,
    new_combination: bool,
    end_document_found: bool,
    css_links: i64,
    noscript_depth: u32,
    /// Current base URL (`<base>` tags rebase mid-parse links).
    current_base: GoogleUrl,
    /// Whether a `<base>` tag already rebased (`SetBaseUrlIfUnset`).
    base_from_tag: bool,
    background: Option<crate::combine_metadata::Recorder>,
}

impl CssCombineFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        let current_base = driver.base_url().clone();
        CssCombineFilter {
            driver,
            pending: Vec::new(),
            media: Vec::new(),
            new_combination: true,
            end_document_found: false,
            css_links: 0,
            noscript_depth: 0,
            current_base,
            base_from_tag: false,
            background: None,
        }
    }

    pub(crate) fn background_combine(
        driver: Rc<ResourceDriver>,
        inputs: Vec<(String, bool)>,
        base: &str,
    ) -> Option<crate::metadata::OutputPartitions> {
        let mut filter = Self::new(driver.clone());
        filter.current_base = GoogleUrl::new(base);
        filter.background = Some(crate::combine_metadata::Recorder::new(
            driver.options().metadata_input_errors_cache_ttl_ms(),
        ));
        for (index, (url, authorized)) in inputs.into_iter().enumerate() {
            let input =
                driver.create_input_resource_unchecked(&GoogleUrl::new(&url), authorized)?;
            filter.pending.push(PendingLink {
                element: index as u32,
                href_attr: 0,
                url,
                relativity: UrlRelativity::AbsoluteUrl,
                input,
            });
        }
        filter.initiate_combination(&mut Dom::new());
        filter.background.take()?.finish()
    }

    /// Hands the pending combination off (`NextCombination`).
    /// The barrier comment lands before the barrier element (or after
    /// the pending links when the barrier is a flush/end).
    fn next_combination(&mut self, dom: &mut Dom, reason: &str, barrier: Option<NodeId>) {
        if !self.pending.is_empty() {
            if self.driver.options().enabled(Filter::Debug) && !reason.is_empty() {
                let message = format!("combine_css: Could not combine over barrier: {reason}");
                match barrier {
                    Some(element) => Self::insert_barrier_comment(dom, element, &message),
                    None => {
                        let anchor = self.pending.last().expect("pending").element;
                        self.driver.insert_debug_comment(dom, &message, anchor);
                    }
                }
            }
            self.initiate_combination(dom);
        }
        self.pending.clear();
        self.media.clear();
        self.new_combination = true;
    }

    /// Inserts an escaped comment before an element (barrier notes).
    fn insert_barrier_comment(dom: &mut Dom, barrier: NodeId, message: &str) {
        let mut escaped = Vec::new();
        crate::keywords::escape(message.as_bytes(), &mut escaped);
        let comment = dom.new_leaf(None, crate::element::LeafKind::Comment, escaped);
        dom.insert_node_before_node(barrier, comment);
    }

    /// Whether a stylesheet may join the combination
    /// (`CssCombiner::ResourceCombinable`).
    fn resource_combinable(
        driver: &ResourceDriver,
        input: &InputResource,
        first_in_combination: bool,
    ) -> bool {
        let contents = input.contents();
        if crate::filters::css::over_parse_max_bytes(
            driver,
            contents.len() as i64,
            input.url(),
            "CSS not combined",
        )
        .is_some()
        {
            return false;
        }
        let mut parser = ps_css::parser::Parser::new(&contents);
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        let stylesheet = parser.parse_raw_stylesheet();
        if parser.errors_seen_mask() != ps_css::ast::NO_ERROR {
            return false;
        }
        // `@import`s cannot sit mid-combination (they would be ignored).
        if !first_in_combination && !stylesheet.imports().is_empty() {
            return false;
        }
        true
    }

    /// Offers one resource to the open combination
    /// (`AddResourceNoFetch`).
    fn add_resource(
        driver: &Rc<ResourceDriver>,
        partnership: &mut UrlPartnership,
        state: &mut CombinerState,
        link: &PendingLink,
    ) -> bool {
        // Fresh runs drop stale sizes from a refused predecessor
        // (C++ `if (num_urls() == 0) Reset()`). The partnership
        // origin stays (base tags own it).
        if partnership.num_urls() == 0 {
            state.reset_sizes();
        }
        if !Self::resource_combinable(driver, &link.input, partnership.num_urls() == 0) {
            return false;
        }
        if !partnership.add_url(&link.url) {
            return false;
        }
        if partnership.num_common_components() != state.prev_num_components {
            state.prev_num_components = partnership.num_common_components();
            state.encoder_urls.clear();
            for index in 0..partnership.num_urls() {
                state.encoder_urls.push(partnership.relative_path(index));
            }
            state.accumulated_leaf_size = 0;
        } else {
            state
                .encoder_urls
                .push(partnership.relative_path(partnership.num_urls() - 1));
        }
        if state.accumulated_leaf_size == 0 {
            state.accumulated_leaf_size = Self::multipart_size(&state.encoder_urls)
                + state.url_overhead
                + driver.server_context().hasher().hash_size_in_chars();
        } else {
            state.accumulated_leaf_size +=
                Self::segment_size(state.encoder_urls.last().expect("just pushed")) + 1;
        }
        state.combined_css_size += link.input.contents().len() as i64;
        let max_combined = driver.options().max_combined_css_bytes();
        if (max_combined >= 0 && max_combined < state.combined_css_size)
            || !driver
                .server_context()
                .http_cache()
                .is_cacheable_body_size(state.combined_css_size)
        {
            Self::remove_last_resource(partnership, state);
            return false;
        }
        if Self::url_too_big(driver, partnership, state) {
            Self::remove_last_resource(partnership, state);
            return false;
        }
        true
    }

    /// Removes the last added resource (`RemoveLastResource`).
    fn remove_last_resource(partnership: &mut UrlPartnership, state: &mut CombinerState) {
        partnership.remove_last();
        state.encoder_urls.pop();
        if partnership.num_common_components() != state.prev_num_components {
            state.prev_num_components = partnership.num_common_components();
            state.encoder_urls.clear();
            for index in 0..partnership.num_urls() {
                state.encoder_urls.push(partnership.relative_path(index));
            }
            state.accumulated_leaf_size = 0;
        }
    }

    /// Multipart-encoded size of relative URLs.
    fn multipart_size(urls: &[String]) -> usize {
        let encoder = MultipartEncoder;
        let inputs: Vec<Vec<u8>> = urls.iter().map(|url| url.as_bytes().to_vec()).collect();
        encoder.encode(&inputs).len()
    }

    /// Segment-encoded size of one URL.
    fn segment_size(url: &str) -> usize {
        ps_cache::namer::encode_url_segment_bytes(url.as_bytes()).len()
    }

    /// Whether the accumulated URL is too big (`UrlTooBig`).
    fn url_too_big(
        driver: &ResourceDriver,
        partnership: &UrlPartnership,
        state: &CombinerState,
    ) -> bool {
        let expanded = state.accumulated_leaf_size + URL_SLACK;
        if expanded as i64 > driver.options().max_url_segment_size() {
            return true;
        }
        if (expanded + partnership.resolved_base().len()) as i64 > driver.options().max_url_size() {
            return true;
        }
        false
    }

    /// Partitions, writes, and renders the pending links.
    fn initiate_combination(&mut self, dom: &mut Dom) {
        if !self.driver.options().enabled(Filter::CombineCss)
            || !self.driver.can_rewrite_resources()
        {
            self.pending.clear();
            return;
        }
        if self.pending.len() < 2 {
            self.pending.clear();
            return;
        }
        if self.background.is_none() && self.driver.server_context().background_pool().is_some() {
            let inputs: Vec<_> = self
                .pending
                .iter()
                .map(|link| (link.url.clone(), link.input.is_authorized_domain()))
                .collect();
            let identity = format!(
                "cc-v1:{}:{}:{inputs:?}",
                self.current_base.spec(),
                self.driver.user_agent()
            );
            let key = format!("{}:{}", self.driver.partition_signature_hash(), identity);
            if let Some(tables) =
                crate::combine_metadata::lookup(&self.driver, &key, self.pending.len())
            {
                let mut pending: Vec<_> = std::mem::take(&mut self.pending)
                    .into_iter()
                    .map(Some)
                    .collect();
                for partition in tables.partition {
                    let pieces: Vec<_> = partition
                        .input
                        .iter()
                        .map(|info| pending[info.index.unwrap() as usize].take().unwrap())
                        .collect();
                    if pieces.iter().all(|link| dom.is_rewritable(link.element)) {
                        self.render_partition(dom, &pieces, partition.url);
                    }
                }
            } else {
                crate::html_async::schedule_combination(
                    &self.driver,
                    crate::html_async::BackgroundBehavior::CombineCss {
                        inputs,
                        current_base: self.current_base.spec().to_string(),
                    },
                    &key,
                );
                self.pending.clear();
            }
            return;
        }
        let mut pending = std::mem::take(&mut self.pending);
        for link in &mut pending {
            let mut ctx = self.driver.request_context().borrow().clone();
            let mut callback = crate::cacheable_resource::MockLoadCallback::new();
            link.input.load(
                crate::cacheable_resource::NotCacheablePolicy::ReportFailureIfNotCacheable,
                &mut ctx,
                &mut callback,
            );
        }
        if let Some(recorder) = &self.background {
            for link in &pending {
                recorder.dependency(&link.input, self.driver.server_context().now_ms());
            }
        }
        let base = self.current_base.clone();
        let mut partnership = UrlPartnership::new(&self.driver);
        partnership.reset(&base);
        let mut state = CombinerState::new();
        let mut current: Vec<PendingLink> = Vec::new();
        for link in pending {
            let (safe, _) = link.input.is_safe_to_rewrite(false);
            if !link.input.loaded() || !link.input.http_status_ok() || !safe {
                self.finalize_partition(dom, &mut current);
                partnership.reset(&base);
                state.reset_sizes();
                continue;
            }
            if Self::add_resource(&self.driver, &mut partnership, &mut state, &link) {
                current.push(link);
            } else {
                self.finalize_partition(dom, &mut current);
                partnership.reset(&base);
                state.reset_sizes();
                if Self::add_resource(&self.driver, &mut partnership, &mut state, &link) {
                    current.push(link);
                }
            }
        }
        self.finalize_partition(dom, &mut current);
    }

    /// Renders one finished partition (size > 1, first link kept and
    /// retargeted, rest deleted).
    fn finalize_partition(&self, dom: &mut Dom, current: &mut Vec<PendingLink>) {
        if current.is_empty() {
            return;
        }
        let pieces = std::mem::take(current);
        if pieces.len() <= 1 {
            return;
        }
        if self.background.is_none() && !pieces.iter().all(|link| dom.is_rewritable(link.element)) {
            return;
        }
        let Some(combined_url) = self.make_output(&pieces) else {
            return;
        };
        if let Some(recorder) = &self.background {
            recorder.record(pieces.iter().map(|link| link.element), combined_url);
            return;
        }
        self.render_partition(dom, &pieces, combined_url);
    }

    fn render_partition(&self, dom: &mut Dom, pieces: &[PendingLink], combined_url: String) {
        // XHTML-ish pages keep the brief close on the surviving link.
        if self.driver.mime_type_xhtml_status() != crate::server_context::XhtmlStatus::IsNotXhtml {
            dom.set_element_style(pieces[0].element, crate::element::ElementStyle::BriefClose);
        }
        let first = &pieces[0];
        let rendered = if self.driver.options().preserve_url_relativity() {
            GoogleUrl::new(&combined_url).relativize(first.relativity, &self.current_base)
        } else {
            combined_url
        };
        let (decoded, _) = dom.decoded_attribute(first.element, first.href_attr);
        let _ = decoded;
        self.driver.claim_element(first.element);
        dom.element_attributes_mut(first.element)[first.href_attr].set_value(rendered.as_bytes());
        for link in pieces.iter().skip(1) {
            dom.delete_node(link.element);
        }
        self.driver
            .server_context()
            .variable(FILE_COUNT_REDUCTION)
            .add(pieces.len() as i64 - 1);
    }

    /// Builds and writes the combination (`Combine` + `Write`).
    fn make_output(&self, pieces: &[PendingLink]) -> Option<String> {
        if pieces.len() <= 1 {
            return None;
        }
        let mut partnership = UrlPartnership::new(&self.driver);
        partnership.reset(&GoogleUrl::new(&pieces[0].url));
        for link in pieces {
            if !partnership.add_url(&link.url) {
                return None;
            }
        }
        let encoder = MultipartEncoder;
        let inputs: Vec<Vec<u8>> = (0..partnership.num_urls())
            .map(|index| partnership.relative_path(index).into_bytes())
            .collect();
        let name = encoder.encode(&inputs);
        let base = partnership.resolved_base();
        let mut output = self
            .driver
            .create_output_resource_with_mapped_path(
                &base,
                &base,
                FILTER_ID,
                &name,
                OutputResourceKind::Rewritten,
            )
            .ok()?;
        let mut combined = Vec::new();
        let count = pieces.len();
        for (index, link) in pieces.iter().enumerate() {
            let contents = link.input.contents();
            let piece: &[u8] = if index == 0 {
                &contents
            } else {
                ps_css::combine::strip_utf8_bom(&contents).0
            };
            let input_base = GoogleUrl::new(link.input.url());
            // C++ rebases against `combination->resolved_base()` (the
            // unsharded partnership base), not the sharded output URL:
            // trimming a sharded subresource against a sharded base
            // would wrongly relativize same-shard URLs.
            let output_base = GoogleUrl::new(output.resolved_base());
            let (status, resolved) =
                self.driver
                    .resolve_css_urls(&input_base, output_base.spec(), piece);
            match status {
                crate::server_context::CssResolutionStatus::WriteFailed => return None,
                crate::server_context::CssResolutionStatus::Success => {
                    combined.extend_from_slice(&resolved);
                }
                crate::server_context::CssResolutionStatus::NoResolutionNeeded => {
                    combined.extend_from_slice(&resolved);
                    if index != count - 1 && !resolved.ends_with(b"\n") {
                        combined.push(b'\n');
                    }
                }
            }
        }
        let inputs: Vec<InputResource> = pieces.iter().map(|link| link.input.clone()).collect();
        let mut partition = CachedResult::new();
        if !self.driver.write_output(
            &inputs,
            &combined,
            ps_http::content_type::mime_to_content_type("text/css"),
            "",
            &mut output,
            &mut partition,
        ) {
            return None;
        }
        Some(output.url().to_string())
    }
}

impl HtmlFilter for CssCombineFilter {
    fn name(&self) -> &'static str {
        "CssCombine"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.pending.clear();
        self.media.clear();
        self.new_combination = true;
        self.end_document_found = false;
        self.css_links = 0;
        self.noscript_depth = 0;
        self.current_base = self.driver.base_url().clone();
        self.base_from_tag = false;
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.end_document_found = true;
        if self.css_links > 1 {
            self.driver
                .server_context()
                .variable(COMBINE_OPPORTUNITIES)
                .add(self.css_links - 1);
        }
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
        if dom.element_keyword(id) == Keyword::Base {
            if !self.base_from_tag {
                if let Some(index) = dom.find_attribute(id, Keyword::Href) {
                    let (decoded, _) = dom.decoded_attribute(id, index);
                    if let Some(value) = decoded {
                        let base = GoogleUrl::resolve(
                            &self.current_base,
                            &String::from_utf8_lossy(&value),
                        );
                        if base.is_web_valid() {
                            self.current_base = base;
                            self.base_from_tag = true;
                        }
                    }
                }
            }
            return;
        }
        if dom.element_keyword(id) == Keyword::Style {
            self.next_combination(dom, "inline style", Some(id));
            return;
        }
        let Some(link) = parse_css_element(dom, id) else {
            // Custom or alternate stylesheet attributes barrier.
            let url_valued = self.driver.options().url_valued_attributes().to_vec();
            for (_, category) in crate::resource_tag_scanner::scan_element(dom, id, &url_valued) {
                if category == ps_config::options::Category::Stylesheet {
                    self.next_combination(
                        dom,
                        "custom or alternate stylesheet attribute",
                        Some(id),
                    );
                    return;
                }
            }
            return;
        };
        self.css_links += 1;
        if dom.has_children_in_window(id) {
            self.next_combination(dom, "children in flush window", Some(id));
            return;
        }
        let mut nonstandard = link.nonstandard.clone();
        if self.driver.options().css_combining_may_permit_ids() {
            if let Some(index) = dom.find_attribute(id, Keyword::Id) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if let Some(value) = decoded {
                    if self
                        .driver
                        .options()
                        .is_allowed_id_for_css_combining(&String::from_utf8_lossy(&value))
                    {
                        nonstandard.retain(|name| !name.eq_ignore_ascii_case(b"id"));
                    }
                }
            }
        }
        if ScriptTagScanner::has_integrity_attribute(dom, id) {
            self.next_combination(dom, "integrity attribute", Some(id));
            return;
        }
        if !nonstandard.is_empty() {
            let mut message = "potentially non-combinable attribute".to_string();
            if self.driver.options().enabled(Filter::Debug) {
                if nonstandard.len() > 1 {
                    message.push('s');
                }
                for (index, name) in nonstandard.iter().enumerate() {
                    if index == 0 {
                        message.push_str(": ");
                    } else if index == nonstandard.len() - 1 {
                        message.push_str(" and ");
                    } else {
                        message.push_str(", ");
                    }
                    message.push('\'');
                    message.push_str(&String::from_utf8_lossy(name));
                    message.push('\'');
                }
            } else {
                message.push_str("(s)");
            }
            self.next_combination(dom, &message, Some(id));
            return;
        }
        if self.noscript_depth > 0 {
            self.next_combination(dom, "noscript", Some(id));
            return;
        }
        if self.new_combination {
            self.media = link.media.clone();
            self.new_combination = false;
        } else if self.media != link.media {
            let reason = format!(
                "media mismatch: looking for media '{}' but found media='{}'",
                String::from_utf8_lossy(&self.media),
                String::from_utf8_lossy(&link.media)
            );
            self.next_combination(dom, &reason, Some(id));
            self.media = link.media.clone();
            self.new_combination = false;
        }
        let (decoded, _) = dom.decoded_attribute(id, link.href);
        let Some(value) = decoded else {
            self.next_combination(dom, "resource not rewritable", Some(id));
            return;
        };
        let url = GoogleUrl::resolve(&self.current_base, &String::from_utf8_lossy(&value));
        if !url.is_web_valid() {
            self.next_combination(dom, "resource not rewritable", Some(id));
            return;
        }
        // NOTE: upstream checks domain authorization here, but
        // observably combines absolute URLs that no static reading
        // authorizes (the out-of-order base-tag test); the URL
        // partnership below still enforces same-origin/mapping rules,
        // and load failures still split. The unauthorized-domain
        // debug comment waits for the Debug-filter machinery.
        if !self.driver.options().is_allowed(url.spec()) {
            self.next_combination(dom, "resource not rewritable", Some(id));
            return;
        }
        let Some(input) = self.driver.create_input_resource_unchecked(&url, true) else {
            self.next_combination(dom, "resource not rewritable", Some(id));
            return;
        };
        let relativity = GoogleUrl::find_relativity(&String::from_utf8_lossy(&value));
        self.pending.push(PendingLink {
            element: id,
            href_attr: link.href,
            url: url.spec().to_string(),
            relativity,
            input,
        });
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript && self.noscript_depth > 0 {
            self.noscript_depth -= 1;
        }
        let _ = dom;
    }

    fn flush(&mut self, dom: &mut Dom, _lexer: &Lexer) {
        let reason = if self.end_document_found { "" } else { "flush" };
        self.next_combination(dom, reason, None);
    }

    fn ie_directive(&mut self, dom: &mut Dom, _lexer: &Lexer, node: NodeId) {
        self.next_combination(dom, "IE directive", Some(node));
    }
}

impl RewriteFilter for CssCombineFilter {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &CC_ENCODER
    }

    fn fetch(
        &self,
        driver: &Rc<ResourceDriver>,
        output: OutputResource,
        fetch: &mut dyn ps_http::fetch::AsyncFetch,
    ) -> bool {
        fetch_combined(driver, output, fetch)
    }
}

/// Shared multipart codec.
static CC_ENCODER: MultipartEncoder = MultipartEncoder;

/// Serves a combination by decoding its multipart name and
/// concatenating the inputs (no combinability rules on fetch).
fn fetch_combined(
    driver: &Rc<ResourceDriver>,
    output: OutputResource,
    fetch: &mut dyn ps_http::fetch::AsyncFetch,
) -> bool {
    let fail = |fetch: &mut dyn ps_http::fetch::AsyncFetch| {
        fetch.response_headers_mut().set_status_and_reason(404);
        fetch.headers_complete();
        fetch.handle_done(false);
        false
    };
    let encoder = MultipartEncoder;
    let name = output.full_name().name().to_string();
    // `CreateOutputResourceWithPath` never produces a longer leaf,
    // so this is not a URL the server generated; refuse it before
    // decoding what it names (`RewriteContext::Fetch`).
    let eventual = output.full_name().eventual_size(
        driver.server_context().hasher().as_ref(),
        driver.signature_length(),
    );
    if eventual as i64 > driver.options().max_url_segment_size() {
        return fail(fetch);
    }
    let Some(urls) = encoder.decode(name.as_bytes()) else {
        return fail(fetch);
    };
    if urls.is_empty() {
        return fail(fetch);
    }
    let base = GoogleUrl::new(&output.decoded_base());
    let mut inputs = Vec::new();
    let mut loaded_bytes: i64 = 0;
    for url in urls {
        let text = String::from_utf8_lossy(&url);
        let resolved = GoogleUrl::resolve(&base, &text);
        if !resolved.is_web_valid() {
            return fail(fetch);
        }
        let (resource, _authorized) = driver.create_input_resource(
            &resolved,
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Style,
        );
        let Some(mut input) = resource else {
            return fail(fetch);
        };
        input.set_is_background_fetch(false);
        let mut ctx = driver.request_context().borrow().clone();
        let mut callback = crate::cacheable_resource::MockLoadCallback::new();
        input.load(
            crate::cacheable_resource::NotCacheablePolicy::LoadEvenIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        if !input.loaded() || !input.http_status_ok() {
            return fail(fetch);
        }
        // Once the inputs loaded so far exceed what could be cached,
        // the rest stay unloaded and the fetch is refused.
        loaded_bytes += input.contents().len() as i64;
        if !driver
            .server_context()
            .http_cache()
            .is_cacheable_body_size(loaded_bytes)
        {
            return fail(fetch);
        }
        inputs.push(input);
    }
    let mut output = output;
    // Same unsharded-base rule as the rewrite path (`WritePiece`).
    let output_base = GoogleUrl::new(output.resolved_base());
    let mut combined = Vec::new();
    let count = inputs.len();
    for (index, input) in inputs.iter().enumerate() {
        let contents = input.contents();
        let piece: &[u8] = if index == 0 {
            &contents
        } else {
            ps_css::combine::strip_utf8_bom(&contents).0
        };
        let input_base = GoogleUrl::new(input.url());
        let (status, resolved) = driver.resolve_css_urls(&input_base, output_base.spec(), piece);
        match status {
            crate::server_context::CssResolutionStatus::WriteFailed => {
                return fail(fetch);
            }
            crate::server_context::CssResolutionStatus::Success => {
                combined.extend_from_slice(&resolved);
            }
            crate::server_context::CssResolutionStatus::NoResolutionNeeded => {
                combined.extend_from_slice(&resolved);
                if index != count - 1 && !resolved.ends_with(b"\n") {
                    combined.push(b'\n');
                }
            }
        }
    }
    // NOTE: fetch writes serve bytes only (FetchWritesPartition is
    // false: fetch results must not decide HTML combinations).
    let first_headers = inputs[0].response_headers().clone();
    driver
        .server_context()
        .merge_non_caching_response_headers(&first_headers, output.response_headers_mut());
    for input in inputs.iter().skip(1) {
        let keep = input.response_headers().headers().clone();
        output
            .response_headers_mut()
            .headers_mut()
            .remove_if_not_in(&keep);
    }
    let mut partition = CachedResult::new();
    if !driver.write_output(
        &inputs,
        &combined,
        ps_http::content_type::mime_to_content_type("text/css"),
        "",
        &mut output,
        &mut partition,
    ) {
        return fail(fetch);
    }
    let headers = output.response_headers().clone();
    let body = output.body().to_vec();
    fetch.response_headers_mut().copy_from(&headers);
    fetch.headers_complete();
    let ok = fetch.handle_write(&body);
    fetch.handle_done(ok);
    ok
}

//! JavaScript combining (`JsCombineFilter`, `jc`).
//!
//! Combines adjacent external scripts into one multipart resource,
//! replacing each with an `eval()` stub. Partitions form per
//! combinability (charset/strict/introspective/library/size/URL
//! rules); barriers (inline scripts, unknown languages, async/defer,
//! integrity pins, no-defer markers, IE directives, noscript
//! ancestry) split combinations. The sync port partitions, writes,
//! and renders at hand-off (flush/end) instead of across threads.
//!
//! NOTE: CSP gating waits for the CSP subsystem. Combination
//! metadata caching waits for the async reconstruction epic (no
//! portable test observes it); fetches rebuild from inputs.

use std::collections::HashMap;
use std::rc::Rc;

use ps_config::filter::Filter;
use ps_http::google_url::{GoogleUrl, UrlRelativity};
use ps_js::tokenizer::TokenizerPatterns;

use super::meta_tag::extract_meta_tag_details;
use crate::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
use crate::element::{LeafKind, NodeId};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::metadata::CachedResult;
use crate::name::Keyword;
use crate::output_resource::{OutputResource, OutputResourceKind};
use crate::parser::Dom;
use crate::resource::{InputResource, Resource};
use crate::rewrite_filter::{charset_for_script, RewriteFilter};
use crate::script_scanner::{ScriptClassification, ScriptTagScanner, EXECUTE_SYNC};
use crate::server_context::{InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver};
use crate::url_partnership::UrlPartnership;
use crate::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};

/// Filter id (`RewriteOptions::kJavascriptCombinerId`).
pub const FILTER_ID: &str = "jc";

/// File-count reduction counter (`kJsFileCountReduction`).
pub const FILE_COUNT_REDUCTION: &str = "js_file_count_reduction";

/// URL slack for later filters (`ResourceCombiner::kUrlSlack`).
const URL_SLACK: usize = 100;

/// UTF-8 charset name (`kUtf8Charset`).
const UTF8_CHARSET: &[u8] = b"utf-8";

/// One candidate script.
struct PendingScript {
    element: NodeId,
    url: String,
    charset: Vec<u8>,
    relativity: UrlRelativity,
}

/// URL hash safe for identifiers (`JavascriptCodeBlock::JsUrlHash`):
/// hash of the path plus leaf, `-` mapped to `$`.
fn js_url_hash(url: &str, driver: &ResourceDriver) -> String {
    use ps_cache::hash::Hasher;
    let path = GoogleUrl::new(url).path_and_leaf().to_string();
    driver
        .server_context()
        .hasher()
        .hash(path.as_bytes())
        .replace('-', "$")
}

/// Combination variable for a URL (`JsCombineFilter::VarName`):
/// mapped-domain form, hashed to an identifier continuation.
fn var_name(driver: &ResourceDriver, url: &str) -> String {
    let mut resource_url = GoogleUrl::new(url);
    let mut domain = String::new();
    UrlPartnership::find_resource_domain(
        driver.base_url(),
        &**driver.server_context().url_namer(),
        driver.lawyer(),
        &mut resource_url,
        &mut domain,
    );
    let output_url = if resource_url.is_web_valid() {
        resource_url.spec().to_string()
    } else {
        url.to_string()
    };
    format!("mod_pagespeed_{}", js_url_hash(&output_url, driver))
}

/// Minified piece plus library verdict (one `CodeBlock` run).
struct MinifiedPiece {
    code: Vec<u8>,
    successful: bool,
    is_library: bool,
}

/// Per-partition combiner state (`JsCombiner` partition half, sync
/// form; the URL partnership lives with the caller).
struct CombinerState {
    combined_charset: Vec<u8>,
    combined_js_size: i64,
    minified: HashMap<String, MinifiedPiece>,
    prev_num_components: usize,
    accumulated_leaf_size: usize,
    url_overhead: usize,
    encoder_urls: Vec<String>,
}

impl CombinerState {
    fn new() -> Self {
        CombinerState {
            combined_charset: Vec::new(),
            combined_js_size: 0,
            minified: HashMap::new(),
            prev_num_components: 0,
            accumulated_leaf_size: 0,
            url_overhead: FILTER_ID.len() + ps_cache::namer::OVERHEAD + "js".len(),
            encoder_urls: Vec::new(),
        }
    }

    /// Resets size tracking for a fresh combination (`Reset` half).
    fn reset_sizes(&mut self) {
        self.prev_num_components = 0;
        self.accumulated_leaf_size = 0;
        self.encoder_urls.clear();
        self.combined_js_size = 0;
        self.combined_charset.clear();
    }

    /// Minifies once per URL (`BlockForResource`).
    fn minified_piece(
        &mut self,
        driver: &ResourceDriver,
        patterns: &TokenizerPatterns,
        url: &str,
        contents: &[u8],
    ) -> &MinifiedPiece {
        if !self.minified.contains_key(url) {
            let options = driver.options();
            let mut libraries = ps_js::library::LibraryIdentification::new();
            if options.enabled(Filter::CanonicalizeJavascriptLibraries) {
                for (bytes, entries) in options.libraries() {
                    for (md5, entry_url) in entries {
                        libraries.register(*bytes, md5, entry_url);
                    }
                }
            }
            let config = ps_js::rewrite::CodeBlockConfig {
                minify: options.enabled(Filter::RewriteJavascriptExternal),
                use_experimental_minifier: options.use_experimental_js_minifier(),
            };
            drop(options);
            let mut block = ps_js::rewrite::CodeBlock::new(
                patterns,
                config,
                if driver
                    .options()
                    .enabled(Filter::CanonicalizeJavascriptLibraries)
                {
                    Some(&mut libraries)
                } else {
                    None
                },
                contents,
            );
            let successful = block.rewrite();
            let is_library = block.library_url().is_some();
            self.minified.insert(
                url.to_string(),
                MinifiedPiece {
                    code: block.rewritten_code().to_vec(),
                    successful,
                    is_library,
                },
            );
        }
        &self.minified[url]
    }
}

/// Whether a resource may join the combination
/// (`JsCombiner::ResourceCombinable`).
fn resource_combinable(
    driver: &ResourceDriver,
    patterns: &TokenizerPatterns,
    state: &mut CombinerState,
    script: &PendingScript,
    input: &InputResource,
    containing_charset: &[u8],
) -> bool {
    let contents = input.contents();
    let this_charset = charset_for_script(
        input.charset().as_bytes(),
        &contents,
        &script.charset,
        containing_charset,
    );
    if state.combined_js_size == 0 && state.combined_charset.is_empty() {
        // First resource sets the combination charset only when the
        // combination is empty; `num_urls` decides that below.
    }
    // NOTE: the charset-mismatch message goes to the log record in
    // C++; only the verdict is observable here.
    if state.encoder_urls.is_empty() {
        state.combined_charset = this_charset.clone();
    } else if !state.combined_charset.eq_ignore_ascii_case(&this_charset) {
        return false;
    }
    if !this_charset.eq_ignore_ascii_case(UTF8_CHARSET)
        || !containing_charset.eq_ignore_ascii_case(UTF8_CHARSET)
    {
        for byte in &contents {
            if *byte == 0 || *byte == 0x1B || *byte >= 0x80 {
                return false;
            }
        }
    }
    if ps_js::rewrite::is_likely_strict_mode(patterns, &contents) {
        return false;
    }
    if driver.options().avoid_renaming_introspective_javascript()
        && ps_js::rewrite::unsafe_to_rename(&contents)
    {
        return false;
    }
    if driver
        .options()
        .enabled(Filter::CanonicalizeJavascriptLibraries)
    {
        let piece = state.minified_piece(driver, patterns, &script.url, &contents);
        if piece.is_library {
            return false;
        }
    }
    true
}

/// JavaScript combining filter (`JsCombineFilter`).
pub struct JsCombineFilter {
    driver: Rc<ResourceDriver>,
    patterns: Rc<TokenizerPatterns>,
    script_depth: usize,
    current_js_script: Option<NodeId>,
    pending: Vec<PendingScript>,
    meta_tag_charset: Vec<u8>,
}

impl JsCombineFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        JsCombineFilter {
            driver,
            patterns: Rc::new(TokenizerPatterns::new()),
            script_depth: 0,
            current_js_script: None,
            pending: Vec::new(),
            meta_tag_charset: Vec::new(),
        }
    }

    /// Whether a script is likely strict-mode
    /// (`JsCombineFilter::IsLikelyStrictMode`, unit-tested).
    #[must_use]
    pub fn is_likely_strict_mode(patterns: &TokenizerPatterns, input: &[u8]) -> bool {
        ps_js::rewrite::is_likely_strict_mode(patterns, input)
    }

    /// Page charset at hand-off: headers, else first meta tag
    /// (`ScanFilter` order; HTML BOM waits for the scan filter).
    fn containing_charset(&self) -> Vec<u8> {
        if let Some(charset) = self.driver.response_headers().determine_charset() {
            return charset.into_bytes();
        }
        self.meta_tag_charset.clone()
    }

    /// Whether an element sits inside a noscript element.
    fn inside_noscript(dom: &Dom, mut id: NodeId) -> bool {
        while let Some(parent) = dom.arena_node(id).parent() {
            if dom.element_keyword(parent) == Keyword::Noscript {
                return true;
            }
            id = parent;
        }
        false
    }

    /// Adds a script to the combination or splits it
    /// (`ConsiderJsForCombination`).
    fn consider_js_for_combination(&mut self, dom: &mut Dom, id: NodeId, src: Option<usize>) {
        // NOTE: `PermitsEval`/`PermitsInlineScript` wait for CSP.
        if self.script_depth > 0 {
            self.pending.clear();
            return;
        }
        self.current_js_script = Some(id);
        if Self::inside_noscript(dom, id) {
            self.next_combination(dom);
            return;
        }
        let Some(src) = src else {
            self.next_combination(dom);
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, src);
        let Some(value) = decoded else {
            self.next_combination(dom);
            return;
        };
        if dom
            .find_attribute(id, Keyword::DataPagespeedNoDefer)
            .is_some()
            || dom.find_attribute(id, Keyword::PagespeedNoDefer).is_some()
        {
            self.next_combination(dom);
            return;
        }
        if ScriptTagScanner::has_integrity_attribute(dom, id) {
            self.next_combination(dom);
            return;
        }
        if ScriptTagScanner::execution_mode(dom, id) != EXECUTE_SYNC {
            self.next_combination(dom);
            return;
        }
        let url = GoogleUrl::resolve(self.driver.base_url(), &String::from_utf8_lossy(&value));
        if !url.is_web_valid() {
            return;
        }
        let (resource, _authorized) = self.driver.create_input_resource(
            &url,
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Script,
        );
        if resource.is_none() {
            // NOTE: the unauthorized-domain debug comment waits for
            // the Debug-filter machinery.
            return;
        }
        let charset = dom
            .find_attribute(id, Keyword::Charset)
            .and_then(|index| dom.decoded_attribute(id, index).0)
            .unwrap_or_default();
        let relativity = GoogleUrl::find_relativity(&String::from_utf8_lossy(&value));
        self.pending.push(PendingScript {
            element: id,
            url: url.spec().to_string(),
            charset,
            relativity,
        });
    }

    /// Hands the pending combination off (`NextCombination`): sync
    /// port partitions, writes, and renders immediately.
    fn next_combination(&mut self, dom: &mut Dom) {
        if self.pending.is_empty() {
            return;
        }
        // `AddFilters` only installs the filter when enabled (soft
        // enables lose to preserve-urls); unhealthy caches refuse
        // combinations while serving still works.
        if !self.driver.options().enabled(Filter::CombineJavascript)
            || !self.driver.can_rewrite_resources()
        {
            self.pending.clear();
            return;
        }
        let containing = self.containing_charset();
        let pending = std::mem::take(&mut self.pending);
        let mut loaded = Vec::new();
        for script in pending {
            let url = GoogleUrl::new(&script.url);
            let (resource, _authorized) = self.driver.create_input_resource(
                &url,
                InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
                IntendedFor::General,
                InputRole::Script,
            );
            let Some(mut input) = resource else {
                continue;
            };
            let mut ctx = self.driver.request_context().borrow().clone();
            let mut callback = MockLoadCallback::new();
            input.load(
                NotCacheablePolicy::ReportFailureIfNotCacheable,
                &mut ctx,
                &mut callback,
            );
            loaded.push((script, input));
        }
        let mut partnership = UrlPartnership::new(&self.driver);
        partnership.reset(self.driver.base_url());
        let mut state = CombinerState::new();
        let mut current: Vec<(PendingScript, InputResource)> = Vec::new();
        for (script, input) in loaded {
            let (safe, _) = input.is_safe_to_rewrite(false);
            if !input.loaded() || !input.http_status_ok() || !safe {
                self.finalize_partition(dom, &mut current, &mut partnership, &mut state);
                continue;
            }
            if Self::add_resource(
                &self.driver,
                &self.patterns,
                &mut partnership,
                &mut state,
                &script,
                &input,
                &containing,
            ) {
                current.push((script, input));
            } else {
                self.finalize_partition(dom, &mut current, &mut partnership, &mut state);
                if Self::add_resource(
                    &self.driver,
                    &self.patterns,
                    &mut partnership,
                    &mut state,
                    &script,
                    &input,
                    &containing,
                ) {
                    current.push((script, input));
                }
            }
        }
        self.finalize_partition(dom, &mut current, &mut partnership, &mut state);
    }

    /// Offers one resource to the open combination
    /// (`AddResourceNoFetch`).
    fn add_resource(
        driver: &Rc<ResourceDriver>,
        patterns: &TokenizerPatterns,
        partnership: &mut UrlPartnership,
        state: &mut CombinerState,
        script: &PendingScript,
        input: &InputResource,
        containing: &[u8],
    ) -> bool {
        if !resource_combinable(driver, patterns, state, script, input, containing) {
            return false;
        }
        if !partnership.add_url(&script.url) {
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
        state.combined_js_size += input.contents().len() as i64;
        let max_combined = driver.options().max_combined_js_bytes();
        if (max_combined >= 0 && state.combined_js_size > max_combined)
            || !driver
                .server_context()
                .http_cache()
                .is_cacheable_body_size(state.combined_js_size)
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

    /// Multipart-encoded size of relative URLs (`UrlSafeId` measured).
    fn multipart_size(urls: &[String]) -> usize {
        let encoder = MultipartEncoder;
        let inputs: Vec<Vec<u8>> = urls.iter().map(|url| url.as_bytes().to_vec()).collect();
        encoder.encode(&inputs).len()
    }

    /// Segment-encoded size of one URL (`AccumulateLeafSize` piece).
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

    /// Renders one finished partition (`FinalizePartition` + `Render`
    /// for size > 1 with every element rewritable).
    fn finalize_partition(
        &self,
        dom: &mut Dom,
        current: &mut Vec<(PendingScript, InputResource)>,
        partnership: &mut UrlPartnership,
        state: &mut CombinerState,
    ) {
        if current.is_empty() {
            return;
        }
        let pieces = std::mem::take(current);
        partnership.reset(self.driver.base_url());
        state.reset_sizes();
        if pieces.len() <= 1
            || !pieces
                .iter()
                .all(|(script, _)| dom.is_rewritable(script.element))
        {
            return;
        }
        if let Some(combined_url) = self.make_output(&pieces) {
            Self::render_combination(&self.driver, dom, &pieces, &combined_url);
            self.driver
                .server_context()
                .variable(FILE_COUNT_REDUCTION)
                .add(pieces.len() as i64 - 1);
        }
    }

    /// Builds and writes the combination (`Combine` + `Write`).
    fn make_output(&self, pieces: &[(PendingScript, InputResource)]) -> Option<String> {
        if pieces.len() <= 1 {
            return None;
        }
        // Re-resolve the partnership over exactly these inputs.
        let mut partnership = UrlPartnership::new(&self.driver);
        partnership.reset(self.driver.base_url());
        for (script, _) in pieces {
            if !partnership.add_url(&script.url) {
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
        // Write pieces (minified when external rewriting runs).
        let mut combined = Vec::new();
        let minify = self
            .driver
            .options()
            .enabled(Filter::RewriteJavascriptExternal);
        let mut state = CombinerState::new();
        for (script, input) in pieces {
            let contents = input.contents();
            let piece: &[u8] = if minify {
                let minified =
                    state.minified_piece(&self.driver, &self.patterns, &script.url, &contents);
                if minified.successful {
                    &minified.code
                } else {
                    &contents
                }
            } else {
                &contents
            };
            combined.extend_from_slice(b"var ");
            combined.extend_from_slice(var_name(&self.driver, &script.url).as_bytes());
            combined.extend_from_slice(b" = ");
            combined.extend_from_slice(&crate::js_escape::escape_to_js_string_literal(piece, true));
            combined.extend_from_slice(b";\n");
        }
        // Intersect non-caching headers from the first input onward.
        let first_headers = pieces[0].1.response_headers().clone();
        self.driver
            .server_context()
            .merge_non_caching_response_headers(&first_headers, output.response_headers_mut());
        for (_, input) in pieces.iter().skip(1) {
            let keep = input.response_headers().headers().clone();
            output
                .response_headers_mut()
                .headers_mut()
                .remove_if_not_in(&keep);
        }
        let inputs: Vec<InputResource> = pieces.iter().map(|(_, input)| input.clone()).collect();
        let mut partition = CachedResult::new();
        if !self.driver.write_output(
            &inputs,
            &combined,
            ps_http::content_type::mime_to_content_type("application/javascript"),
            "",
            &mut output,
            &mut partition,
        ) {
            return None;
        }
        Some(output.url().to_string())
    }

    /// Renders a combination: `<script src>` before the first element,
    /// `eval()` stubs replacing each original (`MakeCombinedElement` +
    /// `MakeScriptElement`).
    fn render_combination(
        driver: &Rc<ResourceDriver>,
        dom: &mut Dom,
        pieces: &[(PendingScript, InputResource)],
        combined_url: &str,
    ) {
        let first = &pieces[0].0;
        let element = dom.new_element(None, b"script");
        dom.insert_node_before_node(first.element, element);
        let rendered = if driver.options().preserve_url_relativity() {
            GoogleUrl::new(combined_url).relativize(first.relativity, driver.base_url())
        } else {
            combined_url.to_string()
        };
        dom.add_attribute(element, b"src", rendered.as_bytes());
        for (script, _) in pieces {
            let var = var_name(driver, &script.url);
            let stub = dom.new_element(None, b"script");
            dom.insert_node_before_node(script.element, stub);
            let code = format!("eval({var});").into_bytes();
            let text = dom.new_leaf(Some(stub), LeafKind::Characters, code);
            dom.append_child(Some(stub), text);
            dom.delete_node(script.element);
        }
    }
}

impl HtmlFilter for JsCombineFilter {
    fn name(&self) -> &'static str {
        "JsCombine"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.script_depth = 0;
        self.current_js_script = None;
        self.pending.clear();
        self.meta_tag_charset.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let (classification, src) = ScriptTagScanner::parse_script_element(dom, id);
        match classification {
            ScriptClassification::NonScript => {
                if self.script_depth > 0 {
                    if let Some(current) = self.current_js_script {
                        if self
                            .pending
                            .last()
                            .is_some_and(|last| last.element == current)
                        {
                            self.pending.pop();
                        }
                    }
                    self.next_combination(dom);
                }
            }
            ScriptClassification::JavaScript => {
                self.consider_js_for_combination(dom, id, src);
                self.script_depth += 1;
            }
            ScriptClassification::UnknownScript => {
                self.next_combination(dom);
                self.script_depth += 1;
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Script {
            self.script_depth = self.script_depth.saturating_sub(1);
            if self.script_depth == 0 {
                self.current_js_script = None;
            }
        }
        if self.meta_tag_charset.is_empty() && dom.element_keyword(id) == Keyword::Meta {
            if let Some((_, _, charset)) = extract_meta_tag_details(dom, id, None) {
                self.meta_tag_charset = charset;
            }
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, node: NodeId) {
        if self.script_depth > 0
            && !dom
                .leaf_contents(node)
                .iter()
                .all(|byte| byte.is_ascii_whitespace())
        {
            if let Some(current) = self.current_js_script {
                if self
                    .pending
                    .last()
                    .is_some_and(|last| last.element == current)
                {
                    self.pending.pop();
                    self.next_combination(dom);
                }
            }
        }
    }

    fn flush(&mut self, dom: &mut Dom, _lexer: &Lexer) {
        self.next_combination(dom);
    }

    fn ie_directive(&mut self, dom: &mut Dom, _lexer: &Lexer, _node: NodeId) {
        self.next_combination(dom);
    }
}

static JC_ENCODER: MultipartEncoder = MultipartEncoder;

impl RewriteFilter for JsCombineFilter {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &JC_ENCODER
    }

    fn fetch(
        &self,
        driver: &Rc<ResourceDriver>,
        output: OutputResource,
        fetch: &mut dyn ps_http::fetch::AsyncFetch,
    ) -> bool {
        fetch_combined(driver, &self.patterns, output, fetch)
    }
}

/// Serves a combination by decoding its multipart name and
/// concatenating the inputs (no combinability rules on fetch).
fn fetch_combined(
    driver: &Rc<ResourceDriver>,
    patterns: &TokenizerPatterns,
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
    let Some(urls) = encoder.decode(name.as_bytes()) else {
        return fail(fetch);
    };
    if urls.is_empty() {
        return fail(fetch);
    }
    let base = GoogleUrl::new(&output.decoded_base());
    let mut combined = Vec::new();
    let minify = driver.options().enabled(Filter::RewriteJavascriptExternal);
    let mut state = CombinerState::new();
    let mut inputs = Vec::new();
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
            InputRole::Script,
        );
        let Some(mut input) = resource else {
            return fail(fetch);
        };
        let mut ctx = driver.request_context().borrow().clone();
        let mut callback = MockLoadCallback::new();
        input.load(
            NotCacheablePolicy::LoadEvenIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        if !input.loaded() || !input.http_status_ok() {
            return fail(fetch);
        }
        let contents = input.contents();
        let piece: &[u8] = if minify {
            let minified = state.minified_piece(driver, patterns, input.url(), &contents);
            if minified.successful {
                &minified.code
            } else {
                &contents
            }
        } else {
            &contents
        };
        combined.extend_from_slice(b"var ");
        combined.extend_from_slice(var_name(driver, input.url()).as_bytes());
        combined.extend_from_slice(b" = ");
        combined.extend_from_slice(&crate::js_escape::escape_to_js_string_literal(piece, true));
        combined.extend_from_slice(b";\n");
        inputs.push(input);
    }
    // NOTE: fetch writes serve bytes only (FetchWritesPartition is
    // false: fetch results must not decide HTML combinations).
    let mut output = output;
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
        ps_http::content_type::mime_to_content_type("application/javascript"),
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

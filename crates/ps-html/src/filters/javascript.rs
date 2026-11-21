//! JavaScript rewriting (`JavascriptFilter`, `jm`).
//!
//! Minifies external scripts (with source maps and canonical
//! library URLs) and inline script bodies, refusing scripts whose
//! minification could end their element or that pin bytes via
//! integrity attributes. The `sm` twin serves source maps.
//!
//! NOTE: CSP gating waits for the CSP subsystem.

use std::rc::Rc;

use ps_config::filter::Filter;
use ps_http::content_type::{mime_to_content_type, ContentTypeKind, TYPES};
use ps_http::google_url::GoogleUrl;
use ps_js::rewrite::{CodeBlock, CodeBlockConfig};
use ps_js::sourcemap as source_map;
use ps_js::tokenizer::TokenizerPatterns;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::metadata::{CachedResult, OutputPartitions};
use crate::name::Keyword;
use crate::output_resource::{OutputResource, OutputResourceKind};
use crate::parser::Dom;
use crate::resource::{InputResource, Resource};
use crate::rewrite_context::{
    ContextSlot, RewriteResult, SingleRewriteBehavior, SingleRewriteContext,
};
use crate::rewrite_filter::RewriteFilter;
use crate::script_scanner::{ScriptClassification, ScriptTagScanner};
use crate::server_context::{InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver};

/// Whether a URL names a JS combination (`jc` filter id).
fn is_combined_resource_url(url: &GoogleUrl) -> bool {
    let mut namer = ps_cache::namer::ResourceNamer::new();
    namer.decode_ignore_hash_and_signature(url.leaf_with_query()) && namer.id() == "jc"
}

/// Filter id (`RewriteOptions::kJavascriptMinId`).
pub const FILTER_ID: &str = "jm";

/// Source-map filter id (`RewriteOptions::kJavascriptMinSourceMapId`).
pub const SOURCE_MAP_ID: &str = "sm";

/// Minify-use counter (`JavascriptRewriteConfig::kMinifyUses`).
pub const MINIFY_USES: &str = "javascript_minify_uses";

/// Elements whose text runs to their end tag.
const TEXT_ELEMENT_END_TAGS: &[&str] = &[
    "</script",
    "</noscript",
    "</noembed",
    "</noframes",
    "</textarea",
    "</title",
    "</style",
    "</iframe",
    "</xmp",
];

/// Whether text holds a text-element end tag (`HasTextElementEndTag`).
fn has_text_element_end_tag(text: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(text) else {
        return false;
    };
    if !text.contains("</") {
        return false;
    }
    let lower = text.to_lowercase();
    TEXT_ELEMENT_END_TAGS.iter().any(|tag| lower.contains(tag))
}

/// Script tracking across events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptType {
    None,
    Inline,
    External,
}

/// Per-context behavior (source-map mode varies by filter).
struct JavascriptBehavior {
    // Unread: rewrite entry points take the driver explicitly;
    // the stored handle stays for C++ `JavascriptFilter` parity.
    #[allow(dead_code)]
    driver: Rc<ResourceDriver>,
    patterns: Rc<TokenizerPatterns>,
    output_source_map: bool,
}

impl JavascriptBehavior {
    /// Minifies one input into its output(s) (`RewriteJavascript`).
    #[allow(clippy::too_many_lines)]
    fn rewrite_javascript(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        output: &mut OutputResource,
        partition: &mut CachedResult,
    ) -> RewriteResult {
        let server = driver.server_context().clone();
        // NOTE: info messages wait for the message handler.
        let mut rewritten_storage;
        let (rewritten, source_map): (&mut OutputResource, Option<&mut OutputResource>) =
            if self.output_source_map {
                let created = driver.create_output_resource_from_resource(
                    FILTER_ID,
                    self.encoder(),
                    input,
                    OutputResourceKind::Rewritten,
                );
                let Ok(created) = created else {
                    return RewriteResult::Failed;
                };
                rewritten_storage = created;
                (&mut rewritten_storage, Some(output))
            } else {
                (output, None)
            };
        // Unused in this branch until source maps need it.
        let _ = &source_map;
        let input_type = input
            .response_headers()
            .determine_content_type()
            .filter(|content_type| content_type.is_js_like());
        let Some(input_type) = input_type else {
            return RewriteResult::Failed;
        };
        let _ = input_type;
        let minify = driver.options().enabled(Filter::RewriteJavascriptExternal)
            || driver.options().enabled(Filter::RewriteJavascriptInline);
        let mut libraries = ps_js::library::LibraryIdentification::new();
        if driver
            .options()
            .enabled(Filter::CanonicalizeJavascriptLibraries)
        {
            for (bytes, entries) in driver.options().libraries() {
                for (md5, url) in entries {
                    libraries.register(*bytes, md5, url);
                }
            }
        }
        let config = CodeBlockConfig {
            minify,
            use_experimental_minifier: driver.options().use_experimental_js_minifier(),
        };
        let mut code_block = CodeBlock::new(
            &self.patterns,
            config,
            if driver
                .options()
                .enabled(Filter::CanonicalizeJavascriptLibraries)
            {
                Some(&mut libraries)
            } else {
                None
            },
            &input.contents(),
        );
        let rewritten_ok = code_block.rewrite();
        let outcome = code_block.outcome();
        server
            .variable("javascript_blocks_minified")
            .add(outcome.blocks_minified as i64);
        server
            .variable("javascript_minification_failures")
            .add(outcome.minification_failures as i64);
        server
            .variable("javascript_total_bytes_saved")
            .add(outcome.bytes_saved as i64);
        server
            .variable("javascript_total_original_bytes")
            .add(outcome.original_bytes as i64);
        server
            .variable("javascript_reducing_minifications")
            .add(outcome.reducing_uses as i64);
        if let Some(library_url) = code_block.library_url() {
            server.variable("javascript_libraries_identified").add(1);
            // Canonical URLs resolve against the base (protocol-relative ok).
            let library_gurl = GoogleUrl::resolve(driver.base_url(), &library_url);
            if !library_gurl.is_web_valid() {
                return RewriteResult::Failed;
            }
            partition.url = library_gurl.spec().to_string();
            partition.canonicalize_url = true;
            return RewriteResult::Failed;
        }
        if !driver.options().enabled(Filter::RewriteJavascriptExternal) {
            server.variable("javascript_minification_disabled").add(1);
            return RewriteResult::Failed;
        }
        if !rewritten_ok {
            server.variable("javascript_did_not_shrink").add(1);
            return RewriteResult::Failed;
        }
        if code_block.source_mappings().is_empty() {
            if self.output_source_map {
                return RewriteResult::Failed;
            }
        } else if driver.options().enabled(Filter::IncludeJsSourceMaps) || self.output_source_map {
            let original = GoogleUrl::new(input.url());
            let source_url = if server.is_pagespeed_resource(&original) {
                original.spec().to_string()
            } else {
                // Keep the source URL out of IPRO rewriting.
                original
                    .copy_and_add_query_param("PageSpeed", "off")
                    .spec()
                    .to_string()
            };
            let map_text =
                source_map::encode(code_block.source_mappings(), &[source_url.as_str()], "");
            if let Some(source_map) = source_map {
                if !self.write_source_map_to(driver, input, &map_text, source_map, partition) {
                    return RewriteResult::Failed;
                }
                code_block.append_source_map_url(&source_map.url());
            } else {
                // HTML/rewritten-JS flow: mint the map output, write it,
                // then link the rewritten JS below (which re-links the
                // partition to the JS URL, as in C++).
                let created = driver.create_output_resource_from_resource(
                    SOURCE_MAP_ID,
                    self.encoder(),
                    input,
                    OutputResourceKind::Rewritten,
                );
                let Ok(mut created) = created else {
                    return RewriteResult::Failed;
                };
                // A scratch partition: the map write must not disturb
                // the JS partition's link (C++ overwrites it next).
                let mut scratch = CachedResult::new();
                if !self.write_source_map_to(driver, input, &map_text, &mut created, &mut scratch) {
                    return RewriteResult::Failed;
                }
                code_block.append_source_map_url(&created.url());
            }
        }
        if !self.write_external_script_to(
            driver,
            input,
            code_block.rewritten_code(),
            rewritten,
            partition,
        ) {
            server.variable("javascript_failed_to_write").add(1);
            return RewriteResult::Failed;
        }
        // Introspection only blocks relocation (AJAX rewrites keep the
        // URL), so it is checked after writing.
        if driver.options().avoid_renaming_introspective_javascript()
            && ps_js::rewrite::unsafe_to_rename(code_block.rewritten_code())
        {
            partition.url_relocatable = false;
        }
        RewriteResult::Ok
    }

    /// Writes minified JS (`WriteExternalScriptTo`).
    fn write_external_script_to(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        script_out: &[u8],
        dest: &mut OutputResource,
        partition: &mut CachedResult,
    ) -> bool {
        driver.server_context().merge_non_caching_response_headers(
            input.response_headers(),
            dest.response_headers_mut(),
        );
        let content_type = input
            .content_type()
            .filter(|content_type| content_type.is_js_like())
            .or_else(|| mime_to_content_type("application/javascript"));
        let inputs = [input.clone()];
        driver.write_output(
            &inputs,
            script_out,
            content_type,
            input.charset(),
            dest,
            partition,
        )
    }

    /// Writes a source map (`WriteSourceMapTo`).
    fn write_source_map_to(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        contents: &str,
        source_map: &mut OutputResource,
        partition: &mut CachedResult,
    ) -> bool {
        source_map
            .response_headers_mut()
            .headers_mut()
            .add("X-Content-Type-Options", "nosniff");
        source_map
            .response_headers_mut()
            .headers_mut()
            .add("Content-Disposition", "attachment");
        let inputs = [input.clone()];
        // `kContentTypeSourceMap` (JS MIME, `.map` extension): looked up
        // by kind, since the bare MIME would resolve to plain JS.
        let map_type = TYPES
            .iter()
            .find(|entry| entry.kind() == ContentTypeKind::SourceMap)
            .copied();
        driver.write_output(
            &inputs,
            contents.as_bytes(),
            map_type,
            "utf-8",
            source_map,
            partition,
        )
    }
}

impl SingleRewriteBehavior for JavascriptBehavior {
    fn id(&self) -> &'static str {
        if self.output_source_map {
            SOURCE_MAP_ID
        } else {
            FILTER_ID
        }
    }

    fn kind(&self) -> OutputResourceKind {
        OutputResourceKind::Rewritten
    }

    fn optimization_only(&self) -> bool {
        // Never fall back to raw JS for source maps.
        !self.output_source_map
    }

    fn fail_on_hash_mismatch(&self) -> bool {
        // A mismatched map describes the wrong program.
        self.output_source_map
    }

    fn policy_permits_rendering(
        &self,
        _driver: &Rc<ResourceDriver>,
        _outputs: &[Option<OutputResource>],
    ) -> bool {
        // NOTE: `AreOutputsAllowedByCsp(kScriptSrc)` waits for CSP.
        true
    }

    fn note_render(
        &self,
        driver: &Rc<ResourceDriver>,
        dom: &mut Dom,
        partitions: &OutputPartitions,
        slots: &[ContextSlot],
    ) {
        if partitions.partition.len() != 1 || slots.is_empty() {
            return;
        }
        let result = &partitions.partition[0];
        let slot = &slots[0];
        if !result.url_relocatable {
            if let Some(element) = slot.element() {
                driver.insert_debug_comment(dom, ps_js::rewrite::INTROSPECTION_COMMENT, element);
            }
            return;
        }
        if !result.optimizable {
            if result.canonicalize_url {
                // Patch the canonical library URL past the naming
                // scheme (rendering stays disabled for it).
                slot.direct_set_url(dom, &result.url);
            }
            return;
        }
        driver.server_context().variable(MINIFY_USES).add(1);
    }

    fn rewrite_single(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        output: &mut OutputResource,
        partition: &mut CachedResult,
    ) -> RewriteResult {
        self.rewrite_javascript(driver, input, output, partition)
    }
}

/// JavaScript rewriting filter (`JavascriptFilter`).
pub struct JavascriptFilter {
    driver: Rc<ResourceDriver>,
    patterns: Rc<TokenizerPatterns>,
    script_type: ScriptType,
    output_source_map: bool,
}

impl JavascriptFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        JavascriptFilter {
            driver,
            patterns: Rc::new(TokenizerPatterns::new()),
            script_type: ScriptType::None,
            output_source_map: false,
        }
    }

    /// Builds the source-map twin (`JavascriptSourceMapFilter`).
    #[must_use]
    pub fn new_source_map(driver: Rc<ResourceDriver>) -> Self {
        JavascriptFilter {
            driver,
            patterns: Rc::new(TokenizerPatterns::new()),
            script_type: ScriptType::None,
            output_source_map: true,
        }
    }

    /// Whether minification could end the element
    /// (`MinifiedScriptMayEndElement`).
    pub fn minified_script_may_end_element(original: &[u8], minified: &[u8]) -> bool {
        minified != original
            && (has_text_element_end_tag(minified)
                || (minified.windows(4).any(|window| window == b"<!--")
                    && minified
                        .windows(7)
                        .any(|window| window.eq_ignore_ascii_case(b"<script"))))
    }

    /// Whether an element sits inside a sometimes-literal element.
    fn inside_sometimes_literal(dom: &Dom, mut id: NodeId) -> bool {
        while let Some(parent) = dom.arena_node(id).parent() {
            match dom.element_keyword(parent) {
                Keyword::Noembed | Keyword::Noframes | Keyword::Noscript => return true,
                _ => {}
            }
            id = parent;
        }
        false
    }

    /// Minifies an external script (`RewriteExternalScript`).
    fn rewrite_external_script(&self, dom: &mut Dom, id: NodeId, src: usize) {
        if ScriptTagScanner::has_integrity_attribute(dom, id) {
            self.driver.insert_debug_comment(
                dom,
                "JS not rewritten: integrity attribute pins the script bytes",
                id,
            );
            return;
        }
        let (decoded, _) = dom.decoded_attribute(id, src);
        let Some(value) = decoded else {
            return;
        };
        let url = GoogleUrl::resolve(self.driver.base_url(), &String::from_utf8_lossy(&value));
        if !url.is_web_valid() {
            return;
        }
        // Combinations render at flush time and bypass the filter
        // chain upstream, but synthesize consumable events here; only
        // they stand down (outlines emit mid-stream and chain
        // upstream too, so they nested-rewrite normally).
        if self.driver.server_context().is_pagespeed_resource(&url)
            && is_combined_resource_url(&url)
        {
            return;
        }
        let (resource, _authorized) = self.driver.create_input_resource(
            &url,
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Script,
        );
        let Some(input) = resource else {
            return;
        };
        let handle: Rc<dyn Resource> = Rc::new(input.clone());
        let slot = self.driver.get_slot(Some(handle), dom, id, src, "jm");
        if self.driver.options().js_preserve_urls() {
            slot.borrow_mut().base_mut().set_preserve_urls(true);
        }
        let behavior = JavascriptBehavior {
            driver: self.driver.clone(),
            patterns: self.patterns.clone(),
            output_source_map: self.output_source_map,
        };
        let mut context = SingleRewriteContext::new(self.driver.clone(), behavior);
        context.add_slot(slot, input);
        context.start(dom);
    }

    /// Minifies an inline script body (`RewriteInlineScript`).
    fn rewrite_inline_script(&self, dom: &mut Dom, node: NodeId) {
        let Some(parent) = dom.arena_node(node).parent() else {
            return;
        };
        if Self::inside_sometimes_literal(dom, parent) {
            return;
        }
        // NOTE: `CanModifyInlineScript` waits for the CSP subsystem.
        let script = dom.leaf_contents(node);
        let options = self.driver.options();
        let minify = options.enabled(Filter::RewriteJavascriptExternal)
            || options.enabled(Filter::RewriteJavascriptInline);
        let mut libraries = ps_js::library::LibraryIdentification::new();
        let identify = options.enabled(Filter::CanonicalizeJavascriptLibraries);
        if identify {
            for (bytes, entries) in options.libraries() {
                for (md5, url) in entries {
                    libraries.register(*bytes, md5, url);
                }
            }
        }
        let config = CodeBlockConfig {
            minify,
            use_experimental_minifier: options.use_experimental_js_minifier(),
        };
        drop(options);
        let mut code_block = CodeBlock::new(
            &self.patterns,
            config,
            if identify { Some(&mut libraries) } else { None },
            &script,
        );
        code_block.rewrite();
        let outcome = code_block.outcome();
        let server = self.driver.server_context().clone();
        server
            .variable("javascript_blocks_minified")
            .add(outcome.blocks_minified as i64);
        server
            .variable("javascript_minification_failures")
            .add(outcome.minification_failures as i64);
        server
            .variable("javascript_total_bytes_saved")
            .add(outcome.bytes_saved as i64);
        server
            .variable("javascript_total_original_bytes")
            .add(outcome.original_bytes as i64);
        server
            .variable("javascript_reducing_minifications")
            .add(outcome.reducing_uses as i64);
        // Library matches on inline scripts stay inline (outlining
        // them is a TODO even in C++).
        let _ = code_block.library_url();
        if !code_block.rewritten_code().is_empty()
            && code_block.rewritten_code().len() < script.len()
            && !Self::minified_script_may_end_element(&script, code_block.rewritten_code())
        {
            // XHTML pages need their CDATA markers preserved.
            use crate::server_context::XhtmlStatus;
            let rewritten = code_block.rewritten_code().to_vec();
            let xhtml_status = self.driver.mime_type_xhtml_status();
            let has_cdata = script.windows(9).any(|window| window == b"<![CDATA[");
            let rewritten_has_cdata_start = rewritten.starts_with(b"<![CDATA");
            if xhtml_status != XhtmlStatus::IsNotXhtml && has_cdata && !rewritten_has_cdata_start {
                if rewritten.windows(3).any(|window| window == b"]]>") {
                    self.driver
                        .server_context()
                        .variable("javascript_did_not_shrink")
                        .add(1);
                    return;
                }
                let mut wrapped = b"//<![CDATA[\n".to_vec();
                wrapped.extend_from_slice(&rewritten);
                wrapped.extend_from_slice(b"\n//]]>");
                *dom.leaf_contents_mut(node) = wrapped;
            } else {
                // NOTE: `known_xhtml` consults finalized headers plus
                // the parse type in C++; the port treats any XHTML
                // verdict as known (the unfinalized-headers case is
                // untestable without server plumbing).
                let known_xhtml = xhtml_status == XhtmlStatus::IsXhtml;
                let changes_markers = xhtml_status != XhtmlStatus::IsNotXhtml
                    && (count_substring(&rewritten, b"<![CDATA[")
                        != count_substring(&script, b"<![CDATA[")
                        || count_substring(&rewritten, b"]]>") != count_substring(&script, b"]]>"));
                let drops_comment =
                    known_xhtml && script.windows(4).any(|window| window == b"<!--");
                if changes_markers || drops_comment {
                    self.driver
                        .server_context()
                        .variable("javascript_did_not_shrink")
                        .add(1);
                    return;
                }
                *dom.leaf_contents_mut(node) = rewritten;
            }
            self.driver.server_context().variable(MINIFY_USES).add(1);
        } else {
            self.driver
                .server_context()
                .variable("javascript_did_not_shrink")
                .add(1);
        }
    }
}

/// Counts non-overlapping occurrences (`CountSubstring`).
fn count_substring(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut rest = haystack;
    while let Some(pos) = rest
        .windows(needle.len())
        .position(|window| window == needle)
    {
        count += 1;
        rest = &rest[pos + needle.len()..];
    }
    count
}

impl HtmlFilter for JavascriptFilter {
    fn name(&self) -> &'static str {
        "Javascript"
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let (classification, src) = ScriptTagScanner::parse_script_element(dom, id);
        if classification != ScriptClassification::JavaScript {
            return;
        }
        let options = self.driver.options();
        if let Some(src) = src {
            if options.enabled(Filter::RewriteJavascriptExternal)
                || options.enabled(Filter::CanonicalizeJavascriptLibraries)
            {
                self.script_type = ScriptType::External;
                drop(options);
                self.rewrite_external_script(dom, id, src);
            }
        } else if options.enabled(Filter::RewriteJavascriptInline) {
            self.script_type = ScriptType::Inline;
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, node: NodeId) {
        match self.script_type {
            ScriptType::Inline => self.rewrite_inline_script(dom, node),
            ScriptType::External => {
                // External bodies hold data or nothing; drop pure
                // whitespace (NUL bytes count as whitespace too).
                let contents = dom.leaf_contents(node);
                if contents
                    .iter()
                    .all(|byte| byte.is_ascii_whitespace() || *byte == 0)
                {
                    dom.delete_node(node);
                }
            }
            ScriptType::None => {}
        }
    }

    fn end_element(&mut self, _dom: &mut Dom, _lexer: &Lexer, _id: NodeId) {
        self.script_type = ScriptType::None;
    }

    fn ie_directive(&mut self, _dom: &mut Dom, _lexer: &Lexer, _node: NodeId) {
        // Concealed scripts stay unminified (tracked for beacons).
    }
}

impl RewriteFilter for JavascriptFilter {
    fn id(&self) -> &'static str {
        if self.output_source_map {
            SOURCE_MAP_ID
        } else {
            FILTER_ID
        }
    }

    fn rewrite_in_place(
        &self,
        _driver: &Rc<ResourceDriver>,
        input: &[u8],
        content_type: &ps_http::content_type::ContentType,
    ) -> Option<crate::rewrite_filter::InPlaceOutput> {
        // Minify-only subset (no combining; the full
        // `.pagespeed.` path stays authoritative for that).
        let (bytes, success) = ps_js::minify::minify_utf8(&self.patterns, input);
        if !success {
            return None;
        }
        Some(crate::rewrite_filter::InPlaceOutput {
            bytes,
            content_type: *content_type,
            optimized_image_type: None,
        })
    }

    fn fetch(
        &self,
        driver: &Rc<ResourceDriver>,
        output: OutputResource,
        fetch: &mut dyn ps_http::fetch::AsyncFetch,
    ) -> bool {
        let behavior = JavascriptBehavior {
            driver: driver.clone(),
            patterns: self.patterns.clone(),
            output_source_map: self.output_source_map,
        };
        let mut context = SingleRewriteContext::new(driver.clone(), behavior);
        context.fetch(output, fetch)
    }
}

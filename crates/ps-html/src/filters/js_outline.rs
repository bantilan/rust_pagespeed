//! JavaScript outlining (`JsOutlineFilter`, `jo`).
//!
//! Moves large inline scripts into external outlined resources,
//! replacing them with `<script src>` elements (async/defer
//! stripped, other attributes preserved). Scripts with `src`,
//! integrity pins, modules, or tags inside are left inline, as are
//! small scripts under the threshold.
//!
//! NOTE: CSP gating waits for the CSP subsystem; error-message paths
//! wait for the message handler.

use std::rc::Rc;

use ps_config::filter::Filter;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::metadata::CachedResult;
use crate::name::Keyword;
use crate::output_resource::OutputResourceKind;
use crate::parser::Dom;
use crate::resource::Resource;
use crate::script_scanner::{ScriptClassification, ScriptTagScanner};
use crate::server_context::ResourceDriver;

/// Filter id (`JsOutlineFilter::kFilterId`).
pub const FILTER_ID: &str = "jo";

/// JavaScript outlining filter (`JsOutlineFilter`).
pub struct JsOutlineFilter {
    driver: Rc<ResourceDriver>,
    inline_element: Option<NodeId>,
    inline_chars: Option<NodeId>,
    size_threshold_bytes: i64,
}

impl JsOutlineFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        let size_threshold_bytes = driver.options().js_outline_min_bytes();
        JsOutlineFilter {
            driver,
            inline_element: None,
            inline_chars: None,
            size_threshold_bytes,
        }
    }

    /// Outlines one script (`OutlineScript`).
    fn outline_script(&self, dom: &mut Dom, inline_element: NodeId, content: &[u8]) {
        // NOTE: the script-src policy check waits for CSP.
        if !dom.is_rewritable(inline_element) {
            return;
        }
        let base_url = self.driver.base_url().clone();
        let output = self.driver.create_output_resource_with_unmapped_url(
            &base_url,
            FILTER_ID,
            b"_",
            OutputResourceKind::Outlined,
        );
        let mut output = match output {
            Ok(output) => output,
            Err(reason) => {
                self.driver
                    .insert_debug_comment(dom, &reason, inline_element);
                return;
            }
        };
        // NOTE: charset intentionally unset (inherited from the page).
        let inputs: Vec<crate::resource::InputResource> = Vec::new();
        let mut partition = CachedResult::new();
        if !self.driver.write_output(
            &inputs,
            content,
            ps_http::content_type::mime_to_content_type("application/javascript"),
            "",
            &mut output,
            &mut partition,
        ) {
            self.driver.insert_debug_comment(
                dom,
                "Failed to write outlined script resource.",
                inline_element,
            );
            return;
        }
        let outline_element = dom.new_element(None, b"script");
        // Clone attributes verbatim except async/defer (ignored
        // inline, meaningful externally).
        let kept: Vec<(Vec<u8>, Vec<u8>, crate::element::QuoteStyle)> = {
            let attributes = dom.element_attributes(inline_element);
            let mut names = Vec::new();
            for index in 0..attributes.len() {
                let keyword = attributes[index].keyword();
                if keyword == Keyword::Async || keyword == Keyword::Defer {
                    continue;
                }
                names.push((
                    index,
                    attributes[index].name_str().to_vec(),
                    attributes[index].quote_style(),
                ));
            }
            let mut kept = Vec::new();
            for (index, name, style) in names {
                kept.push((
                    name,
                    dom.decoded_attribute(inline_element, index)
                        .0
                        .unwrap_or_default(),
                    style,
                ));
            }
            kept
        };
        for (name, value, style) in kept {
            dom.add_attribute(outline_element, &name, &value);
            let last = dom.element_attributes(outline_element).len() - 1;
            dom.element_attributes_mut(outline_element)[last].set_quote_style(style);
        }
        dom.add_attribute(outline_element, b"src", output.url().as_bytes());
        dom.insert_node_before_node(inline_element, outline_element);
        dom.delete_node(inline_element);
    }
}

impl HtmlFilter for JsOutlineFilter {
    fn name(&self) -> &'static str {
        "OutlineJs"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.inline_element = None;
        self.inline_chars = None;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.inline_element.is_some() {
            // NOTE: "tag inside script" error waits for the handler.
            self.inline_element = None;
            self.inline_chars = None;
        }
        let (classification, src) = ScriptTagScanner::parse_script_element(dom, id);
        if classification == ScriptClassification::JavaScript {
            if ScriptTagScanner::has_integrity_attribute(dom, id) {
                self.driver.insert_debug_comment(
                    dom,
                    "JS not outlined: integrity attribute would become enforced on the outlined script",
                    id,
                );
                return;
            }
            self.inline_element = Some(id);
            self.inline_chars = None;
            // Scripts which already have a src are not outlined.
            if src.is_some() {
                self.inline_element = None;
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if let Some(inline) = self.inline_element {
            if id != inline {
                // NOTE: "tag inside script" error waits for the handler.
            } else if let Some(chars) = self.inline_chars {
                let contents = dom.leaf_contents(chars);
                if contents.len() as i64 >= self.size_threshold_bytes
                    && self.driver.options().enabled(Filter::OutlineJavascript)
                {
                    self.outline_script(dom, inline, &contents);
                }
            }
            self.inline_element = None;
            self.inline_chars = None;
        }
    }

    fn characters(&mut self, _dom: &mut Dom, _lexer: &Lexer, node: NodeId) {
        if self.inline_element.is_some() {
            self.inline_chars = Some(node);
        }
    }

    fn flush(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // A script open across a flush cannot be outlined.
        self.inline_element = None;
        self.inline_chars = None;
    }
}

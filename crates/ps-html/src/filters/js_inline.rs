//! JavaScript inlining (`JsInlineFilter`, `ji`).
//!
//! Replaces short external scripts with inline `<script>` bodies,
//! escaping text-element end tags (`</script>`, `</textarea>`, ...)
//! as `\uNNNN` so the bytes cannot end their container. XHTML pages
//! get the body wrapped in a commented CDATA block.
//!
//! NOTE: CSP gating waits for the CSP subsystem.

use std::rc::Rc;

use ps_cache::hash::Hasher;
use ps_config::options::Category;
use ps_http::google_url::GoogleUrl;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::metadata::CachedResult;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::resource::InputResource;
use crate::rewrite_context::{InlineBehavior, InlineRewriteContext};
use crate::script_scanner::{ScriptClassification, ScriptTagScanner, EXECUTE_ASYNC, EXECUTE_DEFER};
use crate::server_context::{InlineAuthorizationPolicy, InputRole, ResourceDriver};

/// Filter id (`RewriteOptions::kJavascriptInlineId`).
pub const FILTER_ID: &str = "ji";

/// Inline counter (`JsInlineFilter::kNumJsInlined`).
pub const NUM_JS_INLINED: &str = "num_js_inlined";

/// Elements whose text runs to their end tag: an inlined script may
/// sit inside any of them, so its text must not contain such an end.
const TEXT_ELEMENT_NAMES: &[&str] = &[
    "script", "noscript", "noembed", "noframes", "textarea", "title", "style", "iframe", "xmp",
];

/// Escapes text-element tags (`EscapeTextElementTags`): the first
/// letter of each `</name` (any case) and each `<script` becomes a
/// `\uNNNN` escape (valid in strings, regexps, identifiers, and
/// cooked template text alike). Returns whether anything changed.
pub fn escape_text_element_tags(contents: &[u8], out: &mut Vec<u8>) -> bool {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut escaped = false;
    out.clear();
    out.reserve(contents.len());
    let mut i = 0;
    while i < contents.len() {
        out.push(contents[i]);
        if contents[i] != b'<' {
            i += 1;
            continue;
        }
        let mut letter = i + 1;
        let end_tag = letter < contents.len() && contents[letter] == b'/';
        if end_tag {
            letter += 1;
        }
        let rest = &contents[letter.min(contents.len())..];
        let mut matched = false;
        for name in TEXT_ELEMENT_NAMES {
            if (end_tag || *name == "script")
                && rest.len() >= name.len()
                && rest[..name.len()].eq_ignore_ascii_case(name.as_bytes())
            {
                if end_tag {
                    out.push(b'/');
                }
                let ch = contents[letter];
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(ch >> 4) as usize]);
                out.push(HEX[(ch & 0xf) as usize]);
                i = letter;
                escaped = true;
                matched = true;
                break;
            }
        }
        let _ = matched;
        i += 1;
    }
    escaped
}

/// Per-context behavior (charsets vary by element and page).
struct JsInlineBehavior {
    driver: Rc<ResourceDriver>,
    attrs_charset: Vec<u8>,
    htmls_charset: Vec<u8>,
    size_threshold_bytes: i64,
}

impl InlineBehavior for JsInlineBehavior {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn input_role(&self) -> InputRole {
        InputRole::Script
    }

    fn allow_unauthorized_domain(&self, driver: &Rc<ResourceDriver>) -> InlineAuthorizationPolicy {
        if driver
            .options()
            .has_inline_unauthorized_resource_type(Category::Script)
        {
            InlineAuthorizationPolicy::InlineUnauthorizedResources
        } else {
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources
        }
    }

    fn intended_for_inlining(&self) -> bool {
        true
    }

    fn cache_key_suffix(&self) -> String {
        let charsets = format!(
            "{}/{}",
            String::from_utf8_lossy(&self.htmls_charset),
            String::from_utf8_lossy(&self.attrs_charset)
        )
        .to_lowercase();
        self.driver
            .server_context()
            .lock_hasher()
            .hash(charsets.as_bytes())
    }

    fn policy_permits_rendering(&self) -> bool {
        // NOTE: `PermitsInlineScript` waits for the CSP subsystem.
        true
    }

    fn should_inline(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        reason: &mut String,
    ) -> bool {
        let contents = input.contents();
        if contents.len() as i64 > self.size_threshold_bytes {
            *reason = format!(
                "JS not inlined since it's bigger than {} bytes",
                self.size_threshold_bytes
            );
            return false;
        }
        if contents.len() >= 10 && contents[0] == 0x1f && contents[1] == 0x8b {
            *reason = "JS not inlined because it appears to be gzip-encoded".to_string();
            return false;
        }
        if driver.options().avoid_renaming_introspective_javascript()
            && ps_js::rewrite::unsafe_to_rename(&contents)
        {
            *reason = "JS not inlined since it may be looking for its source".to_string();
            return false;
        }
        // Escaping changes a template literal's raw text (visible to
        // tag functions), so escape-needing scripts with backticks
        // stay external.
        let mut escaped = Vec::new();
        if contents.contains(&b'`') && escape_text_element_tags(&contents, &mut escaped) {
            *reason =
                "JS not inlined since it has a template literal and text that would need escaping"
                    .to_string();
            return false;
        }
        let script_charset = crate::rewrite_filter::charset_for_script(
            input.charset().as_bytes(),
            &contents,
            &self.attrs_charset,
            &self.htmls_charset,
        );
        if !self.htmls_charset.eq_ignore_ascii_case(&script_charset) {
            let clean = ps_css::combine::strip_utf8_bom(&contents).0;
            if clean.iter().any(|byte| *byte == 0 || *byte >= 0x80) {
                *reason = format!(
                    "JS not inlined due to apparent charset incompatibility; we think the HTML is {} while the JS is {}",
                    String::from_utf8_lossy(&self.htmls_charset),
                    String::from_utf8_lossy(&script_charset)
                );
                return false;
            }
        }
        true
    }

    fn render_inline(
        &self,
        driver: &Rc<ResourceDriver>,
        _input: &InputResource,
        partition: &CachedResult,
        _base_url: &GoogleUrl,
        dom: &mut Dom,
        element: NodeId,
    ) {
        let contents = partition
            .inlined_data
            .clone()
            .expect("rendering without inlined data");
        let mut escaped_buf = Vec::new();
        let escaped: &[u8] = if escape_text_element_tags(&contents, &mut escaped_buf) {
            &escaped_buf
        } else {
            &contents
        };
        if driver.mime_type_xhtml_status() != crate::server_context::XhtmlStatus::IsNotXhtml {
            // CDATA sections cannot nest (they end at the first
            // `]]>`), so such scripts cannot inline on XHTML pages.
            if escaped.windows(3).any(|window| window == b"]]>") {
                return;
            }
            let mut body = b"//<![CDATA[\n".to_vec();
            body.extend_from_slice(escaped);
            body.extend_from_slice(b"\n//]]>");
            let node = dom.new_leaf(Some(element), crate::element::LeafKind::Characters, body);
            dom.append_child(Some(element), node);
            dom.delete_attribute(element, Keyword::Src);
        } else {
            let node = dom.new_leaf(
                Some(element),
                crate::element::LeafKind::Characters,
                escaped.to_vec(),
            );
            dom.append_child(Some(element), node);
            dom.delete_attribute(element, Keyword::Src);
        }
        driver.server_context().variable(NUM_JS_INLINED).add(1);
    }
}

/// JavaScript-inlining filter (`JsInlineFilter`).
pub struct JsInlineFilter {
    driver: Rc<ResourceDriver>,
    size_threshold_bytes: i64,
    should_inline: bool,
}

impl JsInlineFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        let size_threshold_bytes = driver.options().js_inline_max_bytes();
        JsInlineFilter {
            driver,
            size_threshold_bytes,
            should_inline: false,
        }
    }

    /// Whether an element sits inside a sometimes-literal element
    /// (`IsInsideSometimesLiteralElement`).
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
}

impl HtmlFilter for JsInlineFilter {
    fn name(&self) -> &'static str {
        "JsInline"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.should_inline = false;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let (classification, src) = ScriptTagScanner::parse_script_element(dom, id);
        if classification != ScriptClassification::JavaScript {
            return;
        }
        let Some(src) = src else {
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, src);
        if decoded.is_none() {
            return;
        }
        self.should_inline = true;
        // Inline text would read as the enclosing literal element's
        // text (and scripts never run there anyway).
        if Self::inside_sometimes_literal(dom, id) {
            self.should_inline = false;
        }
        // Inlining drops integrity enforcement.
        if self.should_inline && ScriptTagScanner::has_integrity_attribute(dom, id) {
            self.should_inline = false;
            self.driver.insert_debug_comment(
                dom,
                "JS not inlined because of the integrity attribute",
                id,
            );
        }
        // Inlining is synchronous: async/defer scripts run after
        // parsing, possibly out of order. for/event scripts keep
        // their trigger and are unaffected.
        if self.should_inline
            && ScriptTagScanner::execution_mode(dom, id) & (EXECUTE_ASYNC | EXECUTE_DEFER) != 0
        {
            self.should_inline = false;
            self.driver.insert_debug_comment(
                dom,
                "JS not inlined because of async/defer execution mode",
                id,
            );
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.should_inline && dom.is_rewritable(id) && !self.driver.is_element_claimed(id) {
            if let Some(src) = dom.find_attribute(id, Keyword::Src) {
                let (decoded, _) = dom.decoded_attribute(id, src);
                let attrs_charset = dom
                    .find_attribute(id, Keyword::Charset)
                    .and_then(|index| dom.decoded_attribute(id, index).0)
                    .unwrap_or_default();
                let behavior = JsInlineBehavior {
                    driver: self.driver.clone(),
                    attrs_charset,
                    htmls_charset: self.driver.containing_charset().into_bytes(),
                    size_threshold_bytes: self.size_threshold_bytes,
                };
                let base_url = self.driver.base_url().clone();
                let mut context =
                    InlineRewriteContext::new(self.driver.clone(), behavior, id, base_url);
                context.start_inlining(dom, src, decoded.as_deref());
            }
        }
        self.should_inline = false;
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, node: NodeId) {
        if !self.should_inline {
            return;
        }
        let Some(parent) = dom.arena_node(node).parent() else {
            return;
        };
        let contents = dom.leaf_contents(node);
        if dom.is_rewritable(parent) && contents.iter().all(|byte| byte.is_ascii_whitespace()) {
            // Whitespace-only bodies are safely dropped.
            dom.delete_node(node);
        } else {
            // Non-empty bodies may store data for the external
            // script; leave the tag alone.
            self.should_inline = false;
        }
    }
}

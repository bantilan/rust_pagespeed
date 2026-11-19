//! Legacy polyfill stripping (`StripLegacyPolyfillsFilter`).
//!
//! Deletes classic JavaScript `<script src>` elements approved either by an
//! app marker (`data-pagespeed-remove-polyfill="true"`) or by exact match
//! against the operator's allowlist (plus a canonical-leaf check so URL
//! canonicalization cannot broaden it). Modules, fallbacks, data blocks,
//! and no-transform scripts are never touched.

use ps_http::google_url::GoogleUrl;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::script_scanner::{ScriptClassification, ScriptTagScanner};

/// Fixed base for allowlist verification (avoids borrowing the page's own
/// filename when src is only a query or fragment).
const VERIFY_BASE: &str = "https://pagespeed.invalid/";

/// Strips approved legacy polyfill scripts.
#[derive(Debug, Default)]
pub struct StripLegacyPolyfillsFilter {
    approved: Vec<String>,
}

impl StripLegacyPolyfillsFilter {
    /// Builds the filter with the approved filename set.
    #[must_use]
    pub fn new(approved_filenames: &std::collections::HashSet<String>) -> Self {
        StripLegacyPolyfillsFilter {
            approved: approved_filenames.iter().cloned().collect(),
        }
    }
}

impl HtmlFilter for StripLegacyPolyfillsFilter {
    fn name(&self) -> &'static str {
        "StripLegacyPolyfills"
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) != Keyword::Script {
            return;
        }
        let marker = dom
            .find_attribute_by_name(id, b"data-pagespeed-remove-polyfill")
            .and_then(|index| dom.decoded_attribute(id, index).0);
        if dom
            .find_attribute(id, Keyword::DataPagespeedNoTransform)
            .is_some()
            || dom.find_attribute_by_name(id, b"nomodule").is_some()
        {
            return;
        }
        let mut approved = marker
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(b"true"));
        if !approved && self.approved.is_empty() {
            return;
        }
        let (classification, src) = ScriptTagScanner::parse_script_element(dom, id);
        if classification != ScriptClassification::JavaScript {
            return;
        }
        let Some(src) = src else {
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, src);
        let Some(url) = decoded else {
            return;
        };
        let trimmed: Vec<u8> = url.trim_ascii().to_vec();
        if trimmed.is_empty() {
            return;
        }
        if !approved {
            let path = trimmed.split(|byte| *byte == b'?' || *byte == b'#').next();
            let filename = path
                .and_then(|path| path.rsplit(|byte| *byte == b'/').next())
                .unwrap_or(b"");
            if !self.approved.iter().any(|name| name.as_bytes() == filename) {
                return;
            }
            let parsed = GoogleUrl::resolve(
                &GoogleUrl::new(VERIFY_BASE),
                &String::from_utf8_lossy(&trimmed),
            );
            approved = parsed.is_web_valid() && parsed.leaf_sans_query().as_bytes() == filename;
        }
        if approved {
            dom.delete_node(id);
        }
    }
}

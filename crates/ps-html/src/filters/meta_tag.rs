//! Meta-tag to response-header propagation (`MetaTagFilter`).
//!
//! Converts `<meta http-equiv="Content-Type" content="...">` and `<meta
//! charset="...">` tags into response `Content-Type` values (HTML mimetypes
//! only), keeping caching fields current. The headers live with the driver
//! and are shared here so mutations stay visible on both sides.

use std::cell::RefCell;
use std::rc::Rc;

use ps_http::content_type::{mime_to_content_type, ContentTypeKind};
use ps_http::response::ResponseHeaders;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Live response headers shared with the driver.
pub type SharedHeaders = Rc<RefCell<ResponseHeaders>>;

/// Propagates meta content-types into response headers.
#[derive(Debug)]
pub struct MetaTagFilter {
    headers: SharedHeaders,
    live: bool,
    noscript_depth: u32,
    converted: u32,
}

impl MetaTagFilter {
    /// Builds the filter over shared driver headers.
    #[must_use]
    pub fn new(headers: &SharedHeaders) -> Self {
        MetaTagFilter {
            headers: headers.clone(),
            live: true,
            noscript_depth: 0,
            converted: 0,
        }
    }

    /// Tags converted so far (`converted_meta_tags`).
    #[must_use]
    pub fn converted_count(&self) -> u32 {
        self.converted
    }

    /// Extracts and merges one meta tag (`ExtractAndUpdateMetaTagDetails`).
    fn extract_and_update(&mut self, dom: &mut Dom, id: NodeId) -> bool {
        let Some((content, mime, charset)) =
            extract_meta_tag_details(dom, id, Some(&self.headers.borrow()))
        else {
            return false;
        };
        let mut headers = self.headers.borrow_mut();
        if !content.is_empty() {
            if mime.is_empty() {
                return false;
            }
            let mime_str = String::from_utf8_lossy(&mime);
            let is_html = mime_to_content_type(&mime_str)
                .is_some_and(|content_type| content_type.kind() == ContentTypeKind::Html);
            // Only HTML mimetypes propagate (never XHTML guesses).
            if !is_html {
                return false;
            }
            if headers.merge_content_type_str(&String::from_utf8_lossy(&content)) {
                headers.compute_caching();
                return true;
            }
        } else {
            // Charset-only form: merge `; charset=...` into the mime type.
            let mut merged = b"; charset=".to_vec();
            merged.extend_from_slice(&charset);
            if headers.merge_content_type_str(&String::from_utf8_lossy(&merged)) {
                headers.compute_caching();
                return true;
            }
        }
        false
    }
}

/// Extracts (content, mime, charset) from a meta tag
/// (`CommonFilter::ExtractMetaTagDetails`); `None` when nothing applies.
pub(crate) fn extract_meta_tag_details(
    dom: &mut Dom,
    id: NodeId,
    headers: Option<&ResponseHeaders>,
) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    if let (Some(equiv_index), Some(value_index)) = (
        dom.find_attribute(id, Keyword::HttpEquiv),
        dom.find_attribute(id, Keyword::Content),
    ) {
        let (equiv, _) = dom.decoded_attribute(id, equiv_index);
        let (value, _) = dom.decoded_attribute(id, value_index);
        let (Some(attribute), Some(mut content)) = (equiv, value) else {
            return None;
        };
        if content.is_empty() || attribute.is_empty() {
            return None;
        }
        let name: Vec<u8> = attribute.trim_ascii().to_ascii_lowercase();
        if name.as_slice() != b"content-type" {
            return None;
        }
        let Some((mut mime, mut charset)) =
            ps_http::content_type::parse_content_type(&String::from_utf8_lossy(&content))
        else {
            return None;
        };
        // A trailing `;` plus a charset attribute completes the content.
        if charset.is_empty() && content.last() == Some(&b';') {
            if let Some(cs_index) = dom.find_attribute(id, Keyword::Charset) {
                if let (Some(cs), _) = dom.decoded_attribute(id, cs_index) {
                    content.extend_from_slice(b" charset=");
                    content.extend_from_slice(&cs);
                    let reparsed = ps_http::content_type::parse_content_type(
                        &String::from_utf8_lossy(&content),
                    )?;
                    mime = reparsed.0;
                    charset = reparsed.1;
                }
            }
        }
        // Skip values the headers already carry.
        if let Some(headers) = headers {
            let content_str = String::from_utf8_lossy(&content);
            if headers.has_value(&String::from_utf8_lossy(&name), &content_str) {
                return None;
            }
        }
        Some((content, mime.into_bytes(), charset.into_bytes()))
    } else if let Some(cs_index) = dom.find_attribute(id, Keyword::Charset) {
        let (decoded, _) = dom.decoded_attribute(id, cs_index);
        decoded.map(|charset| (Vec::new(), Vec::new(), charset))
    } else {
        None
    }
}

impl HtmlFilter for MetaTagFilter {
    fn name(&self) -> &'static str {
        "ConvertMetaTags"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.live = true;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
        // Past a flush the headers are finalized; noscript content never
        // counts.
        if !self.live || self.noscript_depth > 0 || dom.element_keyword(id) != Keyword::Meta {
            return;
        }
        if self.extract_and_update(dom, id) {
            self.converted += 1;
        }
    }

    fn flush(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.live = false;
    }
}

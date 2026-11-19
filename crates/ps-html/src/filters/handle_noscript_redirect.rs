//! Noscript-redirect canonical links (`HandleNoscriptRedirectFilter`).
//!
//! Appends `<link rel="canonical" href="document-url"/>` to the first
//! `<head>` lacking a canonical link, so the original URL stays canonical
//! when noscript redirects fire. The document URL comes from the driver;
//! it is supplied at construction here.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Canonical-link template (`kLinkRelCanonicalFormatter`; `%s` is the URL).
const LINK_REL_CANONICAL_FORMAT: (&[u8], &[u8]) = (b"<link rel=\"canonical\" href=\"", b"\"/>");

/// Inserts the canonical link into the first bare head.
#[derive(Debug)]
pub struct HandleNoscriptRedirectFilter {
    url: Vec<u8>,
    canonical_present: bool,
    canonical_inserted: bool,
}

impl HandleNoscriptRedirectFilter {
    /// Builds the filter for a document URL (`rewrite_driver_->url()`).
    #[must_use]
    pub fn new(url: &[u8]) -> Self {
        HandleNoscriptRedirectFilter {
            url: url.to_vec(),
            canonical_present: false,
            canonical_inserted: false,
        }
    }
}

impl HtmlFilter for HandleNoscriptRedirectFilter {
    fn name(&self) -> &'static str {
        "HandleNoscriptRedirect"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.canonical_present = false;
        self.canonical_inserted = false;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if !self.canonical_inserted
            && !self.canonical_present
            && dom.element_keyword(id) == Keyword::Link
        {
            // A canonical link needs both rel and href; rel compares
            // case-insensitively against "canonical".
            let rel = dom.find_attribute(id, Keyword::Rel).and_then(|index| {
                let attribute = &mut dom.element_attributes_mut(id)[index];
                attribute.decoded_value_or_null().map(<[u8]>::to_vec)
            });
            let has_href = dom.find_attribute(id, Keyword::Href).is_some();
            self.canonical_present = has_href
                && rel
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(b"canonical"));
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if !self.canonical_inserted
            && !self.canonical_present
            && dom.element_keyword(id) == Keyword::Head
        {
            // The markup goes in as a raw characters node; the writer
            // re-emits character data verbatim.
            let (prefix, suffix) = LINK_REL_CANONICAL_FORMAT;
            let mut markup = Vec::with_capacity(prefix.len() + self.url.len() + suffix.len());
            markup.extend_from_slice(prefix);
            markup.extend_from_slice(&self.url);
            markup.extend_from_slice(suffix);
            let leaf = dom.new_leaf(Some(id), crate::element::LeafKind::Characters, markup);
            dom.append_child(Some(id), leaf);
            self.canonical_inserted = true;
        }
    }
}

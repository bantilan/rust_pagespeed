//! AMP link insertion (`InsertAmpLinkFilter`).
//!
//! Appends `<link rel="amphtml">` to the first `<head>` lacking one,
//! rendering the operator's URL pattern (`${...}` fragments against the
//! document URL). Pattern and document URL come from the driver.

use ps_http::google_url::GoogleUrl;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Inserts the configured AMP link into the first bare head.
#[derive(Debug)]
pub struct InsertAmpLinkFilter {
    pattern: Vec<u8>,
    document_url: GoogleUrl,
    amp_link_found: bool,
}

impl InsertAmpLinkFilter {
    /// Builds the filter (`amp_link_pattern` + document URL).
    #[must_use]
    pub fn new(pattern: &[u8], document_url: &str) -> Self {
        InsertAmpLinkFilter {
            pattern: pattern.to_vec(),
            document_url: GoogleUrl::new(document_url),
            amp_link_found: false,
        }
    }

    /// Renders the pattern against the document URL (`GetAmpUrl`).
    fn amp_url(&self) -> Vec<u8> {
        let mut link = Vec::new();
        let mut fragment_start = None;
        for (offset, &byte) in self.pattern.iter().enumerate() {
            if byte == b'$' && self.pattern.get(offset + 1) == Some(&b'{') {
                // A nested `${` restarts the fragment, as in C++.
                fragment_start = Some(offset + 2);
            } else if byte == b'}' && fragment_start.is_some() {
                let start = fragment_start.take().unwrap();
                append_fragment(&mut link, &self.document_url, &self.pattern[start..offset]);
            } else if fragment_start.is_none() {
                link.push(byte);
            }
        }
        if let Some(start) = fragment_start {
            // Unclosed `${`: append literally from the marker.
            link.extend_from_slice(&self.pattern[start - 2..]);
        }
        link
    }
}

/// Appends one named fragment (`AppendUrlFragment`); unknown names stay
/// literal `${...}`.
fn append_fragment(link: &mut Vec<u8>, url: &GoogleUrl, name: &[u8]) {
    match name {
        b"url" => link.extend_from_slice(url.spec().as_bytes()),
        b"all_except_query" => link.extend_from_slice(url.all_except_query().as_bytes()),
        b"all_except_leaf" => link.extend_from_slice(url.all_except_leaf().as_bytes()),
        b"leaf_sans_query" => link.extend_from_slice(url.leaf_sans_query().as_bytes()),
        b"leaf_with_query" => link.extend_from_slice(url.leaf_with_query().as_bytes()),
        b"maybe_query" => {
            if !url.query().is_empty() {
                link.push(b'?');
                link.extend_from_slice(url.query().as_bytes());
            }
        }
        b"path_no_trailing_slash" => {
            let mut path = url.path_sans_leaf().as_bytes().to_vec();
            path.extend_from_slice(url.leaf_sans_query().as_bytes());
            if path.last() == Some(&b'/') {
                path.pop();
            }
            link.extend_from_slice(&path);
        }
        b"maybe_trailing_slash" => {
            if url.all_except_query().ends_with('/') {
                link.push(b'/');
            }
        }
        _ => {
            link.extend_from_slice(b"${");
            link.extend_from_slice(name);
            link.extend_from_slice(b"}");
        }
    }
}

impl HtmlFilter for InsertAmpLinkFilter {
    fn name(&self) -> &'static str {
        "InsertAmpLink"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        !self.pattern.is_empty()
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.amp_link_found = false;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Link {
            if let Some(index) = dom.find_attribute(id, Keyword::Rel) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if decoded
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(b"amphtml"))
                {
                    self.amp_link_found = true;
                }
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.amp_link_found || dom.element_keyword(id) != Keyword::Head {
            return;
        }
        let link = dom.new_element(Some(id), b"link");
        dom.add_attribute(link, b"rel", b"amphtml");
        let url = self.amp_url();
        dom.add_attribute(link, b"href", &url);
        dom.append_child(Some(id), link);
        self.amp_link_found = true;
    }
}

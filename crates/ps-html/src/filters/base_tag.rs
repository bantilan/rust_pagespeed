//! Base-tag insertion (`BaseTagFilter`).
//!
//! Inserts `<base href="...">` as the first child of the first `<head>`.
//! The base URL comes from the driver (`decoded_base`); it is supplied at
//! construction here.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Inserts one base tag into the first head.
#[derive(Debug, Default)]
pub struct BaseTagFilter {
    base: Vec<u8>,
    added_base_tag: bool,
}

impl BaseTagFilter {
    /// Builds the filter for a document base URL (`decoded_base`).
    #[must_use]
    pub fn new(base: &[u8]) -> Self {
        BaseTagFilter {
            base: base.to_vec(),
            added_base_tag: false,
        }
    }
}

impl HtmlFilter for BaseTagFilter {
    fn name(&self) -> &'static str {
        "BaseTag"
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Head && !self.added_base_tag {
            self.added_base_tag = true;
            let base = dom.new_element(Some(id), b"base");
            dom.add_attribute(base, b"href", &self.base);
            dom.insert_node_after_current(base);
        }
    }
}

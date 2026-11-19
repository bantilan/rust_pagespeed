//! Script stripping (`StripScriptsFilter`).
//!
//! Experimental filter that blindly deletes every script element.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Deletes whole script subtrees at their end tags.
#[derive(Debug, Default)]
pub struct StripScriptsFilter;

impl HtmlFilter for StripScriptsFilter {
    fn name(&self) -> &'static str {
        "StripScripts"
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Script {
            dom.delete_node(id);
        }
    }
}

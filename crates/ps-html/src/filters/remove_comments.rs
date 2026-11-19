//! Comment stripping (`RemoveCommentsFilter`).
//!
//! Deletes every HTML comment except IE directives (which arrive on a
//! separate callback) and comments matching the retain list
//! (`OptionsInterface::IsRetainedComment`, wildcard group).

use ps_config::wildcard::WildcardGroup;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::parser::Dom;

/// Strips HTML comments, keeping retained ones.
#[derive(Debug, Default)]
pub struct RemoveCommentsFilter {
    retain: WildcardGroup,
}

impl RemoveCommentsFilter {
    /// Adds a retain pattern (`RetainComment`).
    pub fn retain_comment(&mut self, pattern: &str) {
        self.retain.allow(pattern);
    }
}

impl HtmlFilter for RemoveCommentsFilter {
    fn name(&self) -> &'static str {
        "RemoveComments"
    }

    fn comment(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let contents = dom.leaf_contents(id);
        // Comment bytes are matched lossily: patterns are text, and an
        // exact byte match only differs on non-UTF-8 input.
        let text = String::from_utf8_lossy(&contents);
        if !self.retain.is_match(&text, false) {
            dom.delete_node(id);
        }
    }
}

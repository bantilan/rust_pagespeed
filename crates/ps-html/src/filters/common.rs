//! Shared filter helpers (the portable core of `CommonFilter`).

use crate::element::NodeId;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// XHTML certainty (`RewriteDriver::XhtmlStatus`), from the response
/// mimetype; unknown conservatively behaves like XHTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XhtmlStatus {
    /// Uncertain: wrap inline JS in CDATA.
    #[default]
    Unknown,
    /// Certainly XHTML: wrap.
    IsXhtml,
    /// Certainly not XHTML: emit raw.
    IsNotXhtml,
}

/// Fills a fresh script element (`CommonFilter::AddJsToElement`): CDATA
/// wrapper unless certainly not XHTML, `type` outside HTML5 (or under
/// Pedantic), then the JS text child.
pub fn add_js_to_element(
    dom: &mut Dom,
    lexer: &Lexer,
    script: NodeId,
    js: &[u8],
    pedantic: bool,
    xhtml: XhtmlStatus,
) {
    let mut text = Vec::new();
    if xhtml != XhtmlStatus::IsNotXhtml {
        text.extend_from_slice(b"//<![CDATA[\n");
        text.extend_from_slice(js);
        text.extend_from_slice(b"\n//]]>");
    } else {
        text.extend_from_slice(js);
    }
    if !lexer.doctype().is_version_5() || pedantic {
        dom.add_attribute(script, b"type", b"text/javascript");
    }
    let child = dom.new_leaf(Some(script), crate::element::LeafKind::Characters, text);
    dom.append_child(Some(script), child);
}

/// Body-end injection point (`CommonFilter::end_body_point_`).
///
/// Tracks the preferred `InsertNodeAtBodyEnd` target: any start element
/// clears it, `</body>` sets it, `</html>` takes over when body is
/// unusable, any other end tag or non-whitespace characters drop it.
/// Filters owning one must call each method from the matching event.
#[derive(Debug, Clone, Copy, Default)]
pub struct BodyEndTracker {
    point: Option<NodeId>,
}

impl BodyEndTracker {
    /// Drops the point (`CommonFilter::StartElement` always clears).
    pub fn clear(&mut self) {
        self.point = None;
    }

    /// Updates the point on end tags (`CommonFilter::EndElement`).
    pub fn observe_end(&mut self, dom: &mut Dom, id: NodeId) {
        match dom.element_keyword(id) {
            Keyword::Body => self.point = Some(id),
            Keyword::Html => {
                let body_usable = self.point.is_some_and(|point| dom.can_append_child(point));
                if (self.point.is_none() || !body_usable) && dom.can_append_child(id) {
                    self.point = Some(id);
                }
            }
            _ => self.point = None,
        }
    }

    /// Drops the point on non-whitespace characters
    /// (`CommonFilter::Characters`).
    pub fn observe_characters(&mut self, dom: &Dom, id: NodeId) {
        if self.point.is_some()
            && dom
                .leaf_contents(id)
                .iter()
                .any(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | 0x0c | b'\r'))
        {
            self.point = None;
        }
    }

    /// Appends at the point, else before the cursor
    /// (`InsertNodeAtBodyEnd`).
    pub fn insert(&self, dom: &mut Dom, node: NodeId) {
        if let Some(point) = self.point {
            if dom.can_append_child(point) {
                dom.append_child(Some(point), node);
                return;
            }
        }
        dom.insert_node_before_current(node);
    }
}

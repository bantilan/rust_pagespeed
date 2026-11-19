//! Reflow prevention (`FixReflowFilter`).
//!
//! Tags `min-height` styles onto divs whose beacon-measured heights arrived
//! via the property cache (`element_rendered_height` as `id:height,...`).
//! The cache lookup (cohort, TTL) belongs to the driver; the filter takes
//! the winning value (or nothing on a miss).

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Property-cache value name (`kElementRenderedHeightPropertyName`).
pub const ELEMENT_RENDERED_HEIGHT_PROPERTY: &str = "element_rendered_height";
/// Marker attribute for fixed divs (`kReflowClassAttribute`).
const REFLOW_CLASS_ATTRIBUTE: &[u8] = b"data-pagespeed-fix-reflow";

/// Pins measured heights onto matching divs.
#[derive(Debug, Default)]
pub struct FixReflowFilter {
    property_value: Option<Vec<u8>>,
    heights: Vec<(Vec<u8>, Vec<u8>)>,
}

impl FixReflowFilter {
    /// Builds the filter with the cache value (`None` on a miss).
    #[must_use]
    pub fn new(property_value: Option<&[u8]>) -> Self {
        FixReflowFilter {
            property_value: property_value.map(<[u8]>::to_vec),
            heights: Vec::new(),
        }
    }
}

impl HtmlFilter for FixReflowFilter {
    fn name(&self) -> &'static str {
        "FixReflowFilter"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // Parse `id:height,...` into pairs (first wins on duplicates).
        self.heights.clear();
        if let Some(value) = &self.property_value {
            let parts: Vec<&[u8]> = value
                .split(|byte| *byte == b',' || *byte == b':')
                .filter(|part| !part.is_empty())
                .collect();
            for pair in parts.chunks_exact(2) {
                if !self.heights.iter().any(|(id, _)| id == pair[0]) {
                    self.heights.push((pair[0].to_vec(), pair[1].to_vec()));
                }
            }
        }
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) != Keyword::Div {
            return;
        }
        let Some(attr) = dom.find_attribute(id, Keyword::Id) else {
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, attr);
        let Some(div_id) = decoded else {
            return;
        };
        if let Some((_, height)) = self.heights.iter().find(|(key, _)| *key == div_id) {
            let mut style = b"min-height:".to_vec();
            style.extend_from_slice(height);
            dom.add_attribute(id, b"style", &style);
            dom.add_attribute(id, REFLOW_CLASS_ATTRIBUTE, b"");
        }
    }
}

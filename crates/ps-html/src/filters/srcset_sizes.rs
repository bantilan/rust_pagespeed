//! Explicit layout sizes for existing width srcsets; no resource operations.
use std::rc::Rc;

use ps_config::options::RewriteOptions;

use crate::element::NodeId;
use crate::filter::{HtmlFilter, ScriptUsage};
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::srcset::{supports_width_sizes, MAX_SIZES_SRCSET_BYTES};

/// Immutable configuration retained by the effective-options chain plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrcsetSizesPolicy {
    class: Vec<u8>,
    sizes: Vec<u8>,
}

impl SrcsetSizesPolicy {
    pub(crate) fn from_options(options: &RewriteOptions) -> Option<Self> {
        if !options.srcset_sizes_enabled() {
            return None;
        }
        let class = options.srcset_sizes_class();
        let sizes = options.srcset_sizes();
        if class.is_empty() || sizes.trim().is_empty() {
            return None;
        }
        Some(Self {
            class: class.into_bytes(),
            sizes: sizes.into_bytes(),
        })
    }
}

/// Request-local document state; policy bytes are shared, never mutated.
pub struct SrcsetSizesFilter {
    policy: Rc<SrcsetSizesPolicy>,
    blocked_depth: usize,
}

impl SrcsetSizesFilter {
    /// Uses an immutable policy from the chain plan.
    #[must_use]
    pub fn new(policy: Rc<SrcsetSizesPolicy>) -> Self {
        Self {
            policy,
            blocked_depth: 0,
        }
    }

    fn blocks(dom: &Dom, id: NodeId) -> bool {
        dom.element_keyword(id) == Keyword::Noscript
            || dom.element_name_equals(id, b"picture")
            || dom.element_name_equals(id, b"template")
            || dom.element_name_equals(id, b"svg")
    }

    // Duplicate attributes are ambiguous; do not choose one to overwrite.
    fn unique_attribute(dom: &Dom, id: NodeId, name: &[u8]) -> Result<Option<usize>, ()> {
        let mut indices = dom
            .element_attributes(id)
            .iter()
            .enumerate()
            .filter(|(_, attr)| attr.name_str().eq_ignore_ascii_case(name))
            .map(|(index, _)| index);
        let first = indices.next();
        if indices.next().is_some() {
            Err(())
        } else {
            Ok(first)
        }
    }

    fn bounded_attribute(dom: &mut Dom, id: NodeId, index: usize, cap: usize) -> Option<Vec<u8>> {
        // Bound the escaped representation before decoded_attribute allocates.
        if dom.element_attributes(id)[index].escaped_value()?.len() > cap {
            return None;
        }
        let (value, error) = dom.decoded_attribute(id, index);
        if error {
            None
        } else {
            value
        }
    }

    fn apply(&self, dom: &mut Dom, id: NodeId) -> Option<()> {
        let class = Self::unique_attribute(dom, id, b"class").ok()??;
        let classes = Self::bounded_attribute(dom, id, class, 1024)?;
        if !classes
            .split(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 12))
            .any(|token| token == self.policy.class)
        {
            return None;
        }
        let srcset = Self::unique_attribute(dom, id, b"srcset").ok()??;
        let sizes = Self::unique_attribute(dom, id, b"sizes").ok()?;
        let value = Self::bounded_attribute(dom, id, srcset, MAX_SIZES_SRCSET_BYTES)?;
        if !supports_width_sizes(&value) {
            return None;
        }
        if let Some(index) = sizes {
            dom.element_attributes_mut(id)[index].set_value(&self.policy.sizes);
        } else {
            dom.add_attribute(id, b"sizes", &self.policy.sizes);
        }
        Some(())
    }
}

impl HtmlFilter for SrcsetSizesFilter {
    fn name(&self) -> &'static str {
        "SrcsetSizesFilter"
    }
    fn can_modify_urls(&self) -> bool {
        false
    }
    fn script_usage(&self) -> ScriptUsage {
        ScriptUsage::NeverInjectsScripts
    }
    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.blocked_depth = 0;
    }
    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if Self::blocks(dom, id) {
            self.blocked_depth += 1;
            return;
        }
        if dom.element_attributes(id).len() > 64
            || self.blocked_depth != 0
            || dom.element_keyword(id) != Keyword::Img
            || !dom.is_rewritable_ignoring_end(id)
            || dom
                .find_attribute(id, Keyword::DataPagespeedNoTransform)
                .is_some()
            || dom
                .find_attribute(id, Keyword::PagespeedNoTransform)
                .is_some()
        {
            return;
        }
        self.apply(dom, id);
    }
    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if Self::blocks(dom, id) {
            self.blocked_depth = self.blocked_depth.saturating_sub(1);
        }
    }
}

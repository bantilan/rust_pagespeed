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
    automatic: bool,
}

impl SrcsetSizesPolicy {
    pub(crate) fn from_options(options: &RewriteOptions) -> Option<Self> {
        let automatic = options.srcset_auto_sizes()
            && !options
                .filters()
                .enabled(ps_config::filter::Filter::LazyloadImages);
        let mut class = options.srcset_sizes_class();
        let mut sizes = options.srcset_sizes();
        if !options.srcset_sizes_enabled() || class.is_empty() || sizes.trim().is_empty() {
            class.clear();
            sizes.clear();
        }
        if !automatic && class.is_empty() {
            return None;
        }
        Some(Self {
            class: class.into_bytes(),
            sizes: sizes.into_bytes(),
            automatic,
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

    fn blocks(dom: &Dom, id: NodeId, keyword: Keyword) -> bool {
        match keyword {
            Keyword::Noscript => true,
            // These HTML5 names have no entry in the C++ keyword table.
            Keyword::NotAKeyword => {
                dom.element_name_equals(id, b"picture")
                    || dom.element_name_equals(id, b"template")
                    || dom.element_name_equals(id, b"svg")
            }
            _ => false,
        }
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

    fn manual_match(&self, dom: &mut Dom, id: NodeId) -> bool {
        if self.policy.class.is_empty() {
            return false;
        }
        let Some(class) = Self::unique_attribute(dom, id, b"class").ok().flatten() else {
            return false;
        };
        let Some(classes) = Self::bounded_attribute(dom, id, class, 1024) else {
            return false;
        };
        classes
            .split(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 12))
            .any(|token| token == self.policy.class)
    }

    fn native_eligible(dom: &mut Dom, id: NodeId) -> Option<()> {
        let loading = Self::unique_attribute(dom, id, b"loading").ok()??;
        if !Self::bounded_attribute(dom, id, loading, 16)?.eq_ignore_ascii_case(b"lazy") {
            return None;
        }
        for name in [b"width".as_slice(), b"height"] {
            let index = Self::unique_attribute(dom, id, name).ok()??;
            let value = Self::bounded_attribute(dom, id, index, 32)?;
            let text = std::str::from_utf8(&value).ok()?;
            if text.is_empty()
                || !text.bytes().all(|b| b.is_ascii_digit())
                || text.parse::<u32>().ok()? == 0
            {
                return None;
            }
        }
        if let Some(index) = Self::unique_attribute(dom, id, b"fetchpriority").ok()? {
            if Self::bounded_attribute(dom, id, index, 16)?.eq_ignore_ascii_case(b"high") {
                return None;
            }
        }
        for name in [
            b"onload".as_slice(),
            b"data-pagespeed-lazy-src",
            b"data-pagespeed-lazy-srcset",
            b"data-src",
            b"data-srcset",
        ] {
            if Self::unique_attribute(dom, id, name).ok()?.is_some() {
                return None;
            }
        }
        Some(())
    }

    fn apply(&self, dom: &mut Dom, id: NodeId) -> Option<()> {
        // Reject ambiguous class attributes even for the automatic path.
        Self::unique_attribute(dom, id, b"class").ok()?;
        let manual = self.manual_match(dom, id);
        if !manual {
            if !self.policy.automatic {
                return None;
            }
            Self::native_eligible(dom, id)?;
        }
        let srcset = Self::unique_attribute(dom, id, b"srcset").ok()??;
        let sizes = Self::unique_attribute(dom, id, b"sizes").ok()?;
        let value = Self::bounded_attribute(dom, id, srcset, MAX_SIZES_SRCSET_BYTES)?;
        if !supports_width_sizes(&value) {
            return None;
        }
        let output = if manual {
            self.policy.sizes.clone()
        } else {
            let fallback = if let Some(index) = sizes {
                Self::bounded_attribute(dom, id, index, 1024)?
            } else {
                Vec::new()
            };
            if fallback.eq_ignore_ascii_case(b"auto")
                || fallback
                    .get(..5)
                    .is_some_and(|p| p.eq_ignore_ascii_case(b"auto,"))
            {
                return Some(());
            }
            let mut output = b"auto, ".to_vec();
            if fallback.iter().all(|b| b.is_ascii_whitespace()) {
                output.extend_from_slice(b"100vw");
            } else {
                output.extend_from_slice(&fallback);
            }
            output
        };
        if let Some(index) = sizes {
            dom.element_attributes_mut(id)[index].set_value(&output);
        } else {
            dom.add_attribute(id, b"sizes", &output);
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
        let keyword = dom.element_keyword(id);
        if Self::blocks(dom, id, keyword) {
            self.blocked_depth += 1;
            return;
        }
        if self.blocked_depth != 0
            || keyword != Keyword::Img
            || dom.element_attributes(id).len() > 64
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
        if self.blocked_depth != 0 && Self::blocks(dom, id, dom.element_keyword(id)) {
            self.blocked_depth = self.blocked_depth.saturating_sub(1);
        }
    }
}

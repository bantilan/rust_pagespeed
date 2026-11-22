//! Rewritable URL placements (`ResourceSlot`).
//!
//! A slot binds a resource to the spot its URL occupies (an HTML
//! attribute today; CSS and fetch slots later). Rewrites swap the
//! resource off-thread and render back into the DOM synchronously.
//!
//! Context tracking records every context that attaches to a slot,
//! in order: the sync engine runs each context to completion inline,
//! so unlike C++ (which detaches on completion before waking
//! successors) the registry is a historical record rather than a
//! live set. Successor wakeups need no machinery here — program
//! order is completion order — and the ids never enter output,
//! hashes, or keys.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use ps_config::options::RewriteOptions;
use ps_http::google_url::{GoogleUrl, UrlRelativity};

use super::cacheable_resource::InputInfo;
use super::resource::ResourceHandle;
use crate::element::NodeId;
use crate::name::Keyword;
use crate::parser::Dom;

/// Rewrite-context identity for slot tracking (process-unique,
/// starting at 1; bookkeeping only, never output).
pub type ContextId = u64;

/// Next context identity.
static NEXT_CONTEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Mints a fresh context identity.
#[must_use]
pub(crate) fn mint_context_id() -> ContextId {
    NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Shared slot flags (`ResourceSlot` state).
#[derive(Debug, Clone, Default)]
pub struct SlotBase {
    resource: Option<ResourceHandle>,
    preserve_urls: bool,
    disable_rendering: bool,
    should_delete_element: bool,
    disable_further_processing: bool,
    was_optimized: bool,
    need_aggregate_input_info: bool,
    contexts: Vec<ContextId>,
    inputs: Option<Vec<InputInfo>>,
}

impl SlotBase {
    /// Builds slot state around a resource (possibly empty).
    #[must_use]
    pub fn new(resource: Option<ResourceHandle>) -> Self {
        SlotBase {
            resource,
            ..SlotBase::default()
        }
    }

    /// Records a context attachment (`AddContext`).
    pub fn add_context(&mut self, id: ContextId) {
        self.contexts.push(id);
    }

    /// Detaches a context (`DetachContext`): only the first or last
    /// attachment may leave, like C++ (anything else is a bug).
    pub fn detach_context(&mut self, id: ContextId) {
        if self.contexts.first() == Some(&id) {
            self.contexts.remove(0);
        } else if self.contexts.last() == Some(&id) {
            self.contexts.pop();
        } else {
            debug_assert!(false, "can only detach first or last context");
        }
    }

    /// Most recently attached context (`LastContext`), if any.
    #[must_use]
    pub fn last_context(&self) -> Option<ContextId> {
        self.contexts.last().copied()
    }

    /// Records one input's validity watermark (`ReportInput`).
    pub fn report_input(&mut self, input: InputInfo) {
        self.inputs.get_or_insert_with(Vec::new).push(input);
    }

    /// Aggregated input infos (`inputs`, `None` until reported).
    #[must_use]
    pub fn inputs(&self) -> Option<&Vec<InputInfo>> {
        self.inputs.as_ref()
    }

    /// Borrows the bound resource, if any.
    #[must_use]
    pub fn resource(&self) -> Option<&ResourceHandle> {
        self.resource.as_ref()
    }

    /// Rebinds the slot (`SetResource`).
    pub fn set_resource(&mut self, resource: ResourceHandle) {
        self.resource = Some(resource);
    }

    /// Whether URL changes are suppressed.
    #[must_use]
    pub fn preserve_urls(&self) -> bool {
        self.preserve_urls
    }

    /// Suppresses URL changes (`set_preserve_urls`).
    pub fn set_preserve_urls(&mut self, preserve: bool) {
        self.preserve_urls = preserve;
    }

    /// Whether rendering is fully suppressed.
    #[must_use]
    pub fn disable_rendering(&self) -> bool {
        self.disable_rendering
    }

    /// Suppresses rendering (`set_disable_rendering`).
    pub fn set_disable_rendering(&mut self, disable: bool) {
        self.disable_rendering = disable;
    }

    /// Whether rendering deletes the element.
    #[must_use]
    pub fn should_delete_element(&self) -> bool {
        self.should_delete_element
    }

    /// Marks the element for deletion, also halting further
    /// processing (`RequestDeleteElement`).
    pub fn request_delete_element(&mut self) {
        self.should_delete_element = true;
        self.disable_further_processing = true;
    }

    /// Whether later filters must leave the slot alone.
    #[must_use]
    pub fn disable_further_processing(&self) -> bool {
        self.disable_further_processing
    }

    /// Halts later filters (`set_disable_further_processing`).
    pub fn set_disable_further_processing(&mut self, disable: bool) {
        self.disable_further_processing = disable;
    }

    /// Whether some context optimized the slot.
    #[must_use]
    pub fn was_optimized(&self) -> bool {
        self.was_optimized
    }

    /// Marks the slot optimized (`set_was_optimized`).
    pub fn set_was_optimized(&mut self, optimized: bool) {
        self.was_optimized = optimized;
    }

    /// Whether chained input infos aggregate here.
    #[must_use]
    pub fn need_aggregate_input_info(&self) -> bool {
        self.need_aggregate_input_info
    }

    /// Requests chained input infos (`set_need_aggregate_input_info`).
    pub fn set_need_aggregate_input_info(&mut self, need: bool) {
        self.need_aggregate_input_info = need;
    }
}

/// Relativizes a rewritten URL, or passes it through absolute
/// (`RelativizeOrPassthrough`), honoring `PreserveUrlRelativity`.
#[must_use]
pub fn relativize_or_passthrough(
    options: &RewriteOptions,
    url: &str,
    relativity: UrlRelativity,
    base_url: &GoogleUrl,
) -> String {
    if options.preserve_url_relativity() {
        GoogleUrl::new(url).relativize(relativity, base_url)
    } else {
        url.to_string()
    }
}

/// A slot whose rendering happens elsewhere (`NullResourceSlot`).
#[derive(Debug)]
pub struct NullResourceSlot {
    base: SlotBase,
    location: String,
}

impl NullResourceSlot {
    /// Builds the slot with a fixed location string.
    #[must_use]
    pub fn new(resource: Option<ResourceHandle>, location: &str) -> Self {
        NullResourceSlot {
            base: SlotBase::new(resource),
            location: location.to_string(),
        }
    }

    /// Borrows the shared state.
    #[must_use]
    pub fn base(&self) -> &SlotBase {
        &self.base
    }

    /// Mutably borrows the shared state.
    pub fn base_mut(&mut self) -> &mut SlotBase {
        &mut self.base
    }

    /// Describes the slot for logs (`LocationString`).
    #[must_use]
    pub fn location_string(&self) -> &str {
        &self.location
    }

    /// Renders nothing, by design.
    pub fn render(&mut self) {}
}

/// A slot standing in for a fetch (`FetchResourceSlot`).
#[derive(Debug, Default)]
pub struct FetchResourceSlot {
    base: SlotBase,
}

impl FetchResourceSlot {
    /// Builds the slot around a resource.
    #[must_use]
    pub fn new(resource: Option<ResourceHandle>) -> Self {
        FetchResourceSlot {
            base: SlotBase::new(resource),
        }
    }

    /// Borrows the shared state.
    #[must_use]
    pub fn base(&self) -> &SlotBase {
        &self.base
    }

    /// Mutably borrows the shared state.
    pub fn base_mut(&mut self) -> &mut SlotBase {
        &mut self.base
    }

    /// Describes the slot for logs (`LocationString`).
    #[must_use]
    pub fn location_string(&self) -> String {
        format!(
            "Fetch of {}",
            self.base
                .resource()
                .expect("fetch slot without a resource")
                .url()
        )
    }
}

/// A slot rewriting one HTML attribute (`HtmlResourceSlot`).
#[derive(Debug, Clone)]
pub struct HtmlResourceSlot {
    base: SlotBase,
    element: Option<NodeId>,
    attribute: usize,
    driver_id: String,
    relativity: UrlRelativity,
    begin_line: i32,
    end_line: i32,
}

impl HtmlResourceSlot {
    /// Binds an attribute, snapshotting its relativity and the
    /// element's line numbers (deep copies: the element may die in
    /// detached rewrites).
    #[must_use]
    pub fn new(
        resource: Option<ResourceHandle>,
        dom: &mut Dom,
        element: NodeId,
        attribute: usize,
        driver_id: &str,
    ) -> Self {
        let (decoded, _) = dom.decoded_attribute(element, attribute);
        let value = decoded.unwrap_or_default();
        let relativity = GoogleUrl::find_relativity(&String::from_utf8_lossy(&value));
        HtmlResourceSlot {
            base: SlotBase::new(resource),
            element: Some(element),
            attribute,
            driver_id: driver_id.to_string(),
            relativity,
            begin_line: dom.element_begin_line(element),
            end_line: dom.element_end_line(element),
        }
    }

    /// Borrows the shared state.
    #[must_use]
    pub fn base(&self) -> &SlotBase {
        &self.base
    }

    /// Mutably borrows the shared state.
    pub fn base_mut(&mut self) -> &mut SlotBase {
        &mut self.base
    }

    /// Borrows the bound element, if not yet deleted.
    #[must_use]
    pub fn element(&self) -> Option<NodeId> {
        self.element
    }

    /// Borrows the bound attribute index.
    #[must_use]
    pub fn attribute(&self) -> usize {
        self.attribute
    }

    /// How relative the original URL was (`url_relativity`).
    #[must_use]
    pub fn url_relativity(&self) -> UrlRelativity {
        self.relativity
    }

    /// Renders the bound resource (`Render`): deletes the element, or
    /// rewrites the attribute to the (possibly relativized) URL. The
    /// base URL and options arrive per call; the driver will own them.
    pub fn render(&mut self, dom: &mut Dom, base_url: &GoogleUrl, options: &RewriteOptions) {
        if self.base.disable_rendering() {
            return;
        }
        if self.base.should_delete_element() {
            if let Some(element) = self.element.take() {
                dom.delete_node(element);
            }
        } else if !self.base.preserve_urls() {
            let url = self
                .base
                .resource()
                .expect("slot rendered without a resource")
                .url()
                .to_string();
            self.direct_set_url(
                dom,
                &relativize_or_passthrough(options, &url, self.relativity, base_url),
            );
        }
    }

    /// Writes a URL byte-identically, refusing unauthorized resources
    /// (`DirectSetUrl`).
    pub fn direct_set_url(&self, dom: &mut Dom, url: &str) -> bool {
        let authorized = self
            .base
            .resource()
            .expect("slot without a resource")
            .is_authorized_domain();
        if !authorized {
            return false;
        }
        let Some(element) = self.element else {
            return false;
        };
        dom.element_attributes_mut(element)[self.attribute].set_value(url.as_bytes());
        true
    }

    /// Describes the slot for logs (`LocationString`).
    #[must_use]
    pub fn location_string(&self) -> String {
        if self.begin_line == self.end_line {
            format!("{}:{}", self.driver_id, self.begin_line)
        } else {
            format!("{}:{}-{}", self.driver_id, self.begin_line, self.end_line)
        }
    }
}

/// Orders HTML slots by element, then attribute
/// (`HtmlResourceSlotComparator`; the C++ compares pointers, which is
/// arbitrary — node ids order stably instead).
#[must_use]
pub fn compare_html_slots(
    left: &Rc<HtmlResourceSlot>,
    right: &Rc<HtmlResourceSlot>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match left.element.cmp(&right.element) {
        Ordering::Equal => left.attribute.cmp(&right.attribute),
        ordering => ordering,
    }
}

/// Assertion helper: an attribute's keyword on an element.
#[must_use]
pub fn attribute_keyword(dom: &Dom, element: NodeId, index: usize) -> Keyword {
    dom.element_attributes(element)[index].keyword()
}

/// Where a CSS URL lives for rewriting.
#[derive(Debug, Clone)]
pub enum CssSlotTarget {
    /// Parsed stylesheet value (external CSS, `<style>`): ruleset,
    /// declaration, and value indices (`UrlLocation`).
    Sheet {
        /// Shared stylesheet (slots run sequentially, so a
        /// `RefCell` suffices).
        sheet: Rc<std::cell::RefCell<ps_css::ast::Stylesheet>>,
        /// Value location.
        location: ps_css::rewrite::UrlLocation,
    },
    /// Parsed `style`-attribute declarations: declaration and
    /// value indices.
    Attribute {
        /// Shared declarations.
        declarations: Rc<std::cell::RefCell<ps_css::ast::Declarations>>,
        /// Declaration index.
        declaration: usize,
        /// Value index.
        value: usize,
    },
}

/// A slot rewriting one CSS `url()` (`CssResourceSlot`).
#[derive(Debug, Clone)]
pub struct CssResourceSlot {
    base: SlotBase,
    target: CssSlotTarget,
    trim_url: GoogleUrl,
    relativity: UrlRelativity,
}

impl CssResourceSlot {
    /// Binds a stylesheet value, snapshotting its relativity.
    #[must_use]
    pub fn new(
        resource: Option<ResourceHandle>,
        trim_url: GoogleUrl,
        target: CssSlotTarget,
        original_url: &[u8],
    ) -> Self {
        let relativity = GoogleUrl::find_relativity(&String::from_utf8_lossy(original_url));
        CssResourceSlot {
            base: SlotBase::new(resource),
            target,
            trim_url,
            relativity,
        }
    }

    /// Borrows the shared state.
    #[must_use]
    pub fn base(&self) -> &SlotBase {
        &self.base
    }

    /// Mutably borrows the shared state.
    pub fn base_mut(&mut self) -> &mut SlotBase {
        &mut self.base
    }

    /// How relative the original URL was (`url_relativity`).
    #[must_use]
    pub fn url_relativity(&self) -> UrlRelativity {
        self.relativity
    }

    /// Borrows the trim base.
    #[must_use]
    pub fn trim_url(&self) -> &GoogleUrl {
        &self.trim_url
    }

    /// Renders the bound resource URL into the CSS value (`Render`),
    /// trimming or relativizing it first.
    pub fn render(&mut self, options: &RewriteOptions) {
        if self.base.disable_rendering() || self.base.preserve_urls() {
            return;
        }
        let url = self
            .base
            .resource()
            .expect("slot rendered without a resource")
            .url()
            .to_string();
        debug_assert!(GoogleUrl::new(&url).is_web_valid());
        let mut trimmed = None;
        if options.trim_urls_in_css() && options.enabled(ps_config::filter::Filter::LeftTrimUrls) {
            trimmed = super::filters::url_left_trim::UrlLeftTrimFilter::trim(&self.trim_url, &url);
        }
        if let Some(trimmed) = trimmed {
            // TODO(sligocki): Make sure this is the correct (final)
            // URL of the CSS.
            self.direct_set_url(&trimmed);
        } else {
            self.direct_set_url(&relativize_or_passthrough(
                options,
                &url,
                self.relativity,
                &self.trim_url,
            ));
        }
    }

    /// Writes a URL into the CSS value, refusing unauthorized
    /// resources (`DirectSetUrl`).
    pub fn direct_set_url(&self, url: &str) -> bool {
        let authorized = self
            .base
            .resource()
            .expect("slot without a resource")
            .is_authorized_domain();
        if !authorized {
            return false;
        }
        let replacement = ps_css::unicode_text::UnicodeText::from_utf8(url.as_bytes());
        match &self.target {
            CssSlotTarget::Sheet { sheet, location } => {
                let mut sheet = sheet.borrow_mut();
                let value = &mut sheet.rulesets_mut()[location.ruleset].declarations_mut()
                    [location.declaration]
                    .values_mut()[location.value];
                let Some(text) = value.string_value_mut() else {
                    return false;
                };
                *text = replacement;
                true
            }
            CssSlotTarget::Attribute {
                declarations,
                declaration,
                value,
            } => {
                let mut declarations = declarations.borrow_mut();
                let value = &mut declarations[*declaration].values_mut()[*value];
                let Some(text) = value.string_value_mut() else {
                    return false;
                };
                *text = replacement;
                true
            }
        }
    }

    /// Describes the slot for logs (`LocationString`).
    #[must_use]
    pub fn location_string(&self) -> String {
        "Inside CSS".to_string()
    }
}

/// A slot recording an association for fallback rewriting
/// (`AssociationSlot`): rendering maps the absolute input URL to
/// the output URL.
#[derive(Debug, Clone)]
pub struct AssociationSlot {
    base: SlotBase,
    map: Rc<std::cell::RefCell<std::collections::HashMap<String, String>>>,
    key: String,
}

impl AssociationSlot {
    /// Binds a fallback association (`AssociationSlot`).
    #[must_use]
    pub fn new(
        resource: Option<ResourceHandle>,
        map: Rc<std::cell::RefCell<std::collections::HashMap<String, String>>>,
        key: &str,
    ) -> Self {
        AssociationSlot {
            base: SlotBase::new(resource),
            map,
            key: key.to_string(),
        }
    }

    /// Borrows the shared state.
    #[must_use]
    pub fn base(&self) -> &SlotBase {
        &self.base
    }

    /// Mutably borrows the shared state.
    pub fn base_mut(&mut self) -> &mut SlotBase {
        &mut self.base
    }

    /// Records the association (`Render`).
    pub fn render(&mut self) {
        let authorized = self
            .base
            .resource()
            .expect("slot rendered without a resource")
            .is_authorized_domain();
        if !authorized {
            return;
        }
        if !self.base.disable_rendering() && !self.base.preserve_urls() {
            let url = self
                .base
                .resource()
                .expect("slot rendered without a resource")
                .url()
                .to_string();
            self.map.borrow_mut().insert(self.key.clone(), url);
        }
    }

    /// Writes a URL into the association, refusing unauthorized
    /// resources (`DirectSetUrl`).
    pub fn direct_set_url(&self, url: &str) -> bool {
        let authorized = self
            .base
            .resource()
            .expect("slot without a resource")
            .is_authorized_domain();
        if !authorized {
            return false;
        }
        self.map
            .borrow_mut()
            .insert(self.key.clone(), url.to_string());
        true
    }

    /// Describes the slot for logs (`LocationString`).
    #[must_use]
    pub fn location_string(&self) -> String {
        format!("Fallback CSS {}", self.key)
    }
}

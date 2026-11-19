//! CSS image rewriting (`CssImageRewriter`).
//!
//! Walks parsed stylesheets for image `url()`s and runs nested
//! image-rewrite plus cache-extension contexts into CSS slots,
//! substituting rewritten URLs (or inline data) back into the
//! values. The sync model runs each nested context to completion
//! inline; debug messages merge into the parent sink deduplicated.

use std::cell::RefCell;
use std::rc::Rc;

use ps_config::filter::Filter;
use ps_http::google_url::GoogleUrl;

use super::cache_extender::CacheExtenderBehavior;
use super::image_rewrite::ImageRewriteBehavior;
use crate::metadata::CachedResult;
use crate::parser::Dom;
use crate::resource_slot::{AssociationSlot, CssResourceSlot, CssSlotTarget};
use crate::rewrite_context::SingleRewriteContext;
use crate::server_context::{InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver};

/// Whether CSS image rewriting runs at all (`RewritesEnabled`).
#[must_use]
pub fn rewrites_enabled(
    options: &ps_config::options::RewriteOptions,
    image_inline_max_bytes: i64,
) -> bool {
    image_inline_max_bytes > 0
        || options.image_optimization_enabled()
        || options.enabled(Filter::LeftTrimUrls)
        || options.enabled(Filter::ExtendCacheImages)
        || options.enabled(Filter::SpriteImages)
}

/// Runs nested image + cache-extension contexts over one slot
/// (`RewriteSlot`), merging debug into the parent sink. Returns
/// whether the slot was optimized.
fn rewrite_slot(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    slot: Rc<RefCell<CssResourceSlot>>,
    input: crate::resource::InputResource,
    image_inline_max_bytes: i64,
    parent_debug: Rc<RefCell<Vec<String>>>,
) -> bool {
    let options = driver.options();
    if options.image_optimization_enabled() || image_inline_max_bytes > 0 {
        // Do not rewrite external resource if preserve_urls is enabled
        // unless we allow preemptive rewriting.
        if !slot.borrow().base().preserve_urls() || options.in_place_preemptive_rewrite_css_images()
        {
            let behavior = ImageRewriteBehavior::for_css(
                driver.clone(),
                image_inline_max_bytes,
                parent_debug.clone(),
            );
            let mut context = SingleRewriteContext::new(driver.clone(), behavior);
            context.add_css_slot(slot.clone(), input.clone());
            context.start(dom);
            merge_nested_debug(&mut context, &parent_debug);
        }
    }

    // An image-optimized slot keeps its URL: the extender stands
    // down, like the HTML path refusing pagespeed resources (C++
    // renders image work last here).
    if !slot.borrow().base().was_optimized() && driver.may_cache_extend_images() {
        let behavior = CacheExtenderBehavior::for_nested(driver.clone(), InputRole::Img);
        let mut context = SingleRewriteContext::new(driver.clone(), behavior);
        context.add_css_slot(slot.clone(), input);
        context.start(dom);
        merge_nested_debug(&mut context, &parent_debug);
    }

    // TODO(sligocki): DomainRewriter or is this done automatically?
    slot.borrow().base().was_optimized()
}

/// Merges a nested context's debug messages into the parent sink
/// (`Propagate` debug half; dedup happens at harvest).
fn merge_nested_debug<B: crate::rewrite_context::SingleRewriteBehavior>(
    context: &mut SingleRewriteContext<B>,
    parent_debug: &Rc<RefCell<Vec<String>>>,
) {
    parent_debug
        .borrow_mut()
        .extend(context.take_debug_messages());
}

/// Rewrites one image URL found in CSS (`RewriteImage`): resolves,
/// authorizes, slots, and nests. Records authorization failures in
/// the debug sink. Returns whether the value was optimized.
#[allow(clippy::too_many_arguments)]
fn rewrite_image(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    image_inline_max_bytes: i64,
    trim_url: &GoogleUrl,
    original_url: &GoogleUrl,
    target: CssSlotTarget,
    raw_url: &[u8],
    parent_debug: Rc<RefCell<Vec<String>>>,
    flattening_failed: bool,
) -> bool {
    // Sprite outputs never nest: C++ shares one slot per value and
    // suppresses the image/cache-extension renders once sprited; the
    // rolled-up pass sees sprited values instead, so it stands down
    // for them directly.
    if original_url.spec().contains(".pagespeed.is.") {
        return false;
    }
    let options = driver.options();
    let (resource, authorized) = driver.create_input_resource(
        original_url,
        InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
        IntendedFor::General,
        InputRole::Img,
    );
    let Some(input) = resource else {
        // This doesn't fail flattening but we want to log it. A
        // failed (or disabled) hierarchy prefixes the reason.
        let mut message = cannot_rewrite_message("rewrite", original_url.spec(), authorized);
        if flattening_failed {
            message = format!("Flattening failed: {message}");
        }
        parent_debug.borrow_mut().push(message);
        return false;
    };

    let slot = Rc::new(RefCell::new(CssResourceSlot::new(
        Some(Rc::new(input.clone())),
        trim_url.clone(),
        target,
        raw_url,
    )));
    if options.image_preserve_urls() {
        slot.borrow_mut().base_mut().set_preserve_urls(true);
    }

    rewrite_slot(
        driver,
        dom,
        slot,
        input,
        image_inline_max_bytes,
        parent_debug,
    )
}

/// Human failure reason (`CannotImportMessage`).
fn cannot_rewrite_message(action: &str, url: &str, is_authorized: bool) -> String {
    format!(
        "Cannot {action} {url} {}",
        if is_authorized {
            "for an unknown reason"
        } else {
            "as it is on an unauthorized domain"
        }
    )
}

/// Rewrites image URLs in a stylesheet (`RewriteCss` image half),
/// returning whether any value was optimized. Import flattening
/// runs separately (sync `expand_and_flatten`); spriting waits for
/// the combiner epic.
pub fn rewrite_css_images(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    sheet: &Rc<RefCell<ps_css::ast::Stylesheet>>,
    css_base: &GoogleUrl,
    css_trim: &GoogleUrl,
    image_inline_max_bytes: i64,
    parent_debug: Rc<RefCell<Vec<String>>>,
    flattening_failed: bool,
) -> bool {
    let options = driver.options();
    if !rewrites_enabled(&options, image_inline_max_bytes) {
        // NOTE: info messages wait for the message handler.
        return false;
    }
    // NOTE: spriting (`kSpriteImages`) waits for the combiner epic.
    let locations = ps_css::rewrite::collect_image_urls(&sheet.borrow());
    let mut css_rewritten = false;
    for location in locations {
        let raw_url = {
            let sheet = sheet.borrow();
            let value = &sheet.rulesets()[location.ruleset].declarations()[location.declaration]
                .values()[location.value];
            value.string_value().as_bytes().to_vec()
        };
        // TODO(abliss): only do this resolution once.
        let original_url = GoogleUrl::resolve(css_base, &String::from_utf8_lossy(&raw_url));
        if !original_url.is_web_valid() {
            continue;
        }
        if !options.is_allowed(original_url.spec()) {
            continue;
        }
        let target = CssSlotTarget::Sheet {
            sheet: sheet.clone(),
            location,
        };
        css_rewritten |= rewrite_image(
            driver,
            dom,
            image_inline_max_bytes,
            css_trim,
            &original_url,
            target,
            &raw_url,
            parent_debug.clone(),
            flattening_failed,
        );
    }
    css_rewritten
}

/// Rewrites image URLs in `style`-attribute declarations (the
/// attribute path shares the walker over declaration values),
/// returning whether any value was optimized.
pub fn rewrite_attribute_images(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    declarations: &Rc<RefCell<ps_css::ast::Declarations>>,
    css_base: &GoogleUrl,
    css_trim: &GoogleUrl,
    image_inline_max_bytes: i64,
    parent_debug: Rc<RefCell<Vec<String>>>,
    flattening_failed: bool,
) -> bool {
    let options = driver.options();
    if !rewrites_enabled(&options, image_inline_max_bytes) {
        return false;
    }
    let mut targets = Vec::new();
    {
        let declarations = declarations.borrow();
        for (decl_index, declaration) in declarations.iter().enumerate() {
            if !ps_css::rewrite::is_image_property(declaration.prop()) {
                continue;
            }
            for (value_index, value) in declaration.values().iter().enumerate() {
                if value.lexical_type() == ps_css::value::ValueType::Uri {
                    targets.push((decl_index, value_index));
                }
            }
        }
    }
    let mut css_rewritten = false;
    for (decl_index, value_index) in targets {
        let raw_url = {
            let declarations = declarations.borrow();
            declarations[decl_index].values()[value_index]
                .string_value()
                .as_bytes()
                .to_vec()
        };
        let original_url = GoogleUrl::resolve(css_base, &String::from_utf8_lossy(&raw_url));
        if !original_url.is_web_valid() {
            continue;
        }
        if !options.is_allowed(original_url.spec()) {
            continue;
        }
        let target = CssSlotTarget::Attribute {
            declarations: declarations.clone(),
            declaration: decl_index,
            value: value_index,
        };
        css_rewritten |= rewrite_image(
            driver,
            dom,
            image_inline_max_bytes,
            css_trim,
            &original_url,
            target,
            &raw_url,
            parent_debug.clone(),
            flattening_failed,
        );
    }
    css_rewritten
}

/// Runs nested contexts over a fallback association slot
/// (`RewriteSlot` for `AssociationSlot`), merging debug. Returns
/// whether the slot was optimized.
pub fn rewrite_assoc_slot(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    slot: Rc<RefCell<AssociationSlot>>,
    input: crate::resource::InputResource,
    image_inline_max_bytes: i64,
    parent_debug: Rc<RefCell<Vec<String>>>,
) -> bool {
    let options = driver.options();
    if options.image_optimization_enabled() || image_inline_max_bytes > 0 {
        if !slot.borrow().base().preserve_urls() || options.in_place_preemptive_rewrite_css_images()
        {
            let behavior = ImageRewriteBehavior::for_css(
                driver.clone(),
                image_inline_max_bytes,
                parent_debug.clone(),
            );
            let mut context = SingleRewriteContext::new(driver.clone(), behavior);
            context.add_assoc_slot(slot.clone(), input.clone());
            context.start(dom);
            merge_nested_debug(&mut context, &parent_debug);
        }
    }

    // An image-optimized slot keeps its URL: the extender stands
    // down, like the HTML path refusing pagespeed resources.
    if !slot.borrow().base().was_optimized() && driver.may_cache_extend_images() {
        let behavior = CacheExtenderBehavior::for_nested(driver.clone(), InputRole::Img);
        let mut context = SingleRewriteContext::new(driver.clone(), behavior);
        context.add_assoc_slot(slot.clone(), input);
        context.start(dom);
        merge_nested_debug(&mut context, &parent_debug);
    }

    slot.borrow().base().was_optimized()
}

/// Appends nested debug to a partition, deduplicated
/// (`AddDebugMessages` dedup half; the DOM budget applies at
/// insertion).
pub fn merge_parent_debug(partition: &mut CachedResult, parent_debug: Rc<RefCell<Vec<String>>>) {
    let mut seen = std::collections::HashSet::new();
    seen.extend(partition.debug_message.iter().cloned());
    for message in parent_debug.borrow().iter() {
        if seen.insert(message.clone()) {
            partition.debug_message.push(message.clone());
        }
    }
}

/// Renders nested debug after a node, deduplicated and in order
/// (`AddDebugMessages` + `InsertDebugComments`).
pub fn insert_parent_debug(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    parent_debug: Rc<RefCell<Vec<String>>>,
    node: crate::element::NodeId,
) {
    let mut seen = std::collections::HashSet::new();
    let mut messages = Vec::new();
    for message in parent_debug.borrow().iter() {
        if seen.insert(message.clone()) {
            messages.push(message.clone());
        }
    }
    driver.insert_debug_comments(dom, &messages, node);
}

/// Counts absolutified URLs (`CssUrlCounter`).
struct UrlCounter<'a> {
    base: &'a GoogleUrl,
    counts: std::collections::HashMap<String, usize>,
}

impl ps_css::scanner::Transformer for UrlCounter<'_> {
    fn transform(&mut self, url: &mut Vec<u8>) -> ps_css::scanner::TransformStatus {
        use ps_css::scanner::TransformStatus;
        // Note: we do not mess with empty URLs at all, nor with
        // fragment-only references such as url(#clip), which name an
        // element of the document using the stylesheet rather than
        // a resource.
        if url.is_empty() || url[0] == b'#' {
            return TransformStatus::NoChange;
        }
        let resolved = GoogleUrl::resolve(self.base, &String::from_utf8_lossy(url));
        if !resolved.is_web_or_data_valid() {
            return TransformStatus::Failure;
        }
        *self.counts.entry(resolved.spec().to_string()).or_insert(0) += 1;
        TransformStatus::NoChange
    }
}

/// Association transformer (`AssociationTransformer`): rewrites
/// counted URLs from the map, absolutifying the rest.
struct AssociationTransformer<'a> {
    base: &'a GoogleUrl,
    options: &'a ps_config::options::RewriteOptions,
    map: std::collections::HashMap<String, String>,
    backup: Option<crate::server_context::RewriteDomainTransformer<'a>>,
}

impl ps_css::scanner::Transformer for AssociationTransformer<'_> {
    fn transform(&mut self, url: &mut Vec<u8>) -> ps_css::scanner::TransformStatus {
        use ps_css::scanner::TransformStatus;
        // Note: we do not mess with empty URLs at all.
        if url.is_empty() {
            return TransformStatus::NoChange;
        }
        let resolved = GoogleUrl::resolve(self.base, &String::from_utf8_lossy(url));
        if !resolved.is_web_or_data_valid() {
            return TransformStatus::Failure;
        }
        if let Some(mapped) = self.map.get(resolved.spec()) {
            let relativity = GoogleUrl::find_relativity(&String::from_utf8_lossy(url));
            *url = crate::resource_slot::relativize_or_passthrough(
                self.options,
                mapped,
                relativity,
                self.base,
            )
            .into_bytes();
            return TransformStatus::Success;
        }
        if let Some(backup) = self.backup.as_mut() {
            return backup.transform(url);
        }
        TransformStatus::NoChange
    }
}

/// Fallback outcome (`FallbackRewriteUrls` + `Harvest` verdict).
pub enum FallbackOutcome {
    /// Phase 1 failed (unparseable URL): plain failure, no
    /// fallback statistics.
    Unparseable,
    /// Phase 2 failed: fallback failure plus debug.
    TransformError,
    /// Rewritten bytes.
    Rewritten(Vec<u8>),
}

/// Rewrites URLs in unparseable CSS (`FallbackRewriteUrls`): counts
/// URLs, nests image/cache-extension contexts into association
/// slots, then substitutes.
pub fn fallback_rewrite_urls(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    css_base: &GoogleUrl,
    css_trim: &GoogleUrl,
    in_text: &[u8],
    image_inline_max_bytes: i64,
    parent_debug: Rc<RefCell<Vec<String>>>,
) -> FallbackOutcome {
    // Phase 1: record all URLs found with the counter.
    let mut counter = UrlCounter {
        base: css_base,
        counts: std::collections::HashMap::new(),
    };
    // TransformUrls succeeds only if all the URLs in the CSS file
    // were parseable. If we encounter any unparseable URLs, we will
    // not be able to absolutify them and so should not rewrite the CSS.
    if ps_css::scanner::transform_urls(in_text, &mut counter).is_none() {
        return FallbackOutcome::Unparseable;
    }

    // Setup absolutifier used by the fallback transformer. Only
    // enable it if we need to absolutify resources.
    let (should_absolutify, proxying) = driver.should_absolutify_url(css_base, css_trim);
    let options = driver.options();
    let namer = driver.server_context().url_namer().clone();
    let mut backup = None;
    if should_absolutify {
        backup = Some(crate::server_context::RewriteDomainTransformer::new(
            css_base,
            css_trim,
            driver.lawyer(),
            &options,
            &*namer,
            driver.server_context().hasher().hash_size_in_chars(),
            driver.signature_length(),
            !proxying,
        ));
    }

    // Nested contexts render into the association map.
    let map = Rc::new(RefCell::new(std::collections::HashMap::new()));
    // TODO(sligocki): Use count of occurrences to decide which URLs
    // to inline.
    let mut urls: Vec<String> = counter.counts.keys().cloned().collect();
    urls.sort();
    for url_text in urls {
        let url = GoogleUrl::new(&url_text);
        // This can be both an image or CSS at very least, so have to
        // be conservative wrt to policy.
        let (resource, authorized) = driver.create_input_resource(
            &url,
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Unknown,
        );
        if let Some(input) = resource {
            use crate::resource::Resource;
            let handle: Rc<dyn Resource> = Rc::new(input.clone());
            let slot = Rc::new(RefCell::new(AssociationSlot::new(
                Some(handle),
                map.clone(),
                &url_text,
            )));
            rewrite_assoc_slot(
                driver,
                dom,
                slot,
                input,
                image_inline_max_bytes,
                parent_debug.clone(),
            );
        } else if !authorized {
            parent_debug.borrow_mut().push(format!(
                "A resource was not rewritten because {} is not an authorized domain",
                url.host()
            ));
        }
    }

    // Phase 2: substitute (in Harvest, after subresources rewrite).
    let mut transformer = AssociationTransformer {
        base: css_base,
        options: &options,
        map: map.borrow().clone(),
        backup,
    };
    match ps_css::scanner::transform_urls(in_text, &mut transformer) {
        Some(out) => FallbackOutcome::Rewritten(out),
        None => FallbackOutcome::TransformError,
    }
}

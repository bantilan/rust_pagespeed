//! Fixed-width responsive srcsets (`ResponsiveImageFirst`).
//!
//! Generates `srcset` candidates at fixed widths for plain `<img>`
//! elements that have none, instead of predicting per-image sizes:
//! every eligible image gets 480w + 640w resized variants minted as
//! unversioned (hash-`0`) image URLs whose bytes materialize on
//! first request through the standard resize pipeline (width-only
//! dims preserve aspect; downscales only, never upscales).
//!
//! Paired `sizes`: lazy images get `auto, 100vw` (browser-evaluated
//! layout width with a viewport fallback); eager images get a static
//! `(max-width: 640px) 480px, 640px` breakpoint. An existing author
//! `sizes` is preserved verbatim (it was inert without a srcset, and
//! guessing wrong is worse than pairing loosely).
//!
//! Author srcsets are never touched: any existing `srcset`
//! attribute skips generation entirely (the `SrcsetSizes` filter
//! owns those). Large displays upscale from 640w by design.

use std::rc::Rc;

use ps_http::content_type::{extension_to_content_type, ContentTypeKind};
use ps_http::google_url::GoogleUrl;

use crate::element::NodeId;
use crate::filter::{HtmlFilter, ScriptUsage};
use crate::filters::image_rewrite::FILTER_ID as IMAGE_FILTER_ID;
use crate::filters::srcset_sizes::SrcsetSizesFilter;
use crate::image_url_encoder::{ImageDim, ImageResourceContext, ImageUrlEncoder};
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::output_resource::OutputResourceKind;
use crate::parser::Dom;
use crate::resource::Resource;
use crate::resource_slot::relativize_or_passthrough;
use crate::server_context::ResourceDriver;

/// Fixed srcset candidate widths (CSS px, ascending).
pub const RESPONSIVE_SRCSET_WIDTHS: [u32; 2] = [480, 640];

/// `sizes` for lazy images (native `auto` + viewport fallback).
pub const LAZY_SIZES: &str = "auto, 100vw";

/// `sizes` for eager images (static small/large breakpoint).
pub const EAGER_SIZES: &str = "(max-width: 640px) 480px, 640px";

/// Source URL cap (bytes, escaped representation).
const MAX_SRC_BYTES: usize = 2048;

/// Image kinds worth generating resized variants for. Vectors
/// scale natively and icons are already tiny.
fn compressible_image_kind(kind: ContentTypeKind) -> bool {
    matches!(
        kind,
        ContentTypeKind::Png
            | ContentTypeKind::Gif
            | ContentTypeKind::Jpeg
            | ContentTypeKind::Webp
            | ContentTypeKind::Avif
    )
}

/// Driver-bound first pass: mints fixed-width srcsets. No second
/// pass exists by design (nothing virtual to combine back, unlike
/// the C++ density-descriptor flow).
pub struct ResponsiveImageFirstFilter {
    driver: Rc<ResourceDriver>,
    blocked_depth: usize,
}

impl ResponsiveImageFirstFilter {
    /// Uses the request's resource driver (base URL, namer, options).
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        Self {
            driver,
            blocked_depth: 0,
        }
    }

    fn loading_is_lazy(dom: &mut Dom, id: NodeId) -> bool {
        let Ok(Some(index)) = SrcsetSizesFilter::unique_attribute(dom, id, b"loading") else {
            return false;
        };
        SrcsetSizesFilter::bounded_attribute(dom, id, index, 16)
            .is_some_and(|value| value.eq_ignore_ascii_case(b"lazy"))
    }

    /// Mints one unversioned resized variant URL (hash `0`, bytes on
    /// demand like deferred rewrites), relativized like any rewrite.
    fn variant_url(
        &self,
        src_text: &str,
        resolved: &GoogleUrl,
        content_type: ps_http::content_type::ContentType,
        width: u32,
    ) -> Option<String> {
        let mapped = self.driver.map_output_base(resolved.spec()).ok()?;
        let leaf = mapped.leaf_with_query().as_bytes().to_vec();
        let mut context = ImageResourceContext::default();
        context.desired_image_dims = ImageDim {
            width: Some(width),
            height: None,
        };
        let name = ImageUrlEncoder.encode(&leaf, &context);
        let mut output = self
            .driver
            .create_output_resource_with_name(
                IMAGE_FILTER_ID,
                &name,
                &mapped,
                resolved,
                OutputResourceKind::Rewritten,
            )
            .ok()?;
        output.set_type(Some(content_type));
        output.set_hash("0");
        let url = output.url().to_string();
        let relativity = GoogleUrl::find_relativity(src_text);
        let base = self.driver.decoded_base_url();
        Some(relativize_or_passthrough(
            &self.driver.options(),
            &url,
            relativity,
            &base,
        ))
    }

    fn apply(&self, dom: &mut Dom, id: NodeId) -> Option<()> {
        // Author srcsets (and ambiguous ones) stay untouched.
        if SrcsetSizesFilter::unique_attribute(dom, id, b"srcset")
            .ok()?
            .is_some()
        {
            return None;
        }
        let src = SrcsetSizesFilter::unique_attribute(dom, id, b"src").ok()??;
        let raw = SrcsetSizesFilter::bounded_attribute(dom, id, src, MAX_SRC_BYTES)?;
        if raw.is_empty() {
            return None;
        }
        let src_text = std::str::from_utf8(&raw).ok()?;
        let base = self.driver.decoded_base_url();
        let resolved = GoogleUrl::resolve(&base, src_text);
        if !resolved.is_web_valid() {
            return None;
        }
        // Skip already-rewritten URLs by marker: is_pagespeed_resource
        // does not recognize image `.ic.` ids, and a false positive
        // here only skips an optimization, never breaks markup.
        if resolved.leaf_sans_query().contains(".pagespeed.") {
            return None;
        }
        let content_type = extension_to_content_type(&resolved.leaf_sans_query())?;
        if !compressible_image_kind(content_type.kind()) {
            return None;
        }
        // All-or-nothing pairs: a lone candidate is pointless.
        let mut urls = Vec::with_capacity(RESPONSIVE_SRCSET_WIDTHS.len());
        for width in RESPONSIVE_SRCSET_WIDTHS {
            urls.push(self.variant_url(src_text, &resolved, content_type, width)?);
        }
        let mut srcset = String::new();
        for (index, (url, width)) in urls.iter().zip(RESPONSIVE_SRCSET_WIDTHS).enumerate() {
            if index > 0 {
                srcset.push_str(", ");
            }
            srcset.push_str(url);
            srcset.push_str(&format!(" {width}w"));
        }
        dom.add_attribute(id, b"srcset", srcset.as_bytes());
        // Pair sizes: keep an existing author value, skip on ambiguity.
        match SrcsetSizesFilter::unique_attribute(dom, id, b"sizes").ok()? {
            Some(_) => {}
            None => {
                let sizes = if Self::loading_is_lazy(dom, id) {
                    LAZY_SIZES
                } else {
                    EAGER_SIZES
                };
                dom.add_attribute(id, b"sizes", sizes.as_bytes());
            }
        }
        Some(())
    }
}

impl HtmlFilter for ResponsiveImageFirstFilter {
    fn name(&self) -> &'static str {
        "ResponsiveImageFirstFilter"
    }
    fn can_modify_urls(&self) -> bool {
        true
    }
    fn script_usage(&self) -> ScriptUsage {
        ScriptUsage::NeverInjectsScripts
    }
    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.blocked_depth = 0;
    }
    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let keyword = dom.element_keyword(id);
        if SrcsetSizesFilter::blocks(dom, id, keyword) {
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
        if self.blocked_depth != 0 && SrcsetSizesFilter::blocks(dom, id, dom.element_keyword(id)) {
            self.blocked_depth = self.blocked_depth.saturating_sub(1);
        }
    }
}

//! Low-resolution image placeholders (`DelayImagesFilter`).
//!
//! Images carrying a pipeline-generated `data-pagespeed-low-res-src`
//! show the low-res bytes first while the high-res original loads on
//! `onload`. On desktop (or for blank previews) the swap happens in
//! place; on aggressive mobile the low-res map moves to the end of
//! the body so it cannot block parsing.

use std::collections::BTreeMap;

use ps_config::options::{Category, RewriteOptions};

use super::common::{add_js_to_element, XhtmlStatus};
use super::lazyload_images::{FinderAvailability, BEACON_IMAGE_ONLOAD_CODE};
use super::url_left_trim::UrlValuedAttributes;
use crate::element::{LeafKind, NodeId};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::resource_tag_scanner::scan_element;
use crate::static_assets::{get_asset, StaticAsset};

/// Delay-library initializer (`kDelayImagesSuffix`).
pub const DELAY_IMAGES_SUFFIX: &[u8] = b"\npagespeed.delayImagesInit();";

/// Inline-library initializer (`kDelayImagesInlineSuffix`).
pub const DELAY_IMAGES_INLINE_SUFFIX: &[u8] = b"\npagespeed.delayImagesInlineInit();";

/// Image onload (`kImageOnloadCode`): swap in the high-res bytes.
pub const IMAGE_ONLOAD_CODE: &[u8] = b"if(window.pagespeed){if(pagespeed.switchToHighResAndMaybeBeacon){pagespeed.switchToHighResAndMaybeBeacon(this);}}";

/// Swap helper installed once per page (`kImageOnloadJsSnippet`).
pub const IMAGE_ONLOAD_JS_SNIPPET: &[u8] = b"window['pagespeed'] = window['pagespeed'] || {};var pagespeed = window['pagespeed'];pagespeed.switchToHighResAndMaybeBeacon = function(elem) {setTimeout(function(){elem.onload = null;var srcset = elem.getAttribute('data-pagespeed-high-res-srcset');if (srcset) {elem.srcset = srcset;}elem.src = elem.getAttribute('data-pagespeed-high-res-src');}, 0);};";

/// `onerror` prefix that stops failure loops (`this.onerror=null;`).
const ONERROR_RESET: &[u8] = b"this.onerror=null;";

/// HTML application status (`RewriterHtmlApplication::Status`, partial).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelayImagesHtmlStatus {
    /// The filter ran.
    Active,
    /// The user agent cannot inline images.
    UserAgentNotSupported,
    /// Critical-image data is still gathering.
    PropertyCacheMiss,
}

/// Driver inputs for image delaying.
#[derive(Debug, Clone)]
pub struct DelayImagesConfig {
    /// Rewrite options (flags, URL-valued attributes).
    pub options: RewriteOptions,
    /// Whether the user agent inlines images
    /// (`RequestProperties::SupportsImageInlining`).
    pub supports_image_inlining: bool,
    /// Whether the user agent is mobile (`RequestProperties::IsMobile`).
    pub is_mobile: bool,
    /// Finder availability (`CriticalImagesFinder::Available`).
    pub finder_availability: FinderAvailability,
    /// Pipeline preview count (`num_inline_preview_images`).
    pub num_inline_preview_images: u32,
    /// Operator URL-valued attributes.
    pub url_valued: UrlValuedAttributes,
    /// `kDebug` (debug asset builds).
    pub debug: bool,
    /// `kPedantic` (script type attributes).
    pub pedantic: bool,
    /// Response mimetype XHTML certainty.
    pub xhtml: XhtmlStatus,
}

impl Default for DelayImagesConfig {
    fn default() -> Self {
        DelayImagesConfig {
            options: RewriteOptions::new(),
            supports_image_inlining: true,
            is_mobile: false,
            finder_availability: FinderAvailability::Disabled,
            num_inline_preview_images: 0,
            url_valued: Vec::new(),
            debug: false,
            pedantic: false,
            xhtml: XhtmlStatus::IsNotXhtml,
        }
    }
}

/// Delays high-res images behind low-res placeholders (`DelayImagesFilter`).
pub struct DelayImagesFilter {
    config: DelayImagesConfig,
    /// Tagged images seen this document.
    num_low_res: u32,
    /// Swap in place (desktop, or blank previews).
    inplace: bool,
    /// Lazyload (vs swap now) the high-res bytes on mobile.
    lazyload_highres: bool,
    /// Delay libraries placed yet.
    script_inserted: bool,
    /// Swap helper placed yet.
    onload_js_added: bool,
    /// Decoded src to decoded low-res bytes, in URL order.
    low_res_map: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Nested `<noscript>` depth (only gates the onload swap, via
    /// `CommonFilter` parity — the pipeline never tags noscript).
    noscript_depth: u32,
    /// HTML status (`LogRewriterHtmlStatus`).
    html_status: Option<DelayImagesHtmlStatus>,
    /// Images rewritten (`APPLIED_OK` count).
    applied: u32,
}

impl DelayImagesFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: DelayImagesConfig) -> Self {
        DelayImagesFilter {
            config,
            num_low_res: 0,
            inplace: false,
            lazyload_highres: false,
            script_inserted: false,
            onload_js_added: false,
            low_res_map: BTreeMap::new(),
            noscript_depth: 0,
            html_status: None,
            applied: 0,
        }
    }

    /// Returns the HTML status plus the rewritten-image count; the log
    /// channel itself does not exist yet, so tests read the filter.
    #[must_use]
    pub fn log_status(&self) -> (Option<DelayImagesHtmlStatus>, u32) {
        (self.html_status, self.applied)
    }

    /// Inplace on desktop or for blank previews; mobile aggressive
    /// moves the bytes to the end of the window (`ShouldRewriteInplace`).
    fn should_rewrite_inplace(&self) -> bool {
        self.config.options.use_blank_image_for_inline_preview()
            || !(self.config.options.enable_aggressive_rewriters_for_mobile()
                && self.config.is_mobile)
    }

    /// Installs the swap helper once, before its first user
    /// (`MaybeAddImageOnloadJsSnippet`).
    fn maybe_add_onload_js(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if self.onload_js_added {
            return;
        }
        self.onload_js_added = true;
        let script = dom.new_element(None, b"script");
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        dom.insert_node_before_node(id, script);
        add_js_to_element(
            dom,
            lexer,
            script,
            IMAGE_ONLOAD_JS_SNIPPET,
            self.config.pedantic,
            self.config.xhtml,
        );
    }

    /// Moves collected low-res bytes after the current element (or
    /// appends them to the body), with the delay libraries first
    /// (`InsertLowResImagesAndJs`).
    fn insert_low_res_and_js(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId, after: bool) {
        if self.low_res_map.is_empty() {
            return;
        }
        let mut current = id;
        if !self.script_inserted {
            let mut lib = get_asset(StaticAsset::DelayImagesInline, self.config.debug).to_vec();
            lib.extend_from_slice(DELAY_IMAGES_INLINE_SUFFIX);
            lib.extend_from_slice(get_asset(StaticAsset::DelayImages, self.config.debug));
            lib.extend_from_slice(DELAY_IMAGES_SUFFIX);
            let script = dom.new_element(Some(id), b"script");
            dom.add_escaped_attribute(
                script,
                b"data-pagespeed-no-defer",
                None,
                crate::element::QuoteStyle::DoubleQuote,
            );
            if after {
                dom.insert_node_after_node(current, script);
                current = script;
            } else {
                dom.append_child(Some(id), script);
            }
            add_js_to_element(
                dom,
                lexer,
                script,
                &lib,
                self.config.pedantic,
                self.config.xhtml,
            );
            self.script_inserted = true;
        }
        let entries: Vec<(Vec<u8>, Vec<u8>)> =
            std::mem::take(&mut self.low_res_map).into_iter().collect();
        for (url, data) in &entries {
            let mut js = b"\nif(window.pagespeed){if(pagespeed.delayImagesInline){pagespeed.delayImagesInline.addLowResImages('".to_vec();
            js.extend_from_slice(url);
            js.extend_from_slice(b"', '");
            js.extend_from_slice(data);
            js.extend_from_slice(b"');\npagespeed.delayImagesInline.replaceWithLowRes();}}\n");
            let script = dom.new_element(Some(current), b"script");
            dom.add_escaped_attribute(
                script,
                b"data-pagespeed-no-defer",
                None,
                crate::element::QuoteStyle::DoubleQuote,
            );
            if after {
                dom.insert_node_after_node(current, script);
                current = script;
            } else {
                dom.append_child(Some(id), script);
            }
            add_js_to_element(
                dom,
                lexer,
                script,
                &js,
                self.config.pedantic,
                self.config.xhtml,
            );
        }
    }

    /// Appends the high-res trigger to the body (`InsertHighResJs`).
    fn insert_high_res_js(&mut self, dom: &mut Dom, lexer: &Lexer, body: NodeId) {
        if self.inplace || !self.script_inserted {
            return;
        }
        let mut js =
            b"\nif(window.pagespeed){if(pagespeed.delayImages){pagespeed.delayImages.".to_vec();
        js.extend_from_slice(if self.lazyload_highres {
            b"registerLazyLoadHighRes();}}\n"
        } else {
            b"replaceWithHighRes();}}\n"
        });
        let script = dom.new_element(Some(body), b"script");
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        dom.append_child(Some(body), script);
        add_js_to_element(
            dom,
            lexer,
            script,
            &js,
            self.config.pedantic,
            self.config.xhtml,
        );
    }

    /// Whether our onload may replace the image's
    /// (`CanAddPagespeedOnloadToImage`): outside noscript, with no
    /// onload or only the beacon's own.
    fn can_add_onload(&self, dom: &mut Dom, id: NodeId) -> bool {
        if self.noscript_depth > 0 {
            return false;
        }
        let Some(index) = dom.find_attribute(id, Keyword::Onload) else {
            return true;
        };
        let (decoded, _) = dom.decoded_attribute(id, index);
        decoded.as_deref() == Some(BEACON_IMAGE_ONLOAD_CODE)
    }
}

impl HtmlFilter for DelayImagesFilter {
    fn name(&self) -> &'static str {
        "DelayImages"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        let status = if !self.config.supports_image_inlining {
            DelayImagesHtmlStatus::UserAgentNotSupported
        } else if self.config.finder_availability == FinderAvailability::NoDataYet {
            DelayImagesHtmlStatus::PropertyCacheMiss
        } else {
            DelayImagesHtmlStatus::Active
        };
        self.html_status = Some(status);
        status == DelayImagesHtmlStatus::Active
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.num_low_res = 0;
        self.inplace = self.should_rewrite_inplace();
        self.lazyload_highres =
            self.config.options.lazyload_highres_images() && self.config.is_mobile;
        self.script_inserted = false;
        self.onload_js_added = false;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
        if dom.element_keyword(id) == Keyword::Body {
            self.insert_low_res_and_js(dom, lexer, id, false);
            self.insert_high_res_js(dom, lexer, id);
            // Tail delete (no-op on bodies, like the C++ unconditional).
            dom.delete_attribute(id, Keyword::DataPagespeedLowResSrc);
        } else if dom.is_rewritable(id)
            && matches!(dom.element_keyword(id), Keyword::Img | Keyword::Input)
        {
            let Some(low_index) = dom.find_attribute(id, Keyword::DataPagespeedLowResSrc) else {
                return;
            };
            let (low_decoded, _) = dom.decoded_attribute(id, low_index);
            let Some(low_res) = low_decoded else {
                return;
            };
            // Only image-valued `src` rewrites (a missing `src`
            // categorizes `Undefined`, like the C++ null-attribute path).
            let src_index = dom.find_attribute(id, Keyword::Src);
            let src_is_image = src_index.is_some_and(|index| {
                scan_element(dom, id, &self.config.url_valued)
                    .iter()
                    .any(|(found, category)| *found == index && *category == Category::Image)
            });
            let src_decoded = src_index.and_then(|index| dom.decoded_attribute(id, index).0);
            let Some(src) = src_decoded else {
                return;
            };
            if !src_is_image {
                return;
            }
            self.num_low_res += 1;
            if self.can_add_onload(dom, id) {
                self.applied += 1;
                let src_index = src_index.expect("checked image-valued above");
                dom.set_attribute_name(id, src_index, b"data-pagespeed-high-res-src");
                if let Some(set_index) = dom.find_attribute(id, Keyword::Srcset) {
                    dom.set_attribute_name(id, set_index, b"data-pagespeed-high-res-srcset");
                }
                if self.inplace {
                    dom.add_attribute(id, b"src", &low_res);
                    dom.delete_attribute(id, Keyword::Onload);
                    dom.add_attribute(id, b"onload", IMAGE_ONLOAD_CODE);
                    dom.delete_attribute(id, Keyword::Onerror);
                    let mut onerror = ONERROR_RESET.to_vec();
                    onerror.extend_from_slice(IMAGE_ONLOAD_CODE);
                    dom.add_attribute(id, b"onerror", &onerror);
                    self.maybe_add_onload_js(dom, lexer, id);
                } else {
                    self.low_res_map.insert(src, low_res);
                }
            }
            if self.num_low_res == self.config.num_inline_preview_images && !self.inplace {
                self.insert_low_res_and_js(dom, lexer, id, true);
            }
            dom.delete_attribute(id, Keyword::DataPagespeedLowResSrc);
        }
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.low_res_map.clear();
    }
}

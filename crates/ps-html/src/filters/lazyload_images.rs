//! Below-the-fold image deferral (`LazyloadImagesFilter`).
//!
//! Rewrites `<img src>` into `data-pagespeed-lazy-src` plus a blank
//! placeholder, an onload that fetches the real image when visible,
//! and an onerror fallback; a runtime script goes at the end of
//! `<head>` (or before the first non-head element), with an
//! attribute-override postscript after each batch of rewrites.
//! Critical images (per the injected finder set), blacklisted URLs,
//! foreign onloads, and hostile contexts (noscript, noembed,
//! marquee, disabled classes, the jquery slider) stay put.

use std::collections::HashSet;

use ps_config::options::RewriteOptions;
use ps_http::data_url::is_data_url;
use ps_http::google_url::GoogleUrl;

use super::common::{add_js_to_element, XhtmlStatus};
use crate::base_tracker::BaseTracker;
use crate::domain_lawyer::DomainLawyer;
use crate::decode_url::{decode_url, DecodeConfig};
use crate::element::{LeafKind, NodeId};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::static_assets::{get_asset, StaticAsset};
use crate::url_namer::UrlNamer;

/// Image onload (`kImageOnloadCode`): fetch when visible, then beacon.
pub const IMAGE_ONLOAD_CODE: &[u8] = b"if(window.pagespeed){if(pagespeed.lazyLoadImages){pagespeed.lazyLoadImages.loadIfVisibleAndMaybeBeacon(this);}}";

/// Abort-script body (`kLoadAllImages`): restore everything now.
pub const LOAD_ALL_IMAGES: &[u8] =
    b"if(window.pagespeed){if(pagespeed.lazyLoadImages){pagespeed.lazyLoadImages.loadAllImages();}}";

/// Blank placeholder (1x1 transparent gif as a data URI): C++
/// serves equivalent bytes from the static-asset handler, whose
/// prefix lives outside this crate, so the driver inlines the bytes
/// instead. Renders identically with no extra fetch; the
/// `LazyloadImagesBlankUrl` option still overrides it.
pub const BLANK_IMAGE_DATA_URL: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

/// Override-script body (`kOverrideAttributeFunctions`).
pub const OVERRIDE_ATTRIBUTE_FUNCTIONS: &[u8] = b"if(window.pagespeed){if(pagespeed.lazyLoadImages){pagespeed.lazyLoadImages.overrideAttributeFunctions();}}";

/// Beacon-added onload, the only foreign onload tolerated
/// (`CriticalImagesBeaconFilter::kImageOnloadCode`).
pub const BEACON_IMAGE_ONLOAD_CODE: &[u8] = b"if(window.pagespeed){if(pagespeed.CriticalImages){pagespeed.CriticalImages.checkImageForCriticality(this);}}";

/// Script-library marker that aborts the rewrite (`kJquerySlider`).
const JQUERY_SLIDER: &[u8] = b"jquery.sexyslider";

/// `onerror` prefix that stops failure loops (`this.onerror=null;`).
const ONERROR_RESET: &[u8] = b"this.onerror=null;";

/// Critical-image finder availability
/// (`CriticalImagesFinder::Availability`, as observed by this filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FinderAvailability {
    /// Beacon data usable; critical images keep their `src`.
    Available,
    /// Beaconing off; every image is a rewrite candidate.
    #[default]
    Disabled,
    /// Data still gathering; the whole filter stands down.
    NoDataYet,
}

/// HTML application status (`RewriterHtmlApplication::Status`, partial).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LazyloadHtmlStatus {
    /// The filter ran.
    Active,
    /// The user agent cannot lazyload.
    UserAgentNotSupported,
    /// XHRs and data-gathering pages skip the filter.
    Disabled,
}

/// Per-image rewrite outcome (`LogLazyloadFilter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LazyloadStatus {
    /// The image was deferred.
    AppliedOk,
    /// The image was left alone.
    NotApplied,
}

/// One per-image log record: status plus the blacklist/critical flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LazyloadLogRecord {
    /// Rewrite outcome.
    pub status: LazyloadStatus,
    /// The URL was disallowed.
    pub blacklisted: bool,
    /// The image is critical.
    pub critical: bool,
}

/// Driver inputs for image lazyloading.
#[derive(Debug, Clone)]
pub struct LazyloadImagesConfig {
    /// Rewrite options (flags, allow rules, lazyload classes).
    pub options: RewriteOptions,
    /// Whether the user agent runs the lazyload runtime
    /// (`RequestProperties::SupportsLazyloadImages`).
    pub supports_lazyload_images: bool,
    /// Whether this is an XHR (`IsXmlHttpRequest`).
    pub is_xml_http_request: bool,
    /// Finder availability (`CriticalImagesFinder::Available`).
    pub finder_availability: FinderAvailability,
    /// Critical-image key hashes (`IsHtmlCriticalImage` set, keyed
    /// by [`crate::beacon_finder::url_hash`] like C++ `GetKeyForUrl`).
    pub critical_images: HashSet<String>,
    /// Blank placeholder URL (the `BLANK_GIF` asset URL; the
    /// `LazyloadImagesBlankUrl` option overrides it when set).
    pub blank_image_url: String,
    /// Whether the runtime already flushed (`is_lazyload_script_flushed`).
    pub lazyload_script_flushed: bool,
    /// Hasher length in characters (URL decoding).
    pub hash_length: usize,
    /// Signature length in characters (0 unless URL signing).
    pub signature_length: usize,
    /// `kDebug` (debug asset builds).
    pub debug: bool,
    /// `kPedantic` (script type attributes).
    pub pedantic: bool,
    /// Response mimetype XHTML certainty.
    pub xhtml: XhtmlStatus,
}

impl Default for LazyloadImagesConfig {
    fn default() -> Self {
        LazyloadImagesConfig {
            options: RewriteOptions::new(),
            supports_lazyload_images: true,
            is_xml_http_request: false,
            finder_availability: FinderAvailability::Disabled,
            critical_images: HashSet::new(),
            blank_image_url: String::new(),
            lazyload_script_flushed: false,
            hash_length: 0,
            signature_length: 0,
            debug: false,
            pedantic: false,
            xhtml: XhtmlStatus::IsNotXhtml,
        }
    }
}

/// Defers below-the-fold images (`LazyloadImagesFilter`).
pub struct LazyloadImagesFilter<N: UrlNamer> {
    config: LazyloadImagesConfig,
    namer: N,
    base: BaseTracker,
    /// Element whose subtree skips rewriting (noembed/marquee/hostile
    /// class).
    skip_rewrite: Option<NodeId>,
    /// Open `<head>` awaiting its main script.
    head: Option<NodeId>,
    /// Runtime script placed yet.
    main_script_inserted: bool,
    /// A slider script vetoed further rewrites.
    abort_rewrite: bool,
    /// The load-everything fallback went out.
    abort_script_inserted: bool,
    /// Images rewritten since the last override script.
    num_lazy: u32,
    /// Nested `<noscript>` depth.
    noscript_depth: u32,
    /// HTML status (`LogRewriterHtmlStatus`).
    html_status: Option<LazyloadHtmlStatus>,
    /// Per-image log records (`LogLazyloadFilter`).
    log_records: Vec<LazyloadLogRecord>,
}

impl<N: UrlNamer> LazyloadImagesFilter<N> {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: LazyloadImagesConfig, namer: N) -> Self {
        LazyloadImagesFilter {
            config,
            namer,
            base: BaseTracker::default(),
            skip_rewrite: None,
            head: None,
            main_script_inserted: false,
            abort_rewrite: false,
            abort_script_inserted: false,
            num_lazy: 0,
            noscript_depth: 0,
            html_status: None,
            log_records: Vec::new(),
        }
    }

    /// Returns the HTML status plus per-image log records; the log
    /// channel itself does not exist yet, so tests read the filter.
    #[must_use]
    pub fn log_status(&self) -> (Option<LazyloadHtmlStatus>, &[LazyloadLogRecord]) {
        (self.html_status, &self.log_records)
    }

    /// Installs the domain lawyer for proxy suffixing.
    pub fn set_lawyer(&mut self, lawyer: &DomainLawyer) {
        self.base.set_lawyer(lawyer);
    }

    /// Overrides the base-URL length cap (`max_url_size`).
    pub fn set_max_url_size(&mut self, max_url_size: i64) {
        self.base.set_max_url_size(max_url_size);
    }

    /// Whether the filter may run (`ShouldApply`).
    fn should_apply(&self) -> LazyloadHtmlStatus {
        if !self.config.supports_lazyload_images {
            LazyloadHtmlStatus::UserAgentNotSupported
        } else if self.config.is_xml_http_request {
            LazyloadHtmlStatus::Disabled
        } else if self.config.finder_availability == FinderAvailability::NoDataYet {
            // Pages awaiting critical-image data rewrite later, once
            // the data arrives.
            LazyloadHtmlStatus::Disabled
        } else {
            LazyloadHtmlStatus::Active
        }
    }

    /// Blank placeholder (`GetBlankImageSrc`): the option override
    /// wins, else the injected asset URL.
    fn blank_image_src(&self) -> String {
        let override_url = self.config.options.lazyload_images_blank_url();
        if override_url.is_empty() {
            self.config.blank_image_url.clone()
        } else {
            override_url
        }
    }

    /// Runtime script body (`GetLazyloadJsSnippet`): the asset plus
    /// its `lazyLoadInit` call (note the newlines).
    fn lazyload_js_snippet(&self) -> Vec<u8> {
        let mut js = get_asset(StaticAsset::LazyloadImages, self.config.debug).to_vec();
        js.extend_from_slice(b"\npagespeed.lazyLoadInit(");
        js.extend_from_slice(if self.config.options.lazyload_images_after_onload() {
            b"true"
        } else {
            b"false"
        });
        js.extend_from_slice(b", \"");
        js.extend_from_slice(self.blank_image_src().as_bytes());
        js.extend_from_slice(b"\");\n");
        js
    }

    /// Places the runtime script (`InsertLazyloadJsCode`): before
    /// `before`, or appended to the head when `None`. Marks the script
    /// inserted even when flushing or aborting skips the insert —
    /// except when the head refuses the child, which retries later.
    fn insert_main_script(&mut self, dom: &mut Dom, lexer: &Lexer, before: Option<NodeId>) {
        if !self.config.lazyload_script_flushed && (!self.abort_rewrite || self.num_lazy > 0) {
            let script = dom.new_element(before.or(self.head), b"script");
            if let Some(target) = before {
                dom.insert_node_before_node(target, script);
            } else if let Some(head) = self.head {
                if dom.can_append_child(head) {
                    dom.append_child(Some(head), script);
                } else {
                    self.main_script_inserted = false;
                    return;
                }
            }
            let js = self.lazyload_js_snippet();
            add_js_to_element(
                dom,
                lexer,
                script,
                &js,
                self.config.pedantic,
                self.config.xhtml,
            );
            dom.add_escaped_attribute(
                script,
                b"data-pagespeed-no-defer",
                None,
                crate::element::QuoteStyle::DoubleQuote,
            );
        }
        self.main_script_inserted = true;
    }

    /// Places an override script around a batch of rewrites
    /// (`InsertOverrideAttributesScript`): before scripts, appended to
    /// bodies. The type attribute is explicit (no doctype gate).
    fn insert_override_script(&mut self, dom: &mut Dom, id: NodeId, before_script: bool) {
        if self.num_lazy == 0 {
            return;
        }
        let script = dom.new_element(dom.arena_node(id).parent(), b"script");
        dom.add_attribute(script, b"type", b"text/javascript");
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        let code = dom.new_leaf(
            Some(script),
            LeafKind::Characters,
            OVERRIDE_ATTRIBUTE_FUNCTIONS.to_vec(),
        );
        if before_script {
            dom.insert_node_before_node(id, script);
        } else {
            dom.append_child(Some(id), script);
        }
        dom.append_child(Some(script), code);
        self.num_lazy = 0;
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

    /// Records a per-image outcome (`LogLazyloadFilter`).
    fn log(&mut self, status: LazyloadStatus, blacklisted: bool, critical: bool) {
        self.log_records.push(LazyloadLogRecord {
            status,
            blacklisted,
            critical,
        });
    }

    /// Decodes a rewritten image URL to its single original, if any.
    fn decode_single_url(&self, url: &str) -> Option<String> {
        let gurl = GoogleUrl::new(url);
        let decoded = decode_url(
            &gurl,
            &DecodeConfig {
                options: &self.config.options,
                namer: &self.namer,
                hash_length: self.config.hash_length,
                signature_length: self.config.signature_length,
            },
        )?;
        if decoded.len() == 1 {
            decoded.into_iter().next()
        } else {
            None
        }
    }
}

impl<N: UrlNamer> HtmlFilter for LazyloadImagesFilter<N> {
    fn name(&self) -> &'static str {
        "LazyloadImages"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        let status = self.should_apply();
        self.html_status = Some(status);
        status == LazyloadHtmlStatus::Active
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        self.base
            .reset(&String::from_utf8_lossy(lexer.document_url()));
        self.skip_rewrite = None;
        self.head = None;
        self.main_script_inserted = false;
        self.abort_rewrite = false;
        self.abort_script_inserted = false;
        self.num_lazy = 0;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        self.base.observe(dom, id);
        if self.noscript_depth > 0 {
            return;
        }
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
        if !self.main_script_inserted && self.head.is_none() {
            match dom.element_keyword(id) {
                Keyword::Html
                | Keyword::Link
                | Keyword::Meta
                | Keyword::Script
                | Keyword::Style => {}
                Keyword::Head => self.head = Some(id),
                _ => self.insert_main_script(dom, lexer, Some(id)),
            }
        }
        if self.skip_rewrite.is_none() {
            let keyword = dom.element_keyword(id);
            if keyword == Keyword::Noembed || keyword == Keyword::Marquee {
                self.skip_rewrite = Some(id);
                return;
            }
            if let Some(index) = dom.find_attribute(id, Keyword::Class) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                let class = decoded.unwrap_or_default();
                if !class.is_empty() {
                    let lowered = class.to_ascii_lowercase();
                    if let Ok(lowered) = std::str::from_utf8(&lowered) {
                        if !self
                            .config
                            .options
                            .is_lazyload_enabled_for_class_name(lowered)
                        {
                            self.skip_rewrite = Some(id);
                            return;
                        }
                    }
                }
            }
        }
        if dom.element_keyword(id) == Keyword::Script {
            // The jquery slider breaks under rewriting: veto the page.
            if let Some(index) = dom.find_attribute(id, Keyword::Src) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                let src = decoded.unwrap_or_default();
                if src.windows(JQUERY_SLIDER.len()).any(|w| w == JQUERY_SLIDER) {
                    self.abort_rewrite = true;
                    return;
                }
            }
            self.insert_override_script(dom, id, true);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
        if self.noscript_depth > 0 || self.skip_rewrite.is_some() {
            if self.skip_rewrite == Some(id) {
                self.skip_rewrite = None;
            }
            return;
        }
        if self.head == Some(id) {
            self.insert_main_script(dom, lexer, None);
            self.head = None;
        }
        if self.abort_rewrite {
            if !self.abort_script_inserted && self.main_script_inserted && self.num_lazy > 0 {
                // Rewrites already landed: load them all back now.
                let script = dom.new_element(dom.arena_node(id).parent(), b"script");
                dom.add_attribute(script, b"type", b"text/javascript");
                let code =
                    dom.new_leaf(Some(script), LeafKind::Characters, LOAD_ALL_IMAGES.to_vec());
                dom.insert_node_after_node(id, script);
                dom.append_child(Some(script), code);
                self.abort_script_inserted = true;
            }
            return;
        }
        if dom.element_keyword(id) == Keyword::Body {
            self.insert_override_script(dom, id, false);
            return;
        }
        // Only `<img>` rewrites (`<input>` onloads misfire).
        if !dom.is_rewritable(id) || dom.element_keyword(id) != Keyword::Img {
            return;
        }
        let Some(src_index) = dom.find_attribute(id, Keyword::Src) else {
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, src_index);
        let src = decoded.unwrap_or_default();
        if src.is_empty()
            || is_data_url(&src)
            || dom
                .find_attribute(id, Keyword::DataPagespeedNoDefer)
                .is_some()
            || dom.find_attribute(id, Keyword::PagespeedNoDefer).is_some()
        {
            return;
        }
        if !self.can_add_onload(dom, id)
            || dom
                .find_attribute(id, Keyword::DataPagespeedLazySrc)
                .is_some()
            || dom.find_attribute(id, Keyword::DataSrc).is_some()
        {
            self.log(LazyloadStatus::NotApplied, false, false);
            return;
        }
        // Decode rewritten URLs (single-original only; sprites pass through).
        let mut full_url = self
            .base
            .resolve(&String::from_utf8_lossy(&src))
            .spec()
            .to_string();
        if let Some(original) = self.decode_single_url(&full_url) {
            full_url = original;
        }
        let resolved = GoogleUrl::new(&full_url);
        if !resolved.is_valid() || full_url.is_empty() {
            return;
        }
        if !self
            .config
            .options
            .allow_resources()
            .is_match(&full_url, true)
        {
            self.log(LazyloadStatus::NotApplied, true, false);
            return;
        }
        if self.config.finder_availability == FinderAvailability::Available
            && self
                .config
                .critical_images
                .contains(crate::beacon_finder::url_hash(&full_url).as_str())
        {
            self.log(LazyloadStatus::NotApplied, false, true);
            return;
        }
        if !self.main_script_inserted {
            self.insert_main_script(dom, lexer, Some(id));
        }
        // Rename in place, then append the placeholder + handlers.
        dom.set_attribute_name(id, src_index, b"data-pagespeed-lazy-src");
        if let Some(set_index) = dom.find_attribute(id, Keyword::Srcset) {
            dom.set_attribute_name(id, set_index, b"data-pagespeed-lazy-srcset");
        }
        let blank = self.blank_image_src();
        dom.add_attribute(id, b"src", blank.as_bytes());
        self.log(LazyloadStatus::AppliedOk, false, false);
        dom.delete_attribute(id, Keyword::Onload);
        dom.add_attribute(id, b"onload", IMAGE_ONLOAD_CODE);
        dom.delete_attribute(id, Keyword::Onerror);
        let mut onerror = ONERROR_RESET.to_vec();
        onerror.extend_from_slice(IMAGE_ONLOAD_CODE);
        dom.add_attribute(id, b"onerror", &onerror);
        self.num_lazy += 1;
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // The C++ records script insertion in the DOM cohort; the
        // property-cache write has no home yet.
    }
}

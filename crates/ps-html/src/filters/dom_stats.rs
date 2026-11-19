//! DOM statistics (`DomStatsFilter`).
//!
//! Counts images (inlined vs external), external stylesheets, and scripts
//! as end tags stream by, plus how many external images the critical-images
//! finder calls critical. The finder is driver context: the injected set
//! plays the mock finder's role (`Available == kAvailable` with identity
//! keys), while no set means beacon data is unavailable and the critical
//! counter stays zero. Image URLs resolve against the filter's own
//! [`BaseTracker`](crate::base_tracker::BaseTracker).

use std::collections::HashSet;

use ps_http::data_url::is_data_url;
use ps_http::google_url::GoogleUrl;

use crate::base_tracker::BaseTracker;
use crate::css_tag_scanner::is_stylesheet_or_alternate;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::script_scanner::{ScriptClassification, ScriptTagScanner};

/// Counts DOM features (`DomStatsFilter`).
#[derive(Debug, Default)]
pub struct DomStatsFilter {
    base: BaseTracker,
    critical_images: Option<HashSet<String>>,
    num_img_tags: i32,
    num_inlined_img_tags: i32,
    num_external_css: i32,
    num_scripts: i32,
    num_critical_images_used: i32,
}

impl DomStatsFilter {
    /// Builds the counter (base resets from the document URL).
    #[must_use]
    pub fn new() -> Self {
        DomStatsFilter::default()
    }

    /// Installs the critical-image URL set (mock finder data).
    pub fn set_critical_images(&mut self, images: HashSet<String>) {
        self.critical_images = Some(images);
    }

    /// Images seen.
    #[must_use]
    pub fn num_img_tags(&self) -> i32 {
        self.num_img_tags
    }

    /// Images with `data:` sources.
    #[must_use]
    pub fn num_inlined_img_tags(&self) -> i32 {
        self.num_inlined_img_tags
    }

    /// Stylesheet links with an `href`.
    #[must_use]
    pub fn num_external_css(&self) -> i32 {
        self.num_external_css
    }

    /// JavaScript elements.
    #[must_use]
    pub fn num_scripts(&self) -> i32 {
        self.num_scripts
    }

    /// External images in the critical set.
    #[must_use]
    pub fn num_critical_images_used(&self) -> i32 {
        self.num_critical_images_used
    }
}

impl HtmlFilter for DomStatsFilter {
    fn name(&self) -> &'static str {
        "DomStats"
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        let url = String::from_utf8_lossy(lexer.document_url());
        self.base.reset(&url);
        self.num_img_tags = 0;
        self.num_inlined_img_tags = 0;
        self.num_external_css = 0;
        self.num_scripts = 0;
        self.num_critical_images_used = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.base.observe(dom, id);
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Img {
            self.num_img_tags += 1;
            if let Some(index) = dom.find_attribute(id, Keyword::Src) {
                let (decoded, _error) = dom.decoded_attribute(id, index);
                if let Some(src) = decoded {
                    if !src.is_empty() {
                        if is_data_url(&src) {
                            self.num_inlined_img_tags += 1;
                        } else if let Some(critical) = &self.critical_images {
                            let reference = String::from_utf8_lossy(&src);
                            let image = GoogleUrl::resolve(&self.base.base(), &reference);
                            if critical.contains(image.spec()) {
                                self.num_critical_images_used += 1;
                            }
                        }
                    }
                }
            }
        } else if dom.element_keyword(id) == Keyword::Link
            && dom.find_attribute(id, Keyword::Rel).is_some_and(|index| {
                let (decoded, _error) = dom.decoded_attribute(id, index);
                decoded.is_some_and(|rel| is_stylesheet_or_alternate(&rel))
            })
            && dom.find_attribute(id, Keyword::Href).is_some()
        {
            self.num_external_css += 1;
        } else if ScriptTagScanner::parse_script_element(dom, id).0
            == ScriptClassification::JavaScript
        {
            self.num_scripts += 1;
        }
    }
}

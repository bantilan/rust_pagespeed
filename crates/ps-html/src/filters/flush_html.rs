//! Flush heuristics (`FlushHtmlFilter`).
//!
//! Event listener that requests a mid-document flush once enough resource
//! references (10 per stylesheet/script, 2 per image; threshold 80) have
//! streamed past, so fetches start early.

use ps_config::options::{Category, RewriteOptions};

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::parser::Dom;
use crate::resource_tag_scanner;

type UrlValuedVec = Vec<(String, String, Category)>;

/// Score that triggers a flush (`kFlushScoreThreshold`).
const FLUSH_SCORE_THRESHOLD: i32 = 80;
/// Stylesheet score (`kFlushCssScore`).
const FLUSH_CSS_SCORE: i32 = 10;
/// Script score (`kFlushScriptScore`).
const FLUSH_SCRIPT_SCORE: i32 = 10;
/// Image score (`kFlushImageScore`).
const FLUSH_IMAGE_SCORE: i32 = 2;

/// Requests flushes after enough resource references.
#[derive(Debug)]
pub struct FlushHtmlFilter {
    url_valued: UrlValuedVec,
    score: i32,
}

impl FlushHtmlFilter {
    /// Builds the listener (snapshots URL-valued overrides).
    #[must_use]
    pub fn new(options: &RewriteOptions) -> Self {
        FlushHtmlFilter {
            url_valued: options.url_valued_attributes().to_vec(),
            score: 0,
        }
    }
}

impl HtmlFilter for FlushHtmlFilter {
    fn name(&self) -> &'static str {
        "FlushHtmlFilter"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.score = 0;
    }

    fn flush(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.score = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        for (_, category) in resource_tag_scanner::scan_element(dom, id, &self.url_valued) {
            match category {
                Category::Stylesheet => self.score += FLUSH_CSS_SCORE,
                Category::Script => self.score += FLUSH_SCRIPT_SCORE,
                Category::Image => self.score += FLUSH_IMAGE_SCORE,
                _ => {}
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if !resource_tag_scanner::scan_element(dom, id, &self.url_valued).is_empty()
            && self.score >= FLUSH_SCORE_THRESHOLD
        {
            self.score = 0;
            dom.request_flush();
        }
    }
}

//! View-source diagnostics (`DebugFilter`).
//!
//! Times parse/render/idle phases across flushes and appends `#`-prefixed
//! comments: one per flush plus an end-of-document summary listing active
//! filters and options, critical images, and dynamically disabled
//! filters. The clock, critical-image set, options strings, beacon flag,
//! and disabled list all come from the driver; literal-block insertions
//! buffer until the enclosing element closes.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;

use ps_http::google_url::GoogleUrl;

use crate::base_tracker::BaseTracker;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Unset stopwatch reading (`kTimeNotSet`).
const TIME_NOT_SET: i64 = -1;

/// One stopwatch (`DebugFilter::Event`).
#[derive(Debug, Clone, Copy)]
struct Event {
    start_us: i64,
    duration_us: i64,
    total_us: i64,
}

impl Event {
    fn new() -> Self {
        Event {
            start_us: TIME_NOT_SET,
            duration_us: 0,
            total_us: 0,
        }
    }

    fn clear(&mut self) {
        self.start_us = TIME_NOT_SET;
        self.duration_us = 0;
        self.total_us = 0;
    }

    fn start(&mut self, now_us: i64) {
        debug_assert_eq!(TIME_NOT_SET, self.start_us);
        self.start_us = now_us;
    }

    fn end(&mut self, now_us: i64) {
        debug_assert_ne!(TIME_NOT_SET, self.start_us);
        self.duration_us += now_us - self.start_us;
        self.start_us = TIME_NOT_SET;
    }

    fn add_to_total(&mut self) {
        debug_assert_eq!(TIME_NOT_SET, self.start_us);
        self.total_us += self.duration_us;
        self.duration_us = 0;
    }
}

/// Driver inputs for diagnostics.
#[derive(Debug, Clone, Default)]
pub struct DebugConfig {
    /// Mockable microsecond clock (`Timer::NowUs`).
    pub clock: Rc<RefCell<i64>>,
    /// Critical image URLs (finder data; none means no finder).
    pub critical_images: Option<HashSet<String>>,
    /// Enabled filters listing (`EnabledFiltersToString`).
    pub enabled_filters: String,
    /// Enabled options listing (`SafeEnabledOptionsToString`).
    pub enabled_options: String,
    /// Critical-images beacon active.
    pub beacon_enabled: bool,
    /// Dynamically disabled filters (parser-filled shared cell).
    pub disabled: Rc<RefCell<Vec<String>>>,
}

/// Appends timing diagnostics (`DebugFilter`).
#[derive(Debug)]
pub struct DebugFilter {
    config: DebugConfig,
    base: BaseTracker,
    num_flushes: i32,
    end_document_seen: bool,
    idle: Event,
    parse: Event,
    render: Event,
    start_doc_time_us: i64,
    flush_messages: Vec<u8>,
    critical_image_urls: BTreeSet<String>,
}

impl DebugFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: DebugConfig) -> Self {
        DebugFilter {
            config,
            base: BaseTracker::default(),
            num_flushes: 0,
            end_document_seen: false,
            idle: Event::new(),
            parse: Event::new(),
            render: Event::new(),
            start_doc_time_us: TIME_NOT_SET,
            flush_messages: Vec::new(),
            critical_image_urls: BTreeSet::new(),
        }
    }

    /// Formats one flush comment (`FormatFlushMessage`).
    #[must_use]
    pub fn format_flush_message(
        since_init_us: i64,
        parse_us: i64,
        render_us: i64,
        idle_us: i64,
    ) -> Vec<u8> {
        format!(
            "\n#Flush after     {since_init_us}us\n#Parse duration  {parse_us}us\n#Render duration {render_us}us\n#Idle duration   {idle_us}us\n"
        )
        .into_bytes()
    }

    /// Formats the end-of-document summary (`FormatEndDocumentMessage`).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn format_end_document_message(
        since_init_us: i64,
        parse_total_us: i64,
        render_total_us: i64,
        idle_total_us: i64,
        num_flushes: i32,
        beacon_enabled: bool,
        critical_image_urls: &BTreeSet<String>,
        disabled_filters: &[String],
    ) -> Vec<u8> {
        let mut out = format!(
            "\n#NumFlushes            {num_flushes}\n#EndDocument after     {since_init_us}us\n#Total Parse duration  {parse_total_us}us\n#Total Render duration {render_total_us}us\n#Total Idle duration   {idle_total_us}us\n"
        )
        .into_bytes();
        if beacon_enabled {
            if critical_image_urls.is_empty() {
                out.extend_from_slice(b"No critical images detected.\n");
            } else {
                out.extend_from_slice(b"Critical Images:\n\t");
                let mut first = true;
                for url in critical_image_urls {
                    if !first {
                        out.extend_from_slice(b"\n\t");
                    }
                    first = false;
                    out.extend_from_slice(url.as_bytes());
                }
                out.push(b'\n');
            }
        }
        if disabled_filters.is_empty() {
            out.extend_from_slice(b"No filters were disabled for this request.\n");
        } else {
            out.extend_from_slice(b"The following filters were disabled for this request:\n\t");
            let mut first = true;
            for entry in disabled_filters {
                if !first {
                    out.extend_from_slice(b"\n\t");
                }
                first = false;
                out.extend_from_slice(entry.as_bytes());
            }
            out.push(b'\n');
        }
        out
    }

    /// Lists active filters and options (`ListActiveFiltersAndOptions`).
    fn list_active_filters_and_options(&self) -> Vec<u8> {
        let mut out = b"\nmod_pagespeed on\nFilters:\n".to_vec();
        out.extend_from_slice(self.config.enabled_filters.as_bytes());
        out.extend_from_slice(b"\nOptions:\n");
        out.extend_from_slice(self.config.enabled_options.as_bytes());
        out
    }

    /// Current clock reading.
    fn now_us(&self) -> i64 {
        *self.config.clock.borrow()
    }
}

impl HtmlFilter for DebugFilter {
    fn name(&self) -> &'static str {
        "Debug"
    }

    fn init_parse(&mut self) {
        self.num_flushes = 0;
        self.end_document_seen = false;
        self.idle.clear();
        self.parse.clear();
        self.render.clear();
        self.start_doc_time_us = TIME_NOT_SET;
        self.flush_messages.clear();
        self.critical_image_urls.clear();
        self.config.disabled.borrow_mut().clear();
        self.start_doc_time_us = self.now_us();
        self.idle.start(self.start_doc_time_us);
    }

    fn start_parse(&mut self) {
        let now = self.now_us();
        self.idle.end(now);
        self.parse.start(now);
    }

    fn end_parse(&mut self) {
        let now = self.now_us();
        self.parse.end(now);
        self.idle.start(now);
    }

    fn start_render(&mut self) {
        let now = self.now_us();
        self.idle.end(now);
        self.render.start(now);
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        let url = String::from_utf8_lossy(lexer.document_url());
        self.base.reset(&url);
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.base.observe(dom, id);
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if !self.flush_messages.is_empty() {
            let pending = std::mem::take(&mut self.flush_messages);
            dom.insert_comment(&pending, lexer.parent());
        }
        if dom.element_keyword(id) != Keyword::Img {
            return;
        }
        // A lazy-loaded image reports its original source.
        let src = dom
            .find_attribute_by_name(id, b"data-pagespeed-lazy-src")
            .or_else(|| dom.find_attribute(id, Keyword::Src));
        let Some(index) = src else { return };
        let (decoded, _error) = dom.decoded_attribute(id, index);
        let decoded = decoded.unwrap_or_default();
        let reference = String::from_utf8_lossy(&decoded);
        let image = GoogleUrl::resolve(&self.base.base(), &reference);
        if let Some(critical) = &self.config.critical_images {
            if critical.contains(image.spec()) {
                self.critical_image_urls.insert(image.spec().to_string());
            }
        }
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // No idle.End here: this runs during rendering, when not idle.
        self.end_document_seen = true;
    }

    fn flush(&mut self, dom: &mut Dom, lexer: &Lexer) {
        let since_init_us = self.render.start_us - self.start_doc_time_us;
        let now_us = self.now_us();
        // Flush doubles as end-of-render (StartRender has no partner call).
        self.render.end(now_us);
        // Mid-document flushes report; a lone end-of-document flush only
        // feeds the summary below.
        if self.num_flushes > 0 || !self.end_document_seen {
            let message = Self::format_flush_message(
                since_init_us,
                self.parse.duration_us,
                self.render.duration_us,
                self.idle.duration_us,
            );
            // Literal blocks reject insertion; buffer until the close tag.
            if !dom.insert_comment(&message, lexer.parent()) {
                self.flush_messages.extend_from_slice(&message);
            }
        }
        self.parse.add_to_total();
        self.render.add_to_total();
        self.idle.add_to_total();
        if self.end_document_seen {
            let mut summary = self.list_active_filters_and_options();
            summary.extend_from_slice(&Self::format_end_document_message(
                since_init_us,
                self.parse.total_us,
                self.render.total_us,
                self.idle.total_us,
                self.num_flushes,
                self.config.beacon_enabled,
                &self.critical_image_urls,
                &self.config.disabled.borrow(),
            ));
            dom.insert_comment(&summary, lexer.parent());
        } else {
            // The end-of-document flush is automatic, not upstream.
            self.num_flushes += 1;
            self.idle.start(now_us);
        }
    }
}

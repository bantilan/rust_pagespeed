//! Rewrite driver: plan-driven filter execution (`RewriteDriver`).
//!
//! Builds a [`ChainPlan`](crate::chain::ChainPlan) from options, wires event
//! listeners, instantiates the implemented slots into the parser's early-pre,
//! pre, and post phases (plus the terminal writer), and runs the `FlushAsync`
//! sequence per flush, honoring mid-write flush requests. Async rewrite
//! initiation lands with the resource pipelines; unbuilt slots are reported
//! via [`Driver::skipped_slots`].

use std::cell::RefCell;
use std::rc::Rc;

use ps_config::options::RewriteOptions;

use crate::chain::{build_chain, ChainContext, ChainFilter, ChainPlan, Phase};
use crate::filters::{self, AmpDocumentFilter, FlushHtmlFilter};
use crate::parser::Parser;
use crate::writer::{ByteSink, WriterFilter};

/// Plan-built streaming rewriter over one parser instance.
pub struct Driver {
    parser: Parser,
    plan: ChainPlan,
    skipped: Vec<(Phase, ChainFilter)>,
    amp_verdict: Rc<RefCell<Option<bool>>>,
}

impl Driver {
    /// Builds the driver: plan, instantiate implemented slots, apply the
    /// size cap. The sink backs the terminal writer, which is only added
    /// when the plan enables it (as in C++ `SetWriter`).
    #[must_use]
    pub fn new(
        options: &RewriteOptions,
        ctx: &ChainContext,
        sink: impl ByteSink + 'static,
    ) -> Self {
        let plan = build_chain(options, ctx);
        let mut parser = Parser::new();
        let mut skipped = Vec::new();
        let amp_verdict = Rc::new(RefCell::new(None));
        for slot in &plan.listeners {
            match slot {
                ChainFilter::AmpDocument => {
                    let verdict = amp_verdict.clone();
                    parser.add_event_listener(Box::new(AmpDocumentFilter::new(move |is_amp| {
                        *verdict.borrow_mut() = Some(is_amp);
                    })));
                }
                ChainFilter::FlushHtml => {
                    parser.add_event_listener(Box::new(FlushHtmlFilter::new(options)));
                }
                _ => skipped.push((Phase::Listener, *slot)),
            }
        }
        for (phase, slots) in [
            (Phase::EarlyPreRender, &plan.early_pre),
            (Phase::PreRender, &plan.pre),
            (Phase::PostRender, &plan.post),
        ] {
            for slot in slots {
                // The terminal writer is constructed from the sink below.
                if *slot == ChainFilter::HtmlWriter {
                    continue;
                }
                match filters::instantiate(*slot) {
                    Some(filter) => match phase {
                        Phase::EarlyPreRender => parser.add_early_pre_render_filter(filter),
                        Phase::PreRender => parser.add_pre_render_filter(filter),
                        _ => parser.add_filter(filter),
                    },
                    None => skipped.push((phase, *slot)),
                }
            }
        }
        if plan.post.contains(&ChainFilter::HtmlWriter) {
            parser.add_filter(Box::new(WriterFilter::new(sink)));
        }
        if let Some(limit) = plan.size_limit {
            parser.set_size_limit(limit);
        }
        Driver {
            parser,
            plan,
            skipped,
            amp_verdict,
        }
    }

    /// The built chain plan.
    #[must_use]
    pub fn plan(&self) -> &ChainPlan {
        &self.plan
    }

    /// Planned slots without a live filter (unported, or needing driver
    /// channels), in phase order.
    #[must_use]
    pub fn skipped_slots(&self) -> &[(Phase, ChainFilter)] {
        &self.skipped
    }

    /// Starts a document (see [`Parser::start_parse`]).
    pub fn start_parse(&mut self, url: &[u8], content_is_xml_like: bool) {
        self.parser.start_parse(url, content_is_xml_like);
    }

    /// AMP verdict once decided (`is_amp_document`; `None` undecided).
    #[must_use]
    pub fn is_amp_document(&self) -> Option<bool> {
        *self.amp_verdict.borrow()
    }

    /// Feeds bytes, flushing when a listener requested it
    /// (`ExecuteFlushIfRequested`).
    pub fn write(&mut self, bytes: &[u8]) {
        self.parser.write(bytes);
        if self.parser.flush_requested() {
            self.parser.flush();
        }
    }

    /// Runs the flush sequence (see [`Parser::flush`]).
    pub fn flush(&mut self) {
        self.parser.flush();
    }

    /// Ends input (see [`Parser::finish`]).
    pub fn finish(&mut self) {
        self.parser.finish();
    }

    /// Whether the size cap tripped (see [`Parser::size_limit_exceeded`]).
    #[must_use]
    pub fn size_limit_exceeded(&self) -> bool {
        self.parser.size_limit_exceeded()
    }

    /// Collected parse messages (see [`Parser::messages`]).
    #[must_use]
    pub fn messages(&self) -> &[crate::parser::Message] {
        self.parser.messages()
    }
}

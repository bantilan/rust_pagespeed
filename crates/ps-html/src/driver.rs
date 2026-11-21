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
use crate::server_context::ResourceDriver;
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
    /// `resource_driver` instantiates the driver-bound CSS/JS
    /// filters (`None` keeps the inline minify subsets).
    #[must_use]
    pub fn new(
        options: &RewriteOptions,
        ctx: &ChainContext,
        sink: impl ByteSink + 'static,
        resource_driver: Option<Rc<ResourceDriver>>,
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
                // Driver-bound filters replace the minify subsets
                // once a resource driver exists (external
                // resources need fetch machinery).
                let bound: Option<Box<dyn crate::filter::HtmlFilter>> = match slot {
                    ChainFilter::CssFilter => resource_driver.clone().map(|driver| {
                        Box::new(filters::css::CssFilter::new(driver))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::Javascript => resource_driver.clone().map(|driver| {
                        Box::new(filters::JavascriptFilter::new(driver))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::CssCombine => resource_driver.clone().map(|driver| {
                        Box::new(filters::css_combine::CssCombineFilter::new(driver))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::CssInline => resource_driver.clone().map(|driver| {
                        Box::new(filters::CssInlineFilter::new(driver))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::JsCombine => resource_driver.clone().map(|driver| {
                        Box::new(filters::js_combine::JsCombineFilter::new(driver))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::JsInline => resource_driver.clone().map(|driver| {
                        Box::new(filters::JsInlineFilter::new(driver))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::CacheExtender => resource_driver.clone().map(|driver| {
                        Box::new(filters::CacheExtenderFilter::new(driver))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::ImageRewrite => resource_driver.clone().map(|driver| {
                        Box::new(filters::ImageRewriteFilter::new(driver))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::CriticalImagesBeacon => resource_driver.clone().map(|driver| {
                        Box::new(filters::critical_images_beacon::CriticalImagesBeaconFilter::new(
                            driver,
                        )) as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::AddHead => Some(Box::new(filters::AddHeadFilter::new(
                        options
                            .filters()
                            .enabled(ps_config::filter::Filter::CombineHeads),
                    ))
                        as Box<dyn crate::filter::HtmlFilter>),
                    ChainFilter::CssInlineImportToLink => resource_driver.clone().map(|driver| {
                        use crate::server_context::XhtmlStatus as DriverXhtml;
                        let xhtml = match driver.mime_type_xhtml_status() {
                            DriverXhtml::IsXhtml => crate::filters::common::XhtmlStatus::IsXhtml,
                            DriverXhtml::IsNotXhtml => {
                                crate::filters::common::XhtmlStatus::IsNotXhtml
                            }
                            DriverXhtml::Unknown => crate::filters::common::XhtmlStatus::Unknown,
                        };
                        Box::new(filters::CssInlineImportToLinkFilter::new(
                            filters::CssInlineImportToLinkConfig { xhtml },
                        )) as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::StripSubresourceHints => {
                        resource_driver.clone().map(|driver| {
                            // `can_modify_urls` aggregates the
                            // chain in C++; here any planned
                            // URL-rewriting slot implies it.
                            let rewrites_urls = plan.pre.iter().chain(plan.post.iter()).any(|s| {
                                matches!(
                                    s,
                                    ChainFilter::CssFilter
                                        | ChainFilter::Javascript
                                        | ChainFilter::ImageRewrite
                                        | ChainFilter::DomainRewrite
                                        | ChainFilter::CacheExtender
                                )
                            });
                            let lawyer = std::rc::Rc::new(
                                crate::domain_lawyer::DomainLawyer::clone(driver.lawyer()),
                            );
                            Box::new(filters::StripSubresourceHintsFilter::new(
                                filters::StripSubresourceHintsConfig {
                                    can_modify_urls: rewrites_urls,
                                    css_preserve_urls: options.filters().css_preserve_urls(),
                                    js_preserve_urls: options.filters().js_preserve_urls(),
                                    image_preserve_urls: options.filters().image_preserve_urls(),
                                    allow_resources: options.allow_resources().clone(),
                                },
                                &lawyer,
                            )) as Box<dyn crate::filter::HtmlFilter>
                        })
                    }
                    ChainFilter::MetaTag => resource_driver.clone().map(|driver| {
                        Box::new(filters::MetaTagFilter::new(
                            &driver.response_headers_shared(),
                        )) as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::SupportNoscript => resource_driver.clone().map(|driver| {
                        let mut properties = driver.request_properties();
                        let should_insert = filters::should_insert_noscript(
                            options.filters(),
                            properties.supports_js_defer(
                                options.enable_aggressive_rewriters_for_mobile(),
                            ),
                            properties.supports_image_inlining(),
                        );
                        let with_param = driver
                            .base_url()
                            .copy_and_add_query_param("PageSpeed", "noscript");
                        let mut escaped = Vec::new();
                        crate::keywords::escape(with_param.spec().as_bytes(), &mut escaped);
                        Box::new(filters::SupportNoscriptFilter::new(&escaped, should_insert))
                            as Box<dyn crate::filter::HtmlFilter>
                    }),
                    _ => None,
                };
                match bound.or_else(|| filters::instantiate(*slot)) {
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

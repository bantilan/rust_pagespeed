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
        // One flush-early page state shared by its filters, like C++
        // `driver()->flush_early_info()`.
        let flush_early_info = Rc::new(RefCell::new(
            crate::flush_early::FlushEarlyInfo::default(),
        ));
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
                    ChainFilter::PrioritizeLcpImages => resource_driver.clone().map(|driver| {
                        Box::new(filters::prioritize_lcp_images::PrioritizeLcpImagesFilter::new(
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
                        let mut filter = filters::CssInlineImportToLinkFilter::new(
                            filters::CssInlineImportToLinkConfig { xhtml },
                        );
                        filter.init_stats(driver.server_context().statistics());
                        Box::new(filter) as Box<dyn crate::filter::HtmlFilter>
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
                            let mut filter = filters::StripSubresourceHintsFilter::new(
                                filters::StripSubresourceHintsConfig {
                                    can_modify_urls: rewrites_urls,
                                    css_preserve_urls: options.filters().css_preserve_urls(),
                                    js_preserve_urls: options.filters().js_preserve_urls(),
                                    image_preserve_urls: options.filters().image_preserve_urls(),
                                    allow_resources: options.allow_resources().clone(),
                                },
                                &lawyer,
                            );
                            filter.set_max_url_size(options.max_url_size());
                            Box::new(filter) as Box<dyn crate::filter::HtmlFilter>
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
                    ChainFilter::MakeShowAdsAsync => {
                        let mut filter = filters::MakeShowAdsAsyncFilter::new();
                        if let Some(driver) = resource_driver.clone() {
                            filter.init_stats(driver.server_context().statistics());
                        }
                        Some(Box::new(filter) as Box<dyn crate::filter::HtmlFilter>)
                    }
                    ChainFilter::GoogleAnalytics => {
                        let mut filter = filters::GoogleAnalyticsFilter::new();
                        if let Some(driver) = resource_driver.clone() {
                            filter.init_stats(driver.server_context().statistics());
                        }
                        Some(Box::new(filter) as Box<dyn crate::filter::HtmlFilter>)
                    }
                    ChainFilter::UrlLeftTrim => {
                        let mut filter = filters::UrlLeftTrimFilter::new(
                            options.url_valued_attributes().to_vec(),
                        );
                        filter.set_max_url_size(options.max_url_size());
                        if let Some(driver) = resource_driver.clone() {
                            filter.init_stats(driver.server_context().statistics());
                            filter.set_lawyer(driver.lawyer());
                        }
                        Some(Box::new(filter) as Box<dyn crate::filter::HtmlFilter>)
                    }
                    ChainFilter::InsertDnsPrefetch => resource_driver.clone().map(|driver| {
                        let properties = driver.request_properties();
                        let mut filter = filters::InsertDnsPrefetchFilter::new(
                            filters::InsertDnsPrefetchConfig {
                                options: options.clone(),
                                supports_dns_prefetch: properties.supports_dns_prefetch(),
                                use_rel_prefetch: properties
                                    .supports_dns_prefetch_using_rel_prefetch(),
                                url_valued: options.url_valued_attributes().to_vec(),
                                flush_early_info: flush_early_info.clone(),
                            },
                        );
                        filter.set_lawyer(driver.lawyer());
                        filter.set_max_url_size(options.max_url_size());
                        Box::new(filter) as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::LazyloadImages => resource_driver.clone().map(|driver| {
                        use crate::server_context::XhtmlStatus as DriverXhtml;
                        let mut properties = driver.request_properties();
                        let xhtml = match driver.mime_type_xhtml_status() {
                            DriverXhtml::IsXhtml => crate::filters::common::XhtmlStatus::IsXhtml,
                            DriverXhtml::IsNotXhtml => {
                                crate::filters::common::XhtmlStatus::IsNotXhtml
                            }
                            DriverXhtml::Unknown => crate::filters::common::XhtmlStatus::Unknown,
                        };
                        let (finder_availability, critical_images) =
                            Self::critical_images_for(&driver, options);
                        let mut filter = filters::LazyloadImagesFilter::new(
                            filters::LazyloadImagesConfig {
                                options: options.clone(),
                                supports_lazyload_images: properties.supports_lazyload_images(),
                                is_xml_http_request: driver
                                    .request_headers()
                                    .is_xml_http_request(),
                                finder_availability,
                                critical_images,
                                blank_image_url: filters::lazyload_images::BLANK_IMAGE_DATA_URL
                                    .to_string(),
                                lazyload_script_flushed: false,
                                hash_length: driver
                                    .server_context()
                                    .hasher()
                                    .hash_size_in_chars(),
                                signature_length: driver.signature_length(),
                                debug: options
                                    .filters()
                                    .enabled(ps_config::filter::Filter::Debug),
                                pedantic: options
                                    .filters()
                                    .enabled(ps_config::filter::Filter::Pedantic),
                                xhtml,
                            },
                            driver.server_context().url_namer().clone(),
                        );
                        filter.set_lawyer(driver.lawyer());
                        filter.set_max_url_size(options.max_url_size());
                        Box::new(filter) as Box<dyn crate::filter::HtmlFilter>
                    }),
                    ChainFilter::DomainRewrite => resource_driver.clone().map(|driver| {
                        use crate::server_context::XhtmlStatus as DriverXhtml;
                        let xhtml = match driver.mime_type_xhtml_status() {
                            DriverXhtml::IsXhtml => crate::filters::common::XhtmlStatus::IsXhtml,
                            DriverXhtml::IsNotXhtml => {
                                crate::filters::common::XhtmlStatus::IsNotXhtml
                            }
                            DriverXhtml::Unknown => crate::filters::common::XhtmlStatus::Unknown,
                        };
                        // The AMP verdict resolves during parsing, after
                        // construction; the client shim additionally
                        // requires ClientDomainRewrite plus rewritten-to
                        // domains, so non-AMP documents behave exactly
                        // like C++.
                        let lawyer = std::rc::Rc::new(
                            crate::domain_lawyer::DomainLawyer::clone(driver.lawyer()),
                        );
                        let mut filter = filters::DomainRewriteFilter::new(
                            filters::DomainRewriteConfig {
                                options: options.clone(),
                                disable_domain_rewrite: false,
                                is_amp_document: false,
                                debug: options
                                    .filters()
                                    .enabled(ps_config::filter::Filter::Debug),
                                pedantic: options
                                    .filters()
                                    .enabled(ps_config::filter::Filter::Pedantic),
                                xhtml,
                                hash_length: driver
                                    .server_context()
                                    .hasher()
                                    .hash_size_in_chars(),
                                signature_length: driver.signature_length(),
                                url_valued: options.url_valued_attributes().to_vec(),
                                response_headers: Some(driver.response_headers_shared()),
                            },
                            driver.server_context().url_namer().clone(),
                            &lawyer,
                        );
                        filter.init_stats(driver.server_context().statistics());
                        filter.set_max_url_size(options.max_url_size());
                        Box::new(filter) as Box<dyn crate::filter::HtmlFilter>
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

    /// Resolves lazyload critical-image inputs (`Availability` +
    /// `GetHtmlCriticalImages`): beaconing off means every image is
    /// a candidate, learned evidence yields the critical key hashes,
    /// and anything else stands the filter down until data arrives.
    fn critical_images_for(
        driver: &ResourceDriver,
        options: &RewriteOptions,
    ) -> (
        filters::lazyload_images::FinderAvailability,
        std::collections::HashSet<String>,
    ) {
        use filters::lazyload_images::FinderAvailability;
        if !crate::beacon_finder::beaconing_active(options) {
            return (FinderAvailability::Disabled, std::collections::HashSet::new());
        }
        let server = driver.server_context();
        let options_hash = server.hasher().hash(options.signature().as_bytes());
        let properties = driver.request_properties();
        let device_suffix =
            ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
                properties.device_type(),
            );
        let finder = crate::beacon_finder::BeaconFinder::new(server.metadata_cache());
        let lookup = finder.load(
            driver.base_url().spec(),
            &options_hash,
            device_suffix,
        );
        match lookup.record() {
            Some(record) if record.valid_beacons >= 1 => (
                FinderAvailability::Available,
                crate::beacon_finder::html_critical_images(record),
            ),
            _ => (FinderAvailability::NoDataYet, std::collections::HashSet::new()),
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

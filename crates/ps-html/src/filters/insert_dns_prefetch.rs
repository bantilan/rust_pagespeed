//! DNS prefetch injection (`InsertDnsPrefetchFilter`).
//!
//! Collects the domains of resources seen in the body, stores them in
//! the shared [`FlushEarlyInfo`](crate::flush_early::FlushEarlyInfo)
//! at end of document, and — once the eligible-domain count is stable
//! across rewrites — appends `<link rel=dns-prefetch href=//domain>`
//! tags to the first head from the previously stored list.
//!
//! NOTE: the C++ `EndElementImpl` returns early for unsupported user
//! agents *before* clearing the in-head flag, so with such agents
//! every body domain lands in the ignore set and nothing is ever
//! stored (proven with gdb against the C++ test binary —
//! `DisableInsertDnsPrefetchForUserAgentsNotSupported` reads
//! `(0, 0, 0)`). The early return below preserves that quirk.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use ps_config::filter::Filter;
use ps_config::options::{Category, RewriteOptions};

use super::url_left_trim::UrlValuedAttributes;
use crate::base_tracker::BaseTracker;
use crate::domain_lawyer::DomainLawyer;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::flush_early::FlushEarlyInfo;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::resource_tag_scanner::scan_element;

/// Maximum DNS prefetch tags stored/inserted (`kMaxDnsPrefetchTags`).
const MAX_DNS_PREFETCH_TAGS: usize = 8;

/// Stability tolerance across rewrites (`kMaxDomainDiff`).
const MAX_DOMAIN_DIFF: i32 = 2;

/// `rel` for IE9-class agents (`kRelPrefetch`).
const REL_PREFETCH: &[u8] = b"prefetch";

/// `rel` for modern agents (`kRelDnsPrefetch`).
const REL_DNS_PREFETCH: &[u8] = b"dns-prefetch";

/// Shared flush-early info, one per page (the driver's
/// `flush_early_info()`, shared across rewrites in tests).
pub type SharedFlushEarlyInfo = Rc<RefCell<FlushEarlyInfo>>;

/// HTML application status (`RewriterHtmlApplication::Status`, partial).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsPrefetchHtmlStatus {
    /// The filter ran.
    Active,
    /// The user agent cannot use DNS prefetch.
    UserAgentNotSupported,
}

/// Driver inputs for DNS prefetch injection.
#[derive(Debug, Clone)]
pub struct InsertDnsPrefetchConfig {
    /// Rewrite options (flush-subresources flag, URL-valued attributes).
    pub options: RewriteOptions,
    /// Whether the user agent supports DNS prefetch
    /// (`SupportsDnsPrefetch`).
    pub supports_dns_prefetch: bool,
    /// Whether to use `rel=prefetch` instead of `rel=dns-prefetch`
    /// (`SupportsDnsPrefetchUsingRelPrefetch`, IE9).
    pub use_rel_prefetch: bool,
    /// Operator URL-valued attributes.
    pub url_valued: UrlValuedAttributes,
    /// Flush-early info shared with the driver.
    pub flush_early_info: SharedFlushEarlyInfo,
}

impl Default for InsertDnsPrefetchConfig {
    fn default() -> Self {
        InsertDnsPrefetchConfig {
            options: RewriteOptions::new(),
            supports_dns_prefetch: true,
            use_rel_prefetch: false,
            url_valued: Vec::new(),
            flush_early_info: Rc::new(RefCell::new(FlushEarlyInfo::default())),
        }
    }
}

/// Injects DNS prefetch tags (`InsertDnsPrefetchFilter`).
pub struct InsertDnsPrefetchFilter {
    config: InsertDnsPrefetchConfig,
    base: BaseTracker,
    /// Tags inserted yet (first head only).
    inserted: bool,
    /// Currently inside `<head>`.
    in_head: bool,
    /// Domains never to prefetch (page host plus head resources).
    to_ignore: HashSet<String>,
    /// New body domains seen this document.
    in_body: HashSet<String>,
    /// Eligible domains in first-seen order.
    prefetch_domains: Vec<String>,
    /// Nested `<noscript>` depth (resources inside are skipped).
    noscript_depth: u32,
    /// Last HTML status logged (`LogRewriterHtmlStatus`).
    html_status: Option<DnsPrefetchHtmlStatus>,
    /// `APPLIED_OK` log count (one per inserted tag).
    applied_ok: u32,
    /// `NOT_APPLIED` log count (one per unstable head).
    not_applied: u32,
}

impl InsertDnsPrefetchFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: InsertDnsPrefetchConfig) -> Self {
        InsertDnsPrefetchFilter {
            config,
            base: BaseTracker::default(),
            inserted: false,
            in_head: false,
            to_ignore: HashSet::new(),
            in_body: HashSet::new(),
            prefetch_domains: Vec::new(),
            noscript_depth: 0,
            html_status: None,
            applied_ok: 0,
            not_applied: 0,
        }
    }

    /// Installs the domain lawyer for proxy suffixing.
    pub fn set_lawyer(&mut self, lawyer: &DomainLawyer) {
        self.base.set_lawyer(lawyer);
    }

    /// Overrides the base-URL length cap (`max_url_size`).
    pub fn set_max_url_size(&mut self, max_url_size: i64) {
        self.base.set_max_url_size(max_url_size);
    }

    /// Returns the recorded log statuses (html status plus
    /// `APPLIED_OK`/`NOT_APPLIED` counts); the log channel itself does
    /// not exist yet, so tests read the filter instead.
    #[must_use]
    pub fn log_status(&self) -> (Option<DnsPrefetchHtmlStatus>, u32, u32) {
        (self.html_status, self.applied_ok, self.not_applied)
    }

    /// Files a resource URL's domain for potential prefetching
    /// (`MarkAlreadyInHead`).
    fn mark_already_in_head(&mut self, dom: &mut Dom, id: NodeId, index: usize) {
        let (decoded, _) = dom.decoded_attribute(id, index);
        let Some(value) = decoded else {
            return;
        };
        let Ok(value) = std::str::from_utf8(&value) else {
            return;
        };
        let url = self.base.resolve(value);
        if !url.is_web_valid() {
            return;
        }
        let domain = url.host().to_string();
        if domain.is_empty() {
            return;
        }
        if self.in_head {
            let fresh = self.to_ignore.insert(domain.clone());
            if fresh
                && self
                    .config
                    .options
                    .filters()
                    .enabled(Filter::FlushSubresources)
            {
                // With flush subresources, head domains prefetch too.
                self.prefetch_domains.push(domain);
            }
        } else if !self.to_ignore.contains(&domain) {
            let fresh = self.in_body.insert(domain.clone());
            if fresh {
                self.prefetch_domains.push(domain);
            }
        }
    }

    /// Whether a link's `rel` makes its prefetch category eligible: any
    /// `prefetch`, or `dns-prefetch` seen inside head (whole-value,
    /// case-insensitive, like `StringCaseEqual`).
    fn prefetch_rel_eligible(&self, dom: &mut Dom, id: NodeId) -> bool {
        let Some(index) = dom.find_attribute(id, Keyword::Rel) else {
            return false;
        };
        let (decoded, _) = dom.decoded_attribute(id, index);
        let Some(rel) = decoded else {
            return false;
        };
        rel.eq_ignore_ascii_case(REL_PREFETCH)
            || (self.in_head && rel.eq_ignore_ascii_case(REL_DNS_PREFETCH))
    }
}

impl HtmlFilter for InsertDnsPrefetchFilter {
    fn name(&self) -> &'static str {
        "InsertDnsPrefetch"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        // Always enabled; the C++ also sets the driver's
        // write-property-cache-dom-cohort flag, which has no home yet.
        true
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        self.base
            .reset(&String::from_utf8_lossy(lexer.document_url()));
        self.inserted = false;
        self.in_head = false;
        self.to_ignore.clear();
        self.in_body.clear();
        self.prefetch_domains.clear();
        self.noscript_depth = 0;
        // Never prefetch the page's own host.
        self.to_ignore.insert(self.base.base().host().to_string());
        self.html_status = Some(if self.config.supports_dns_prefetch {
            DnsPrefetchHtmlStatus::Active
        } else {
            DnsPrefetchHtmlStatus::UserAgentNotSupported
        });
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.base.observe(dom, id);
        // The head check runs before the noscript check, exactly like
        // the C++: a head inside noscript still flips the flag.
        if dom.element_keyword(id) == Keyword::Head {
            self.in_head = true;
            return;
        }
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
        if self.noscript_depth > 0 {
            return;
        }
        for (index, category) in scan_element(dom, id, &self.config.url_valued) {
            match category {
                Category::Image
                | Category::Script
                | Category::Stylesheet
                | Category::OtherResource => {
                    self.mark_already_in_head(dom, id, index);
                }
                Category::Prefetch => {
                    if dom.element_keyword(id) == Keyword::Link
                        && self.prefetch_rel_eligible(dom, id)
                    {
                        self.mark_already_in_head(dom, id, index);
                    }
                }
                Category::Hyperlink => {}
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        // The UA gate stays first: with unsupported agents the in-head
        // flag is never cleared and nothing is ever stored (see the
        // module docs). Noscript depth still unwinds below it, matching
        // `CommonFilter::EndElement`, which runs regardless.
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
        if !self.config.supports_dns_prefetch {
            return;
        }
        if dom.element_keyword(id) != Keyword::Head {
            return;
        }
        self.in_head = false;
        if self.inserted {
            return;
        }
        self.inserted = true;
        let info = self.config.flush_early_info.borrow();
        if info.is_domain_list_stable(MAX_DOMAIN_DIFF) {
            let rel = if self.config.use_rel_prefetch {
                REL_PREFETCH
            } else {
                REL_DNS_PREFETCH
            };
            let domains: Vec<String> = info.dns_prefetch_domains().to_vec();
            drop(info);
            for domain in &domains {
                let link = dom.new_element(Some(id), b"link");
                dom.add_attribute(link, b"rel", rel);
                dom.add_attribute(link, b"href", format!("//{domain}").as_bytes());
                dom.append_child(Some(id), link);
                self.applied_ok += 1;
            }
        } else {
            drop(info);
            self.not_applied += 1;
        }
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.config
            .flush_early_info
            .borrow_mut()
            .store_dns_prefetch_domains(&self.prefetch_domains, MAX_DNS_PREFETCH_TAGS);
    }
}

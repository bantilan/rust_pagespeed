//! Per-filter base-URL tracking (`RewriteDriver::base_url` + `ScanFilter`).
//!
//! The C++ driver tracks one mutable base URL: it starts at the document
//! URL and the first `<base href>` re-points it (resolved against the
//! current base; undecodable hrefs are ignored). Upstream that state flows
//! through the driver's event-major fan-out (`ScanFilter` always runs
//! first), but filter-major engines — C++ `HtmlParse` and this parser
//! alike — run each filter over the whole window, so no shared cell can
//! stay event-ordered across filters. Each URL-resolving filter therefore
//! owns a [`BaseTracker`] fed from its own event stream, which observes
//! identical bases whenever no earlier filter deletes or rewrites base
//! tags (inserted base tags latch first-wins here exactly as `ScanFilter`
//! latches onto `BaseTagFilter`'s insertion upstream).
//!
//! One `ScanFilter` guard is deliberately unported: CSP `base-uri`
//! conservatism. Upstream poisons the base gate whenever the response
//! carries a `base-uri` directive (it cannot prove the base safe); the
//! port has no CSP parsing or enforcement subsystem, so it resolves
//! bases normally. Impact is limited to conservatism parity: rewriting
//! still only touches same-origin, explicitly allowed URLs, and no
//! security boundary depends on the gate. Do not add ad hoc CSP
//! matching here; port the subsystem if the behavior is ever needed.
//!
//! A shared driver-level scan filter will replace per-filter tracking
//! once live chains exist.

use ps_http::google_url::GoogleUrl;

use crate::domain_lawyer::DomainLawyer;
use crate::element::NodeId;
use crate::name::Keyword;
use crate::parser::Dom;

/// Default URL length cap (C++ `kDefaultMaxUrlSize`).
const DEFAULT_MAX_URL_SIZE: i64 = 2083;

/// Live base URL for one filter (`ScanFilter`'s base half).
#[derive(Debug, Clone)]
pub struct BaseTracker {
    base: GoogleUrl,
    document_url: String,
    lawyer: Option<DomainLawyer>,
    base_was_set: bool,
    base_problem: bool,
    max_url_size: i64,
}

impl Default for BaseTracker {
    fn default() -> Self {
        BaseTracker {
            base: GoogleUrl::default(),
            document_url: String::new(),
            lawyer: None,
            base_was_set: false,
            base_problem: false,
            max_url_size: DEFAULT_MAX_URL_SIZE,
        }
    }
}

impl BaseTracker {
    /// Reads the current base URL.
    #[must_use]
    pub fn base(&self) -> GoogleUrl {
        self.base.clone()
    }

    /// Whether URLs may resolve against the base
    /// (`CommonFilter::BaseUrlIsValid`, reduced). Upstream the gate is
    /// `!refs_before_base && !problem` before the base tag and `seen_base
    /// && !problem` after; the refs flag is only raised at base time and
    /// every filter observes the base event, so both reduce to "no base
    /// problem". (The `DomainRewrite` comment claims pre-base elements
    /// skip; the code resolves them against the document URL instead, and
    /// no upstream test covers a pre-base same-origin URL either way.)
    #[must_use]
    pub fn base_url_is_valid(&self) -> bool {
        !self.base_problem
    }

    /// Resolves a URL reference (`CommonFilter::ResolveUrl`): against the
    /// base when the gate holds and the base is web-fetchable, as an
    /// absolute URL when the gate fails, else invalid. Empty input stays
    /// invalid.
    #[must_use]
    pub fn resolve(&self, input: &str) -> GoogleUrl {
        if input.is_empty() {
            return GoogleUrl::new("");
        }
        if !self.base_url_is_valid() {
            return GoogleUrl::new(input);
        }
        if self.base.is_web_valid() {
            return GoogleUrl::resolve(&self.base, input);
        }
        GoogleUrl::new("")
    }

    /// Installs the domain lawyer for proxy suffixing (absent by
    /// default, matching an empty suffix).
    pub fn set_lawyer(&mut self, lawyer: &DomainLawyer) {
        self.lawyer = Some(lawyer.clone());
    }

    /// Overrides the URL length cap (`max_url_size`, default 2083).
    pub fn set_max_url_size(&mut self, max_url_size: i64) {
        self.max_url_size = max_url_size;
    }

    /// Resets to the document URL at parse start.
    pub fn reset(&mut self, document_url: &str) {
        self.base = GoogleUrl::new(document_url);
        self.document_url = document_url.to_string();
        self.base_was_set = false;
        self.base_problem = false;
    }

    /// Adopts a base element's href unless one already won
    /// (`SetBaseUrlIfUnset`); relative hrefs resolve against the current
    /// base and invalid results are ignored so a later base tag may still
    /// win, but an undecodable href poisons the gate for the whole
    /// document (`other_base_problem`). No-op for non-base elements.
    pub fn observe(&mut self, dom: &mut Dom, id: NodeId) {
        if dom.element_keyword(id) != Keyword::Base {
            return;
        }
        let Some(index) = dom.find_attribute(id, Keyword::Href) else {
            return;
        };
        let (decoded, _error) = dom.decoded_attribute(id, index);
        let Some(href) = decoded else {
            self.base_problem = true;
            return;
        };
        let Ok(href) = std::str::from_utf8(&href) else {
            self.base_problem = true;
            return;
        };
        // Overlong bases are never adopted (C++ compares the decoded
        // length against `max_url_size`; equality still adopts).
        if href.len() as i64 > self.max_url_size {
            self.base_problem = true;
            return;
        }
        // Proxy deployments suffix the href against the document URL
        // before adopting it.
        let mut href = href.to_string();
        if let Some(lawyer) = &self.lawyer {
            let document = GoogleUrl::new(&self.document_url);
            if let Some(suffixed) = lawyer.add_proxy_suffix(&document, &href) {
                href = suffixed;
            }
        }
        let candidate = GoogleUrl::resolve(&self.base, &href);
        if candidate.is_valid() && !self.base_was_set {
            self.base_was_set = true;
            self.base = candidate;
        }
    }

    /// Pins a base href to the adopted base (`ScanFilter::EndElement`'s
    /// base half): with a proxy suffix configured, the tag always shows
    /// the adopted base. No-op without a lawyer or suffix.
    pub fn finish_base_element(&mut self, dom: &mut Dom, id: NodeId) {
        if dom.element_keyword(id) != Keyword::Base {
            return;
        }
        let Some(lawyer) = &self.lawyer else {
            return;
        };
        if lawyer.proxy_suffix().is_empty() {
            return;
        }
        if let Some(index) = dom.find_attribute(id, Keyword::Href) {
            let pinned = self.base.all_except_query().as_bytes().to_vec();
            dom.element_attributes_mut(id)[index].set_value(&pinned);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_matrix() {
        let mut tracker = BaseTracker::default();
        tracker.reset("http://test.com/doc.html");
        assert!(tracker.base_url_is_valid());
        // Healthy web base resolves relatively.
        assert_eq!(tracker.resolve("a/b").spec(), "http://test.com/a/b");
        assert_eq!(
            tracker.resolve("http://other.com/x").spec(),
            "http://other.com/x"
        );
        // Empty input stays invalid.
        assert!(!tracker.resolve("").is_valid());
        // A non-web base resolves nothing (gate holds, base unusable).
        tracker.reset("ftp://what.the/heck/");
        assert!(tracker.base_url_is_valid());
        assert!(!tracker.resolve("a/b").is_valid());
    }

    #[test]
    fn observe_first_wins_and_problems_poison() {
        use crate::parser::Dom;

        let mut tracker = BaseTracker::default();
        tracker.reset("http://test.com/doc.html");
        let mut dom = Dom::new();
        let base = dom.new_element(None, b"base");
        dom.add_attribute(base, b"href", b"/directory/");
        tracker.observe(&mut dom, base);
        assert_eq!(tracker.base().spec(), "http://test.com/directory/");
        assert!(tracker.base_url_is_valid());
        // A second base tag loses.
        let late = dom.new_element(None, b"base");
        dom.add_attribute(late, b"href", b"http://other.com/");
        tracker.observe(&mut dom, late);
        assert_eq!(tracker.base().spec(), "http://test.com/directory/");

        // An undecodable href poisons the gate document-wide.
        use crate::element::QuoteStyle;

        let mut tracker = BaseTracker::default();
        tracker.reset("http://test.com/doc.html");
        let bad = dom.new_element(None, b"base");
        dom.add_escaped_attribute(
            bad,
            b"href",
            Some(b"http://foo.bar/&lang;".as_slice()),
            QuoteStyle::SingleQuote,
        );
        tracker.observe(&mut dom, bad);
        assert!(!tracker.base_url_is_valid());
        // ... so resolution falls back to absolute interpretation.
        assert_eq!(
            tracker.resolve("http://other.com/x").spec(),
            "http://other.com/x"
        );
        assert_eq!(tracker.resolve("a/b").spec(), "");
        // A later valid base still adopts, but the gate stays shut.
        let late = dom.new_element(None, b"base");
        dom.add_attribute(late, b"href", b"http://foo.bar/");
        tracker.observe(&mut dom, late);
        assert_eq!(tracker.base().spec(), "http://foo.bar/");
        assert!(!tracker.base_url_is_valid());
    }

    #[test]
    fn max_url_size_boundary() {
        use crate::parser::Dom;

        // Default cap is 2083; equality still adopts.
        for (length, adopts) in [(2082, true), (2083, true), (2084, false)] {
            let mut tracker = BaseTracker::default();
            tracker.reset("http://test.com/doc.html");
            let mut dom = Dom::new();
            let base = dom.new_element(None, b"base");
            let href = format!("http://example.com/{}", "a".repeat(length - 19));
            assert_eq!(href.len(), length);
            dom.add_attribute(base, b"href", href.as_bytes());
            tracker.observe(&mut dom, base);
            assert_eq!(tracker.base_url_is_valid(), adopts, "length {length}");
            if adopts {
                assert_eq!(tracker.base().spec(), href);
            } else {
                assert_eq!(tracker.base().spec(), "http://test.com/doc.html");
            }
        }
        // Overrides apply the same boundary.
        for (length, adopts) in [(10, true), (11, false)] {
            let mut tracker = BaseTracker::default();
            tracker.set_max_url_size(10);
            tracker.reset("http://test.com/doc.html");
            let mut dom = Dom::new();
            let base = dom.new_element(None, b"base");
            let href = "http://z/".to_string() + &"a".repeat(length - 9);
            dom.add_attribute(base, b"href", href.as_bytes());
            tracker.observe(&mut dom, base);
            assert_eq!(tracker.base_url_is_valid(), adopts, "length {length}");
        }
    }

    #[test]
    fn lawyer_suffixes_adopted_bases() {
        use crate::parser::Dom;

        let mut lawyer = DomainLawyer::new();
        lawyer.set_proxy_suffix(".sfx");
        let mut tracker = BaseTracker::default();
        tracker.set_lawyer(&lawyer);
        tracker.reset("http://example.com.sfx/page.html");
        let mut dom = Dom::new();
        let base = dom.new_element(None, b"base");
        dom.add_attribute(base, b"href", b"http://example.com/dir/");
        tracker.observe(&mut dom, base);
        assert!(tracker.base_url_is_valid());
        assert_eq!(tracker.base().spec(), "http://example.com.sfx/dir/");
        // Without a lawyer the href adopts verbatim.
        let mut plain = BaseTracker::default();
        plain.reset("http://example.com.sfx/page.html");
        let mut dom = Dom::new();
        let base = dom.new_element(None, b"base");
        dom.add_attribute(base, b"href", b"http://example.com/dir/");
        plain.observe(&mut dom, base);
        assert_eq!(plain.base().spec(), "http://example.com/dir/");
    }
}

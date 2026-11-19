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
//! TODO(step4): the remaining `ScanFilter` guards — CSP `base-uri`
//! conservatism and `max_url_size` — need options/CSP plumbing that has
//! not landed yet. Trackers without a lawyer also skip proxy suffixing
//! (equivalent to an empty suffix); the driver's shared scan filter will
//! replace per-filter tracking once live chains exist.

use std::rc::Rc;

use ps_http::google_url::GoogleUrl;

use crate::domain_lawyer::DomainLawyer;
use crate::element::NodeId;
use crate::name::Keyword;
use crate::parser::Dom;

/// Live base URL for one filter (`ScanFilter`'s base half).
#[derive(Debug, Clone, Default)]
pub struct BaseTracker {
    base: GoogleUrl,
    document_url: String,
    lawyer: Option<Rc<DomainLawyer>>,
    base_was_set: bool,
    base_problem: bool,
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
    pub fn set_lawyer(&mut self, lawyer: &Rc<DomainLawyer>) {
        self.lawyer = Some(lawyer.clone());
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
}

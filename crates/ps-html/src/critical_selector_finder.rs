//! Critical-selector lookup (`CriticalSelectorFinder`).
//!
//! Maps a page URL to the beaconed critical-selector set. The
//! beacon-backed finder waits for the property-cache/beacon
//! subsystem; filters read whatever is installed, and tests
//! install the fake below.

use std::cell::RefCell;
use std::collections::HashSet;

/// Critical-selector source (`CriticalSelectorFinder`).
pub trait CriticalSelectorFinder {
    /// Beaconed critical selectors for a page, empty when unknown
    /// (`GetCriticalSelectors`).
    fn get_critical_selectors(&self, page_url: &str) -> HashSet<String>;
}

/// Canned selector sets for tests.
#[derive(Debug, Default)]
pub struct FakeCriticalSelectorFinder {
    selectors: RefCell<HashSet<String>>,
}

impl FakeCriticalSelectorFinder {
    /// Builds a finder serving `selectors` for every page.
    #[must_use]
    pub fn with_selectors(selectors: &[&str]) -> Self {
        FakeCriticalSelectorFinder {
            selectors: RefCell::new(selectors.iter().map(ToString::to_string).collect()),
        }
    }

    /// Replaces the canned set (later beacon rounds).
    pub fn set_selectors(&self, selectors: &[&str]) {
        *self.selectors.borrow_mut() = selectors.iter().map(ToString::to_string).collect();
    }
}

impl CriticalSelectorFinder for FakeCriticalSelectorFinder {
    fn get_critical_selectors(&self, _page_url: &str) -> HashSet<String> {
        self.selectors.borrow().clone()
    }
}

/// Snapshot the effective selector input and its unambiguous canonical identity.
/// No finder means full CSS is retained, matching C++ DetermineEnabled.
pub fn snapshot(driver: &crate::server_context::ResourceDriver) -> (HashSet<String>, Vec<u8>) {
    let selectors = driver
        .server_context()
        .critical_selector_finder()
        .map(|finder| finder.get_critical_selectors(driver.base_url().spec()))
        .unwrap_or_default();
    let mut sorted: Vec<&str> = selectors.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut identity = b"critical-css-selectors/1\n".to_vec();
    for selector in sorted {
        identity.extend_from_slice(selector.len().to_string().as_bytes());
        identity.push(b':');
        identity.extend_from_slice(selector.as_bytes());
    }
    identity.extend_from_slice(b"\nblocking:\n");
    identity.extend_from_slice(&crate::critical_css_blocking::snapshot(driver));
    (selectors, identity)
}

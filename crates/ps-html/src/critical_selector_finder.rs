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

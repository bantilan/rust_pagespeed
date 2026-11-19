//! URL combination partnerships (`UrlPartnership`).
//!
//! Decides which resource URLs may combine (same mapped origin,
//! optionally same path) and tracks their greatest common path
//! prefix plus per-URL relative remainders for multipart naming.
//!
//! NOTE: log messages wait for the message handler.

use ps_http::google_url::GoogleUrl;

use super::domain_lawyer::DomainLawyer;
use super::url_namer::UrlNamer;
use crate::server_context::ResourceDriver;

/// URL combination partnership (`UrlPartnership`).
pub struct UrlPartnership<'a> {
    driver: &'a ResourceDriver,
    urls: Vec<GoogleUrl>,
    domain_and_path_prefix: String,
    original_origin_and_path: GoogleUrl,
    common_components: Vec<String>,
}

impl<'a> UrlPartnership<'a> {
    /// Borrows the driver for options, namer, and lawyer.
    #[must_use]
    pub fn new(driver: &'a ResourceDriver) -> Self {
        UrlPartnership {
            driver,
            urls: Vec::new(),
            domain_and_path_prefix: String::new(),
            original_origin_and_path: GoogleUrl::new(""),
            common_components: Vec::new(),
        }
    }

    /// Adds a URL when legally combinable (`AddUrl`).
    pub fn add_url(&mut self, untrimmed_resource_url: &str) -> bool {
        let resource_url = untrimmed_resource_url.trim().to_string();
        if resource_url.is_empty() || !self.original_origin_and_path.is_web_valid() {
            return false;
        }
        let resolved = GoogleUrl::resolve(&self.original_origin_and_path, &resource_url);
        if !resolved.is_web_valid() {
            return false;
        }
        if !self.driver.options().is_allowed(resolved.spec()) {
            return false;
        }
        let (mut mapped, mut mapped_domain) = (resolved, String::new());
        if !Self::find_resource_domain(
            &self.original_origin_and_path,
            &**self.driver.server_context().url_namer(),
            self.driver.lawyer(),
            &mut mapped,
            &mut mapped_domain,
        ) {
            return false;
        }
        let mut added = false;
        if self.urls.is_empty() {
            self.domain_and_path_prefix = mapped_domain;
            added = true;
        } else {
            let domain_url = GoogleUrl::new(&self.domain_and_path_prefix);
            let mapped_url = GoogleUrl::new(&mapped_domain);
            if domain_url.origin() == mapped_url.origin() {
                added = true;
                if !self.driver.options().combine_across_paths()
                    && self.resolved_base() != mapped.all_except_leaf()
                {
                    added = false;
                }
            }
        }
        if added {
            self.urls.push(mapped);
            self.incremental_resolve(self.urls.len() - 1);
        }
        added
    }

    /// Finds the mapped domain for a resource, adjusting it to the
    /// served form (`FindResourceDomain`).
    pub fn find_resource_domain(
        base_url: &GoogleUrl,
        namer: &dyn UrlNamer,
        lawyer: &DomainLawyer,
        resource: &mut GoogleUrl,
        domain: &mut String,
    ) -> bool {
        if let Some(decoded) = namer.decode(resource) {
            *resource = GoogleUrl::new(&decoded);
            if !resource.is_web_valid() {
                return false;
            }
            *domain = resource.origin().to_string();
            true
        } else if let Some((mapped_domain, mapped)) =
            lawyer.map_request_to_domain(base_url, resource.spec())
        {
            *domain = mapped_domain;
            *resource = mapped;
            true
        } else {
            false
        }
    }

    /// Removes the last added URL (`RemoveLast`).
    pub fn remove_last(&mut self) {
        assert!(!self.urls.is_empty());
        self.urls.pop();
        self.common_components.clear();
        for index in 0..self.urls.len() {
            self.incremental_resolve(index);
        }
    }

    /// Resets around a new original request (`Reset`).
    pub fn reset(&mut self, original_request: &GoogleUrl) {
        self.urls.clear();
        self.common_components.clear();
        if original_request.is_web_valid() {
            self.original_origin_and_path = GoogleUrl::new(original_request.all_except_leaf());
        }
    }

    /// Number of added URLs (`num_urls`).
    #[must_use]
    pub fn num_urls(&self) -> usize {
        self.urls.len()
    }

    /// Number of common path components (`NumCommonComponents`).
    #[must_use]
    pub fn num_common_components(&self) -> usize {
        self.common_components.len()
    }

    /// Common base of all URLs, trailing slash included
    /// (`ResolvedBase`).
    #[must_use]
    pub fn resolved_base(&self) -> String {
        let mut base = String::new();
        for component in &self.common_components {
            base.push_str(component);
            base.push('/');
        }
        base
    }

    /// URL remainder past the common base (`RelativePath`).
    #[must_use]
    pub fn relative_path(&self, index: usize) -> String {
        let base = self.resolved_base();
        let spec = self.urls[index].spec();
        assert!(spec.len() >= base.len());
        assert_eq!(&spec[..base.len()], base.as_str());
        spec[base.len()..].to_string()
    }

    /// Full resolved URL (`FullPath`).
    #[must_use]
    pub fn full_path(&self, index: usize) -> &GoogleUrl {
        &self.urls[index]
    }

    /// Extends the common prefix with one URL (`IncrementalResolve`).
    /// Empty segments are kept (`http://x` must not alias `/http:/x`).
    fn incremental_resolve(&mut self, index: usize) {
        assert!(index < self.urls.len());
        let base = self.urls[index].all_except_leaf().to_string();
        let mut components: Vec<String> = base.split('/').map(str::to_string).collect();
        assert!(!components.is_empty());
        components.pop();
        assert!(components.len() >= 3);
        if index == 0 {
            self.common_components = components;
        } else {
            if components.len() < self.common_components.len() {
                self.common_components.truncate(components.len());
            }
            let mut common = self.common_components.len();
            for (position, component) in components.iter().enumerate() {
                if position >= common {
                    break;
                }
                if self.common_components[position] != *component {
                    common = position;
                    break;
                }
            }
            self.common_components.truncate(common);
        }
    }
}

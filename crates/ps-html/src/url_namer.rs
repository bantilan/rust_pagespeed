//! URL naming policy (`UrlNamer` + `TestUrlNamer`).
//!
//! The naming interface isolates how rewritten-resource URLs map back to
//! originals. The base trait keeps the C++ defaults (`Decode` and
//! `IsProxyEncoded` refuse); `TestUrlNamer` ports the path-encoding test
//! policy (`http://cdn.com/<scheme>/<host>/...`) used across the filter
//! suites.

use std::rc::Rc;

use ps_cache::hash::hash_string_case_preserve;
use ps_cache::namer::ResourceNamer;
use ps_http::google_url::GoogleUrl;

use super::domain_lawyer::DomainLawyer;
use super::output_resource::OutputResource;

/// Proxy-domain policy (`UrlNamer::ProxyExtent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyExtent {
    /// No proxy domain.
    #[default]
    None,
    /// Proxy URLs accepted but never produced.
    InputOnly,
    /// All resources move to the proxy domain.
    Full,
}

/// Sharding choice for encoding (`EncodeOption`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeOption {
    /// Shard the domain by content hash.
    Sharded,
    /// Keep the domain as-is.
    Unsharded,
}

/// URL naming policy (`UrlNamer`).
pub trait UrlNamer {
    /// Recovers the original URL (`Decode`); `None` was not encoded here.
    fn decode(&self, _request_url: &GoogleUrl) -> Option<String> {
        None
    }

    /// Whether the URL sits on the proxy domain (`IsProxyEncoded`).
    fn is_proxy_encoded(&self, _url: &GoogleUrl) -> bool {
        false
    }

    /// Proxy-domain policy (`ProxyMode`).
    fn proxy_mode(&self) -> ProxyExtent {
        ProxyExtent::None
    }

    /// Names an output resource (`Encode`): the encoded leaf on the
    /// (optionally sharded) resolved base. The lawyer arrives
    /// separately because options cannot own it (dependency cycle).
    fn encode(
        &self,
        lawyer: &DomainLawyer,
        resource: &OutputResource,
        option: EncodeOption,
    ) -> String {
        base_encode(lawyer, resource, option)
    }
}

/// Shared base naming (`UrlNamer::Encode`).
fn base_encode(lawyer: &DomainLawyer, resource: &OutputResource, option: EncodeOption) -> String {
    let leaf = resource.full_name().encode();
    let base = GoogleUrl::new(resource.resolved_base());
    let hash = hash_string_case_preserve(resource.full_name().hash().as_bytes());
    let domain = format!("{}/", base.origin());
    let path = if option == EncodeOption::Sharded {
        match lawyer.shard_domain(&domain, hash) {
            Some(sharded) => {
                let rest = base.path_and_leaf();
                format!("{sharded}{}", &rest[1.min(rest.len())..])
            }
            None => resource.resolved_base().to_string(),
        }
    } else {
        resource.resolved_base().to_string()
    };
    format!("{path}{leaf}")
}

/// Forwards to the installed namer, so driver-owned filters can
/// hold the shared policy object instead of a concrete namer.
impl UrlNamer for Rc<dyn UrlNamer> {
    fn decode(&self, request_url: &GoogleUrl) -> Option<String> {
        (**self).decode(request_url)
    }

    fn is_proxy_encoded(&self, url: &GoogleUrl) -> bool {
        (**self).is_proxy_encoded(url)
    }

    fn proxy_mode(&self) -> ProxyExtent {
        (**self).proxy_mode()
    }

    fn encode(
        &self,
        lawyer: &DomainLawyer,
        resource: &OutputResource,
        option: EncodeOption,
    ) -> String {
        (**self).encode(lawyer, resource, option)
    }
}

/// Production namer (`StandardUrlNamer`): base behavior throughout.
#[derive(Debug, Clone, Copy, Default)]
pub struct StandardUrlNamer;

impl UrlNamer for StandardUrlNamer {}

/// Path-encoding test namer (`TestUrlNamer`).
///
/// The C++ keeps the mode flags in globals reset by construction; this
/// port holds them per instance instead (no shared test state).
#[derive(Debug, Clone, Copy, Default)]
pub struct TestUrlNamer {
    proxy_mode: ProxyExtent,
    use_normal_encoding: bool,
}

impl TestUrlNamer {
    /// Test proxy domain (`kTopDomain`).
    pub const TOP_DOMAIN: &'static str = "http://cdn.com";

    /// Builds the namer (modes reset, like the C++ constructor).
    #[must_use]
    pub fn new() -> Self {
        TestUrlNamer::default()
    }

    /// Sets the proxy mode (`SetProxyMode`).
    pub fn set_proxy_mode(&mut self, mode: ProxyExtent) {
        self.proxy_mode = mode;
    }

    /// Selects normal (non-proxy) encoding (`UseNormalEncoding`).
    pub fn set_use_normal_encoding(&mut self, normal: bool) {
        self.use_normal_encoding = normal;
    }

    /// Whether the origin is the test proxy domain (`IsOriginEncoded`).
    #[must_use]
    pub fn is_origin_encoded(url: &GoogleUrl) -> bool {
        url.origin() == Self::TOP_DOMAIN
    }

    /// Whether the path carries an encoded origin (`IsPathEncoded`):
    /// `<scheme>/<host>/<scheme>/<host>/...` with http(s) schemes.
    #[must_use]
    pub fn is_path_encoded(url: &GoogleUrl) -> bool {
        let parts: Vec<&str> = url.path_sans_leaf().split('/').collect();
        if parts.len() < 5 {
            return false;
        }
        let is_web_scheme = |part: &str| part == "http" || part == "https";
        is_web_scheme(parts[1]) && is_web_scheme(parts[3])
    }

    /// Encodes a resource URL from its parts (`EncodeUrl`, static half).
    #[must_use]
    pub fn encode_url(
        original_base: &str,
        unmapped_base: &str,
        resolved_path: &str,
        leaf: &ResourceNamer,
    ) -> String {
        let original = GoogleUrl::new(original_base);
        let unmapped = GoogleUrl::new(unmapped_base);
        format!(
            "{}/{}/{}/{}/{}{}{}",
            Self::TOP_DOMAIN,
            original.scheme(),
            original.host_and_port(),
            unmapped.scheme(),
            unmapped.host_and_port(),
            resolved_path,
            leaf.encode(),
        )
    }
}

impl UrlNamer for TestUrlNamer {
    fn decode(&self, request_url: &GoogleUrl) -> Option<String> {
        if !Self::is_path_encoded(request_url) {
            return None;
        }
        let parts: Vec<&str> = request_url.path_sans_leaf().split('/').collect();
        let mut decoded = format!("{}://{}", parts[3], parts[4]);
        for part in &parts[5..parts.len() - 1] {
            decoded.push('/');
            decoded.push_str(part);
        }
        decoded.push('/');
        decoded.push_str(request_url.leaf_with_query());
        Some(decoded)
    }

    fn is_proxy_encoded(&self, url: &GoogleUrl) -> bool {
        url.is_web_valid() && Self::is_origin_encoded(url)
    }

    fn proxy_mode(&self) -> ProxyExtent {
        self.proxy_mode
    }

    fn encode(
        &self,
        lawyer: &DomainLawyer,
        resource: &OutputResource,
        option: EncodeOption,
    ) -> String {
        let base = GoogleUrl::new(resource.resolved_base());
        if self.use_normal_encoding
            || lawyer.can_rewrite_domains()
            || (base.scheme() != "http" && base.scheme() != "https")
        {
            return base_encode(lawyer, resource, option);
        }
        if Self::is_path_encoded(&base) && Self::is_origin_encoded(&base) {
            // Already encoded (e.g. rebuilt from cache): do not nest.
            format!(
                "{}{}{}",
                Self::TOP_DOMAIN,
                base.path_sans_leaf(),
                resource.full_name().encode()
            )
        } else {
            Self::encode_url(
                resource.original_base(),
                resource.unmapped_base(),
                base.path_and_leaf(),
                resource.full_name(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: hand-derived from `test_url_namer.cc` (the upstream suite
    // needs output resources + domain lawyering); the pipeline phase
    // will exercise these through real encode/decode round trips.

    #[test]
    fn path_encoding_matrix() {
        assert!(TestUrlNamer::is_path_encoded(&GoogleUrl::new(
            "http://cdn.com/http/a.com/https/b.com/p/leaf.pagespeed.cf.0.css"
        )));
        // Short paths and non-web schemes are not encoded.
        assert!(!TestUrlNamer::is_path_encoded(&GoogleUrl::new(
            "http://cdn.com/http/a.com/leaf.css"
        )));
        assert!(!TestUrlNamer::is_path_encoded(&GoogleUrl::new(
            "http://cdn.com/ftp/a.com/https/b.com/p/leaf.css"
        )));
        assert!(!TestUrlNamer::is_path_encoded(&GoogleUrl::new(
            "http://cdn.com/http/a.com/ftp/b.com/p/leaf.css"
        )));
        assert!(!TestUrlNamer::is_path_encoded(&GoogleUrl::new(
            "http://test.com/a.css"
        )));
    }

    #[test]
    fn decode_reconstruction() {
        let namer = TestUrlNamer::new();
        assert_eq!(
            namer.decode(&GoogleUrl::new(
                "http://cdn.com/http/x.com/https/y.com/a/b/leaf.pagespeed.cf.0.css"
            )),
            Some("https://y.com/a/b/leaf.pagespeed.cf.0.css".to_string())
        );
        // Queries and fragments ride on the leaf.
        assert_eq!(
            namer.decode(&GoogleUrl::new(
                "http://cdn.com/http/x.com/https/y.com/a/leaf.css?x=1#frag"
            )),
            Some("https://y.com/a/leaf.css?x=1#frag".to_string())
        );
        // Unencoded URLs refuse.
        assert_eq!(namer.decode(&GoogleUrl::new("http://test.com/a.css")), None);
    }

    #[test]
    fn proxy_encoding_matrix() {
        let namer = TestUrlNamer::new();
        assert!(namer.is_proxy_encoded(&GoogleUrl::new("http://cdn.com/d.css.pagespeed.cf.0.css")));
        assert!(!namer.is_proxy_encoded(&GoogleUrl::new("http://d.com/c.js.pagespeed.jm.0.js")));
        assert!(!namer.is_proxy_encoded(&GoogleUrl::new("bogus")));
    }

    #[test]
    fn encode_url_assembly() {
        let mut leaf = ResourceNamer::new();
        leaf.set_name("n");
        leaf.set_id("cf");
        leaf.set_hash("HASH");
        leaf.set_ext("css");
        let url = TestUrlNamer::encode_url("http://a.com/x", "https://b.com/y", "/p/q/", &leaf);
        assert_eq!(
            url,
            "http://cdn.com/http/a.com/https/b.com/p/q/n.pagespeed.cf.HASH.css"
        );
        // The assembly decodes back to the unmapped base + path + leaf.
        let gurl = GoogleUrl::new(&url);
        assert!(TestUrlNamer::is_path_encoded(&gurl));
        assert_eq!(
            TestUrlNamer::new().decode(&gurl),
            Some("https://b.com/p/q/n.pagespeed.cf.HASH.css".to_string())
        );
    }
}

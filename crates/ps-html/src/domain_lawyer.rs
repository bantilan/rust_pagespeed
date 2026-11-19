//! Domain authorization, mapping, and sharding (`DomainLawyer`).
//!
//! Tracks known domains (exact and wildcarded), rewrite-domain mappings,
//! origin mappings, shards, and proxy domains; answers authorization,
//! mapping, and sharding queries for URLs. Domains live in an arena so
//! mappings stay index-stable (the C++ uses raw pointers); the registry
//! map is ordered like the C++ `std::map` so signatures are stable.
//!
//! TODO(pipeline): origin mapping (`MapOrigin*`, `AddOriginDomainMapping`),
//! proxy support (`AddProxyDomainMapping`, `IsProxyMapped`, suffix
//! helpers), `Merge`, and `DoDomainsServeSameContent` land with their
//! slices; the `Domain` record already carries their fields.

use std::collections::BTreeMap;

use ps_http::google_url::GoogleUrl;

use ps_config::message_handler::{MessageHandler, MessageLevel};
use ps_config::wildcard::Wildcard;

/// One tracked domain (`DomainLawyer::Domain`).
#[derive(Debug, Clone)]
struct Domain {
    /// Normalized name (`http://host[:port]/[path/]`).
    name: String,
    /// Wildcard matcher over the name.
    wildcard: Wildcard,
    /// Rewrite target, if mapped.
    rewrite: Option<usize>,
    /// Fetch origin, if mapped.
    origin: Option<usize>,
    /// Explicit host header for origin fetches.
    host_header: String,
    /// Shards of this domain.
    shards: Vec<usize>,
    /// Authorized for rewriting.
    authorized: bool,
    /// Cycle guard for origin merges.
    breadcrumb: bool,
    /// Proxy-domain participant.
    is_proxy: bool,
}

impl Domain {
    fn new(name: String) -> Self {
        Domain {
            wildcard: Wildcard::new(&name),
            name,
            rewrite: None,
            origin: None,
            host_header: String::new(),
            shards: Vec::new(),
            authorized: false,
            breadcrumb: false,
            is_proxy: false,
        }
    }

    fn is_wildcarded(&self) -> bool {
        !self.wildcard.is_simple()
    }
}

/// Which mapping a helper installs (`SetDomainFn`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapKind {
    Rewrite,
    ShardFrom,
    Origin,
    Proxy,
}

/// Domain authorization and mapping (`DomainLawyer`).
#[derive(Debug, Default)]
pub struct DomainLawyer {
    domains: Vec<Domain>,
    map: BTreeMap<String, usize>,
    wildcards: Vec<usize>,
    authorize_all: bool,
    can_rewrite_domains: bool,
    proxy_suffix: String,
}

/// Reborrowable handler reference (avoids moving the caller's option).
/// The `'static` object bound keeps reborrows short (`&mut` invariance
/// would otherwise stretch them over the whole call).
type Handler<'a> = Option<&'a mut (dyn MessageHandler + 'static)>;

impl DomainLawyer {
    /// Creates an empty lawyer (`Clear` state).
    #[must_use]
    pub fn new() -> Self {
        DomainLawyer::default()
    }

    /// Drops all domains and flags (`Clear`).
    pub fn clear(&mut self) {
        self.domains.clear();
        self.map.clear();
        self.wildcards.clear();
        self.authorize_all = false;
        self.can_rewrite_domains = false;
        self.proxy_suffix.clear();
    }

    /// Whether any resource might be domain-mapped: mappings, shards,
    /// or a proxy suffix.
    #[must_use]
    pub fn can_rewrite_domains(&self) -> bool {
        self.can_rewrite_domains || !self.proxy_suffix.is_empty()
    }

    /// Sets the proxy suffix (`set_proxy_suffix`).
    pub fn set_proxy_suffix(&mut self, suffix: &str) {
        self.proxy_suffix = suffix.to_string();
    }

    /// Reads the proxy suffix (`proxy_suffix`).
    #[must_use]
    pub fn proxy_suffix(&self) -> &str {
        &self.proxy_suffix
    }

    /// Registers and authorizes a domain (`AddDomain`).
    pub fn add_domain(&mut self, domain_name: &str, handler: Handler<'_>) -> bool {
        self.add_domain_helper(domain_name, true, true, false, handler)
            .is_some()
    }

    /// Registers a domain without authorizing it (`AddKnownDomain`).
    pub fn add_known_domain(&mut self, domain_name: &str, handler: Handler<'_>) -> bool {
        self.add_domain_helper(domain_name, false, false, false, handler)
            .is_some()
    }

    /// Normalizes a domain spec (`NormalizeDomainName`): defaults the
    /// scheme to http, ensures a trailing slash, lowercases through the
    /// host slash only, and strips redundant default ports.
    #[must_use]
    pub fn normalize_domain_name(domain_name: &str) -> String {
        let (mut normalized, scheme_end) = match domain_name.find("://") {
            Some(pos) => (domain_name.to_string(), pos),
            None => (format!("http://{domain_name}"), 4),
        };
        if !normalized.ends_with('/') {
            normalized.push('/');
        }
        let origin_start = scheme_end + 3;
        if let Some(slash) = normalized[origin_start..]
            .find('/')
            .map(|relative| origin_start + relative)
        {
            normalized[..slash].make_ascii_lowercase();
            let scheme = normalized[..scheme_end].to_string();
            let origin = normalized[origin_start..slash].to_string();
            if scheme == "https" && origin.ends_with(":443") {
                normalized.replace_range(slash - 4..slash, "");
            } else if scheme == "http" && origin.ends_with(":80") {
                normalized.replace_range(slash - 3..slash, "");
            }
        }
        normalized
    }

    /// Registers a domain, returning its index (`AddDomainHelper`).
    fn add_domain_helper(
        &mut self,
        domain_name: &str,
        warn_on_duplicate: bool,
        authorize: bool,
        is_proxy: bool,
        mut handler: Handler<'_>,
    ) -> Option<usize> {
        if domain_name.is_empty() {
            if let Some(handler) = handler.as_deref_mut() {
                handler.message(
                    MessageLevel::Warning,
                    "Empty domain passed to AddDomain".to_string(),
                );
            }
            return None;
        }
        if authorize && domain_name == "*" {
            self.authorize_all = true;
        }
        let normalized = Self::normalize_domain_name(domain_name);
        let index = if let Some(&index) = self.map.get(&normalized) {
            if warn_on_duplicate && authorize == self.domains[index].authorized {
                if let Some(handler) = handler.as_deref_mut() {
                    handler.message(
                        MessageLevel::Warning,
                        format!("AddDomain of domain already in map: {normalized}"),
                    );
                }
                return None;
            }
            index
        } else {
            let index = self.domains.len();
            let wildcarded = !Wildcard::new(&normalized).is_simple();
            self.domains.push(Domain::new(normalized.clone()));
            self.map.insert(normalized, index);
            if wildcarded {
                self.wildcards.push(index);
            }
            index
        };
        if authorize {
            self.domains[index].authorized = true;
        }
        if is_proxy {
            self.domains[index].is_proxy = true;
        }
        Some(index)
    }

    /// Finds the longest registered prefix of a URL (`FindDomain`):
    /// exact origin+path matches from longest to shortest, then
    /// wildcards in insertion order.
    fn find_domain(&self, url: &GoogleUrl) -> Option<usize> {
        let mut domain_path = url.all_except_leaf().to_string();
        let components: Vec<&str> = url.path_sans_leaf().split('/').collect();
        if components.len() >= 2
            && components[0].is_empty()
            && components[components.len() - 1].is_empty()
        {
            let mut shave = 0;
            let mut index = components.len() - 1;
            loop {
                domain_path.truncate(domain_path.len() - shave);
                debug_assert!(domain_path.ends_with('/'));
                if let Some(&found) = self.map.get(&domain_path) {
                    return Some(found);
                }
                if index == 1 {
                    break;
                }
                shave = components[index - 1].len() + 1;
                index -= 1;
            }
        }
        for &candidate in &self.wildcards {
            if self.domains[candidate].wildcard.matches(&domain_path) {
                return Some(candidate);
            }
        }
        None
    }

    /// Whether a resource URL may be rewritten in a document
    /// (`IsDomainAuthorized`): same origin always passes, otherwise the
    /// URL's domain must be registered and authorized.
    #[must_use]
    pub fn is_domain_authorized(
        &self,
        original_request: &GoogleUrl,
        domain_to_check: &GoogleUrl,
    ) -> bool {
        if self.authorize_all {
            return true;
        }
        if !domain_to_check.is_web_valid() {
            return false;
        }
        if original_request.is_web_valid() && original_request.origin() == domain_to_check.origin()
        {
            return true;
        }
        self.find_domain(domain_to_check)
            .is_some_and(|index| self.domains[index].authorized)
    }

    /// Whether any registration covers a URL (`IsOriginKnown`).
    #[must_use]
    pub fn is_origin_known(&self, domain_to_check: &GoogleUrl) -> bool {
        domain_to_check.is_web_valid() && self.find_domain(domain_to_check).is_some()
    }

    /// Maps a resource URL onto its rewrite domain (`MapRequestToDomain`).
    /// Returns the mapped domain name (always slash-terminated) plus the
    /// remapped request URL.
    #[must_use]
    pub fn map_request_to_domain(
        &self,
        original_request: &GoogleUrl,
        resource_url: &str,
    ) -> Option<(String, GoogleUrl)> {
        let resolved = GoogleUrl::resolve(original_request, resource_url);
        if !resolved.is_web_valid() {
            return None;
        }
        let resolved_domain = self.find_domain(&resolved);
        let original_origin = GoogleUrl::new(original_request.origin());
        let resolved_origin = GoogleUrl::new(resolved.origin());
        let mut mapped_domain_name;
        if resolved_origin == original_origin {
            mapped_domain_name = resolved_origin.spec().to_string();
            if !mapped_domain_name.ends_with('/') {
                // GURL canonicalization trails a slash here.
                mapped_domain_name.push('/');
            }
        } else if let Some(index) = resolved_domain {
            if !self.domains[index].authorized {
                return None;
            }
            if self.domains[index].is_wildcarded() {
                // Sharded domains map without rewriting here.
                mapped_domain_name = resolved_origin.spec().to_string();
                if !mapped_domain_name.ends_with('/') {
                    // GURL canonicalization trails a slash here.
                    mapped_domain_name.push('/');
                }
            } else {
                mapped_domain_name = self.domains[index].name.clone();
            }
        } else {
            return None;
        }
        if let Some(index) = resolved_domain {
            if let Some(rewrite) = self.domains[index].rewrite {
                debug_assert!(!self.domains[rewrite].is_wildcarded());
                debug_assert!(rewrite != index);
                mapped_domain_name = self.domains[rewrite].name.clone();
                let mapped = self.map_url_helper(index, rewrite, &resolved)?;
                return Some((mapped_domain_name, mapped));
            }
        }
        Some((mapped_domain_name, resolved))
    }

    /// Re-bases a URL from one domain onto another (`MapUrlHelper`).
    fn map_url_helper(&self, from: usize, to: usize, url: &GoogleUrl) -> Option<GoogleUrl> {
        debug_assert!(!self.domains[to].is_wildcarded());
        let from_gurl = GoogleUrl::new(&self.domains[from].name);
        let from_path = from_gurl.path_sans_leaf();
        let path_and_leaf = url.path_and_leaf();
        debug_assert!(path_and_leaf.starts_with(from_path));
        // The `./` prefix keeps `data:`-shaped remainders relative.
        let relative = format!(
            "./{}",
            &path_and_leaf[from_path.len().min(path_and_leaf.len())..]
        );
        debug_assert!(!GoogleUrl::new(&relative).is_web_valid());
        let to_gurl = GoogleUrl::new(&self.domains[to].name);
        let mapped = GoogleUrl::resolve(&to_gurl, &relative);
        if mapped.is_web_valid() {
            Some(mapped)
        } else {
            None
        }
    }

    /// Whether a mapping target uses a mappable scheme
    /// (`IsSchemeSafeToMapTo`): schemeless and http always, https only
    /// when allowed.
    fn is_scheme_safe_to_map_to(domain_name: &str, allow_https: bool) -> bool {
        !domain_name.contains("://")
            || domain_name.starts_with("http://")
            || (allow_https && domain_name.starts_with("https://"))
    }

    /// Installs one mapping (`MapDomainHelper`).
    #[allow(clippy::too_many_arguments)]
    fn map_domain_helper(
        &mut self,
        to_domain_name: &str,
        comma_separated_from_domains: &str,
        host_header: &str,
        kind: MapKind,
        allow_wildcards: bool,
        allow_map_to_https: bool,
        authorize_to_domain: bool,
        mut handler: Handler<'_>,
    ) -> bool {
        if !Self::is_scheme_safe_to_map_to(to_domain_name, allow_map_to_https) {
            return false;
        }
        // Register first without authorizing: a rejected mapping must not
        // leave an authorization behind.
        let to_domain =
            self.add_domain_helper(to_domain_name, false, false, false, handler.as_deref_mut());
        let Some(to_domain) = to_domain else {
            return false;
        };
        if self.domains[to_domain].is_wildcarded() {
            if let Some(handler) = handler.as_deref_mut() {
                handler.message(
                    MessageLevel::Error,
                    format!("Cannot map to a wildcarded domain: {to_domain_name}"),
                );
            }
            return false;
        }
        if authorize_to_domain {
            self.domains[to_domain].authorized = true;
        }
        let to_origin = GoogleUrl::new(&self.domains[to_domain].name)
            .origin()
            .to_string();
        let mut ret = true;
        let mut mapped_a_domain = false;
        for from_name in comma_separated_from_domains
            .split(',')
            .filter(|part| !part.is_empty())
        {
            let from_domain =
                self.add_domain_helper(from_name, false, false, false, handler.as_deref_mut());
            let Some(from_domain) = from_domain else {
                continue;
            };
            let from_origin = GoogleUrl::new(&self.domains[from_domain].name)
                .origin()
                .to_string();
            if to_origin == from_origin {
                // Same origin: authorize, don't map.
                self.domains[from_domain].authorized = true;
            } else if !allow_wildcards && self.domains[from_domain].is_wildcarded() {
                if let Some(handler) = handler.as_deref_mut() {
                    handler.message(
                        MessageLevel::Error,
                        format!("Cannot map from a wildcarded domain: {to_domain_name}"),
                    );
                }
                ret = false;
            } else {
                if from_name == "*" {
                    self.authorize_all = true;
                }
                self.domains[from_domain].authorized = true;
                let ok =
                    self.set_domain_mapped(from_domain, to_domain, kind, handler.as_deref_mut());
                ret &= ok;
                mapped_a_domain |= ok;
            }
        }
        debug_assert!(host_header.is_empty() || !self.domains[to_domain].is_proxy);
        if mapped_a_domain {
            self.domains[to_domain].host_header = host_header.to_string();
        }
        ret && mapped_a_domain
    }

    /// Applies one mapping edge (rewrite/origin/proxy/shard setters).
    fn set_domain_mapped(
        &mut self,
        from: usize,
        to: usize,
        kind: MapKind,
        mut handler: Handler<'_>,
    ) -> bool {
        match kind {
            MapKind::Rewrite => {
                if self.domains[from].rewrite == Some(to) {
                    return true;
                }
                if self.domains[from].is_proxy && self.domains[from].rewrite.is_some() {
                    if let Some(handler) = handler {
                        let current = self.domains[self.domains[from].rewrite.unwrap()]
                            .name
                            .clone();
                        handler.message(
                            MessageLevel::Error,
                            format!(
                                "ProxyDomain {} has conflicting proxies {current} and {}",
                                self.domains[from].name, self.domains[to].name
                            ),
                        );
                    }
                    return false;
                }
                self.domains[from].rewrite = Some(to);
                let origin = self.domains[from].origin;
                self.merge_origin(to, origin, handler);
                true
            }
            MapKind::Origin => {
                if self.domains[from].origin == Some(to) {
                    return true;
                }
                if self.domains[from].origin.is_some()
                    && (self.domains[self.domains[from].origin.unwrap()].is_proxy
                        || self.domains[to].is_proxy)
                {
                    if let Some(handler) = handler {
                        let current = self.domains[self.domains[from].origin.unwrap()]
                            .name
                            .clone();
                        handler.message(
                            MessageLevel::Error,
                            format!(
                                "ProxyDomain {} has conflicting origins {current} and {}",
                                self.domains[from].name, self.domains[to].name
                            ),
                        );
                    }
                    return false;
                }
                // Non-proxy origin conflicts are noisily overwritten by
                // MergeOrigin itself.
                let origin = Some(to);
                self.merge_origin(from, origin, handler.as_deref_mut());
                if let Some(rewrite) = self.domains[from].rewrite {
                    let origin = self.domains[from].origin;
                    self.merge_origin(rewrite, origin, handler);
                }
                true
            }
            MapKind::Proxy => {
                self.domains[to].is_proxy = true;
                if !self.set_domain_mapped(from, to, MapKind::Origin, handler.as_deref_mut()) {
                    return false;
                }
                self.set_domain_mapped(to, from, MapKind::Rewrite, handler)
            }
            MapKind::ShardFrom => {
                if self.domains[from].rewrite.is_some() && self.domains[from].rewrite != Some(to) {
                    if let Some(handler) = handler.as_deref_mut() {
                        let current = self.domains[self.domains[from].rewrite.unwrap()]
                            .name
                            .clone();
                        handler.message(
                            MessageLevel::Error,
                            format!(
                                "Shard {} has conflicting rewrite_domain {current} and {}",
                                self.domains[from].name, self.domains[to].name
                            ),
                        );
                        return false;
                    }
                }
                let origin = self.domains[to].origin;
                self.merge_origin(from, origin, handler.as_deref_mut());
                self.domains[to].shards.push(from);
                self.domains[from].rewrite = Some(to);
                true
            }
        }
    }

    /// Propagates an origin through shards and rewrites (`MergeOrigin`),
    /// cycle-guarded.
    fn merge_origin(&mut self, at: usize, origin: Option<usize>, mut handler: Handler<'_>) {
        if self.domains[at].breadcrumb {
            return;
        }
        self.domains[at].breadcrumb = true;
        if origin != self.domains[at].origin && origin.is_some() {
            if self.domains[at].origin.is_some() {
                if let Some(handler) = handler.as_deref_mut() {
                    let current = self.domains[self.domains[at].origin.unwrap()].name.clone();
                    let next = self.domains[origin.unwrap()].name.clone();
                    handler.message(
                        MessageLevel::Error,
                        format!(
                            "RewriteDomain {} has conflicting origins {current} and {next}, overriding to {next}",
                            self.domains[at].name
                        ),
                    );
                }
            }
            self.domains[at].origin = origin;
            let shards = self.domains[at].shards.clone();
            for shard in shards {
                self.merge_origin(shard, origin, handler.as_deref_mut());
            }
            if let Some(rewrite) = self.domains[at].rewrite {
                let origin = self.domains[at].origin;
                self.merge_origin(rewrite, origin, handler);
            }
        }
        self.domains[at].breadcrumb = false;
    }

    /// Maps a rewrite domain (`AddRewriteDomainMapping`).
    pub fn add_rewrite_domain_mapping(
        &mut self,
        to_domain_name: &str,
        comma_separated_from_domains: &str,
        handler: Handler<'_>,
    ) -> bool {
        let result = self.map_domain_helper(
            to_domain_name,
            comma_separated_from_domains,
            "",
            MapKind::Rewrite,
            true,
            true,
            true,
            handler,
        );
        self.can_rewrite_domains |= result;
        result
    }

    /// Splits a domain into http/https twins (`DomainNameToTwoProtocols`).
    fn domain_name_to_two_protocols(domain_name: &str) -> Option<(String, String)> {
        let normalized = Self::normalize_domain_name(domain_name);
        if !normalized.starts_with("http:") {
            return None;
        }
        let https = format!("https{}", &normalized[4..]);
        Some((normalized, https))
    }

    /// Maps both protocol twins (`TwoProtocolDomainHelper`).
    fn two_protocol_domain_helper(
        &mut self,
        to_domain_name: &str,
        from_domain_name: &str,
        host_header: &str,
        kind: MapKind,
        authorize: bool,
        mut handler: Handler<'_>,
    ) -> bool {
        let (Some((http_to, https_to)), Some((http_from, https_from))) = (
            Self::domain_name_to_two_protocols(to_domain_name),
            Self::domain_name_to_two_protocols(from_domain_name),
        ) else {
            return false;
        };
        if !self.map_domain_helper(
            &http_to,
            &http_from,
            host_header,
            kind,
            false,
            false,
            authorize,
            handler.as_deref_mut(),
        ) {
            return false;
        }
        // A failed https leg keeps its http twin.
        self.map_domain_helper(
            &https_to,
            &https_from,
            host_header,
            kind,
            false,
            true,
            authorize,
            handler,
        )
    }

    /// Maps both protocol twins (`AddTwoProtocolRewriteDomainMapping`).
    pub fn add_two_protocol_rewrite_domain_mapping(
        &mut self,
        to_domain_name: &str,
        from_domain_name: &str,
        handler: Handler<'_>,
    ) -> bool {
        let result = self.two_protocol_domain_helper(
            to_domain_name,
            from_domain_name,
            "",
            MapKind::Rewrite,
            true,
            handler,
        );
        self.can_rewrite_domains |= result;
        result
    }

    /// Adds shards (`AddShard`).
    pub fn add_shard(
        &mut self,
        shard_domain_name: &str,
        comma_separated_shards: &str,
        handler: Handler<'_>,
    ) -> bool {
        let result = self.map_domain_helper(
            shard_domain_name,
            comma_separated_shards,
            "",
            MapKind::ShardFrom,
            false,
            true,
            true,
            handler,
        );
        self.can_rewrite_domains |= result;
        result
    }

    /// Maps a fetch origin (`AddOriginDomainMapping`).
    pub fn add_origin_domain_mapping(
        &mut self,
        to_domain_name: &str,
        comma_separated_from_domains: &str,
        host_header: &str,
        handler: Handler<'_>,
    ) -> bool {
        self.map_domain_helper(
            to_domain_name,
            comma_separated_from_domains,
            host_header,
            MapKind::Origin,
            true,
            true,
            false,
            handler,
        )
    }

    /// Maps fetch origins for both protocol twins
    /// (`AddTwoProtocolOriginDomainMapping`).
    pub fn add_two_protocol_origin_domain_mapping(
        &mut self,
        to_domain_name: &str,
        from_domain_name: &str,
        host_header: &str,
        handler: Handler<'_>,
    ) -> bool {
        self.two_protocol_domain_helper(
            to_domain_name,
            from_domain_name,
            host_header,
            MapKind::Origin,
            false,
            handler,
        )
    }

    /// Maps a proxy domain (`AddProxyDomainMapping`): either proxied
    /// fetches served from the proxy path, or additionally re-based
    /// onto a CDN domain.
    pub fn add_proxy_domain_mapping(
        &mut self,
        proxy_domain_name: &str,
        origin_domain_name: &str,
        to_domain_name: &str,
        mut handler: Handler<'_>,
    ) -> bool {
        if to_domain_name.is_empty() {
            // Rewrite origin to proxy; flag the origin; map the proxy
            // path back to the origin for fetching.
            self.map_domain_helper(
                origin_domain_name,
                proxy_domain_name,
                "",
                MapKind::Proxy,
                false,
                true,
                true,
                handler,
            )
        } else {
            // Rewrite origin to CDN; flag the origin; rewrite proxy to
            // CDN for cache keys; map the proxy path to the origin.
            // All three legs always run (`&=` in the C++, no
            // short-circuit).
            let first = self.map_domain_helper(
                origin_domain_name,
                to_domain_name,
                "",
                MapKind::Proxy,
                false,
                true,
                true,
                handler.as_deref_mut(),
            );
            let second = self.map_domain_helper(
                to_domain_name,
                proxy_domain_name,
                "",
                MapKind::Rewrite,
                false,
                true,
                true,
                handler.as_deref_mut(),
            );
            let third = self.map_domain_helper(
                origin_domain_name,
                proxy_domain_name,
                "",
                MapKind::Origin,
                false,
                true,
                true,
                handler,
            );
            first & second & third
        }
    }

    /// Maps a fetch URL onto its origin (`MapOrigin`).
    pub fn map_origin(&self, url: &str) -> Option<(String, String, bool)> {
        let gurl = GoogleUrl::new(url);
        if !gurl.is_web_valid() {
            return None;
        }
        self.map_origin_url(&gurl)
    }

    /// Maps a parsed fetch URL onto its origin (`MapOriginUrl`):
    /// remapped URL, host header (explicit, else the request host), and
    /// the proxy flag.
    pub fn map_origin_url(&self, gurl: &GoogleUrl) -> Option<(String, String, bool)> {
        if !gurl.is_web_valid() {
            return None;
        }
        // Canonical specs throughout: GURL strips default ports, so
        // fetches never carry `:80`/`:443` noise.
        let mut out = gurl.canonical_spec().to_string();
        let mut is_proxy = false;
        let mut host_header = String::new();
        if let Some(index) = self.find_domain(gurl) {
            if let Some(origin) = self.domains[index].origin {
                if let Some(mapped) = self.map_url_helper(index, origin, gurl) {
                    out = mapped.canonical_spec().to_string();
                }
                is_proxy = self.domains[origin].is_proxy;
                if !self.domains[origin].host_header.is_empty() {
                    host_header = self.domains[origin].host_header.clone();
                }
            }
        }
        if host_header.is_empty() {
            host_header = gurl.host_and_port();
        }
        Some((out, host_header, is_proxy))
    }

    /// Whether a URL sits under a proxied origin (`IsProxyMapped`).
    #[must_use]
    pub fn is_proxy_mapped(&self, url: &GoogleUrl) -> bool {
        self.find_domain(url).is_some_and(|index| {
            self.domains[index]
                .origin
                .is_some_and(|origin| self.domains[origin].is_proxy)
        })
    }

    /// Merges another lawyer in (`Merge`): clones every domain with its
    /// edges (silently overriding), reorders wildcards by source order,
    /// and unions flags (proxy suffix prefers the source, warning on
    /// conflict — the warning has no channel here yet).
    pub fn merge(&mut self, src: &DomainLawyer) {
        let existing_wildcards = self.wildcards.len();
        for name in src.map.keys() {
            let src_index = src.map[name];
            let dst_index = self.clone_and_add(&src.domains[src_index]);
            if let Some(rewrite) = src.domains[src_index].rewrite {
                let dst_rewrite = self.clone_and_add(&src.domains[rewrite]);
                self.set_domain_mapped(dst_index, dst_rewrite, MapKind::Rewrite, None);
            }
            if let Some(origin) = src.domains[src_index].origin {
                let dst_origin = self.clone_and_add(&src.domains[origin]);
                self.set_domain_mapped(dst_index, dst_origin, MapKind::Origin, None);
            }
            for shard in src.domains[src_index].shards.clone() {
                let dst_shard = self.clone_and_add(&src.domains[shard]);
                self.set_domain_mapped(dst_shard, dst_index, MapKind::ShardFrom, None);
            }
        }
        // Re-add wildcards in source order, skipping duplicates already
        // present from before the merge.
        self.wildcards.truncate(existing_wildcards);
        let mut seen: Vec<usize> = self.wildcards.clone();
        for src_index in &src.wildcards {
            let name = &src.domains[*src_index].name;
            if let Some(&dst_index) = self.map.get(name) {
                if !seen.contains(&dst_index) {
                    seen.push(dst_index);
                    self.wildcards.push(dst_index);
                }
            } else {
                debug_assert!(false, "merged domain missing: {name}");
            }
        }
        self.can_rewrite_domains |= src.can_rewrite_domains;
        self.authorize_all |= src.authorize_all;
        if !src.proxy_suffix.is_empty() {
            self.proxy_suffix = src.proxy_suffix.clone();
        }
    }

    /// Clones one domain in by name (`CloneAndAdd`), adopting auth,
    /// proxy, and host-header state.
    fn clone_and_add(&mut self, src: &Domain) -> usize {
        let index = self
            .add_domain_helper(&src.name, false, src.authorized, src.is_proxy, None)
            .expect("clone target registers");
        self.domains[index].host_header = src.host_header.clone();
        index
    }

    /// Adds the proxy suffix to same- or related-host links
    /// (`AddProxySuffix`).
    pub fn add_proxy_suffix(&self, base_url: &GoogleUrl, href: &str) -> Option<String> {
        let base_host = base_url.host();
        if self.proxy_suffix.is_empty() || !base_host.ends_with(&self.proxy_suffix) {
            return None;
        }
        let base_no_suffix = &base_host[..base_host.len() - self.proxy_suffix.len()];
        let href_gurl = GoogleUrl::resolve(base_url, href);
        if !href_gurl.is_web_valid() || !base_url.is_web_valid() {
            return None;
        }
        // Schemes intentionally unchecked (http-to-https inclusion).
        let href_host = href_gurl.host();
        if href_host == *base_no_suffix {
            // TODO(jmarantz): handle alternate ports.
            return Some(format!(
                "{}://{}{}",
                href_gurl.scheme(),
                base_host,
                href_gurl.path_and_leaf()
            ));
        }
        if ps_http::domain_registry::minimal_private_suffix(&href_host)
            == ps_http::domain_registry::minimal_private_suffix(base_no_suffix)
        {
            return Some(format!(
                "{}://{}{}{}",
                href_gurl.scheme(),
                href_host,
                self.proxy_suffix,
                href_gurl.path_and_leaf()
            ));
        }
        None
    }

    /// Strips the proxy suffix from a URL (`StripProxySuffix`).
    pub fn strip_proxy_suffix(&self, gurl: &GoogleUrl) -> Option<(String, String)> {
        if !gurl.is_web_valid() || self.proxy_suffix.is_empty() {
            return None;
        }
        let host_and_port = gurl.host_and_port();
        let host = host_and_port.strip_suffix(self.proxy_suffix.as_str())?;
        let url = format!("{}://{}{}", gurl.scheme(), host, gurl.path_and_leaf());
        Some((url, host.to_string()))
    }

    /// Picks a shard by hash (`ShardDomain`).
    #[must_use]
    pub fn shard_domain(&self, domain_name: &str, hash: u32) -> Option<String> {
        let gurl = GoogleUrl::new(&Self::normalize_domain_name(domain_name));
        let domain = self.find_domain(&gurl)?;
        if self.domains[domain].shards.is_empty() {
            return None;
        }
        let shard = self.domains[domain].shards[hash as usize % self.domains[domain].shards.len()];
        Some(self.domains[shard].name.clone())
    }

    /// Whether mappings or shards may move a URL (`WillDomainChange`).
    #[must_use]
    pub fn will_domain_change(&self, url: &GoogleUrl) -> bool {
        let Some(domain) = self.find_domain(url) else {
            return false;
        };
        let mut mapped = self.domains[domain].rewrite.unwrap_or(domain);
        let shards = self.domains[mapped].shards.len();
        if shards != 0 {
            if shards == 1 {
                mapped = self.domains[mapped].shards[0];
            } else {
                // The shard index is unknowable here; assume movement.
                return true;
            }
        }
        domain != mapped
    }

    /// Stable configuration fingerprint (`Signature`).
    #[must_use]
    pub fn signature(&self) -> String {
        let mut out = String::new();
        for index in self.map.values() {
            out.push_str("D:");
            out.push_str(&self.domain_signature(*index));
            out.push('-');
        }
        if !self.proxy_suffix.is_empty() {
            out.push_str(",PS:");
            out.push_str(&self.proxy_suffix);
        }
        out
    }

    /// Human-readable listing (`ToString`, empty prefix).
    #[must_use]
    pub fn to_string_list(&self) -> String {
        let mut out = String::new();
        for index in self.map.values() {
            out.push_str(&self.domain_to_string(*index));
            out.push('\n');
        }
        if !self.proxy_suffix.is_empty() {
            out.push_str("Proxy Suffix: ");
            out.push_str(&self.proxy_suffix);
        }
        out
    }

    /// One domain's listing line.
    fn domain_to_string(&self, at: usize) -> String {
        let domain = &self.domains[at];
        let mut out = domain.name.clone();
        if domain.authorized {
            out.push_str(" Auth");
        }
        if let Some(rewrite) = domain.rewrite {
            out.push_str(if domain.is_proxy {
                " ProxyDomain:"
            } else {
                " RewriteDomain:"
            });
            out.push_str(&self.domains[rewrite].name);
        }
        if let Some(origin) = domain.origin {
            out.push_str(if self.domains[origin].is_proxy {
                " ProxyOriginDomain:"
            } else {
                " OriginDomain:"
            });
            out.push_str(&self.domains[origin].name);
        }
        if !domain.shards.is_empty() {
            out.push_str(" Shards:{");
            for (index, shard) in domain.shards.iter().enumerate() {
                if index != 0 {
                    out.push_str(", ");
                }
                out.push_str(&self.domains[*shard].name);
            }
            out.push('}');
        }
        out
    }

    /// Whether two domains resolve to the same content
    /// (`DoDomainsServeSameContent`): same registration, direct rewrite
    /// edge either way, or a shared rewrite target.
    #[must_use]
    pub fn do_domains_serve_same_content(&self, first: &str, second: &str) -> bool {
        let first_gurl = GoogleUrl::new(&Self::normalize_domain_name(first));
        let first = self.find_domain(&first_gurl);
        let second_gurl = GoogleUrl::new(&Self::normalize_domain_name(second));
        let second = self.find_domain(&second_gurl);
        let (Some(first), Some(second)) = (first, second) else {
            return false;
        };
        if first == second {
            return true;
        }
        let rewrite_first = self.domains[first].rewrite;
        let rewrite_second = self.domains[second].rewrite;
        if rewrite_first == Some(second) || rewrite_second == Some(first) {
            return true;
        }
        rewrite_first.is_some() && rewrite_first == rewrite_second
    }

    /// Names mapped onto a URL's domain (`FindDomainsRewrittenTo`), in
    /// registry (sorted-name) order.
    pub fn find_domains_rewritten_to(&self, url: &GoogleUrl) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(index) = self.find_domain(url) {
            for candidate in self.map.values() {
                if self.domains[*candidate].rewrite == Some(index) {
                    names.push(self.domains[*candidate].name.clone());
                }
            }
        }
        names
    }

    /// Wildcard registrations (`num_wildcarded_domains`, test hook).
    #[must_use]
    pub fn num_wildcarded_domains(&self) -> usize {
        self.wildcards.len()
    }

    /// One domain's fingerprint contribution.
    fn domain_signature(&self, at: usize) -> String {
        let domain = &self.domains[at];
        let mut out = format!(
            "{}_{}_",
            domain.name,
            if domain.authorized { "_a" } else { "_n" }
        );
        if let Some(rewrite) = domain.rewrite {
            out.push_str(&format!("R:{}_", self.domains[rewrite].name));
        }
        if !domain.host_header.is_empty() {
            out.push_str(&format!("H:{}|", domain.host_header));
        }
        if let Some(origin) = domain.origin {
            let prefix = if self.domains[origin].is_proxy {
                "P:"
            } else {
                "O:"
            };
            out.push_str(&format!("{prefix}{}_", self.domains[origin].name));
        }
        for shard in &domain.shards {
            out.push_str(&format!("S:{}_", self.domains[*shard].name));
        }
        out
    }
}

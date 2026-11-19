//! Domain rewriting and sharding (`DomainRewriteFilter`).
//!
//! Rewrites resource (and optionally hyperlink) URLs onto mapped or
//! sharded domains, fixes up Location/Refresh/Set-Cookie response
//! headers plus refresh/set-cookie meta tags, and can inject a
//! client-side domain-rewriting shim. Mappings come from the shared
//! `DomainLawyer`; options, allow rules, namer, response headers, and
//! document context arrive via config.
//!
//! TODO(step4): publish the rewrite counter as the `domain_rewrites`
//! statistics variable once the driver owns a registry.

use std::rc::Rc;

use ps_cache::hash::hash_string_case_preserve;
use ps_config::options::RewriteOptions;
use ps_http::google_url::GoogleUrl;
use ps_http::response::ResponseHeaders;

use super::common::{add_js_to_element, BodyEndTracker, XhtmlStatus};
use crate::base_tracker::BaseTracker;
use crate::decode_url::{decode_url, DecodeConfig};
use crate::domain_lawyer::DomainLawyer;
use crate::element::{LeafKind, NodeId};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::resource_tag_scanner::scan_element;
use crate::script_scanner::ScriptTagScanner;
use crate::static_assets::{get_asset, StaticAsset};
use crate::url_namer::UrlNamer;

use super::url_left_trim::UrlValuedAttributes;

/// Sticky-redirect marker (`kStickyRedirectHeader`): present means the
/// headers were already redirected elsewhere and stay untouched.
pub const STICKY_REDIRECT_HEADER: &str = "X-PSA-Sticky-Redirect";

/// Resolves, maps, and shards one URL (`Rewrite`, dependency-free
/// form for CSS resolution and other non-filter callers).
#[allow(clippy::too_many_arguments)]
pub(crate) fn rewrite_url(
    lawyer: &DomainLawyer,
    options: &RewriteOptions,
    namer: &dyn UrlNamer,
    hash_length: usize,
    signature_length: usize,
    url_to_rewrite: &[u8],
    base_url: &GoogleUrl,
    apply_sharding: bool,
    apply_domain_suffix: bool,
) -> (RewriteResult, Vec<u8>) {
    if url_to_rewrite.is_empty() {
        return (RewriteResult::DomainUnchanged, Vec::new());
    }
    let reference = String::from_utf8_lossy(url_to_rewrite);
    let orig_url = GoogleUrl::resolve(base_url, &reference);
    if !orig_url.is_web_or_data_valid() {
        return (RewriteResult::Fail, url_to_rewrite.to_vec());
    }
    if !orig_url.is_web_valid() {
        return (RewriteResult::DomainUnchanged, url_to_rewrite.to_vec());
    }
    let orig_spec = orig_url.spec().to_string();
    // A proxy suffix overrides every other mapping.
    if apply_domain_suffix {
        if let Some(suffixed) = lawyer.add_proxy_suffix(base_url, &reference) {
            return (RewriteResult::RewroteDomain, suffixed.into_bytes());
        }
    }
    if !options.allow_resources().is_match(&orig_spec, true)
        || decode_url(
            &orig_url,
            &DecodeConfig {
                options,
                namer,
                hash_length,
                signature_length,
            },
        )
        .is_some()
    {
        return (RewriteResult::DomainUnchanged, orig_spec.into_bytes());
    }
    let resolved = match lawyer.map_request_to_domain(base_url, &reference) {
        Some((_, resolved)) => resolved,
        None => return (RewriteResult::DomainUnchanged, orig_spec.into_bytes()),
    };
    let mut rewritten = resolved.spec().to_string();
    if apply_sharding {
        let domain = format!("{}/", resolved.origin());
        let hash = hash_string_case_preserve(rewritten.as_bytes());
        if let Some(sharded) = lawyer.shard_domain(&domain, hash) {
            let leaf = resolved.path_and_leaf();
            rewritten = format!("{sharded}{}", &leaf[1.min(leaf.len())..]);
        }
    }
    if orig_spec == rewritten {
        (RewriteResult::DomainUnchanged, rewritten.into_bytes())
    } else {
        (RewriteResult::RewroteDomain, rewritten.into_bytes())
    }
}

/// Rewrite outcome (`RewriteResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteResult {
    /// Domain changed.
    RewroteDomain,
    /// Absolute URL produced, domain kept.
    DomainUnchanged,
    /// Unresolvable input.
    Fail,
}

/// Header source (`HeaderSource`): real headers or meta http-equiv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderSource {
    Http,
    MetaHttpEquiv,
}

/// Driver inputs for domain rewriting.
#[derive(Debug, Clone)]
pub struct DomainRewriteConfig {
    /// Rewrite options (flags, allow rules, forbidden list).
    pub options: RewriteOptions,
    /// Virtual override (`DisableDomainRewrite`, default false).
    pub disable_domain_rewrite: bool,
    /// AMP verdict (vetoes the client shim).
    pub is_amp_document: bool,
    /// `kDebug` (debug asset builds).
    pub debug: bool,
    /// `kPedantic` (script type attributes).
    pub pedantic: bool,
    /// Response mimetype XHTML certainty.
    pub xhtml: XhtmlStatus,
    /// Hasher length in characters (hash/signature split).
    pub hash_length: usize,
    /// Signature length in characters (0 unless URL signing).
    pub signature_length: usize,
    /// Operator URL-valued attributes.
    pub url_valued: UrlValuedAttributes,
    /// Response headers for the header pass (shared with the driver).
    pub response_headers: Option<Rc<std::cell::RefCell<ResponseHeaders>>>,
}

impl Default for DomainRewriteConfig {
    fn default() -> Self {
        DomainRewriteConfig {
            options: RewriteOptions::new(),
            disable_domain_rewrite: false,
            is_amp_document: false,
            debug: false,
            pedantic: false,
            xhtml: XhtmlStatus::IsNotXhtml,
            hash_length: 0,
            signature_length: 0,
            url_valued: Vec::new(),
            response_headers: None,
        }
    }
}

/// Rewrites domains (`DomainRewriteFilter`).
pub struct DomainRewriteFilter<N: UrlNamer> {
    config: DomainRewriteConfig,
    namer: N,
    lawyer: Rc<DomainLawyer>,
    base: BaseTracker,
    body_end: BodyEndTracker,
    rewrite_count: i64,
}

impl<N: UrlNamer> DomainRewriteFilter<N> {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: DomainRewriteConfig, namer: N, lawyer: &Rc<DomainLawyer>) -> Self {
        let mut base = BaseTracker::default();
        base.set_lawyer(lawyer);
        DomainRewriteFilter {
            config,
            namer,
            lawyer: lawyer.clone(),
            base,
            body_end: BodyEndTracker::default(),
            rewrite_count: 0,
        }
    }

    /// Rewritten URLs (`domain_rewrites`).
    #[must_use]
    pub fn rewrite_count(&self) -> i64 {
        self.rewrite_count
    }

    /// Resolves, maps, and shards one URL (`Rewrite`).
    fn rewrite(
        &self,
        url_to_rewrite: &[u8],
        base_url: &GoogleUrl,
        apply_sharding: bool,
        apply_domain_suffix: bool,
    ) -> (RewriteResult, Vec<u8>) {
        rewrite_url(
            &self.lawyer,
            &self.config.options,
            &self.namer,
            self.config.hash_length,
            self.config.signature_length,
            url_to_rewrite,
            base_url,
            apply_sharding,
            apply_domain_suffix,
        )
    }

    /// Whether a URL names a rewritten resource (`IsPagespeedResource`,
    /// via URL decoding).
    fn is_pagespeed_resource(&self, url: &GoogleUrl) -> bool {
        decode_url(
            url,
            &DecodeConfig {
                options: &self.config.options,
                namer: &self.namer,
                hash_length: self.config.hash_length,
                signature_length: self.config.signature_length,
            },
        )
        .is_some()
    }

    /// Rewrites Location/Refresh/Set-Cookie headers (`UpdateDomainHeaders`).
    pub fn update_domain_headers(&self, base_url: &GoogleUrl, headers: &mut ResponseHeaders) {
        // A sticky redirect means an iframe fetcher already redirected;
        // respect it.
        if headers.headers().has(STICKY_REDIRECT_HEADER) {
            return;
        }
        for name in ["Location", "Refresh"] {
            if let Some(value) = headers.headers().lookup1(name) {
                if let Some(new) = self.update_one_domain_header(
                    HeaderSource::Http,
                    base_url,
                    name,
                    value.as_bytes(),
                ) {
                    headers
                        .headers_mut()
                        .replace(name, &String::from_utf8_lossy(&new));
                }
            }
        }
        // Set-Cookie needs care: there can be several.
        for index in 0..headers.headers().len() {
            if headers
                .headers()
                .name(index)
                .eq_ignore_ascii_case("Set-Cookie")
            {
                let value = headers.headers().value(index).as_bytes().to_vec();
                if let Some(new) = self.update_set_cookie_header(base_url, &value) {
                    headers
                        .headers_mut()
                        .set_value(index, &String::from_utf8_lossy(&new));
                }
            }
        }
    }

    /// Rewrites one Location/Refresh/Set-Cookie value (`UpdateOneDomainHeader`).
    fn update_one_domain_header(
        &self,
        src: HeaderSource,
        base_url: &GoogleUrl,
        name: &str,
        value: &[u8],
    ) -> Option<Vec<u8>> {
        if !self.config.options.domain_rewrite_hyperlinks() {
            return None;
        }
        if src == HeaderSource::Http && name.eq_ignore_ascii_case("Location") {
            let (status, rewritten) = self.rewrite(value, base_url, false, true);
            return if status == RewriteResult::RewroteDomain {
                Some(rewritten)
            } else {
                None
            };
        }
        if name.eq_ignore_ascii_case("Refresh") {
            if let Some((before, url, after)) = Self::parse_refresh_content(value) {
                let (status, rewritten) = self.rewrite(&url, base_url, false, true);
                if status == RewriteResult::RewroteDomain {
                    // Quote with `"`: pre-existing quotes %-encode safely
                    // (RFC 3986: `"` is unreserved, `'` is not).
                    let mut quoted = replace_substring(&rewritten, b"\"", b"%22");
                    let mut out = before;
                    out.push(b'"');
                    out.append(&mut quoted);
                    out.push(b'"');
                    out.extend_from_slice(&after);
                    return Some(out);
                }
            }
            return None;
        }
        if name.eq_ignore_ascii_case("Set-Cookie") {
            return self.update_set_cookie_header(base_url, value);
        }
        None
    }

    /// Rewrites a Set-Cookie value (`UpdateSetCookieHeader`).
    pub fn update_set_cookie_header(&self, base_url: &GoogleUrl, value: &[u8]) -> Option<Vec<u8>> {
        if !self.config.options.domain_rewrite_cookies() {
            return None;
        }
        if !base_url.is_web_valid() {
            return None;
        }
        let (cookie_string, attributes) = Self::parse_set_cookie_attributes(value);
        // Last domain/path wins per spec.
        let mut has_domain = false;
        let mut has_path = false;
        let mut domain = Vec::new();
        let mut path = Vec::new();
        for (key, val) in &attributes {
            if key.eq_ignore_ascii_case(b"Path") {
                has_path = true;
                path = val.clone();
            } else if key.eq_ignore_ascii_case(b"Domain") {
                has_domain = true;
                domain = val.clone();
            }
        }
        // Paths not starting with `/` are ineffective (RFC 6265 5.2.4).
        if has_path && !path.starts_with(b"/") {
            has_path = false;
            path = b"/".to_vec();
        }
        if !has_path && !has_domain {
            return None;
        }
        let mut domain_and_scheme;
        if has_domain {
            // A leading dot is irrelevant per spec.
            if let Some(stripped) = domain.strip_prefix(b".") {
                domain = stripped.to_vec();
            }
            domain_and_scheme = base_url.scheme().as_bytes().to_vec();
            domain_and_scheme.extend_from_slice(b"://");
            domain_and_scheme.extend_from_slice(&domain);
        } else {
            domain_and_scheme = base_url.origin().as_bytes().to_vec();
        }
        domain_and_scheme.extend_from_slice(&path);
        let (status, rewritten) = self.rewrite(&domain_and_scheme, base_url, false, true);
        if status != RewriteResult::RewroteDomain {
            return None;
        }
        let parsed = GoogleUrl::new(&String::from_utf8_lossy(&rewritten));
        let out_domain = parsed.host().into_bytes();
        let out_path = replace_substring(parsed.path_sans_query().as_bytes(), b";", b"%3b");
        let mut out = cookie_string;
        for (key, val) in &attributes {
            out.extend_from_slice(b"; ");
            let mut val = val.clone();
            if has_path && key.eq_ignore_ascii_case(b"Path") {
                val = out_path.clone();
            } else if has_domain && key.eq_ignore_ascii_case(b"Domain") {
                val = out_domain.clone();
            }
            if val.is_empty() {
                out.extend_from_slice(key);
            } else {
                out.extend_from_slice(key);
                out.push(b'=');
                out.extend_from_slice(&val);
            }
        }
        Some(out)
    }

    /// Splits refresh content into before/url/after (`ParseRefreshContent`).
    pub fn parse_refresh_content(input: &[u8]) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let mut parse = input;
        parse = trim_leading_whitespace(parse);
        // Skip the delay digits.
        while let Some((&byte, _)) = parse.split_first() {
            if byte.is_ascii_digit() || byte == b'.' {
                parse = &parse[1..];
            } else {
                break;
            }
        }
        parse = trim_leading_whitespace(parse);
        if parse.is_empty() || (parse[0] != b',' && parse[0] != b';') {
            return None;
        }
        parse = &parse[1..];
        parse = trim_leading_whitespace(parse);
        // The `url=` is effectively optional in the wild.
        if parse.len() >= 3 && parse[..3].eq_ignore_ascii_case(b"url") {
            let mut spec = &parse[3..];
            spec = trim_leading_whitespace(spec);
            if let Some(rest) = spec.strip_prefix(b"=") {
                parse = rest;
            }
        }
        parse = trim_leading_whitespace(parse);
        let before_len = input.len() - parse.len();
        let before = input[..before_len].to_vec();
        let mut quote = b' ';
        if let Some(rest) = parse.strip_prefix(b"'") {
            quote = b'\'';
            parse = rest;
        } else if let Some(rest) = parse.strip_prefix(b"\"") {
            quote = b'"';
            parse = rest;
        }
        let (url, after) = if quote == b' ' {
            (parse.to_vec(), Vec::new())
        } else if let Some(pos) = parse.iter().position(|byte| *byte == quote) {
            let after_start = parse.len() - (parse.len() - pos) + 1;
            (
                parse[..pos].to_vec(),
                parse[after_start.min(parse.len())..].to_vec(),
            )
        } else {
            (parse.to_vec(), Vec::new())
        };
        let url = trim_whitespace(&url).to_vec();
        if url.is_empty() {
            return None;
        }
        Some((before, url, after))
    }

    /// Splits a Set-Cookie value (`ParseSetCookieAttributes`).
    pub fn parse_set_cookie_attributes(input: &[u8]) -> (Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>) {
        let mut attributes = Vec::new();
        let Some(semi) = input.iter().position(|byte| *byte == b';') else {
            return (trim_whitespace(input).to_vec(), attributes);
        };
        let cookie_string = trim_whitespace(&input[..semi]).to_vec();
        let mut parse = &input[semi + 1..];
        loop {
            let (chunk, rest, last) = match parse.iter().position(|byte| *byte == b';') {
                Some(pos) => (&parse[..pos], &parse[pos + 1..], false),
                None => (parse, &[][..], true),
            };
            let (key, val) = match chunk.iter().position(|byte| *byte == b'=') {
                Some(pos) => (
                    trim_whitespace(&chunk[..pos]).to_vec(),
                    trim_whitespace(&chunk[pos + 1..]).to_vec(),
                ),
                None => (trim_whitespace(chunk).to_vec(), Vec::new()),
            };
            if !key.is_empty() || !val.is_empty() {
                attributes.push((key, val));
            }
            if last {
                break;
            }
            parse = rest;
        }
        (cookie_string, attributes)
    }
}

/// Trims ASCII whitespace on both ends.
fn trim_whitespace(text: &[u8]) -> &[u8] {
    // Manual trim (whitespace set: space, tab, CR, LF, FF).
    let mut start = 0;
    while start < text.len() && is_space(text[start]) {
        start += 1;
    }
    let mut end = text.len();
    while end > start && is_space(text[end - 1]) {
        end -= 1;
    }
    &text[start..end]
}

/// Trims leading ASCII whitespace.
fn trim_leading_whitespace(mut text: &[u8]) -> &[u8] {
    while let Some(&first) = text.first() {
        if !is_space(first) {
            break;
        }
        text = &text[1..];
    }
    text
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
}

/// Replaces all occurrences of a byte needle.
fn replace_substring(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return haystack.to_vec();
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(pos) = rest
        .windows(needle.len())
        .position(|window| window == needle)
    {
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(replacement);
        rest = &rest[pos + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
}

impl<N: UrlNamer> HtmlFilter for DomainRewriteFilter<N> {
    fn name(&self) -> &'static str {
        "DomainRewrite"
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        let url = String::from_utf8_lossy(lexer.document_url());
        self.base.reset(&url);
        if let Some(headers) = &self.config.response_headers {
            let base = GoogleUrl::new(&url);
            self.update_domain_headers(&base, &mut headers.borrow_mut());
        }
        self.body_end.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.clear();
        self.base.observe(dom, id);
        if !self.base.base_url_is_valid() || self.config.disable_domain_rewrite {
            return;
        }
        let found: Vec<(usize, ps_config::options::Category)> =
            scan_element(dom, id, &self.config.url_valued);
        let keyword = dom.element_keyword(id);
        let frame_like = matches!(keyword, Keyword::Embed | Keyword::Frame | Keyword::Iframe);
        let has_integrity = ScriptTagScanner::has_integrity_attribute(dom, id);
        let is_script = keyword == Keyword::Script;
        let script_type = if is_script {
            dom.find_attribute(id, Keyword::Type).and_then(|index| {
                let (decoded, _) = dom.decoded_attribute(id, index);
                decoded
            })
        } else {
            None
        };
        let is_cors_script = script_type
            .as_deref()
            .is_some_and(|mime| mime.eq_ignore_ascii_case(b"module"))
            || (is_script && dom.find_attribute_by_name(id, b"crossorigin").is_some());
        for (index, category) in found {
            use ps_config::options::Category::*;
            if has_integrity && matches!(category, Script | Stylesheet) {
                continue;
            }
            if is_cors_script && category == Script {
                continue;
            }
            let is_resource = matches!(category, Image | Script | Stylesheet);
            if !self.config.options.domain_rewrite_hyperlinks() && !is_resource {
                continue;
            }
            let (decoded, _error) = dom.decoded_attribute(id, index);
            let val = decoded.unwrap_or_default();
            if val.is_empty() {
                continue;
            }
            let apply_sharding = !frame_like && category != Hyperlink && category != Prefetch;
            let apply_domain_suffix = category == Hyperlink || is_resource;
            let (status, rewritten) =
                self.rewrite(&val, &self.base.base(), apply_sharding, apply_domain_suffix);
            if status == RewriteResult::RewroteDomain {
                dom.element_attributes_mut(id)[index].set_value(&rewritten);
                self.rewrite_count += 1;
            }
        }
        if keyword == Keyword::Meta {
            let equiv = dom
                .find_attribute(id, Keyword::HttpEquiv)
                .and_then(|index| {
                    let (decoded, _) = dom.decoded_attribute(id, index);
                    decoded
                });
            let content = dom.find_attribute(id, Keyword::Content).and_then(|index| {
                let (decoded, _) = dom.decoded_attribute(id, index);
                decoded.map(|decoded| (index, decoded))
            });
            if let (Some(equiv), Some((index, content))) = (equiv, content) {
                let equiv_str = String::from_utf8_lossy(&equiv);
                if let Some(out) = self.update_one_domain_header(
                    HeaderSource::MetaHttpEquiv,
                    &self.base.base(),
                    &equiv_str,
                    &content,
                ) {
                    dom.element_attributes_mut(id)[index].set_value(&out);
                }
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_end(dom, id);
        self.base.finish_base_element(dom, id);
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_characters(dom, id);
    }

    fn end_document(&mut self, dom: &mut Dom, lexer: &Lexer) {
        if !self.config.options.client_domain_rewrite() || self.config.is_amp_document {
            return;
        }
        let from_domains = self.lawyer.find_domains_rewritten_to(&self.base.base());
        if from_domains.is_empty() {
            return;
        }
        let mut quoted = Vec::new();
        for (index, domain) in from_domains.iter().enumerate() {
            quoted.push(b'"');
            quoted.extend_from_slice(domain.as_bytes());
            quoted.push(b'"');
            if index + 1 != from_domains.len() {
                quoted.push(b',');
            }
        }
        let script = dom.new_element(None, b"script");
        self.body_end.insert(dom, script);
        let mut js = get_asset(StaticAsset::ClientDomainRewriter, self.config.debug).to_vec();
        js.extend_from_slice(b"pagespeed.clientDomainRewriterInit([");
        js.extend_from_slice(&quoted);
        js.extend_from_slice(b"]);");
        add_js_to_element(
            dom,
            lexer,
            script,
            &js,
            self.config.pedantic,
            self.config.xhtml,
        );
    }
}

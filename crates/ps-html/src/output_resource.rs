//! Rewritten output resources (`OutputResource`).
//!
//! Names (`ResourceNamer` + `UrlNamer`), content hashing on write
//! completion, and URL signatures. Serving, metadata-cache
//! attachment (`CachedResult`), and disk dumps land with the
//! contexts and serving layers.

use std::rc::Rc;

use ps_cache::hash::Hasher;
use ps_cache::namer::ResourceNamer;
use ps_http::content_type::{ContentType, MAX_EXTENSION_LEN};
use ps_http::google_url::GoogleUrl;
use ps_http::response::{HttpOptions, ResponseHeaders};

use super::resource::Resource;
use super::server_context::ResourceDriver;
use super::url_namer::EncodeOption;

/// Output provenance (`OutputResourceKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputResourceKind {
    /// Derived from input URL(s), cached normally.
    Rewritten,
    /// Cheaply derived; contents recomputed per use.
    OnTheFly,
    /// Derived from inline input; kept in metadata only.
    Inline,
    /// External output derived from inline input.
    Outlined,
}

/// A rewritten output resource (`OutputResource`).
#[derive(Clone)]
pub struct OutputResource {
    driver: Rc<ResourceDriver>,
    resolved_base: String,
    unmapped_base: String,
    original_base: String,
    full_name: ResourceNamer,
    kind: OutputResourceKind,
    content_type: Option<ContentType>,
    headers: ResponseHeaders,
    body: Vec<u8>,
    writing_complete: bool,
    computed_url: String,
    authorized: bool,
    charset: String,
    cache_control_suffix: String,
}

impl OutputResource {
    /// Builds an output resource (`OutputResource`); the resolved base
    /// must end in a slash.
    pub fn new(
        driver: Rc<ResourceDriver>,
        resolved_base: &str,
        unmapped_base: &str,
        original_base: &str,
        full_name: ResourceNamer,
        kind: OutputResourceKind,
    ) -> Self {
        assert!(
            resolved_base.ends_with('/'),
            "resolved_base must end in a slash, was: {resolved_base}"
        );
        let http_options = {
            let options = driver.options();
            HttpOptions {
                respect_vary: options.respect_vary(),
                implicit_cache_ttl_ms: options.implicit_cache_ttl_ms(),
            }
        };
        let mut resource = OutputResource {
            driver,
            resolved_base: resolved_base.to_string(),
            unmapped_base: unmapped_base.to_string(),
            original_base: original_base.to_string(),
            full_name,
            kind,
            content_type: None,
            headers: ResponseHeaders::new(http_options),
            body: Vec::new(),
            writing_complete: false,
            computed_url: String::new(),
            authorized: true,
            charset: String::new(),
            cache_control_suffix: String::new(),
        };
        resource.refresh_url();
        resource
    }

    /// Recomputes the cached URL (runs after every naming mutation;
    /// C++ memoizes lazily instead — same observable).
    fn refresh_url(&mut self) {
        let namer = self.driver.server_context().url_namer().clone();
        let lawyer = self.driver.lawyer();
        self.computed_url = namer.encode(lawyer, self, EncodeOption::Sharded);
    }

    /// Borrows the full name.
    #[must_use]
    pub fn full_name(&self) -> &ResourceNamer {
        &self.full_name
    }

    /// Borrows the resolved base (mapped, slashed).
    #[must_use]
    pub fn resolved_base(&self) -> &str {
        &self.resolved_base
    }

    /// Borrows the unmapped base.
    #[must_use]
    pub fn unmapped_base(&self) -> &str {
        &self.unmapped_base
    }

    /// Borrows the original base.
    #[must_use]
    pub fn original_base(&self) -> &str {
        &self.original_base
    }

    /// Returns the kind.
    #[must_use]
    pub fn kind(&self) -> OutputResourceKind {
        self.kind
    }

    /// Borrows the response headers.
    #[must_use]
    pub fn response_headers(&self) -> &ResponseHeaders {
        &self.headers
    }

    /// Mutably borrows the response headers.
    pub fn response_headers_mut(&mut self) -> &mut ResponseHeaders {
        &mut self.headers
    }

    /// Borrows the written body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Whether writing finished.
    #[must_use]
    pub fn writing_complete(&self) -> bool {
        self.writing_complete
    }

    /// Borrows the charset.
    #[must_use]
    pub fn charset(&self) -> &str {
        &self.charset
    }

    /// Sets the charset (`set_charset`).
    pub fn set_charset(&mut self, charset: &str) {
        self.charset = charset.to_string();
    }

    /// Borrows the cache-control suffix.
    #[must_use]
    pub fn cache_control_suffix(&self) -> &str {
        &self.cache_control_suffix
    }

    /// Sets the cache-control suffix (once).
    pub fn set_cache_control_suffix(&mut self, suffix: &str) {
        assert!(self.cache_control_suffix.is_empty());
        self.cache_control_suffix = suffix.to_string();
    }

    /// File extension for the suffix (`suffix`, sans dot).
    #[must_use]
    pub fn suffix(&self) -> &str {
        let extension = self
            .content_type
            .expect("suffix without a content type")
            .extension();
        extension.strip_prefix('.').unwrap_or(extension)
    }

    /// Cache key path (`name_key`): resolved base plus id+name.
    #[must_use]
    pub fn name_key(&self) -> String {
        assert!(!self.resolved_base.is_empty());
        format!("{}{}", self.resolved_base, self.full_name.encode_id_name())
    }

    /// Starts a write (`BeginWrite`): clears prior bytes, hash, and URL.
    pub fn begin_write(&mut self) -> &mut Vec<u8> {
        assert!(!self.writing_complete);
        self.body.clear();
        self.full_name.clear_hash();
        self.refresh_url();
        &mut self.body
    }

    /// Finishes a write (`EndWrite`): stamps headers, hashes contents,
    /// and signs the name.
    pub fn end_write(&mut self) {
        assert!(!self.writing_complete);
        let hash = self.driver.server_context().hasher().hash(&self.body);
        self.full_name.set_hash(&hash);
        let signature = self.compute_signature();
        self.full_name.set_signature(&signature);
        self.refresh_url();
        self.writing_complete = true;
    }

    /// Sets the content type, refreshing the extension (`SetType`).
    pub fn set_type(&mut self, content_type: Option<ContentType>) {
        self.content_type = content_type;
        if let Some(content_type) = content_type {
            let extension = content_type.extension();
            self.full_name
                .set_ext(extension.strip_prefix('.').unwrap_or(extension));
            self.refresh_url();
            debug_assert!(
                self.full_name.ext().len() <= MAX_EXTENSION_LEN,
                "output extension exceeds {MAX_EXTENSION_LEN}"
            );
        }
    }

    /// Pins the hash explicitly (`SetHash`, pre-write only).
    pub fn set_hash(&mut self, hash: &str) {
        assert!(!self.writing_complete);
        assert!(self.full_name.hash().is_empty());
        self.full_name.set_hash(hash);
        self.refresh_url();
    }

    /// Clears the hash (`clear_hash`, for size probing).
    pub fn clear_hash(&mut self) {
        self.full_name.clear_hash();
        self.refresh_url();
    }

    /// Replaces the encoded name, recomputing the URL (`set_name`
    /// on the full name, e.g. dropping resize dimensions).
    pub fn set_name(&mut self, name: &str) {
        self.full_name.set_name(name);
        self.refresh_url();
    }

    /// Copies hash/extension/signature from another namer,
    /// recomputing the URL (fetch/canonical comparison).
    pub fn sync_naming_from(&mut self, other: &ResourceNamer) {
        self.full_name.clear_hash();
        self.full_name.set_hash(other.hash());
        self.full_name.set_ext(other.ext());
        self.full_name.set_signature(other.signature());
        self.refresh_url();
    }

    /// Whether a hash is set (`has_hash`).
    #[must_use]
    pub fn has_hash(&self) -> bool {
        !self.full_name.hash().is_empty()
    }

    /// Computes the URL signature (`ComputeSignature`): empty unless a
    /// signing key is configured.
    #[must_use]
    pub fn compute_signature(&self) -> String {
        let key = self.driver.options().url_signing_key();
        if key.is_empty() {
            return String::new();
        }
        let data = self.http_cache_key();
        let suffix_len =
            self.full_name.ext().len() + self.full_name.hash().len() + 2 + self.signature_len();
        let signed_len = data.len().saturating_sub(suffix_len);
        ps_cache::signature::Sha1Signature::new()
            .sign(key.as_bytes(), &data.as_bytes()[..signed_len])
    }

    /// Checks the embedded signature (`CheckSignature`), without
    /// short-circuiting (timing-attack hardening, like C++).
    #[must_use]
    pub fn check_signature(&self) -> bool {
        // NOTE: logging the computed signature would leak valid URLs,
        // so only the provided one is named (no log channel yet).
        let computed = self.compute_signature();
        let provided = self.full_name.signature();
        let mut mismatches = 0;
        for (left, right) in computed.bytes().zip(provided.bytes()) {
            mismatches += i32::from(left != right);
        }
        mismatches += (computed.len() as i32 - provided.len() as i32).abs();
        mismatches == 0 || self.driver.options().accept_invalid_signatures()
    }

    /// HTTP cache key (`HttpCacheKey`): the unsharded URL, remapped
    /// through the lawyer when mappings apply.
    #[must_use]
    pub fn http_cache_key(&self) -> String {
        let namer = self.driver.server_context().url_namer().clone();
        let canonical = namer.encode(self.driver.lawyer(), self, EncodeOption::Unsharded);
        let base = GoogleUrl::new(&canonical);
        if !base.is_web_valid() {
            return canonical;
        }
        match self
            .driver
            .lawyer()
            .map_request_to_domain(&base, &canonical)
        {
            Some((_, resolved)) => resolved.spec().to_string(),
            None => canonical,
        }
    }

    /// Base with proxy encoding stripped (`decoded_base`).
    #[must_use]
    pub fn decoded_base(&self) -> String {
        let namer = self.driver.server_context().url_namer().clone();
        let url = GoogleUrl::new(&self.url());
        let mut base = url;
        if let Some(decoded) = namer.decode(&base) {
            base = GoogleUrl::new(&decoded);
        }
        base.all_except_leaf().to_string()
    }

    /// URL with a placeholder hash when unset (`UrlEvenIfHashNotSet`).
    pub fn url_even_if_hash_not_set(&mut self) -> String {
        if self.has_hash() {
            self.url().to_string()
        } else {
            self.set_hash("0");
            let url = self.url().to_string();
            self.clear_hash();
            url
        }
    }

    /// Signature length in characters.
    fn signature_len(&self) -> usize {
        ps_cache::signature::Sha1Signature::new().signature_size_in_chars()
    }
}

impl std::fmt::Debug for OutputResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputResource")
            .field("resolved_base", &self.resolved_base)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Resource for OutputResource {
    fn url(&self) -> &str {
        &self.computed_url
    }

    fn is_authorized_domain(&self) -> bool {
        self.authorized
    }

    fn set_is_authorized_domain(&mut self, authorized: bool) {
        self.authorized = authorized;
    }
}

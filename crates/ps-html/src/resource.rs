//! Rewritable web resources (`Resource`, fetch-free bottom).
//!
//! A resource is fetched content plus metadata, shared by handle
//! between slots and rewrite contexts. Loading, caching, and
//! refreshing land with the pipeline; this slice carries identity
//! (`url`, authorization) and the fetch-free data-URL kind.

use ps_http::content_type::ContentType;
use ps_http::data_url::{decode_data_url_content, parse_data_url};
use ps_http::request::RequestContext;
use ps_http::response::{HttpOptions, ResponseHeaders};

use super::cacheable_resource::{LoadCallback, NotCacheablePolicy};
use super::font_service_input_resource::FontServiceInputResource;
use super::url_input_resource::UrlInputResource;

/// Shared resource handle (`ResourcePtr`).
pub type ResourceHandle = std::rc::Rc<dyn Resource>;

/// A fetched (or fetchable) web resource (`Resource`).
pub trait Resource: std::fmt::Debug {
    /// Absolute URL of the resource.
    fn url(&self) -> &str;

    /// Whether the resource's domain is explicitly authorized.
    /// Unauthorized resources may inline but never render as-is.
    fn is_authorized_domain(&self) -> bool;

    /// Marks the domain authorized or not.
    fn set_is_authorized_domain(&mut self, authorized: bool);
}

/// An input resource backed by a `data:` URL (`DataUrlInputResource`).
///
/// Data URLs never expire and skip the HTTP cache; validity is just
/// successful decoding.
#[derive(Debug, Clone)]
pub struct DataUrlInputResource {
    url: String,
    contents: Vec<u8>,
    content_type: Option<ContentType>,
    headers: ResponseHeaders,
    authorized: bool,
}

impl DataUrlInputResource {
    /// Dismantles a `data:` URL (`Make`); parse failure returns `None`.
    /// Decode failure still returns a resource, marked invalid.
    #[must_use]
    pub fn make(url: &str) -> Option<Self> {
        let (content_type, encoding, encoded) = parse_data_url(url.as_bytes())?;
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        let contents = match decode_data_url_content(encoding, encoded) {
            Some(contents) => {
                headers.set_first_line(1, 1, 200, "OK");
                if let Some(content_type) = content_type {
                    headers
                        .headers_mut()
                        .add("Content-Type", content_type.mime_type());
                }
                contents
            }
            None => Vec::new(),
        };
        Some(DataUrlInputResource {
            url: url.to_string(),
            contents,
            content_type,
            headers,
            authorized: true,
        })
    }

    /// Decoded payload bytes.
    #[must_use]
    pub fn contents(&self) -> &[u8] {
        &self.contents
    }

    /// Dismantled content type, if the MIME maps to one.
    #[must_use]
    pub fn content_type(&self) -> Option<ContentType> {
        self.content_type
    }

    /// Borrows the synthesized headers (200 + content type when valid).
    #[must_use]
    pub fn response_headers(&self) -> &ResponseHeaders {
        &self.headers
    }

    /// Whether the payload decoded (`IsValidAndCacheable`).
    #[must_use]
    pub fn is_valid_and_cacheable(&self) -> bool {
        self.headers.status_code() == 200
    }
}

impl Resource for DataUrlInputResource {
    fn url(&self) -> &str {
        &self.url
    }

    fn is_authorized_domain(&self) -> bool {
        self.authorized
    }

    fn set_is_authorized_domain(&mut self, authorized: bool) {
        self.authorized = authorized;
    }
}

/// Any driver-created input resource (`ResourcePtr` from
/// `CreateInputResource`, file inputs deferred).
#[derive(Debug, Clone)]
pub enum InputResource {
    /// Fetched URL (`UrlInputResource`).
    Url(UrlInputResource),
    /// Font-service fetch (`FontServiceInputResource`).
    Font(FontServiceInputResource),
    /// Inline payload (`DataUrlInputResource`).
    DataUrl(DataUrlInputResource),
}

impl InputResource {
    /// Borrows the cache key (URL for data inputs).
    #[must_use]
    pub fn cache_key(&self) -> &str {
        match self {
            InputResource::Url(resource) => resource.cache_key(),
            InputResource::Font(resource) => resource.cache_key(),
            InputResource::DataUrl(resource) => resource.url(),
        }
    }

    /// Borrows the content type, if known.
    #[must_use]
    pub fn content_type(&self) -> Option<ContentType> {
        match self {
            InputResource::Url(resource) => resource.content_type(),
            InputResource::Font(resource) => resource.content_type(),
            InputResource::DataUrl(resource) => resource.content_type(),
        }
    }

    /// Borrows the charset, if known (empty for data URLs).
    #[must_use]
    pub fn charset(&self) -> &str {
        match self {
            InputResource::Url(resource) => resource.charset(),
            InputResource::Font(resource) => resource.charset(),
            InputResource::DataUrl(_) => "",
        }
    }

    /// Borrows the response headers.
    #[must_use]
    pub fn response_headers(&self) -> &ResponseHeaders {
        match self {
            InputResource::Url(resource) => resource.response_headers(),
            InputResource::Font(resource) => resource.response_headers(),
            InputResource::DataUrl(resource) => resource.response_headers(),
        }
    }

    /// Reads the (uncompressed) contents.
    #[must_use]
    pub fn contents(&self) -> Vec<u8> {
        match self {
            InputResource::Url(resource) => resource.contents(),
            InputResource::Font(resource) => resource.contents(),
            InputResource::DataUrl(resource) => resource.contents().to_vec(),
        }
    }

    /// Whether the resource is fresh and proxy-cacheable.
    #[must_use]
    pub fn is_valid_and_cacheable(&self) -> bool {
        match self {
            InputResource::Url(resource) => resource.is_valid_and_cacheable(),
            InputResource::Font(resource) => resource.is_valid_and_cacheable(),
            InputResource::DataUrl(resource) => resource.is_valid_and_cacheable(),
        }
    }

    /// Whether the status is 200 (`HttpStatusOk`).
    #[must_use]
    pub fn http_status_ok(&self) -> bool {
        self.response_headers().status_code() == 200
    }

    /// Whether contents are loaded (`loaded`).
    #[must_use]
    pub fn loaded(&self) -> bool {
        self.response_headers().status_code() != 0
    }

    /// Whether the loaded resource may rewrite (`IsSafeToRewrite`).
    #[must_use]
    pub fn is_safe_to_rewrite(&self, rewrite_uncacheable: bool) -> (bool, String) {
        match self {
            InputResource::Url(resource) => resource.is_safe_to_rewrite(rewrite_uncacheable),
            InputResource::Font(resource) => resource.is_safe_to_rewrite(rewrite_uncacheable),
            // Data URLs synthesize their headers; validity is enough.
            InputResource::DataUrl(resource) => {
                if resource.is_valid_and_cacheable() {
                    (true, String::new())
                } else {
                    (false, format!("preventing rewriting of {}", resource.url()))
                }
            }
        }
    }

    /// Records validity watermarks (`FillInPartitionInputInfo`).
    pub fn fill_in_partition_input_info(
        &self,
        include_hash: bool,
        input: &mut super::cacheable_resource::InputInfo,
    ) {
        match self {
            InputResource::Url(resource) => {
                resource.fill_in_partition_input_info(include_hash, input)
            }
            InputResource::Font(resource) => {
                resource.fill_in_partition_input_info(include_hash, input)
            }
            InputResource::DataUrl(_) => {
                input.info_type = super::cacheable_resource::InputInfoType::AlwaysValid;
            }
        }
    }

    /// Content hash (`ContentsHash`).
    #[must_use]
    pub fn contents_hash(&self) -> String {
        match self {
            InputResource::Url(resource) => resource.contents_hash(),
            InputResource::Font(resource) => resource.contents_hash(),
            InputResource::DataUrl(resource) => {
                use ps_cache::hash::{Hasher, Md5Hasher};
                Md5Hasher::with_size(21).hash(resource.contents())
            }
        }
    }

    /// Fetch outcome classification.
    #[must_use]
    pub fn fetch_response_status(&self) -> ps_cache::ttl::FetchStatus {
        match self {
            InputResource::Url(resource) => resource.fetch_response_status(),
            InputResource::Font(resource) => resource.fetch_response_status(),
            InputResource::DataUrl(_) => ps_cache::ttl::FetchStatus::NotSet,
        }
    }

    /// Whether the last load held its fetch (inconclusive, nothing
    /// remembered). Data URLs load inline and never pend.
    #[must_use]
    pub fn load_pending(&self) -> bool {
        match self {
            InputResource::Url(resource) => resource.load_pending(),
            InputResource::Font(resource) => resource.load_pending(),
            InputResource::DataUrl(_) => false,
        }
    }

    /// Cache expiry watermark.
    #[must_use]
    pub fn cache_expiration_time_ms(&self) -> i64 {
        self.response_headers().cache_expiration_time_ms()
    }

    /// Marks background fetching (`set_is_background_fetch`;
    /// meaningless for data URLs).
    pub fn set_is_background_fetch(&mut self, background: bool) {
        match self {
            InputResource::Url(resource) => {
                resource.set_is_background_fetch(background);
            }
            InputResource::Font(resource) => {
                resource.set_is_background_fetch(background);
            }
            InputResource::DataUrl(_) => {}
        }
    }

    /// Loads, completing inline (`LoadAsync`).
    pub fn load(
        &mut self,
        policy: NotCacheablePolicy,
        ctx: &mut RequestContext,
        callback: &mut dyn LoadCallback,
    ) {
        match self {
            InputResource::Url(resource) => resource.load(policy, ctx, callback),
            InputResource::Font(resource) => resource.load(policy, ctx, callback),
            // Data URLs arrive loaded; report their validity.
            InputResource::DataUrl(resource) => {
                let _ = policy;
                let _ = ctx;
                callback.done(false, resource.is_valid_and_cacheable())
            }
        }
    }
}

impl Resource for InputResource {
    fn url(&self) -> &str {
        match self {
            InputResource::Url(resource) => resource.url(),
            InputResource::Font(resource) => resource.url(),
            InputResource::DataUrl(resource) => resource.url(),
        }
    }

    fn is_authorized_domain(&self) -> bool {
        match self {
            InputResource::Url(resource) => resource.is_authorized_domain(),
            InputResource::Font(resource) => resource.is_authorized_domain(),
            InputResource::DataUrl(resource) => resource.is_authorized_domain(),
        }
    }

    fn set_is_authorized_domain(&mut self, authorized: bool) {
        match self {
            InputResource::Url(resource) => resource.set_is_authorized_domain(authorized),
            InputResource::Font(resource) => resource.set_is_authorized_domain(authorized),
            InputResource::DataUrl(resource) => resource.set_is_authorized_domain(authorized),
        }
    }
}

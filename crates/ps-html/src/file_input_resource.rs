//! File-backed input resources (`FileInputResource`).
//!
//! Loads a resource straight from disk when the file-load policy
//! maps its URL (`LoadFromFile[Match]` plus rules). Like C++, the
//! resource never touches the HTTP cache (reloaded per request),
//! synthesizes 200 + content-type + date/caching + last-modified
//! headers, and reports invalid — for HTTP fallback — when the file
//! is missing, unreadable, over the size cap, not a regular file,
//! or lacks a usable mtime. All filesystem failures are graceful
//! `false` outcomes, never panics.
//!
//! Divergences from C++, all deliberate: non-regular files (devices,
//! pipes, directories) are refused up front, where C++ would open
//! and fail (or block) mid-read; the input-content hash is always
//! recorded, where C++ fills it only with a filesystem metadata
//! cache (the port's validity check compares against it); and the
//! missing-content-type error has nowhere to log (no handler is
//! threaded here), so the header is silently omitted — reachable
//! only when the URL itself lacks an extension, since the policy
//! gate guarantees a typed filename.
#![forbid(unsafe_code)]

use ps_http::content_type::ContentType;
use ps_http::request::RequestContext;
use ps_http::response::{HttpOptions, ResponseHeaders};

use super::cacheable_resource::{InputInfo, InputInfoType, LoadCallback, NotCacheablePolicy};
use super::resource::Resource;

/// A file-backed input resource (`FileInputResource`).
#[derive(Debug, Clone)]
pub struct FileInputResource {
    url: String,
    filename: String,
    content_type: Option<ContentType>,
    contents: Vec<u8>,
    headers: ResponseHeaders,
    authorized: bool,
    last_modified_time_sec: i64,
    max_file_size: i64,
    file_ttl_ms: Option<i64>,
    now_ms: i64,
}

impl FileInputResource {
    /// Builds an unloaded file resource: `content_type` comes from
    /// the URL leaf (like C++, may be `None`), `file_ttl_ms` carries
    /// an explicit `LoadFromFileCacheTtlMs` (else the implicit TTL
    /// applies), and `now_ms` stamps the `Date` header at load.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn make(
        url: &str,
        filename: &str,
        content_type: Option<ContentType>,
        authorized: bool,
        max_file_size: i64,
        file_ttl_ms: Option<i64>,
        now_ms: i64,
    ) -> Self {
        FileInputResource {
            url: url.to_string(),
            filename: filename.to_string(),
            content_type,
            contents: Vec::new(),
            headers: ResponseHeaders::new(HttpOptions::default()),
            authorized,
            last_modified_time_sec: 0,
            max_file_size,
            file_ttl_ms,
            now_ms,
        }
    }

    /// Absolute URL of the resource.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Mapped filesystem path.
    #[must_use]
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Cache key (the URL, like C++).
    #[must_use]
    pub fn cache_key(&self) -> &str {
        &self.url
    }

    /// Content type from the URL leaf, if it mapped to one.
    #[must_use]
    pub fn content_type(&self) -> Option<ContentType> {
        self.content_type
    }

    /// Loaded payload bytes (empty until [`FileInputResource::load`]).
    #[must_use]
    pub fn contents(&self) -> &[u8] {
        &self.contents
    }

    /// Borrows the synthesized headers.
    #[must_use]
    pub fn response_headers(&self) -> &ResponseHeaders {
        &self.headers
    }

    /// File mtime in seconds (0 until loaded).
    #[must_use]
    pub fn last_modified_time_sec(&self) -> i64 {
        self.last_modified_time_sec
    }

    /// Whether the file loaded with a 200 (`IsValidAndCacheable`).
    #[must_use]
    pub fn is_valid_and_cacheable(&self) -> bool {
        self.headers.status_code() == 200
    }

    /// Whether contents are loaded (`loaded`).
    #[must_use]
    pub fn loaded(&self) -> bool {
        self.headers.status_code() != 0
    }

    /// Content hash (`ContentsHash`).
    #[must_use]
    pub fn contents_hash(&self) -> String {
        use ps_cache::hash::{Hasher, Md5Hasher};
        Md5Hasher::with_size(21).hash(&self.contents)
    }

    /// Records validity watermarks (`FillInPartitionInputInfo`):
    /// file-based type, mtime, filename, and content hash.
    pub fn fill_in_partition_input_info(&self, _include_hash: bool, input: &mut InputInfo) {
        input.info_type = InputInfoType::FileBased;
        input.last_modified_time_ms = Some(self.last_modified_time_sec * 1000);
        input.filename = Some(self.filename.clone());
        input.input_content_hash = Some(self.contents_hash());
    }

    /// Loads the file from disk, completing inline (`LoadAndCallback`):
    /// mtime first (missing/unusable fails fast), then a size-capped
    /// read; failures clear the resource and report invalid so the
    /// caller falls back to HTTP, like C++.
    pub fn load(
        &mut self,
        _policy: NotCacheablePolicy,
        _ctx: &mut RequestContext,
        callback: &mut dyn LoadCallback,
    ) {
        if !self.loaded() {
            let mut contents = Vec::new();
            if let Some(mtime_sec) = read_capped(&self.filename, self.max_file_size, &mut contents)
            {
                self.last_modified_time_sec = mtime_sec;
                self.contents = contents;
                self.set_default_headers();
            } else {
                self.contents.clear();
                self.headers = ResponseHeaders::new(HttpOptions::default());
                self.last_modified_time_sec = 0;
            }
        }
        callback.done(false, self.loaded());
    }

    /// Synthesizes 200 + content-type + date/caching + last-modified
    /// headers (`SetDefaultHeaders`).
    fn set_default_headers(&mut self) {
        self.headers.set_first_line(1, 1, 200, "OK");
        self.headers.headers_mut().remove_all("Content-Type");
        if let Some(content_type) = self.content_type {
            self.headers
                .headers_mut()
                .add("Content-Type", content_type.mime_type());
        }
        let ttl_ms = self
            .file_ttl_ms
            .unwrap_or_else(|| self.headers.implicit_cache_ttl_ms());
        self.headers.set_date_and_caching(self.now_ms, ttl_ms, "");
        self.headers
            .set_last_modified(self.last_modified_time_sec * 1000);
        self.headers.compute_caching();
    }
}

impl Resource for FileInputResource {
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

/// Stats a file and reads it under a size cap (negative cap means
/// unlimited, like C++ `kUnlimitedSize`): returns the mtime in
/// seconds on success. Missing files, non-regular files, unset or
/// pre-epoch mtimes, over-cap sizes, and read errors all fail
/// gracefully. The read itself is capped, so a file that grows
/// between stat and read cannot blow the bound.
fn read_capped(filename: &str, max_file_size: i64, out: &mut Vec<u8>) -> Option<i64> {
    let metadata = std::fs::metadata(filename).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let mtime_sec = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|age| age.as_secs() as i64)
        .filter(|secs| *secs != 0)?;
    if max_file_size >= 0 && metadata.len() > max_file_size as u64 {
        return None;
    }
    let file = std::fs::File::open(filename).ok()?;
    {
        use std::io::Read;
        let limit = if max_file_size >= 0 {
            max_file_size as u64 + 1
        } else {
            u64::MAX
        };
        file.take(limit).read_to_end(out).ok()?;
    }
    if max_file_size >= 0 && out.len() as u64 > max_file_size as u64 {
        out.clear();
        return None;
    }
    Some(mtime_sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_http::content_type::extension_to_content_type;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Writes a uniquely-named temp file, returning its path.
    fn temp_file(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("ps_file_input_{}_{}_{name}", std::process::id(), id));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn load_it(resource: &mut FileInputResource) -> bool {
        let mut callback = super::super::cacheable_resource::MockLoadCallback::new();
        let mut ctx = RequestContext::new(HttpOptions::default());
        resource.load(
            NotCacheablePolicy::LoadEvenIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        assert!(callback.done());
        callback.success()
    }

    fn make_for(path: &std::path::Path) -> FileInputResource {
        FileInputResource::make(
            "http://example.com/a.css",
            path.to_str().unwrap(),
            extension_to_content_type("a.css"),
            true,
            1024,
            None,
            1_700_000_000_000,
        )
    }

    #[test]
    fn loads_valid_file_with_headers() {
        let path = temp_file("ok.css", b"a{color:red}");
        let mut resource = make_for(&path);
        assert!(!resource.loaded());
        assert!(load_it(&mut resource));
        assert!(resource.loaded());
        assert!(resource.is_valid_and_cacheable());
        assert_eq!(resource.contents(), b"a{color:red}");
        assert_eq!(resource.response_headers().status_code(), 200);
        assert_eq!(
            resource
                .response_headers()
                .headers()
                .lookup1("Content-Type")
                .as_deref(),
            Some("text/css")
        );
        assert!(resource.last_modified_time_sec() > 0);
        // Implicit 5-minute TTL (explicit unset).
        assert_eq!(
            resource
                .response_headers()
                .headers()
                .lookup1("Cache-Control")
                .as_deref(),
            Some("max-age=300")
        );
        // Partition watermark carries file identity.
        let mut info = InputInfo::default();
        resource.fill_in_partition_input_info(false, &mut info);
        assert_eq!(info.info_type, InputInfoType::FileBased);
        assert_eq!(info.filename.as_deref(), path.to_str());
        assert_eq!(
            info.last_modified_time_ms,
            Some(resource.last_modified_time_sec() * 1000)
        );
        assert_eq!(info.input_content_hash, Some(resource.contents_hash()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn explicit_ttl_wins() {
        let path = temp_file("ttl.css", b"a{}");
        let mut resource = FileInputResource::make(
            "http://example.com/a.css",
            path.to_str().unwrap(),
            extension_to_content_type("a.css"),
            true,
            1024,
            Some(60_000),
            1_700_000_000_000,
        );
        assert!(load_it(&mut resource));
        assert_eq!(
            resource
                .response_headers()
                .headers()
                .lookup1("Cache-Control")
                .as_deref(),
            Some("max-age=60")
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn typeless_url_loads_without_content_type() {
        let path = temp_file("notype.css", b"a{}");
        let mut resource = FileInputResource::make(
            "http://example.com/no-extension",
            path.to_str().unwrap(),
            None,
            true,
            1024,
            None,
            1_700_000_000_000,
        );
        assert!(load_it(&mut resource));
        assert!(resource.is_valid_and_cacheable());
        assert!(resource
            .response_headers()
            .headers()
            .lookup1("Content-Type")
            .is_none());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn failures_are_graceful_and_retryable() {
        // Missing file.
        let mut missing = make_for(std::path::Path::new(
            "/nonexistent-dir-ps-file-input/missing.css",
        ));
        assert!(!load_it(&mut missing));
        assert!(!missing.loaded());
        assert!(!missing.is_valid_and_cacheable());
        assert!(missing.contents().is_empty());
        // Retries fail the same way (no panic, no latch).
        assert!(!load_it(&mut missing));
        // Directories refuse.
        let dir = std::env::temp_dir();
        let mut dir_resource = make_for(&dir);
        assert!(!load_it(&mut dir_resource));
        // Over-cap files refuse.
        let big = temp_file("big.css", &[b'x'; 2048]);
        let mut over = make_for(&big);
        assert!(!load_it(&mut over));
        assert!(over.contents().is_empty());
        std::fs::remove_file(&big).unwrap();
    }

    #[test]
    fn negative_cap_is_unlimited() {
        let big = temp_file("unlimited.css", &[b'y'; 4096]);
        let mut resource = FileInputResource::make(
            "http://example.com/a.css",
            big.to_str().unwrap(),
            extension_to_content_type("a.css"),
            true,
            -1,
            None,
            1_700_000_000_000,
        );
        assert!(load_it(&mut resource));
        assert_eq!(resource.contents().len(), 4096);
        std::fs::remove_file(&big).unwrap();
    }

    #[test]
    fn empty_file_loads() {
        let path = temp_file("empty.css", b"");
        let mut resource = make_for(&path);
        assert!(load_it(&mut resource));
        assert!(resource.is_valid_and_cacheable());
        assert!(resource.contents().is_empty());
        std::fs::remove_file(&path).unwrap();
    }
}

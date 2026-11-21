//! Same-server file fetcher (subrequest substitute over `root`).
//!
//! C++ fetches same-server inputs through nginx subrequests; the
//! sync bridge cannot park for them, so this fetcher reads the
//! server root directly (zero-config where the C++ needs no
//! `LoadFromFile` mapping either). Mapping is strict: `http`
//! URLs whose host matches a server name, paths unescaped then
//! jailed under the canonical root (`..`, NUL, and escaping
//! symlinks fail closed), regular files under the cacheable
//! size cap. Served headers mirror what static would send
//! (200, extension content-type, length, mtime
//! `Last-Modified`, nginx-format ETag, `Accept-Ranges`,
//! current `Date`) with NO caching headers, so downstream
//! cacheability verdicts compute exactly like C++'s
//! (synthesizing a TTL here would over-optimize inputs C++
//! refuses as uncacheable).
//!
//! Known gaps: per-location roots/aliases/`try_files`/index
//! resolution stay unmapped (server root only); regex server
//! names never match; non-UTF8 filenames fail closed;
//! `https` fails (no TLS); ports are ignored; extension table
//! approximates mime.types.

use ps_http::content_type::extension_to_content_type;
use ps_http::fetch::{AsyncFetch, AsyncFetcher};
use ps_http::google_url::GoogleUrl;
use ps_http::names::header;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Same-server file fetcher.
#[derive(Debug)]
pub struct FileFetcher {
    /// Canonical server root (jail).
    root: PathBuf,
    /// Lowercased server names (`*.` wildcards kept verbatim).
    names: Vec<String>,
    /// Cacheable size cap (`max_cacheable_response_content_length`).
    max_file_bytes: i64,
}

impl FileFetcher {
    /// Builds a fetcher over `root` (must be absolute, static,
    /// and canonicalizable) for `names`. `None` fails closed to
    /// the unavailable fetcher.
    #[must_use]
    pub fn new(root: &str, names: &[String], max_file_bytes: i64) -> Option<Self> {
        if root.is_empty() || root.contains('$') {
            return None;
        }
        let path = Path::new(root);
        if !path.is_absolute() {
            return None;
        }
        let root = path.canonicalize().ok()?;
        let names = names.iter().map(|name| name.to_lowercase()).collect();
        Some(FileFetcher {
            root,
            names,
            max_file_bytes,
        })
    }

    /// Whether `host` matches a server name (exact or `*.`
    /// wildcard over subdomains; regex names never match).
    fn host_matches(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        // URL hosts never carry the port here (`GoogleUrl::host`
        // strips it); trailing dots compare verbatim and fail.
        self.names.iter().any(|name| {
            if let Some(suffix) = name.strip_prefix("*.") {
                // Single level only (conservative: deeper valid
                // subdomains fail closed rather than risk a
                // mismatch with nginx's own matching).
                host.strip_suffix(suffix).is_some_and(|head| {
                    head.len() > 1 && head.ends_with('.') && !head[..head.len() - 1].contains('.')
                })
            } else if name.starts_with('~') {
                false
            } else {
                host == *name
            }
        })
    }

    /// Whether this fetcher maps a URL (pure, no I/O): the
    /// chain leg routes on this, so mapped names never fall
    /// through to the origin (a mapped-but-missing name answers
    /// the file leg's 404 rather than loopbacking).
    #[must_use]
    pub fn can_fetch(&self, url: &str) -> bool {
        self.join_root(url).is_some()
    }

    /// Maps a URL to a jailed file path (pure join + checks; the
    /// canonicalization read is the only I/O).
    /// Lexically joins a URL path under the root (no filesystem
    /// touch): shape-valid, jail-contained-or-missing names.
    /// `None` only for malformed URLs, foreign hosts, and
    /// `..` escapes.
    fn join_root(&self, url: &str) -> Option<PathBuf> {
        let gurl = GoogleUrl::new(url);
        if !gurl.is_web_valid() || gurl.scheme() != "http" {
            return None;
        }
        if !self.host_matches(&gurl.host()) {
            return None;
        }
        let raw_path = gurl.path_sans_query();
        let unescaped = GoogleUrl::unescape_ignore_plus(raw_path);
        if unescaped.contains(&0) {
            return None;
        }
        let path = String::from_utf8(unescaped).ok()?;
        let mut joined = self.root.clone();
        let mut segments = 0;
        for segment in path.split('/') {
            if segment.is_empty() || segment == "." {
                continue;
            }
            if segment == ".." {
                return None;
            }
            joined.push(segment);
            segments += 1;
        }
        if segments == 0 {
            return None;
        }
        Some(joined)
    }

    fn resolve(&self, url: &str) -> Option<PathBuf> {
        let joined = self.join_root(url)?;
        let canonical = joined.canonicalize().ok()?;
        if !canonical.starts_with(&self.root) {
            return None;
        }
        if !canonical.is_file() {
            return None;
        }
        Some(canonical)
    }

    /// Reports a mapped-but-missing name as the subrequest's
    /// 404 status (downstream classifies Error4xx like C++).
    /// The transport still fails (`Done(false)`), matching the
    /// fetcher's existing missing-file contract.
    fn serve_not_found(&self, fetch: &mut dyn AsyncFetch) {
        let headers = fetch.response_headers_mut();
        headers.set_status_and_reason(404);
        headers.headers_mut().remove_all(header::CONTENT_TYPE);
        headers.headers_mut().add(header::CONTENT_TYPE, "text/html");
        headers.set_date(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|age| age.as_millis() as i64)
                .unwrap_or(0),
        );
        fetch.headers_complete();
        fetch.handle_done(false);
    }
}

impl AsyncFetcher for FileFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        let Some(path) = self.resolve(url) else {
            // Mapped-but-missing names answer the subrequest's
            // 404 (completed transaction); malformed URLs and
            // foreign hosts fail the transport instead.
            if self.join_root(url).is_none() {
                fetch.handle_done(false);
                return;
            }
            self.serve_not_found(fetch);
            return;
        };
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|age| age.as_millis() as i64);
        // A mapped-but-missing file synthesizes the subrequest's
        // 404 (completed transaction, not a transport failure),
        // so downstream classifies Error4xx like C++.
        let (Some(mtime_ms), Ok(bytes)) = (modified, std::fs::read(&path)) else {
            self.serve_not_found(fetch);
            return;
        };
        if self.max_file_bytes >= 0 && bytes.len() as i64 > self.max_file_bytes {
            fetch.handle_done(false);
            return;
        }
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0);
        {
            // Static-faithful headers (what a subrequest would
            // see): no caching headers, so cacheability verdicts
            // compute exactly like C++'s.
            let headers = fetch.response_headers_mut();
            headers.set_status_and_reason(200);
            headers.headers_mut().remove_all(header::CONTENT_TYPE);
            if let Some(content_type) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(extension_to_content_type)
            {
                headers
                    .headers_mut()
                    .add(header::CONTENT_TYPE, content_type.mime_type());
            }
            headers.set_date(now_ms);
            headers.set_last_modified(mtime_ms);
            headers.set_original_content_length(bytes.len() as i64);
            headers.headers_mut().add(
                header::ETAG,
                &format!("\"{:x}-{:x}\"", mtime_ms / 1000, bytes.len()),
            );
            headers.headers_mut().add(header::ACCEPT_RANGES, "bytes");
            headers
                .headers_mut()
                .add(header::CONTENT_LENGTH, &bytes.len().to_string());
            headers.compute_caching();
        }
        fetch.headers_complete();
        let ok = fetch.handle_write(&bytes);
        fetch.handle_done(ok);
    }

    fn supports_https(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_http::fetch::BufferingFetch;
    use ps_http::request::RequestContext;
    use ps_http::response::HttpOptions;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Scratch root with `files` written (`name` → bytes).
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(files: &[(&str, &[u8])]) -> Self {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("psff-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for (name, bytes) in files {
                let path = dir.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&path, bytes).unwrap();
            }
            Scratch { dir }
        }

        fn fetcher(&self) -> FileFetcher {
            FileFetcher::new(
                self.dir.to_str().unwrap(),
                &["localhost".to_string()],
                16 * 1024 * 1024,
            )
            .expect("scratch root fetches")
        }

        fn fetch(&self, fetcher: &FileFetcher, url: &str) -> BufferingFetch {
            let mut fetch =
                BufferingFetch::new(url, RequestContext::new(HttpOptions::default_for_tests()));
            fetcher.fetch(url, &mut fetch);
            fetch
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn round_trip_serves_file_headers() {
        let scratch = Scratch::new(&[("a.css", b"a{color:red}")]);
        let fetcher = scratch.fetcher();
        let fetch = scratch.fetch(&fetcher, "http://localhost/a.css");
        assert!(fetch.success());
        assert_eq!(b"a{color:red}".as_slice(), fetch.body());
        let headers = fetch.response_headers();
        assert_eq!(200, headers.status_code());
        assert_eq!(
            Some("text/css".to_string()),
            headers.headers().lookup1(header::CONTENT_TYPE)
        );
        // Mtime round-trips through Last-Modified, and the ETag
        // matches nginx's `"<hexmtime>-<hexsize>"` shape.
        let mtime = std::fs::metadata(scratch.dir.join("a.css"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert_eq!(mtime / 1000 * 1000, headers.last_modified_time_ms());
        assert_eq!(
            format!("\"{:x}-{:x}\"", mtime / 1000, 12),
            headers.headers().lookup_joined(header::ETAG)
        );
    }

    #[test]
    fn static_headers_derive_implicit_caching() {
        // Static-faithful fetch carries no caching headers, but
        // caching computation derives the implicit TTL exactly
        // like C++ (same inputs, same code).
        let scratch = Scratch::new(&[("a.css", b"a{}")]);
        let fetcher = scratch.fetcher();
        let fetch = scratch.fetch(&fetcher, "http://localhost/a.css");
        assert!(fetch.success());
        let headers = fetch.response_headers();
        assert_eq!(
            "max-age=300",
            headers.headers().lookup_joined(header::CACHE_CONTROL)
        );
        assert_eq!(
            "bytes",
            headers.headers().lookup_joined(header::ACCEPT_RANGES)
        );
        assert_eq!("3", headers.headers().lookup_joined(header::CONTENT_LENGTH));
    }

    #[test]
    fn jail_rejects_escapes() {
        let outside = Scratch::new(&[("secret.css", b"top")]);
        let inside = Scratch::new(&[("a.css", b"a{}")]);
        let fetcher = inside.fetcher();
        // `..` after unescape, plain and encoded.
        for url in [
            "http://localhost/../secret.css",
            "http://localhost/%2e%2e/secret.css",
            "http://localhost/a/../../secret.css",
            "http://localhost/%2E%2E%2Fsecret.css",
        ] {
            assert!(!inside.fetch(&fetcher, url).success(), "{url}");
        }
        // The outside file exists (the test would pass vacuously
        // without it).
        assert!(outside.dir.join("secret.css").is_file());
        // NUL fails closed.
        assert!(!inside
            .fetch(&fetcher, "http://localhost/a%00.css")
            .success());
    }

    #[test]
    fn symlink_escape_fails_closed() {
        let outside = Scratch::new(&[("secret.css", b"top")]);
        let inside = Scratch::new(&[("a.css", b"a{}")]);
        std::os::unix::fs::symlink(outside.dir.join("secret.css"), inside.dir.join("link.css"))
            .unwrap();
        let fetcher = inside.fetcher();
        assert!(!inside
            .fetch(&fetcher, "http://localhost/link.css")
            .success());
    }

    #[test]
    fn host_scheme_and_shape_gate() {
        let scratch = Scratch::new(&[("a.css", b"a{}")]);
        let fetcher = scratch.fetcher();
        // Wrong host, https, garbage: all fail.
        for url in [
            "http://evil.example/a.css",
            "https://localhost/a.css",
            "not a url",
            "http://localhost/",
            "http://localhost",
        ] {
            assert!(!scratch.fetch(&fetcher, url).success(), "{url}");
        }
        // Uppercase host matches (DNS case-insensitive).
        assert!(scratch.fetch(&fetcher, "http://LOCALHOST/a.css").success());
        // Query strings strip (file bytes don't vary by them).
        assert!(scratch
            .fetch(&fetcher, "http://localhost/a.css?v=1&x=2")
            .success());
    }

    #[test]
    fn wildcard_matches_one_level() {
        let scratch = Scratch::new(&[("a.css", b"a{}")]);
        let fetcher = FileFetcher::new(
            scratch.dir.to_str().unwrap(),
            &["*.example.com".to_string()],
            16 * 1024 * 1024,
        )
        .expect("scratch root fetches");
        assert!(scratch
            .fetch(&fetcher, "http://www.example.com/a.css")
            .success());
        // Bare domain, deeper levels, and lookalikes fail.
        for url in [
            "http://example.com/a.css",
            "http://a.b.example.com/a.css",
            "http://notexample.com/a.css",
            "http://example.com.evil.com/a.css",
        ] {
            assert!(!scratch.fetch(&fetcher, url).success(), "{url}");
        }
    }

    #[test]
    fn missing_dirs_caps_and_types() {
        let scratch = Scratch::new(&[("a.css", b"a{}"), ("blob.xyz", b"??")]);
        std::fs::create_dir_all(scratch.dir.join("sub")).unwrap();
        std::fs::write(scratch.dir.join("sub/index.html"), b"<html>").unwrap();
        let fetcher = scratch.fetcher();
        // Missing files and directories (no index resolution) fail.
        assert!(!scratch
            .fetch(&fetcher, "http://localhost/nope.css")
            .success());
        assert!(!scratch.fetch(&fetcher, "http://localhost/sub").success());
        assert!(!scratch.fetch(&fetcher, "http://localhost/sub/").success());
        // Unknown extensions serve without a content-type.
        let fetch = scratch.fetch(&fetcher, "http://localhost/blob.xyz");
        assert!(fetch.success());
        assert!(fetch
            .response_headers()
            .headers()
            .lookup1(header::CONTENT_TYPE)
            .is_none());
        // Oversized files fail closed.
        let small = FileFetcher::new(scratch.dir.to_str().unwrap(), &["localhost".to_string()], 2)
            .expect("scratch root fetches");
        assert!(!scratch.fetch(&small, "http://localhost/a.css").success());
    }

    #[test]
    fn construction_rejects_bad_roots() {
        let names = ["localhost".to_string()];
        assert!(FileFetcher::new("/nonexistent-root-psff", &names, 1).is_none());
        assert!(FileFetcher::new("relative/root", &names, 1).is_none());
        assert!(FileFetcher::new("/root/$var", &names, 1).is_none());
        assert!(FileFetcher::new("", &names, 1).is_none());
    }

    /// Full HTML flow over file-backed inputs (mirrors the live
    /// module path: context, driver, CoreFilters, beacon-less
    /// chain): the small stylesheet inlines (verbatim C++).
    #[test]
    fn html_driver_inlines_file_backed_link() {
        use ps_cache::cache::MemoryCache;
        use ps_config::options::RewriteOptions;
        use std::cell::RefCell;

        let scratch = Scratch::new(&[
            ("a.css", b".a { color: red; }\n"),
            (
                "index.html",
                b"<html><head><link rel=\"stylesheet\" href=\"a.css\"></head>\
                  <body>hi</body></html>",
            ),
        ]);
        // Live-likeness probe: a stale context clock
        // (child-init lags file mtimes).
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0)
            - 3_600_000;
        let fetcher = Rc::new(
            FileFetcher::new(
                scratch.dir.to_str().unwrap(),
                &["test.com".to_string()],
                16 * 1024 * 1024,
            )
            .expect("scratch root fetches"),
        );
        let html = crate::rewrite::html_context(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            Arc::new(ps_stats::core::Statistics::new()),
            now_ms,
            fetcher,
        );
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .set_option(
                "RewriteLevel",
                "CoreFilters",
                ps_config::scope::Scope::Directory,
            )
            .unwrap();
        let resource_driver = Rc::new(ps_html::server_context::ResourceDriver::new(
            html,
            options.clone(),
            "http://test.com:18081/index.html",
        ));
        let output = Rc::new(RefCell::new(Vec::new()));
        struct VecSink(Rc<RefCell<Vec<u8>>>);
        impl ps_html::writer::ByteSink for VecSink {
            fn write_bytes(&mut self, bytes: &[u8]) -> bool {
                self.0.borrow_mut().extend_from_slice(bytes);
                true
            }
        }
        let context = ps_html::chain::ChainContext::new(true, false);
        let mut driver = ps_html::driver::Driver::new(
            &options.borrow(),
            &context,
            VecSink(Rc::clone(&output)),
            Some(resource_driver),
        );
        driver.start_parse(b"http://test.com:18081/index.html", false);
        driver.write(
            b"<html><head><link rel=\"stylesheet\" href=\"a.css\"></head><body>hi</body></html>",
        );
        driver.finish();
        let out = String::from_utf8(output.borrow().clone()).unwrap();
        assert_eq!(
            "<html><head><style>.a{color:red}</style></head><body>hi</body></html>",
            out
        );
    }

    #[test]
    fn missing_file_reports_404_status() {
        use ps_http::fetch::AsyncFetch;
        let scratch = Scratch::new(&[("a.css", b".a{color:red}")]);
        let fetcher = scratch.fetcher();
        let fetch = scratch.fetch(&fetcher, "http://localhost/missing.css");
        assert!(fetch.done());
        assert!(!fetch.success());
        assert_eq!(404, fetch.response_headers().status_code());
    }

    #[test]
    fn unmapped_host_still_fails_transport() {
        use ps_http::fetch::AsyncFetch;
        let scratch = Scratch::new(&[("a.css", b".a{color:red}")]);
        let fetcher = scratch.fetcher();
        let fetch = scratch.fetch(&fetcher, "http://elsewhere.test/a.css");
        assert!(fetch.done());
        assert!(!fetch.success());
    }
}

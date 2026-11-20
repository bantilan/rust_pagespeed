//! Same-server file fetcher (`FileInputResource` behavior over `root`).
//!
//! C++ fetches same-server inputs through nginx subrequests; the
//! sync bridge cannot park for them, so this fetcher reads the
//! server root directly (zero-config where the C++ needs no
//! `LoadFromFile` mapping either). Mapping is strict: `http`
//! URLs whose host matches a server name, paths unescaped then
//! jailed under the canonical root (`..`, NUL, and escaping
//! symlinks fail closed), regular files under the cacheable
//! size cap. Served headers mirror
//! `FileInputResource::SetDefaultHeaders` exactly (1.1/200,
//! extension content-type, `SetDateAndCaching` with the
//! file-input TTL, mtime `Last-Modified`).
//!
//! Known gaps: per-location roots/aliases/`try_files`/index
//! resolution stay unmapped (server root only); regex server
//! names never match; non-UTF8 filenames fail closed;
//! `https` fails (no TLS); ports are ignored.

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
    /// File-input TTL (`load_from_file_cache_ttl_ms`).
    file_cache_ttl_ms: i64,
    /// Whether the file-input TTL was explicitly set.
    file_cache_ttl_set: bool,
    /// Fallback TTL (`implicit_cache_ttl_ms`).
    implicit_cache_ttl_ms: i64,
}

impl FileFetcher {
    /// Builds a fetcher over `root` (must be absolute, static,
    /// and canonicalizable) for `names`. `None` fails closed to
    /// the unavailable fetcher.
    #[must_use]
    pub fn new(
        root: &str,
        names: &[String],
        max_file_bytes: i64,
        file_cache_ttl_ms: i64,
        file_cache_ttl_set: bool,
        implicit_cache_ttl_ms: i64,
    ) -> Option<Self> {
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
            file_cache_ttl_ms,
            file_cache_ttl_set,
            implicit_cache_ttl_ms,
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

    /// Maps a URL to a jailed file path (pure join + checks; the
    /// canonicalization read is the only I/O).
    fn resolve(&self, url: &str) -> Option<PathBuf> {
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
        let canonical = joined.canonicalize().ok()?;
        if !canonical.starts_with(&self.root) {
            return None;
        }
        if !canonical.is_file() {
            return None;
        }
        Some(canonical)
    }

    /// File-input TTL (`SetDefaultHeaders` selection).
    fn cache_ttl_ms(&self) -> i64 {
        if self.file_cache_ttl_set {
            self.file_cache_ttl_ms
        } else {
            self.implicit_cache_ttl_ms
        }
    }
}

impl AsyncFetcher for FileFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        let Some(path) = self.resolve(url) else {
            fetch.handle_done(false);
            return;
        };
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|age| age.as_millis() as i64);
        // C++ fails the load when the mtime or the read fails.
        let (Some(mtime_ms), Ok(bytes)) = (modified, std::fs::read(&path)) else {
            fetch.handle_done(false);
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
            headers.set_date_and_caching(now_ms, self.cache_ttl_ms(), "");
            headers.set_last_modified(mtime_ms);
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
    use std::sync::atomic::{AtomicU64, Ordering};

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
                5 * 60 * 1000,
                false,
                5 * 60 * 1000,
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
        assert_eq!(
            "max-age=300",
            headers.headers().lookup_joined(header::CACHE_CONTROL)
        );
        // Mtime round-trips through Last-Modified.
        let mtime = std::fs::metadata(scratch.dir.join("a.css"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert_eq!(mtime / 1000 * 1000, headers.last_modified_time_ms());
    }

    #[test]
    fn explicit_file_ttl_wins() {
        let scratch = Scratch::new(&[("a.css", b"a{}")]);
        let fetcher = FileFetcher::new(
            scratch.dir.to_str().unwrap(),
            &["localhost".to_string()],
            16 * 1024 * 1024,
            60_000,
            true,
            5 * 60 * 1000,
        )
        .expect("scratch root fetches");
        let fetch = scratch.fetch(&fetcher, "http://localhost/a.css");
        assert!(fetch.success());
        assert_eq!(
            "max-age=60",
            fetch
                .response_headers()
                .headers()
                .lookup_joined(header::CACHE_CONTROL)
        );
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
            5 * 60 * 1000,
            false,
            5 * 60 * 1000,
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
        let small = FileFetcher::new(
            scratch.dir.to_str().unwrap(),
            &["localhost".to_string()],
            2,
            5 * 60 * 1000,
            false,
            5 * 60 * 1000,
        )
        .expect("scratch root fetches");
        assert!(!scratch.fetch(&small, "http://localhost/a.css").success());
    }

    #[test]
    fn construction_rejects_bad_roots() {
        let names = ["localhost".to_string()];
        assert!(FileFetcher::new("/nonexistent-root-psff", &names, 1, 1, false, 1).is_none());
        assert!(FileFetcher::new("relative/root", &names, 1, 1, false, 1).is_none());
        assert!(FileFetcher::new("/root/$var", &names, 1, 1, false, 1).is_none());
        assert!(FileFetcher::new("", &names, 1, 1, false, 1).is_none());
    }
}

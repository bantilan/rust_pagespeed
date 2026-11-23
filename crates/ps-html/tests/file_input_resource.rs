//! File-load integration tests: `LoadFromFile*` configuration through
//! `create_input_resource_unchecked`, covering mappings, rules, gates,
//! traversal safety, and graceful file failures.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_config::options::RewriteOptions;
use ps_html::resource::{InputResource, Resource};
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;

const START_MS: i64 = 1_700_000_000_000;

/// Unique temp dir for one test (removed by the test at the end).
fn temp_dir(name: &str) -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ps_file_load_{}_{}_{name}",
        std::process::id(),
        START_MS
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &std::path::Path, name: &str, contents: &[u8]) {
    if let Some(parent) = std::path::Path::new(name).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(dir.join(parent)).unwrap();
        }
    }
    std::fs::write(dir.join(name), contents).unwrap();
}

/// Driver with a file-load mapping onto `dir`.
fn mapped_driver(dir: &std::path::Path) -> Rc<ResourceDriver> {
    let metadata: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let http: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let server = Rc::new(ServerContext::new_with_metadata(
        http,
        metadata,
        Arc::new(MockUrlFetcher::new()),
        START_MS,
    ));
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .parse_and_set_2(
            "LoadFromFile",
            "http://example.com/res/",
            dir.to_str().unwrap(),
        )
        .unwrap();
    Rc::new(ResourceDriver::new(
        server,
        options,
        "http://example.com/res/page.html",
    ))
}

fn create(driver: &Rc<ResourceDriver>, url: &str) -> Option<InputResource> {
    driver.create_input_resource_unchecked(&GoogleUrl::new(url), true)
}

#[test]
fn mapped_urls_become_file_resources() {
    let dir = temp_dir("mapped");
    write(&dir, "style.css", b"a{color:red}");
    let driver = mapped_driver(&dir);
    let resource = create(&driver, "http://example.com/res/style.css").expect("resource");
    let InputResource::File(file) = &resource else {
        panic!("must be a file resource, got {resource:?}");
    };
    assert_eq!(file.url(), "http://example.com/res/style.css");
    assert!(file.filename().ends_with("style.css"));
    assert!(file.is_authorized_domain());
    // Loading serves the bytes with synthesized headers.
    let mut resource = resource;
    let mut callback = ps_html::cacheable_resource::MockLoadCallback::new();
    let mut ctx = ps_http::request::RequestContext::new(ps_http::response::HttpOptions::default());
    resource.load(
        ps_html::cacheable_resource::NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(resource.contents(), b"a{color:red}");
    assert!(resource.is_valid_and_cacheable());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unmapped_and_gated_urls_stay_http() {
    let dir = temp_dir("gated");
    write(&dir, "style.css", b"a{}");
    write(&dir, "page.html", b"<html>");
    let driver = mapped_driver(&dir);
    // Other hosts never map.
    assert!(matches!(
        create(&driver, "http://other.com/res/style.css"),
        Some(InputResource::Url(_))
    ));
    // Unknown extensions fail the content-type gate.
    assert!(matches!(
        create(&driver, "http://example.com/res/file.bogus"),
        Some(InputResource::Url(_))
    ));
    // Non-static types fail the gate even when the file exists.
    assert!(matches!(
        create(&driver, "http://example.com/res/page.html"),
        Some(InputResource::Url(_))
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rules_and_regexps_apply() {
    let dir = temp_dir("rules");
    write(&dir, "ok.js", b"1");
    write(&dir, "cgi/run.js", b"2");
    // Disallow the CGI subtree (rules see collapsed filenames).
    let metadata: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let http: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let server = Rc::new(ServerContext::new_with_metadata(
        http,
        metadata,
        Arc::new(MockUrlFetcher::new()),
        START_MS,
    ));
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .parse_and_set_2(
            "LoadFromFile",
            "http://example.com/res/",
            dir.to_str().unwrap(),
        )
        .unwrap();
    options
        .borrow_mut()
        .parse_and_set_2(
            "LoadFromFileRule",
            "Disallow",
            &format!("{}/cgi/", dir.to_str().unwrap()),
        )
        .unwrap();
    let driver = Rc::new(ResourceDriver::new(
        server,
        Rc::clone(&options),
        "http://example.com/res/page.html",
    ));
    assert!(matches!(
        create(&driver, "http://example.com/res/ok.js"),
        Some(InputResource::File(_))
    ));
    assert!(matches!(
        create(&driver, "http://example.com/res/cgi/run.js"),
        Some(InputResource::Url(_))
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn traversal_urls_never_map_to_files() {
    let dir = temp_dir("traversal");
    write(&dir, "style.css", b"a{}");
    // A secret outside the mapped tree.
    let outside = dir.parent().unwrap().join(format!(
        "ps_file_load_{}_{}_secret.css",
        std::process::id(),
        START_MS
    ));
    std::fs::write(&outside, b"secret").unwrap();
    let driver = mapped_driver(&dir);
    for url in [
        "http://example.com/res/../secret.css",
        "http://example.com/res/%2e%2e/secret.css",
        "http://example.com/res/./../secret.css",
    ] {
        let resource = create(&driver, url).expect("resource");
        assert!(
            matches!(resource, InputResource::Url(_)),
            "{url} must not map to a file: {resource:?}"
        );
    }
    // Even the canonical same-name URL stays HTTP (outside the prefix).
    assert!(matches!(
        create(&driver, "http://example.com/secret.css"),
        Some(InputResource::Url(_))
    ));
    std::fs::remove_dir_all(&dir).unwrap();
    let _ = std::fs::remove_file(&outside);
}

#[test]
fn missing_mapped_files_fail_gracefully() {
    let dir = temp_dir("missing");
    let driver = mapped_driver(&dir);
    // Creation succeeds (the file need not exist yet); loading fails clean.
    let mut resource =
        create(&driver, "http://example.com/res/absent.css").expect("resource");
    assert!(matches!(resource, InputResource::File(_)));
    let mut callback = ps_html::cacheable_resource::MockLoadCallback::new();
    let mut ctx = ps_http::request::RequestContext::new(ps_http::response::HttpOptions::default());
    resource.load(
        ps_html::cacheable_resource::NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.done());
    assert!(!callback.success());
    assert!(!resource.is_valid_and_cacheable());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn regexp_mappings_map() {
    let dir = temp_dir("regexp");
    write(&dir, "users/pat/app.js", b"1");
    let metadata: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let http: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let server = Rc::new(ServerContext::new_with_metadata(
        http,
        metadata,
        Arc::new(MockUrlFetcher::new()),
        START_MS,
    ));
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .parse_and_set_2(
            "LoadFromFileMatch",
            "^http://example.com/~([^/]*)/",
            &format!("{}/users/\\1/", dir.to_str().unwrap()),
        )
        .unwrap();
    let driver = Rc::new(ResourceDriver::new(
        server,
        options,
        "http://example.com/page.html",
    ));
    let resource = create(&driver, "http://example.com/~pat/app.js").expect("resource");
    let InputResource::File(file) = &resource else {
        panic!("must be a file resource, got {resource:?}");
    };
    assert!(file.filename().ends_with("users/pat/app.js"), "{}", file.filename());
    std::fs::remove_dir_all(&dir).unwrap();
}

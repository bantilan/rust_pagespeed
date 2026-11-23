//! Output resource contract tests (hand-written: upstream covers
//! naming only through the context suites, which land next).
//!
//! Each case pins one `OutputResource` behavior the context tests
//! will exercise: leaf naming, write hashing, type/extension sync,
//! placeholder URLs, unsigned and signed signatures, and cache keys.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_cache::hash::Hasher;
use ps_cache::namer::ResourceNamer;
use ps_config::options::RewriteOptions;
use ps_html::output_resource::{OutputResource, OutputResourceKind};
use ps_html::resource::Resource;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::content_type::mime_to_content_type;
use ps_http::mock::MockUrlFetcher;

fn fixture() -> (
    Rc<ServerContext>,
    Rc<ResourceDriver>,
    Rc<RefCell<RewriteOptions>>,
) {
    let backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let fetcher = Arc::new(MockUrlFetcher::new());
    let server = Rc::new(ServerContext::new(backend, fetcher, 1_700_000_000_000));
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    let driver = Rc::new(ResourceDriver::new(
        server.clone(),
        options.clone(),
        "http://test.com/dir/page.html",
    ));
    (server, driver, options)
}

fn named_full_name() -> ResourceNamer {
    let mut namer = ResourceNamer::new();
    namer.set_id("cf");
    namer.set_name("a.css");
    namer.set_hash("HASH");
    namer.set_ext("css");
    namer
}

#[test]
fn leaf_naming_and_url() {
    let (_server, driver, _options) = fixture();
    let resource = OutputResource::new(
        driver,
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        named_full_name(),
        OutputResourceKind::Rewritten,
    );
    assert_eq!(resource.full_name().encode(), "a.css.pagespeed.cf.HASH.css");
    assert_eq!(resource.name_key(), "http://test.com/dir/cf.a.css");
    // No shards configured: the resolved base carries the leaf.
    assert_eq!(
        resource.url(),
        "http://test.com/dir/a.css.pagespeed.cf.HASH.css"
    );
    assert_eq!(
        resource.http_cache_key(),
        "http://test.com/dir/a.css.pagespeed.cf.HASH.css"
    );
    assert_eq!(resource.decoded_base(), "http://test.com/dir/");
}

#[test]
fn write_hashes_contents() {
    let (_server, driver, _options) = fixture();
    let mut namer = ResourceNamer::new();
    namer.set_id("cf");
    namer.set_name("a.css");
    namer.set_ext("css");
    assert!(namer.hash().is_empty());
    let mut resource = OutputResource::new(
        driver.clone(),
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        namer,
        OutputResourceKind::Rewritten,
    );
    assert!(!resource.has_hash());
    assert!(!resource.writing_complete());
    resource.begin_write().extend_from_slice(b"body{color:red}");
    assert!(!resource.has_hash());
    resource.end_write();
    assert!(resource.writing_complete());
    assert!(resource.has_hash());
    // The hash is the server hasher over the bytes, and the URL
    // carries it.
    let expected = driver.server_context().hasher().hash(b"body{color:red}");
    assert_eq!(resource.full_name().hash(), expected);
    assert!(resource.url().contains(&expected), "{}", resource.url());
    assert_eq!(resource.body(), b"body{color:red}");
}

#[test]
fn set_type_syncs_extension() {
    let (_server, driver, _options) = fixture();
    let mut namer = ResourceNamer::new();
    namer.set_id("jm");
    namer.set_name("a");
    namer.set_hash("HASH");
    namer.set_ext("css");
    let mut resource = OutputResource::new(
        driver,
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        namer,
        OutputResourceKind::Rewritten,
    );
    let js = mime_to_content_type("application/javascript").unwrap();
    resource.set_type(Some(js));
    assert_eq!(resource.suffix(), "js");
    assert!(resource.url().ends_with(".js"), "{}", resource.url());
}

#[test]
fn placeholder_url_without_hash() {
    let (_server, driver, _options) = fixture();
    let mut namer = ResourceNamer::new();
    namer.set_id("cf");
    namer.set_name("a.css");
    namer.set_ext("css");
    let mut resource = OutputResource::new(
        driver,
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        namer,
        OutputResourceKind::Rewritten,
    );
    let url = resource.url_even_if_hash_not_set();
    assert!(url.contains(".0."), "{url}");
    // The placeholder does not stick.
    assert!(!resource.has_hash());
}

#[test]
fn unsigned_signatures_pass_empty() {
    let (_server, driver, _options) = fixture();
    let mut resource = OutputResource::new(
        driver,
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        named_full_name(),
        OutputResourceKind::Rewritten,
    );
    resource.begin_write().extend_from_slice(b"x");
    resource.end_write();
    assert_eq!(resource.compute_signature(), "");
    assert_eq!(resource.full_name().signature(), "");
    assert!(resource.check_signature());
}

#[test]
fn signed_urls_embed_deterministic_signatures() {
    // NOTE: upstream signs the write-time URL (no signature embedded
    // yet) but verifies against the longer served URL, so an
    // end-write/check roundtrip disagrees in C++ too (unpinned
    // upstream — only decode of fixed URLs is tested there). This
    // port replicates the formula exactly (see the vector test in
    // `ps-cache::signature`); here we pin determinism, key
    // sensitivity, and the leniency escape hatch.
    let (_server, driver, options) = fixture();
    options
        .borrow_mut()
        .set_option("UrlSigningKey", "sekret", ps_config::Scope::ProcessStrict)
        .unwrap();
    let mut resource = OutputResource::new(
        driver.clone(),
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        named_full_name(),
        OutputResourceKind::Rewritten,
    );
    resource.begin_write().extend_from_slice(b"body");
    resource.end_write();
    let embedded = resource.full_name().signature().to_string();
    assert_eq!(embedded.len(), 10);
    // Same bytes, same key: same signature.
    let mut twin = OutputResource::new(
        driver,
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        named_full_name(),
        OutputResourceKind::Rewritten,
    );
    twin.begin_write().extend_from_slice(b"body");
    twin.end_write();
    assert_eq!(twin.full_name().signature(), embedded);
    // Rotating the key moves the computed signature off the embedded one.
    options
        .borrow_mut()
        .set_option("UrlSigningKey", "other", ps_config::Scope::ProcessStrict)
        .unwrap();
    assert_ne!(resource.compute_signature(), embedded);
    assert!(!resource.check_signature());
    // ... unless leniency is configured.
    options
        .borrow_mut()
        .set_option(
            "AcceptInvalidSignatures",
            "on",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    assert!(resource.check_signature());
}

#[test]
fn cache_key_remaps_domains() {
    let (server, _driver, options) = fixture();
    // Without mappings the key is the plain URL.
    let plain = Rc::new(ResourceDriver::new(
        server.clone(),
        options.clone(),
        "http://test.com/dir/page.html",
    ));
    let resource = OutputResource::new(
        plain,
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        named_full_name(),
        OutputResourceKind::Rewritten,
    );
    assert_eq!(
        resource.http_cache_key(),
        "http://test.com/dir/a.css.pagespeed.cf.HASH.css"
    );

    // With a rewrite mapping the key follows the mapped domain.
    let mut lawyer = ps_html::domain_lawyer::DomainLawyer::new();
    assert!(lawyer.add_rewrite_domain_mapping("http://cdn.com/", "http://test.com/", None));
    let mut mapped = ResourceDriver::new(server, options, "http://test.com/dir/page.html");
    mapped.set_lawyer(lawyer);
    let resource = OutputResource::new(
        Rc::new(mapped),
        "http://test.com/dir/",
        "http://test.com/dir/",
        "http://test.com/dir/page.html",
        named_full_name(),
        OutputResourceKind::Rewritten,
    );
    assert_eq!(
        resource.http_cache_key(),
        "http://cdn.com/dir/a.css.pagespeed.cf.HASH.css"
    );
}

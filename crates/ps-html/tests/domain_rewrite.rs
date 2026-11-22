//! Domain rewrite tests ported from `domain_rewrite_filter_test.cc`.
//!
//! NOTE: inputs skip the body wrapper (it carries no URLs, so it
//! commutes with this filter). Tests passing a URL where C++ takes a
//! case id replicate the ugly dummy URL verbatim (see `ProxyBaseUrl`).
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;

use harness::{SharedFilter, SharedSink};
use ps_config::message_handler::NullMessageHandler;
use ps_config::options::RewriteOptions;
use ps_config::Scope;
use ps_html::domain_lawyer::DomainLawyer;
use ps_html::filters::common::XhtmlStatus;
use ps_html::filters::domain_rewrite::{
    DomainRewriteConfig, DomainRewriteFilter, DOMAIN_REWRITES,
};
use ps_html::parser::Parser;
use ps_html::static_assets::{get_asset, StaticAsset};
use ps_html::url_namer::TestUrlNamer;
use ps_html::writer::WriterFilter;
use ps_http::google_url::GoogleUrl;
use ps_http::response::{HttpOptions, ResponseHeaders};
use ps_stats::core::Statistics;

type Filter = DomainRewriteFilter<TestUrlNamer>;

const HTML_DOMAIN: &str = "http://test.com/";
const FROM1_DOMAIN: &str = "http://from1.test.com/";
const FROM2_DOMAIN: &str = "http://from2.test.com/";
const TO1_DOMAIN: &str = "http://to1.test.com/";
const TO2_DOMAIN: &str = "http://to2.test.com/";

/// Fixture lawyer: TO1←FROM1, TO2←FROM2, TO2 sharded over TO2A/TO2B.
fn fixture_lawyer() -> DomainLawyer {
    let mut lawyer = DomainLawyer::new();
    let mut handler = NullMessageHandler;
    assert!(lawyer.add_rewrite_domain_mapping(TO1_DOMAIN, FROM1_DOMAIN, Some(&mut handler)));
    assert!(lawyer.add_rewrite_domain_mapping(TO2_DOMAIN, FROM2_DOMAIN, Some(&mut handler)));
    assert!(lawyer.add_shard(
        TO2_DOMAIN,
        "http://to2a.test.com/,http://to2b.test.com/",
        Some(&mut handler)
    ));
    lawyer
}

fn lawyer() -> Rc<DomainLawyer> {
    Rc::new(fixture_lawyer())
}

/// Fixture options: the C++ `SetUp` disallows `*dont_shard*`.
fn base_options() -> RewriteOptions {
    let mut options = RewriteOptions::new();
    options.allow_resources_mut().disallow("*dont_shard*");
    options
}

fn config() -> DomainRewriteConfig {
    DomainRewriteConfig {
        xhtml: XhtmlStatus::IsNotXhtml,
        hash_length: 1,
        options: base_options(),
        ..DomainRewriteConfig::default()
    }
}

fn run_case(
    case: &str,
    input: &[u8],
    config: DomainRewriteConfig,
    lawyer: &Rc<DomainLawyer>,
) -> (Vec<u8>, SharedFilter<Filter>) {
    run_case_url(
        format!("http://test.com/{case}.html").as_bytes(),
        input,
        config,
        lawyer,
    )
}

fn run_case_url(
    url: &[u8],
    input: &[u8],
    config: DomainRewriteConfig,
    lawyer: &Rc<DomainLawyer>,
) -> (Vec<u8>, SharedFilter<Filter>) {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(DomainRewriteFilter::new(
        config,
        TestUrlNamer::new(),
        lawyer,
    ));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url, false);
    parser.write(input);
    parser.finish();
    (sink.bytes(), filter)
}

fn validate_expected(
    case: &str,
    input: &[u8],
    expected: &[u8],
    config: DomainRewriteConfig,
    lawyer: &Rc<DomainLawyer>,
) -> SharedFilter<Filter> {
    let (output, filter) = run_case(case, input, config, lawyer);
    assert_eq!(output, expected, "case {case}");
    filter
}

fn expect_change(tag: &str, url: &str, expected: &str, lawyer: &Rc<DomainLawyer>) {
    let input = format!("<link rel=stylesheet href={url}>");
    let output = format!("<link rel=stylesheet href={expected}>");
    let filter = validate_expected(tag, input.as_bytes(), output.as_bytes(), config(), lawyer);
    assert_eq!(filter.with(|filter| filter.rewrite_count()), 1, "tag {tag}");
}

fn expect_no_change(tag: &str, url: &str, lawyer: &Rc<DomainLawyer>) {
    let input = format!("<link rel=stylesheet href={url}>");
    let filter = validate_expected(tag, input.as_bytes(), input.as_bytes(), config(), lawyer);
    assert_eq!(filter.with(|filter| filter.rewrite_count()), 0, "tag {tag}");
}

#[test]
fn rewrite_classic_script() {
    let lawyer = lawyer();
    validate_expected(
        "classic",
        format!("<script src='{FROM1_DOMAIN}app.js'></script>").as_bytes(),
        format!("<script src='{TO1_DOMAIN}app.js'></script>").as_bytes(),
        config(),
        &lawyer,
    );
}

#[test]
fn dont_rewrite_module_script() {
    let lawyer = lawyer();
    let input = format!("<script type=\"module\" src='{FROM1_DOMAIN}app.js'></script>");
    validate_expected(
        "module",
        input.as_bytes(),
        input.as_bytes(),
        config(),
        &lawyer,
    );
}

#[test]
fn dont_rewrite_crossorigin_script() {
    let lawyer = lawyer();
    let input = format!("<script crossorigin src='{FROM1_DOMAIN}lib.js'></script>");
    validate_expected(
        "crossorigin",
        input.as_bytes(),
        input.as_bytes(),
        config(),
        &lawyer,
    );
}

#[test]
fn dont_touch() {
    let lawyer = lawyer();
    expect_no_change("", "", &lawyer);
    expect_no_change("relative", "relative.css", &lawyer);
    expect_no_change("absolute", "/absolute.css", &lawyer);
    expect_no_change(
        "html domain",
        &format!("{HTML_DOMAIN}absolute.css"),
        &lawyer,
    );
    expect_no_change("other domain", "http://other.com/absolute.css", &lawyer);
    expect_no_change(
        "disallow1",
        &format!("{FROM1_DOMAIN}dont_shard.css"),
        &lawyer,
    );
    expect_no_change(
        "disallow2",
        &format!("{FROM2_DOMAIN}dont_shard.css"),
        &lawyer,
    );
    expect_no_change("http://absolute.css", "http://absolute.css", &lawyer);
    expect_no_change(
        "data:image/gif;base64,AAAA",
        "data:image/gif;base64,AAAA",
        &lawyer,
    );
}

#[test]
fn relative_up_reference_rewrite() {
    let lawyer = lawyer();
    expect_no_change("subdir/relative", "under_subdir.css", &lawyer);
    expect_no_change("subdir/relative", "../under_top.css", &lawyer);

    let mut mapped = fixture_lawyer();
    let mut handler = NullMessageHandler;
    assert!(mapped.add_rewrite_domain_mapping(TO1_DOMAIN, HTML_DOMAIN, Some(&mut handler)));
    let mapped = Rc::new(mapped);
    expect_change(
        "subdir/relative",
        "under_subdir.css",
        &format!("{TO1_DOMAIN}subdir/under_subdir.css"),
        &mapped,
    );
    expect_change(
        "subdir/relative",
        "../under_top2.css",
        &format!("{TO1_DOMAIN}under_top2.css"),
        &mapped,
    );
}

#[test]
fn relative_up_reference_shard() {
    let mut mapped = fixture_lawyer();
    let mut handler = NullMessageHandler;
    assert!(mapped.add_rewrite_domain_mapping(TO2_DOMAIN, HTML_DOMAIN, Some(&mut handler)));
    let mapped = Rc::new(mapped);
    expect_change(
        "subdir/relative",
        "under_subdir.css",
        "http://to2a.test.com/subdir/under_subdir.css",
        &mapped,
    );
    expect_change(
        "subdir/relative",
        "../under_top1.css",
        "http://to2b.test.com/under_top1.css",
        &mapped,
    );
}

#[test]
fn mapped_and_sharded() {
    let lawyer = lawyer();
    expect_change(
        "rewrite",
        &format!("{FROM1_DOMAIN}absolute.css"),
        &format!("{TO1_DOMAIN}absolute.css"),
        &lawyer,
    );
    expect_change(
        "rewrite",
        &format!("{FROM1_DOMAIN}absolute.css?p1=v1"),
        &format!("{TO1_DOMAIN}absolute.css?p1=v1"),
        &lawyer,
    );
    expect_change(
        "shard0",
        &format!("{FROM2_DOMAIN}0.css"),
        "http://to2a.test.com/0.css",
        &lawyer,
    );
    expect_change(
        "shard0",
        &format!("{FROM2_DOMAIN}0.css?p1=v1&amp;p2=v2"),
        "http://to2b.test.com/0.css?p1=v1&amp;p2=v2",
        &lawyer,
    );
}

#[test]
fn dont_touch_if_already_rewritten() {
    let lawyer = lawyer();
    expect_no_change(
        "other domain",
        "http://from1.test.com/a.css.pagespeed.cf.0.css",
        &lawyer,
    );
}

#[test]
fn rewrite_hyperlinks() {
    let lawyer = lawyer();
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;
    validate_expected(
        "forms and a tags",
        format!(
            "<a href=\"{FROM1_DOMAIN}link.html\"/><form action=\"{FROM1_DOMAIN}blank\"/><a href=\"https://from1.test.com/1.html\"/><area href=\"{FROM1_DOMAIN}2.html\"/><iframe src=\"{FROM1_DOMAIN}iframe.html\"/><iframe id=\"psmob-iframe\" src=\"{FROM1_DOMAIN}iframe.html\"/>"
        )
        .as_bytes(),
        format!(
            "<a href=\"{TO1_DOMAIN}link.html\"/><form action=\"{TO1_DOMAIN}blank\"/><a href=\"https://from1.test.com/1.html\"/><area href=\"{TO1_DOMAIN}2.html\"/><iframe src=\"{TO1_DOMAIN}iframe.html\"/><iframe id=\"psmob-iframe\" src=\"{TO1_DOMAIN}iframe.html\"/>"
        )
        .as_bytes(),
        config,
        &lawyer,
    );
}

#[test]
fn rewrite_but_do_not_shard_hyperlinks() {
    let lawyer = lawyer();
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;
    validate_expected(
        "forms and a tags",
        format!(
            "<a href=\"{FROM2_DOMAIN}link.html\"/><form action=\"{FROM2_DOMAIN}blank\"/><a href=\"https://from2.test.com/1.html\"/><area href=\"{FROM2_DOMAIN}2.html\"/>"
        )
        .as_bytes(),
        format!(
            "<a href=\"{TO2_DOMAIN}link.html\"/><form action=\"{TO2_DOMAIN}blank\"/><a href=\"https://from2.test.com/1.html\"/><area href=\"{TO2_DOMAIN}2.html\"/>"
        )
        .as_bytes(),
        config,
        &lawyer,
    );
}

#[test]
fn rewrite_redirect_locations() {
    let lawyer = lawyer();
    let headers = Rc::new(RefCell::new(ResponseHeaders::new(
        HttpOptions::default_for_tests(),
    )));
    headers
        .borrow_mut()
        .headers_mut()
        .add("Location", &format!("{FROM1_DOMAIN}redirect"));
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;
    config.response_headers = Some(headers.clone());
    let (output, _) = run_case("headers", b"", config, &lawyer);
    assert!(output.is_empty());
    assert_eq!(
        headers.borrow().headers().lookup1("Location").as_deref(),
        Some(&format!("{TO1_DOMAIN}redirect")[..])
    );
}

#[test]
fn no_client_domain_rewrite() {
    let lawyer = lawyer();
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;
    validate_expected(
        "client domain rewrite",
        b"<html><body></body></html>",
        b"<html><body></body></html>",
        config,
        &lawyer,
    );
}

#[test]
fn client_domain_rewrite() {
    let mut lawyer = DomainLawyer::new();
    let mut handler = NullMessageHandler;
    assert!(lawyer.add_rewrite_domain_mapping(
        HTML_DOMAIN,
        "http://clientrewrite.com/",
        Some(&mut handler)
    ));
    let lawyer = Rc::new(lawyer);
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    options
        .set_option("ClientDomainRewrite", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;

    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(DomainRewriteFilter::new(
        config,
        TestUrlNamer::new(),
        &lawyer,
    )));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/", false);
    parser.write(b"<html><body>");
    parser.flush();
    parser.write(b"</body></html>");
    parser.finish();

    let mut expected = b"<html><body><script type=\"text/javascript\">".to_vec();
    expected.extend_from_slice(get_asset(StaticAsset::ClientDomainRewriter, false));
    expected.extend_from_slice(
        b"pagespeed.clientDomainRewriterInit([\"http://clientrewrite.com/\"]);</script></body></html>",
    );
    assert_eq!(sink.bytes(), expected);
}

#[test]
fn client_domain_rewrite_disabled_due_to_amp() {
    let mut lawyer = DomainLawyer::new();
    let mut handler = NullMessageHandler;
    assert!(lawyer.add_rewrite_domain_mapping(
        HTML_DOMAIN,
        "http://clientrewrite.com/",
        Some(&mut handler)
    ));
    let lawyer = Rc::new(lawyer);
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    options
        .set_option("ClientDomainRewrite", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;
    config.is_amp_document = true;
    validate_expected(
        "whatever",
        b"<html amp><body>Hello, world!</body></html>",
        b"<html amp><body>Hello, world!</body></html>",
        config,
        &lawyer,
    );
}

#[test]
fn proxy_suffix() {
    let mut lawyer = DomainLawyer::new();
    lawyer.set_proxy_suffix(".suffix");
    assert!(lawyer.can_rewrite_domains());
    let lawyer = Rc::new(lawyer);
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;

    // Relative URLs stay relative to the suffixed domain.
    validate_expected(
        "unchanged",
        b"<a href='relative.html'>r</a>",
        b"<a href='relative.html'>r</a>",
        config.clone(),
        &lawyer,
    );
    // Absolute references into the origin domain gain the suffix.
    let doc = b"http://www.example.com.suffix/index.html";
    let (output, _) = run_case_url(
        doc,
        b"<a href='http://www.example.com/absolute.html'>r</a>",
        config.clone(),
        &lawyer,
    );
    assert_eq!(
        output,
        b"<a href='http://www.example.com.suffix/absolute.html'>r</a>"
    );
    // Related hosts suffix via the public-suffix list.
    let (output, _) = run_case_url(
        doc,
        b"<a href='http://other.example.com/x.html'>r</a>",
        config.clone(),
        &lawyer,
    );
    assert_eq!(
        output,
        b"<a href='http://other.example.com.suffix/x.html'>r</a>"
    );
    // Unrelated hosts, and everything under a non-suffixed document,
    // stay put. NOTE: the C++ passes the URL as a *case id* here, so
    // these parse under an unrelated test.com URL — replicated verbatim.
    let odd_doc = b"http://test.com/http://www.example.com.suffix/index.html.html";
    for input in [
        "<a href='http://other.com/x.html'>r</a>",
        "<img src='http://other.example/image.png'>",
        "<iframe src='http://www.example.com/frame.html'></iframe>",
    ] {
        let (output, _) = run_case_url(odd_doc, input.as_bytes(), config.clone(), &lawyer);
        assert_eq!(output, input.as_bytes());
    }
    for (input, expected) in [
        (
            "<img src='http://www.example.com/image.png'>",
            "<img src='http://www.example.com.suffix/image.png'>",
        ),
        (
            "<link rel=stylesheet href='http://www.example.com/style.css'>",
            "<link rel=stylesheet href='http://www.example.com.suffix/style.css'>",
        ),
        (
            "<script src='http://www.example.com/script.js'></script>",
            "<script src='http://www.example.com.suffix/script.js'></script>",
        ),
    ] {
        let (output, _) = run_case_url(doc, input.as_bytes(), config.clone(), &lawyer);
        assert_eq!(output, expected.as_bytes());
    }

    // Location headers suffix too.
    let headers = Rc::new(RefCell::new(ResponseHeaders::new(
        HttpOptions::default_for_tests(),
    )));
    headers
        .borrow_mut()
        .headers_mut()
        .add("Location", "https://sub.example.com/a.html");
    let mut config = config;
    config.response_headers = Some(headers.clone());
    let _ = run_case_url(doc, b"", config, &lawyer);
    assert_eq!(
        headers.borrow().headers().lookup1("Location").as_deref(),
        Some("https://sub.example.com.suffix/a.html")
    );
}

#[test]
fn proxy_base_url() {
    // NOTE: cases 2-3 pass the URL as a *case id* (like the C++
    // `ValidateNoChanges(url, ...)` overload), so they parse under an
    // unrelated test.com URL — which is why they stay put.
    let mut lawyer = DomainLawyer::new();
    lawyer.set_proxy_suffix(".suffix");
    assert!(lawyer.can_rewrite_domains());
    let lawyer = Rc::new(lawyer);

    let (output, _) = run_case_url(
        b"http://www.example.com.suffix/index.html",
        b"<html><head><base href='http://www.example.com'/></head></html>",
        config(),
        &lawyer,
    );
    assert_eq!(
        output,
        b"<html><head><base href='http://www.example.com.suffix/'/></head></html>"
    );

    for input in [
        "<html><head><base href='http://other.example.com/'/></head></html>",
        "<html><head><base href='http://www.example.com.suffix/'/></head></html>",
    ] {
        let (output, _) = run_case_url(
            b"http://test.com/http://www.example.com.suffix/index.html.html",
            input.as_bytes(),
            config(),
            &lawyer,
        );
        assert_eq!(output, input.as_bytes());
    }
}

#[test]
fn parse_refresh_content() {
    use ps_html::filters::domain_rewrite::DomainRewriteFilter;
    for input in ["42", "42 !", "42, "] {
        assert!(
            DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(input.as_bytes()).is_none(),
            "rejects {input}"
        );
    }
    // `url=` is effectively optional in the wild.
    let parsed = DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(b"42, rul").unwrap();
    assert_eq!(
        (&parsed.0, &parsed.1, &parsed.2),
        (&b"42, ".to_vec(), &b"rul".to_vec(), &b"".to_vec())
    );
    let parsed = DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(b"43, url").unwrap();
    assert_eq!(
        (&parsed.0, &parsed.1, &parsed.2),
        (&b"43, ".to_vec(), &b"url".to_vec(), &b"".to_vec())
    );
    assert!(DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(b"42, url = ").is_none());
    let parsed =
        DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(b"44, url=a.org").unwrap();
    assert_eq!(
        (&parsed.0, &parsed.1, &parsed.2),
        (&b"44, url=".to_vec(), &b"a.org".to_vec(), &b"".to_vec())
    );
    let parsed =
        DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(b" 42.45; UrL = b.com  ")
            .unwrap();
    assert_eq!(
        (&parsed.0, &parsed.1, &parsed.2),
        (
            &b" 42.45; UrL = ".to_vec(),
            &b"b.com".to_vec(),
            &b"".to_vec()
        )
    );
    let parsed =
        DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(b" 42 ; url='c.gov ' ").unwrap();
    assert_eq!(
        (&parsed.0, &parsed.1, &parsed.2),
        (&b" 42 ; url=".to_vec(), &b"c.gov".to_vec(), &b" ".to_vec())
    );
    let parsed =
        DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(b" 42 ; url='c.gov 'foo")
            .unwrap();
    assert_eq!(
        (&parsed.0, &parsed.1, &parsed.2),
        (
            &b" 42 ; url=".to_vec(),
            &b"c.gov".to_vec(),
            &b"foo".to_vec()
        )
    );
    let parsed =
        DomainRewriteFilter::<TestUrlNamer>::parse_refresh_content(b" 42 ; url=\"d.edu ").unwrap();
    assert_eq!(
        (&parsed.0, &parsed.1, &parsed.2),
        (&b" 42 ; url=".to_vec(), &b"d.edu".to_vec(), &b"".to_vec())
    );
}

#[test]
fn parse_set_cookie_attributes() {
    use ps_html::filters::domain_rewrite::DomainRewriteFilter;
    let (cookie, attributes) =
        DomainRewriteFilter::<TestUrlNamer>::parse_set_cookie_attributes(b"");
    assert!(attributes.is_empty());
    let _ = cookie;
    let (cookie, attributes) =
        DomainRewriteFilter::<TestUrlNamer>::parse_set_cookie_attributes(b"a=b");
    assert_eq!(cookie, b"a=b");
    assert!(attributes.is_empty());
    let (cookie, attributes) =
        DomainRewriteFilter::<TestUrlNamer>::parse_set_cookie_attributes(b"c=d;");
    assert_eq!(cookie, b"c=d");
    assert!(attributes.is_empty());
    let (cookie, attributes) =
        DomainRewriteFilter::<TestUrlNamer>::parse_set_cookie_attributes(b"e=f; foo = bar");
    assert_eq!(cookie, b"e=f");
    assert_eq!(attributes.len(), 1);
    assert_eq!(attributes[0].0, b"foo");
    assert_eq!(attributes[0].1, b"bar");
    let (cookie, attributes) = DomainRewriteFilter::<TestUrlNamer>::parse_set_cookie_attributes(
        b"g=h; foo = bar; httponly  ",
    );
    assert_eq!(cookie, b"g=h");
    assert_eq!(attributes.len(), 2);
    assert_eq!(attributes[0].0, b"foo");
    assert_eq!(attributes[0].1, b"bar");
    assert_eq!(attributes[1].0, b"httponly");
    assert_eq!(attributes[1].1, b"");
    // Nameless attributes cannot happen, but stay robust.
    let (cookie, attributes) = DomainRewriteFilter::<TestUrlNamer>::parse_set_cookie_attributes(
        b"i=j;  = bar; secure; a = b",
    );
    assert_eq!(cookie, b"i=j");
    assert_eq!(attributes.len(), 3);
    assert_eq!(attributes[0].0, b"");
    assert_eq!(attributes[0].1, b"bar");
    assert_eq!(attributes[1].0, b"secure");
    assert_eq!(attributes[1].1, b"");
    assert_eq!(attributes[2].0, b"a");
    assert_eq!(attributes[2].1, b"b");
}

fn cookie_lawyer() -> Rc<DomainLawyer> {
    let mut lawyer = DomainLawyer::new();
    let mut handler = NullMessageHandler;
    assert!(lawyer.add_rewrite_domain_mapping(
        "http://someotherhost.com/after/",
        "www.example.com",
        Some(&mut handler)
    ));
    Rc::new(lawyer)
}

fn cookie_config() -> DomainRewriteConfig {
    let mut options = base_options();
    options
        .set_option("DomainRewriteCookies", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;
    config
}

#[test]
fn update_set_cookie_header() {
    let lawyer = cookie_lawyer();
    let filter = DomainRewriteFilter::new(cookie_config(), TestUrlNamer::new(), &lawyer);
    // The page may come from elsewhere; the domain still rewrites.
    let unrelated = GoogleUrl::new("http://unrelated.com");
    let base = GoogleUrl::new("http://www.example.com/page/");
    let update = |base: &GoogleUrl, value: &[u8]| filter.update_set_cookie_header(base, value);

    assert_eq!(update(&unrelated, b"foo = var"), None);
    assert_eq!(update(&unrelated, b"foo = var; Secure; HttpOnly"), None);
    assert_eq!(
        update(&unrelated, b"foo = var; Domain=www.example.com"),
        Some(b"foo = var; Domain=someotherhost.com".to_vec())
    );
    assert_eq!(
        update(&unrelated, b"foo = var; Domain=.www.example.com"),
        Some(b"foo = var; Domain=someotherhost.com".to_vec())
    );
    assert_eq!(
        update(&unrelated, b"foo = var; Domain=www.example.com;   HttpOnly"),
        Some(b"foo = var; Domain=someotherhost.com; HttpOnly".to_vec())
    );
    assert_eq!(
        update(
            &unrelated,
            b"foo = var; Domain=www.huh.com; Domain=www.example.com"
        ),
        Some(b"foo = var; Domain=someotherhost.com; Domain=someotherhost.com".to_vec())
    );
    assert_eq!(
        update(
            &unrelated,
            b"foo = var; Domain=www.example.com; Domain=www.huh.com"
        ),
        None
    );
    assert_eq!(
        update(&base, b"foo = var; Path=/subdir"),
        Some(b"foo = var; Path=/after/subdir".to_vec())
    );
    assert_eq!(
        update(&base, b"foo = var; Path=/subdir/"),
        Some(b"foo = var; Path=/after/subdir/".to_vec())
    );
    assert_eq!(
        update(&base, b"foo = var; Domain=www.example.com; Path=/subdir/"),
        Some(b"foo = var; Domain=someotherhost.com; Path=/after/subdir/".to_vec())
    );
    assert_eq!(
        update(&base, b"foo = var; Domain=unrelated.com; Path=/subdir/"),
        None
    );
}

#[test]
fn update_set_cookie_header_disabled() {
    let lawyer = cookie_lawyer();
    let filter = DomainRewriteFilter::new(cookie_config(), TestUrlNamer::new(), &lawyer);
    let base = GoogleUrl::new("http://www.example.com/page/");
    assert_eq!(
        filter.update_set_cookie_header(&base, b"foo = var; Domain=unrelated.com; Path=/subdir/"),
        None
    );
}

#[test]
fn proxy_suffix_refresh() {
    let mut lawyer = DomainLawyer::new();
    lawyer.set_proxy_suffix(".suffix");
    let lawyer = Rc::new(lawyer);
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    let mut config = config();
    config.options = options;
    let (output, _) = run_case_url(
        b"http://www.example.com.suffix/index.html",
        b"<html><head><meta http-equiv=\"refresh\" content=\"5;url=https://sub.example.com/a.html\"/></head></html>",
        config,
        &lawyer,
    );
    assert_eq!(
        output,
        b"<html><head><meta http-equiv=\"refresh\" content=\"5;url=&quot;https://sub.example.com.suffix/a.html&quot;\"/></head></html>"
    );
}

#[test]
fn proxy_suffix_set_cookie() {
    let mut lawyer = DomainLawyer::new();
    lawyer.set_proxy_suffix(".suffix");
    let lawyer = Rc::new(lawyer);
    let mut options = base_options();
    options
        .set_option("DomainRewriteHyperlinks", "on", Scope::ProcessStrict)
        .unwrap();
    options
        .set_option("DomainRewriteCookies", "on", Scope::ProcessStrict)
        .unwrap();
    let mut first = config();
    first.options = options.clone();
    let (output, _) = run_case_url(
        b"http://www.example.com.suffix/index.html",
        b"<meta http-equiv=set-cookie content=\"a=b; domain= www.example.com\">",
        first,
        &lawyer,
    );
    assert_eq!(
        output,
        b"<meta http-equiv=set-cookie content=\"a=b; domain=www.example.com.suffix\">"
    );

    // Multiple HTTP headers, rewritten twice for idempotency.
    let headers = Rc::new(RefCell::new(ResponseHeaders::new(
        HttpOptions::default_for_tests(),
    )));
    headers
        .borrow_mut()
        .headers_mut()
        .add("Set-Cookie", "a=b; Domain=www.example.com");
    headers
        .borrow_mut()
        .headers_mut()
        .add("Set-Cookie", "c=d; Secure; Domain=www.example.com");
    let mut idem = config();
    idem.options = options;
    let mut filter = DomainRewriteFilter::new(idem, TestUrlNamer::new(), &lawyer);
    let base = GoogleUrl::new("http://www.example.com.suffix/index.html");
    filter.update_domain_headers(&base, &mut headers.borrow_mut());
    filter.update_domain_headers(&base, &mut headers.borrow_mut());
    let headers = headers.borrow();
    let values = headers.headers().lookup("Set-Cookie").unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0], "a=b; Domain=www.example.com.suffix");
    assert_eq!(values[1], "c=d; Secure; Domain=www.example.com.suffix");
}

#[test]
fn publishes_rewrite_counter() {
    let mut mapped = fixture_lawyer();
    let mut handler = NullMessageHandler;
    assert!(mapped.add_rewrite_domain_mapping(TO1_DOMAIN, HTML_DOMAIN, Some(&mut handler)));
    let mapped = Rc::new(mapped);
    let stats = Statistics::new();
    let mut filter = DomainRewriteFilter::new(config(), TestUrlNamer::new(), &mapped);
    filter.init_stats(&stats);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/subdir/relative.html", false);
    parser.write(b"<link rel=stylesheet href=under_subdir.css>");
    parser.finish();
    assert_eq!(stats.get_variable(DOMAIN_REWRITES).unwrap().get(), 1);
}

//! DNS prefetch tests ported from `insert_dns_prefetch_filter_test.cc`.
//!
//! NOTE: inputs skip the `<html>` wrapper and doctype (neither carries
//! URLs, so both commute with this filter). Log assertions read the
//! filter's recorded statuses instead of a log channel, which does not
//! exist yet. `InsertDnsPrefetchFilterWithOtherFilters` is skipped: it
//! needs the defer/lazyload/delay filters, which are not ported.
#[path = "harness.rs"]
mod harness;

use harness::{SharedFilter, SharedSink};
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_html::filters::insert_dns_prefetch::{
    DnsPrefetchHtmlStatus, InsertDnsPrefetchConfig, InsertDnsPrefetchFilter, SharedFlushEarlyInfo,
};
use ps_html::flush_early::FlushEarlyInfo;
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;
use std::cell::RefCell;
use std::rc::Rc;

fn shared_info() -> SharedFlushEarlyInfo {
    Rc::new(RefCell::new(FlushEarlyInfo::default()))
}

fn config() -> InsertDnsPrefetchConfig {
    InsertDnsPrefetchConfig::default()
}

/// Parses one document, returning output + filter.
fn run_case(
    case: &str,
    html: &str,
    config: InsertDnsPrefetchConfig,
) -> (String, SharedFilter<InsertDnsPrefetchFilter>) {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(InsertDnsPrefetchFilter::new(config));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(html.as_bytes());
    parser.finish();
    (String::from_utf8(sink.bytes()).unwrap(), filter)
}

/// Asserts the shared info totals and stored domains (`CheckPrefetchInfo`).
fn check_prefetch_info(info: &SharedFlushEarlyInfo, current: i32, previous: i32, stored: &[&str]) {
    let info = info.borrow();
    assert_eq!(info.total_dns_prefetch_domains(), current);
    assert_eq!(info.total_dns_prefetch_domains_previous(), previous);
    let domains: Vec<&str> = info
        .dns_prefetch_domains()
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(domains, stored);
}

fn create_html(num_scripts: u32) -> String {
    let mut html = String::from("<head><script></script></head><body>");
    for i in 1..=num_scripts {
        html.push_str(&format!("<script src=\"http://{i}.com/\"/>"));
    }
    html.push_str("</body>");
    html
}

fn create_html_with_prefetch_tags(num_scripts: u32, num_tags: u32) -> String {
    let mut html = String::from("<head><script></script>");
    for i in 1..=num_tags {
        html.push_str(&format!("<link rel=\"dns-prefetch\" href=\"//{i}.com\">"));
    }
    html.push_str("</head><body>");
    for i in 1..=num_scripts {
        html.push_str(&format!("<script src=\"http://{i}.com/\"/>"));
    }
    html.push_str("</body>");
    html
}

fn create_domains(num: u32) -> Vec<String> {
    (1..=num).map(|i| format!("{i}.com")).collect()
}

#[test]
fn ignore_domains_in_head() {
    let html = "<head>\
        <link type=\"text/css\" rel=\"stylesheet\" href=\"http://a.com/\">\
        <script src=\"http://b.com/\"/>\
        <link rel=\"dns-prefetch\" href=\"http://c.com\">\
        </head><body></body>";
    let info = shared_info();
    let mut config = config();
    config.flush_early_info = info.clone();
    let (output, _) = run_case("ignore_domains_in_head", html, config);
    assert_eq!(output, html);
    check_prefetch_info(&info, 0, 0, &[]);
}

#[test]
fn store_domains_in_body() {
    let html = "<head></head><body>\
        <link type=\"text/css\" rel=\"stylesheet\" href=\"http://a.com/\">\
        <script src=\"http://b.com/\"/>\
        <img src=\"http://c.com/\"/>\
        </body>";
    let info = shared_info();
    let mut config = config();
    config.flush_early_info = info.clone();
    let (output, _) = run_case("store_domains_in_body", html, config);
    assert_eq!(output, html);
    check_prefetch_info(&info, 3, 0, &["a.com", "b.com", "c.com"]);
}

#[test]
fn ignore_current_domain() {
    let html = format!(
        "<head></head><body>\
        <link type=\"text/css\" rel=\"stylesheet\" href=\"http://test.com/style.css\">\
        <script src=\"http://test.com/script.js\">\
        <img src=\"http://test.com/img.src\">\
        </body>"
    );
    let info = shared_info();
    let mut config = config();
    config.flush_early_info = info.clone();
    let (output, _) = run_case("ignore_current_domain", &html, config);
    assert_eq!(output, html);
    check_prefetch_info(&info, 0, 0, &[]);
}

#[test]
fn disable_for_user_agents_not_supported() {
    let html = "<head></head><body>\
        <link type=\"text/css\" rel=\"stylesheet\" href=\"http://a.com/\">\
        <script src=\"http://b.com/\"/>\
        <img src=\"http://c.com/\"/>\
        </body>";
    let info = shared_info();
    let mut config = config();
    config.flush_early_info = info.clone();
    config.supports_dns_prefetch = false;
    let (output, filter) = run_case("store_domains_in_body", html, config);
    assert_eq!(output, html);
    // The stuck in-head flag (see module docs) stores nothing.
    check_prefetch_info(&info, 0, 0, &[]);
    assert_eq!(
        filter.with(|filter| filter.log_status()),
        (Some(DnsPrefetchHtmlStatus::UserAgentNotSupported), 0, 0)
    );
}

#[test]
fn store_domains_only_in_body() {
    let html = "<head><script src=\"http://b.com/\"/></head><body>\
        <link type=\"text/css\" rel=\"stylesheet\" href=\"http://a.com/\">\
        <script src=\"http://b.com/\"/>\
        <img src=\"http://c.com/\"/>\
        </body>";
    let info = shared_info();
    let mut config = config();
    config.flush_early_info = info.clone();
    let (output, _) = run_case("store_domains_in_body", html, config);
    assert_eq!(output, html);
    // b.com is not stored since it is already in HEAD.
    check_prefetch_info(&info, 2, 0, &["a.com", "c.com"]);
}

#[test]
fn store_domains_in_body_max() {
    let html = create_html(10);
    let info = shared_info();
    let mut config = config();
    config.flush_early_info = info.clone();
    let (output, _) = run_case("store_domains_in_body_max", &html, config);
    assert_eq!(output, html);
    // Only 8/10 domains get stored.
    let expected = create_domains(8);
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    check_prefetch_info(&info, 10, 0, &expected);
}

#[test]
fn link_tag_test() {
    let html = "<head><script></script>\
        <link rel=\"alternate\" href=\"http://a.com\">\
        <link rel=\"author\" href=\"http://b.com\">\
        <link rel=\"dns-prefetch\" href=\"http://c.com\">\
        <link rel=\"help\" href=\"http://d.com\">\
        <link rel=\"icon\" href=\"http://e.com\">\
        <link rel=\"license\" href=\"http://f.com\">\
        <link rel=\"next\" href=\"http://g.com\">\
        <link rel=\"prefetch\" href=\"http://h.com\">\
        <link rel=\"prev\" href=\"http://i.com\">\
        <link rel=\"search\" href=\"http://j.com\">\
        <link rel=\"stylesheet\" href=\"http://k.com\">\
        </head><body>\
        <script src=\"http://a.com/\"/>\
        <script src=\"http://b.com/\"/>\
        <script src=\"http://c.com/\"/>\
        <script src=\"http://d.com/\"/>\
        <script src=\"http://e.com/\"/>\
        <script src=\"http://f.com/\"/>\
        <script src=\"http://g.com/\"/>\
        <script src=\"http://h.com/\"/>\
        <script src=\"http://i.com/\"/>\
        <script src=\"http://j.com/\"/>\
        <script src=\"http://k.com/\"/>\
        </body>";
    let info = shared_info();
    let mut config = config();
    config.flush_early_info = info.clone();
    let (output, _) = run_case("test_different_link_tags", html, config);
    assert_eq!(output, html);
    // dns-prefetch, icon, prefetch, stylesheet mark their domains in
    // HEAD; the hyperlink rels do not, so those get stored.
    check_prefetch_info(
        &info,
        7,
        0,
        &[
            "a.com", "b.com", "d.com", "f.com", "g.com", "i.com", "j.com",
        ],
    );
}

#[test]
fn full_flow() {
    // One filter across all parses, like the C++ driver's reuse.
    let info = shared_info();
    let mut filter_config = config();
    filter_config.flush_early_info = info.clone();
    let filter = SharedFilter::new(InsertDnsPrefetchFilter::new(filter_config));
    let parse = |case: &str, html: &str, filter: &SharedFilter<InsertDnsPrefetchFilter>| {
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(filter.clone()));
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    };

    let html_input = create_html(10);
    assert_eq!(parse("store_8_of_10", &html_input, &filter), html_input);
    let expected = create_domains(8);
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    check_prefetch_info(&info, 10, 0, &expected);

    let html_input = create_html(9);
    assert_eq!(parse("store_8_of_9", &html_input, &filter), html_input);
    check_prefetch_info(&info, 9, 10, &expected);

    // 8 DNS prefetch tags inserted since the difference in the number
    // of domains in the last two rewrites (10, 9) is <= 2.
    let html_input = create_html(6);
    let html_output = create_html_with_prefetch_tags(6, 8);
    assert_eq!(
        parse("stable_domain_list_so_insert_tags", &html_input, &filter),
        html_output
    );
    let expected = create_domains(6);
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    check_prefetch_info(&info, 6, 9, &expected);
    // 8 tags applied in this rewrite; the unstable rewrite before it
    // logged NOT_APPLIED once.
    assert_eq!(
        filter.with(|filter| filter.log_status()),
        (Some(DnsPrefetchHtmlStatus::Active), 8, 1)
    );

    // Since the last response caused instability in the domain list,
    // we don't insert any prefetch tags in this rewrite.
    assert_eq!(
        parse("after_unstable_response", &html_input, &filter),
        html_input
    );
    check_prefetch_info(&info, 6, 6, &expected);
    assert_eq!(
        filter.with(|filter| filter.log_status()),
        (Some(DnsPrefetchHtmlStatus::Active), 8, 2)
    );
}

#[test]
fn full_flow_logging() {
    let info = shared_info();
    let make_config = || {
        let mut config = config();
        config.flush_early_info = info.clone();
        config
    };

    let html_input = create_html(10);
    let (output, filter) = run_case("store_8_of_10", &html_input, make_config());
    assert_eq!(output, html_input);
    let expected = create_domains(8);
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    check_prefetch_info(&info, 10, 0, &expected);
    // Stable but nothing stored yet: no tags, no NOT_APPLIED either.
    assert_eq!(
        filter.with(|filter| filter.log_status()),
        (Some(DnsPrefetchHtmlStatus::Active), 0, 0)
    );

    let html_input = create_html(9);
    let (output, filter) = run_case("store_8_of_9", &html_input, make_config());
    assert_eq!(output, html_input);
    check_prefetch_info(&info, 9, 10, &expected);
    assert_eq!(
        filter.with(|filter| filter.log_status()),
        (Some(DnsPrefetchHtmlStatus::Active), 0, 1)
    );
}

#[test]
fn insert_domains_in_head_for_flush_early_flow() {
    let mut options = RewriteOptions::new();
    options.filters_mut().enable(Filter::FlushSubresources);
    let html = "<head>\
        <script src=\"http://b.com/\"/>\
        <script src=\"http://d.com/\"/>\
        </head><body>\
        <link type=\"text/css\" rel=\"stylesheet\" href=\"http://a.com/\">\
        <script src=\"http://b.com/\"/>\
        <img src=\"http://c.com/\"/>\
        </body>";
    let info = shared_info();
    let mut config = config();
    config.options = options;
    config.flush_early_info = info.clone();
    let (output, _) = run_case("store_domains_in_body", html, config);
    assert_eq!(output, html);
    check_prefetch_info(&info, 4, 0, &["b.com", "d.com", "a.com", "c.com"]);
}

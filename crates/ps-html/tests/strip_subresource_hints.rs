//! Hint-stripping tests ported from
//! `strip_subresource_hints_filter_test.cc`.
//!
//! NOTE: the rewrite-level cases port as `can_modify_urls` runs —
//! upstream an empty chain yields the frozen-URLs verdict through the
//! driver, which is exactly what the flag injects here.
#[path = "harness.rs"]
mod harness;

use std::rc::Rc;

use harness::SharedSink;
use ps_config::message_handler::NullMessageHandler;
use ps_config::wildcard::WildcardGroup;
use ps_html::domain_lawyer::DomainLawyer;
use ps_html::filters::strip_subresource_hints::{
    StripSubresourceHintsConfig, StripSubresourceHintsFilter,
};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

/// Builds the fixture lawyer (mappings + shards, mirroring SetUp).
fn fixture_lawyer() -> Rc<DomainLawyer> {
    let mut lawyer = DomainLawyer::new();
    let mut handler = NullMessageHandler;
    assert!(lawyer.add_rewrite_domain_mapping(
        "http://to1.test.com/",
        "http://from1.test.com/",
        Some(&mut handler)
    ));
    assert!(lawyer.add_rewrite_domain_mapping(
        "http://to2.test.com/",
        "http://from2.test.com/",
        Some(&mut handler)
    ));
    assert!(lawyer.add_shard(
        "http://to2.test.com/",
        "http://to2a.test.com/,http://to2b.test.com/",
        Some(&mut handler)
    ));
    Rc::new(lawyer)
}

fn allow_group() -> WildcardGroup {
    let mut allow = WildcardGroup::new();
    allow.disallow("*dontdropme*");
    allow
}

fn base_config() -> StripSubresourceHintsConfig {
    StripSubresourceHintsConfig {
        can_modify_urls: true,
        allow_resources: allow_group(),
        ..StripSubresourceHintsConfig::default()
    }
}

fn run_case(
    case: &str,
    input: &[u8],
    config: StripSubresourceHintsConfig,
    lawyer: &Rc<DomainLawyer>,
) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(StripSubresourceHintsFilter::new(config, lawyer)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(input);
    parser.finish();
    sink.bytes()
}

/// Ports `ValidateStripSubresourceHint`: rewritten when URLs move,
/// untouched when frozen.
fn validate_strip(
    input: &[u8],
    expected: &[u8],
    config: StripSubresourceHintsConfig,
    lawyer: &Rc<DomainLawyer>,
) {
    let mut moving = config.clone();
    moving.can_modify_urls = true;
    assert_eq!(
        run_case(
            "validaterewrite_can_modify_urls_true",
            input,
            moving,
            lawyer
        ),
        expected
    );
    let mut frozen = config;
    frozen.can_modify_urls = false;
    assert_eq!(
        run_case(
            "validaterewrite_can_modify_urls_false",
            input,
            frozen,
            lawyer
        ),
        input
    );
}

#[test]
fn single_resource_no_link() {
    validate_strip(
        b"<head><link rel=\"subresource\"/></head><body><img src=\"1.jpg\"/></body>",
        b"<head></head><body><img src=\"1.jpg\"/></body>",
        base_config(),
        &fixture_lawyer(),
    );
}

#[test]
fn single_resource_src_link() {
    validate_strip(
        b"<head><link rel=\"subresource\" src=\"/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        b"<head></head><body><img src=\"1.jpg\"/></body>",
        base_config(),
        &fixture_lawyer(),
    );
}

#[test]
fn single_resource_valid_link() {
    validate_strip(
        b"<head><link rel=\"subresource\" href=\"/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        b"<head></head><body><img src=\"1.jpg\"/></body>",
        base_config(),
        &fixture_lawyer(),
    );
}

#[test]
fn single_resource_valid_preload_link() {
    validate_strip(
        b"<head><link rel=\"preload\" href=\"/test.gif\" as=\"image\"/></head><body><img src=\"1.jpg\"/></body>",
        b"<head></head><body><img src=\"1.jpg\"/></body>",
        base_config(),
        &fixture_lawyer(),
    );
}

#[test]
fn single_resource_external_link() {
    let input = b"<head><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body><img src=\"1.jpg\"/></body>";
    validate_strip(input, input, base_config(), &fixture_lawyer());
}

#[test]
fn multi_resource_mixed_links() {
    validate_strip(
        b"<head><link rel=\"subresource\" href=\"/test.gif\"/><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        b"<head><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        base_config(),
        &fixture_lawyer(),
    );
}

#[test]
fn single_resource_rewrite_domain() {
    validate_strip(
        b"<head><link rel=\"subresource\" href=\"http://from1.test.com/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        b"<head></head><body><img src=\"1.jpg\"/></body>",
        base_config(),
        &fixture_lawyer(),
    );
}

#[test]
fn single_resource_disallow() {
    let input = b"<head><link rel=\"subresource\" href=\"/dontdropme/test.gif\"/></head><body><img src=\"1.jpg\"/></body>";
    validate_strip(input, input, base_config(), &fixture_lawyer());
}

#[test]
fn multi_subresource_preserve_images() {
    let mut config = base_config();
    config.image_preserve_urls = true;
    validate_strip(
        b"<head><link rel=\"subresource\" href=\"/dontdropme.gif\"/><link rel=\"subresource\" href=\"/test.gif\"/><link rel=\"subresource\" href=\"http://from1.test.com/test.gif\"/><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        b"<head><link rel=\"subresource\" href=\"/dontdropme.gif\"/><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        config,
        &fixture_lawyer(),
    );
}

#[test]
fn multi_subresource_preserve_all() {
    let mut config = base_config();
    config.css_preserve_urls = true;
    config.js_preserve_urls = true;
    config.image_preserve_urls = true;
    // Full preserve still strips untyped `subresource` hints.
    validate_strip(
        b"<head><link rel=\"subresource\" href=\"/dontdropme.gif\"/><link rel=\"subresource\" href=\"/test.gif\"/><link rel=\"subresource\" href=\"http://from1.test.com/test.gif\"/><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        b"<head><link rel=\"subresource\" href=\"/dontdropme.gif\"/><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body><img src=\"1.jpg\"/></body>",
        config,
        &fixture_lawyer(),
    );
}

#[test]
fn images_preserved() {
    let mut config = base_config();
    config.image_preserve_urls = true;
    validate_strip(
        b"<link rel=preload as=image href=a.jpg><link rel=preload as=script href=a.js><link rel=preload as=style href=a.css>",
        b"<link rel=preload as=image href=a.jpg>",
        config,
        &fixture_lawyer(),
    );
}

#[test]
fn scripts_preserved() {
    let mut config = base_config();
    config.js_preserve_urls = true;
    validate_strip(
        b"<link rel=preload as=image href=a.jpg><link rel=preload as=script href=a.js><link rel=preload as=style href=a.css>",
        b"<link rel=preload as=script href=a.js>",
        config,
        &fixture_lawyer(),
    );
}

#[test]
fn styles_preserved() {
    let mut config = base_config();
    config.css_preserve_urls = true;
    validate_strip(
        b"<link rel=preload as=image href=a.jpg><link rel=preload as=script href=a.js><link rel=preload as=style href=a.css>",
        b"<link rel=preload as=style href=a.css>",
        config,
        &fixture_lawyer(),
    );
}

#[test]
fn dont_strip_unknown_types() {
    let input = b"<link rel=preload as=font href=a.woff>";
    validate_strip(input, input, base_config(), &fixture_lawyer());
}

#[test]
fn passthrough_level_freezes_urls() {
    // kPassThrough empties the chain, so nothing may modify URLs.
    let input = b"<head><link rel=\"subresource\" href=\"/dontdropme.gif\"/><link rel=\"subresource\" href=\"/test.gif\"/><link rel=\"subresource\" href=\"http://from1.test.com/test.gif\"/><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body></body>";
    let mut config = base_config();
    config.can_modify_urls = false;
    assert_eq!(
        run_case("multi_resource", input, config, &fixture_lawyer()),
        input
    );
}

#[test]
fn core_filters_level_strips() {
    let mut config = base_config();
    config.can_modify_urls = true;
    assert_eq!(
        run_case(
            "multi_resource",
            b"<head><link rel=\"subresource\" href=\"/dontdropme.gif\"/><link rel=\"subresource\" href=\"/test.gif\"/><link rel=\"subresource\" href=\"http://from1.test.com/test.gif\"/><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body></body>",
            config,
            &fixture_lawyer()
        ),
        b"<head><link rel=\"subresource\" href=\"/dontdropme.gif\"/><link rel=\"subresource\" href=\"http://www.example.com/test.gif\"/></head><body></body>"
    );
}

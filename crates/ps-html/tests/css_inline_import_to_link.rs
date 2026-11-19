//! Import-to-link tests ported from
//! `css_inline_import_to_link_filter_test.cc`.
//!
//! NOTE: `ConvertThenCacheExtend` and `DontConvertOrCacheExtend` are not
//! ported — they need the cache-extender pipeline plus fetch mocking.
//! Inputs skip the body wrapper (it carries no styles, so it commutes
//! with this filter).
#[path = "harness.rs"]
mod harness;

use harness::{SharedFilter, SharedSink};
use ps_html::filters::common::XhtmlStatus;
use ps_html::filters::css_inline_import_to_link::{
    CssInlineImportToLinkConfig, CssInlineImportToLinkFilter,
};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

fn config() -> CssInlineImportToLinkConfig {
    CssInlineImportToLinkConfig {
        xhtml: XhtmlStatus::IsNotXhtml,
    }
}

fn wrap_page(style: &[u8]) -> Vec<u8> {
    let mut out = b"<head>\n".to_vec();
    out.extend_from_slice(style);
    out.extend_from_slice(b"</head>\n<body>Hello, world!</body>\n");
    out
}

fn validate_style_to_link(
    case: &str,
    input_style: &[u8],
    expected_style: &[u8],
) -> SharedFilter<CssInlineImportToLinkFilter> {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(CssInlineImportToLinkFilter::new(config()));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/test.html", false);
    parser.write(&wrap_page(input_style));
    parser.finish();
    assert_eq!(sink.bytes(), wrap_page(expected_style), "case {case}");
    filter
}

fn validate_unchanged(case: &str, style: &[u8]) {
    validate_style_to_link(case, style, style);
}

#[test]
fn css_preserve_url_off() {
    // The preserve-URLs option is irrelevant to this filter; the
    // single-filter harness matches the C++ full chain here.
    let filter = validate_style_to_link(
        "css_preserve_url_off",
        b"<style>@import url(assets/styles.css);</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\">",
    );
    assert_eq!(filter.with(|filter| filter.imports_to_links()), 1);
}

#[test]
fn always_allow_unauthorized_domain() {
    validate_style_to_link(
        "always_allow_unauthorized_domain",
        b"<style>@import url(http://unauth.com/assets/styles.css);</style>",
        b"<link rel=\"stylesheet\" href=\"http://unauth.com/assets/styles.css\">",
    );
}

#[test]
fn convert_good_style() {
    let link = b"<link rel=\"stylesheet\" href=\"assets/styles.css\">";
    for (index, input) in [
        "<style>@import url(assets/styles.css);</style>",
        "<style>@import url(\"assets/styles.css\");</style>",
        "<style>\n\t@import \"assets/styles.css\"\t;\n\t</style>",
        "<style>@import 'assets/styles.css';</style>",
        "<style>@import url( assets/styles.css);</style>",
        "<style>@import url('assets/styles.css');</style>",
        "<style>@import url( 'assets/styles.css' );</style>",
        // Missing final semicolon: invalid by the latest draft, valid by
        // the 2003 spec and accepted by some browsers, so accepted here.
        "<style>@import url(assets/styles.css)</style>",
    ]
    .iter()
    .enumerate()
    {
        validate_style_to_link(&format!("good_style_{index}"), input.as_bytes(), link);
    }
}

#[test]
fn do_not_convert_scoped() {
    validate_unchanged(
        "do_not_convert_scoped",
        b"<style type=\"text/css\" scoped>@import url(assets/styles.css);</style>",
    );
}

#[test]
fn convert_style_with_multiple_imports() {
    let filter = validate_style_to_link(
        "multiple_imports",
        b"<style>@import \"first.css\" all;\n@import url(\"second.css\" );\n@import 'third.css';\n</style>",
        b"<link rel=\"stylesheet\" href=\"first.css\" media=\"all\"><link rel=\"stylesheet\" href=\"second.css\"><link rel=\"stylesheet\" href=\"third.css\">",
    );
    // One counter bump per converted style, however many imports.
    assert_eq!(filter.with(|filter| filter.imports_to_links()), 1);

    validate_style_to_link(
        "multiple_media",
        b"<style>@import \"first.css\" screen;\n@import \"third.css\" print;\n</style>",
        b"<link rel=\"stylesheet\" href=\"first.css\" media=\"screen\"><link rel=\"stylesheet\" href=\"third.css\" media=\"print\">",
    );

    // modpagespeed issue #491: style attributes copy onto every link.
    let mut input = b"<style type=\"text/css\" title=\"currentStyle\" media=\"screen\">".to_vec();
    let mut expected = Vec::new();
    for leaf in [
        "universal.css?63310",
        "navigation_beta.css?123",
        "navigation.css?321",
        "teases.css",
        "homepage.css?nocache=987",
        "yourPicks.css?nocache=123",
        "sportsTabsHomepage.css",
        "businessTabsHomepage.css",
        "slider.css?09",
        "weather.css",
        "style3.css",
        "style3_tmp.css",
    ] {
        input.extend_from_slice(format!("   @import \"http://example.com/{leaf}\";").as_bytes());
        expected.extend_from_slice(
            format!(
                "<link rel=\"stylesheet\" href=\"http://example.com/{leaf}\" type=\"text/css\" title=\"currentStyle\" media=\"screen\">"
            )
            .as_bytes(),
        );
    }
    input.extend_from_slice(b"</style>");
    validate_style_to_link("issue_491", &input, &expected);

    // Trailing CSS stays inline.
    validate_style_to_link(
        "trailing_css",
        b"<style>@import \"first.css\" all;\n@import url('second.css' );\n@import \"third.css\";\n.a { background-color: red }</style>",
        b"<link rel=\"stylesheet\" href=\"first.css\" media=\"all\"><link rel=\"stylesheet\" href=\"second.css\"><link rel=\"stylesheet\" href=\"third.css\"><style>.a { background-color: red }</style>",
    );

    // An invalid @import vetoes everything.
    validate_unchanged(
        "invalid_import_veto",
        b"<style>@import \"first.css\" all;\n@import url( );\n@import \"third.css\";\n</style>",
    );
    // As does a leading @charset.
    validate_unchanged(
        "charset_veto",
        b"<style>@charset \"ISO-8859-1\";\n@import \"first.css\" all;\n@import url('second.css' );\n@import \"third.css\";\n</style>",
    );

    // Media combinations beyond pairwise equality are left alone.
    for (index, style) in [
        "<style>@import \"first.css\" screen;\n@import \"third.css\" not screen;\n</style>",
        "<style media=\"all\">@import \"first.css\" screen;\n@import \"third.css\" print;\n</style>",
        "<style media=\"all\">@import \"first.css\" screen;\n@import \"third.css\" not screen;\n",
        "<style media=\"screen, not screen\">@import \"first.css\" screen;\n@import \"third.css\" not screen;\n</style>",
        "<style media=\"screen\">@import \"first.css\" screen;\n@import \"third.css\" not screen;\n</style>",
    ]
    .iter()
    .enumerate()
    {
        validate_unchanged(&format!("media_veto_{index}"), style.as_bytes());
    }
}

#[test]
fn only_convert_prefix() {
    validate_style_to_link(
        "trailing_rule",
        b"<style>@import url(assets/styles.css);\na { color: red; }</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\"><style>a { color: red; }</style>",
    );
    validate_style_to_link(
        "trailing_nonsense",
        b"<style>@import url(assets/styles.css);\n@foobar</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\"><style>@foobar</style>",
    );
    validate_style_to_link(
        "later_import_kept",
        b"<style>@import url(a.css);\n@font-face { src: url(b.woff) }\n@import url(c.css);</style>",
        b"<link rel=\"stylesheet\" href=\"a.css\"><style>@font-face { src: url(b.woff) }\n@import url(c.css);</style>",
    );
}

#[test]
fn convert_style_with_attributes() {
    validate_style_to_link(
        "type_copied",
        b"<style type=\"text/css\">@import url(assets/styles.css);</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\" type=\"text/css\">",
    );
    validate_style_to_link(
        "type_media_copied",
        b"<style type=\"text/css\" media=\"screen\">@import url(assets/styles.css);</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\" type=\"text/css\" media=\"screen\">",
    );
}

#[test]
fn convert_style_with_same_media() {
    validate_style_to_link(
        "import_media_rides",
        b"<style>@import url(assets/styles.css) all</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\" media=\"all\">",
    );
    validate_style_to_link(
        "import_media_typed",
        b"<style type=\"text/css\">@import url(assets/styles.css) all;</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\" type=\"text/css\" media=\"all\">",
    );
    validate_style_to_link(
        "matching_media",
        b"<style type=\"text/css\" media=\"screen\">@import url(assets/styles.css) screen;</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\" type=\"text/css\" media=\"screen\">",
    );
    // Media order is insignificant.
    validate_style_to_link(
        "media_reordered",
        b"<style type=\"text/css\" media=\"screen,printer\">@import url(assets/styles.css) printer,screen;</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\" type=\"text/css\" media=\"screen,printer\">",
    );
    // The style's own (spaced) spelling survives the match.
    validate_style_to_link(
        "media_spaced",
        b"<style type=\"text/css\" media=\" screen , printer \">@import 'assets/styles.css' printer, screen ;</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\" type=\"text/css\" media=\" screen , printer \">",
    );
}

#[test]
fn convert_style_with_different_media() {
    validate_unchanged(
        "different_media",
        b"<style type=\"text/css\" media=\"screen\">@import url(assets/styles.css) all;</style>",
    );
    validate_unchanged(
        "different_media_pair",
        b"<style type=\"text/css\" media=\"screen,printer\">@import url(assets/styles.css) screen;</style>",
    );
}

#[test]
fn media_queries() {
    // A bare import keeps even complex style media on the link.
    validate_style_to_link(
        "bare_import_keeps_media",
        b"<style type=\"text/css\" media=\"not screen\">@import url(assets/styles.css);</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\" type=\"text/css\" media=\"not screen\">",
    );
    // Complex import queries always veto.
    for (index, style) in [
        "<style type=\"text/css\">@import url(assets/styles.css) not screen;</style>",
        "<style type=\"text/css\" media=\"not screen\">@import url(assets/styles.css) not screen;</style>",
        "<style media=\"not screen and (color), only print\">@import url(assets/styles.css) not screen and (color), only print;</style>",
        "<style type=\"text/css\" media=\"not screen\">@import url(assets/styles.css) screen;</style>",
        "<style type=\"text/css\" media=\"screen and (x)\">@import url(assets/styles.css) screen;</style>",
    ]
    .iter()
    .enumerate()
    {
        validate_unchanged(&format!("complex_veto_{index}"), style.as_bytes());
    }
}

#[test]
fn do_not_convert_bad_style() {
    for (index, style) in [
        "<style/>",
        "<style></style>",
        "<style>@import assets/styles.css;</style>",
        "<style>@import assets/styles.css</style>",
        "<style>@import styles.css</style>",
        "<style>@import foo</style>",
        "<style>@import url (assets/styles.css);</style>",
        "<style>@ import url(assets/styles.css)</style>",
        "<style>*border: 0px</style>",
        "<style>@charset \"ISO-8859-1\";\n@import \"mystyle.css\" all;</style>",
        "<style><p/>@import url(assets/styles.css)</style>",
        "<style><![CDATA[@import url(assets/styles.css);]]\n",
        "<style><![CDATA[\njunky junk junk!\n]]\\>\n@import url(assets/styles.css);</style>",
        "<style><!-- comment -->@import url(assets/styles.css);</style>",
        "<style href='x'>@import url(styles.css);</style>",
        "<style rel='x'>@import url(styles.css);</style>",
        "<style type=\"text/javascript\">@import url(assets/styles.css);</style>",
        "<style>@import url(styles.css)<style/></style>",
    ]
    .iter()
    .enumerate()
    {
        validate_unchanged(&format!("bad_{index}"), style.as_bytes());
    }

    // Errors after valid imports still convert.
    validate_style_to_link(
        "trailing_tag_soup",
        b"<style>@import url(assets/styles.css);<p/</style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\"><style><p/</style>",
    );
    validate_style_to_link(
        "trailing_cdata",
        b"<style>@import url(assets/styles.css);\n<![CDATA[\njunky junk junk!\n]]\\></style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\"><style><![CDATA[\njunky junk junk!\n]]\\></style>",
    );
    validate_style_to_link(
        "trailing_comment",
        b"<style>@import url(assets/styles.css);<!-- comment --></style>",
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\"><style><!-- comment --></style>",
    );
}

#[test]
fn unclosed_style_gets_converted() {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(CssInlineImportToLinkFilter::new(config()));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/unclosed_style.html", false);
    parser.write(b"<style>@import url(assets/styles.css)");
    parser.finish();
    assert_eq!(
        sink.bytes(),
        b"<link rel=\"stylesheet\" href=\"assets/styles.css\">"
    );
    assert_eq!(filter.with(|filter| filter.imports_to_links()), 1);
}

//! Per-filter golden tests ported from the C++ filter test bodies.
#[path = "harness.rs"]
mod harness;

use harness::{run_with_filters, wrap_body};
use ps_html::filter::HtmlFilter;
use ps_html::filters::js_disable::JsDisableConfig;
use ps_html::filters::{
    instantiate, should_insert_noscript, AddHeadFilter, AddIdsFilter, BaseTagFilter,
    CollapseWhitespaceFilter, CssMinifyFilter, DeferIframeFilter, DeterministicJsFilter,
    ElideAttributesFilter, FixReflowFilter, FlushHtmlFilter, HandleNoscriptRedirectFilter,
    InsertAmpLinkFilter, JsDisableFilter, JsMinifyFilter, PedanticFilter, QuoteRemovalFilter,
    RemoveCommentsFilter, StripLegacyPolyfillsFilter, StripScriptsFilter, SupportNoscriptFilter,
    XhtmlStatus, DEFER_IFRAME_IFRAME_JS, ELEMENT_ONLOAD_CODE,
};
use ps_html::parser::Parser;
use ps_html::static_assets::{get_asset, StaticAsset};
use ps_html::writer::WriterFilter;

use harness::SharedSink;
use ps_html::chain::ChainFilter;

/// Ports `ValidateExpected` through one filter (body-wrapped).
fn validate_filtered(id: &str, input: &[u8], expected: &[u8], build: impl FnOnce(&mut Parser)) {
    let wrapped_input = wrap_body(input);
    let wrapped_expected = wrap_body(expected);
    let output = run_with_filters(&wrapped_input, build);
    assert_eq!(output, wrapped_expected, "case {id}");
}

#[test]
fn remove_comments_cases() {
    // Ports RemoveCommentsFilterTest (no body wrap).
    let plain: &[(&str, &[u8], &[u8])] = &[
        (
            "no_comments",
            b"<head><title>Hello</title></head><body>Why, hello there!</body>",
            b"<head><title>Hello</title></head><body>Why, hello there!</body>",
        ),
        (
            "remove_comment",
            b"<body>hello <!--world--></body>",
            b"<body>hello </body>",
        ),
        (
            "remove_multiple_comments",
            b"<head><!--1--><title>Hi</title><!--2--></head><body><!--3-->hello<!--4--><!--5--></body>",
            b"<head><title>Hi</title></head><body>hello</body>",
        ),
        (
            "do_not_remove_ie_directive",
            b"<body>hello <!--[if IE 8]>world<![endif]--></body>",
            b"<body>hello <!--[if IE 8]>world<![endif]--></body>",
        ),
        (
            "comment_in_tag",
            b"<div><!--</div>-->",
            b"<div>",
        ),
        (
            "empty_comment",
            b"<p>a<!--></p><textarea>--><b>text</b></textarea>",
            b"<p>a</p><textarea>--><b>text</b></textarea>",
        ),
        (
            "empty_comment_dash",
            b"<p>a<!---></p><textarea>--><b>text</b></textarea>",
            b"<p>a</p><textarea>--><b>text</b></textarea>",
        ),
        (
            "comment_end_bang",
            b"<p>a<!--x--!></p><textarea>--><b>text</b></textarea>",
            b"<p>a</p><textarea>--><b>text</b></textarea>",
        ),
        (
            "comment_in_xmp",
            b"<xmp><!-- keep me --></xmp>",
            b"<xmp><!-- keep me --></xmp>",
        ),
        (
            "comment_in_overlapping_xmp",
            b"<xmp><!--</xmp>-->",
            b"<xmp><!--</xmp>-->",
        ),
    ];
    for (id, input, expected) in plain {
        let output = run_with_filters(input, |parser| {
            parser.add_filter(Box::new(RemoveCommentsFilter::default()));
        });
        assert_eq!(output, *expected, "case {id}");
    }
    // Retained comments survive; others still go.
    let output = run_with_filters(
        b"<body>hello <!-- google_ad_section_start --></body>",
        |parser| {
            let mut filter = RemoveCommentsFilter::default();
            filter.retain_comment("*google_ad_section_*");
            parser.add_filter(Box::new(filter));
        },
    );
    assert_eq!(
        output,
        b"<body>hello <!-- google_ad_section_start --></body>"
    );
    let output = run_with_filters(b"<body>hello <!--world--></body>", |parser| {
        let mut filter = RemoveCommentsFilter::default();
        filter.retain_comment("*google_ad_section_*");
        parser.add_filter(Box::new(filter));
    });
    assert_eq!(output, b"<body>hello </body>");
}

#[test]
fn collapse_whitespace_cases() {
    // Ports CollapseWhitespaceFilterTest (body wrap).
    validate_filtered(
        "no_change",
        b"<head><title>Hello</title></head><body>Why, hello there!</body>",
        b"<head><title>Hello</title></head><body>Why, hello there!</body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
    validate_filtered(
        "collapse_whitespace",
        b"<body>hello   world,   it\n    is good  to     see you   </body>",
        b"<body>hello world, it\nis good to see you </body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
    validate_filtered(
        "newline_takes_precedence",
        b"<body>hello world, it      \n    is good to see you</body>",
        b"<body>hello world, it\nis good to see you</body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
    validate_filtered(
        "do_not_collapse_within_code",
        b"<body><code>hello   world,   it\n    is good  to     see you   </code></body>",
        b"<body><code>hello   world,   it\n    is good  to     see you   </code></body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
    validate_filtered(
        "do_not_collapse_within_pre",
        b"<body><pre>hello   world,   it\n    is good  to     see you   </pre></body>",
        b"<body><pre>hello   world,   it\n    is good  to     see you   </pre></body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
    validate_filtered(
        "collapse_after_nested_pre",
        b"<body><pre>hello   <pre>world,   it</pre>\n    is good</pre>  to     see you   </body>",
        b"<body><pre>hello   <pre>world,   it</pre>\n    is good</pre> to see you </body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
    validate_filtered(
        "do_not_collapse_within_script",
        b"<head><script>x = \"don't    collapse\"</script></head><body>do       collapse</body>",
        b"<head><script>x = \"don't    collapse\"</script></head><body>do collapse</body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
    validate_filtered(
        "do_not_collapse_within_style",
        b"<head><style>P{font-family:\"don't   collapse\";}</style></head><body></body>",
        b"<head><style>P{font-family:\"don't   collapse\";}</style></head><body></body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
    validate_filtered(
        "do_not_collapse_within_textarea",
        b"<body><textarea>hello   world,   it\n    is good  to     see you   </textarea></body>",
        b"<body><textarea>hello   world,   it\n    is good  to     see you   </textarea></body>",
        |parser| parser.add_filter(Box::new(CollapseWhitespaceFilter::default())),
    );
}

#[test]
fn collapse_whitespace_pass_through() {
    // Ports TextPassedThroughIsLeftAlone (no wrap; manual flush boundary).
    let head = b"<html><body>one   two<br>";
    let tail = b"<pre>three   four</pre>five   six</body></html>";
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(CollapseWhitespaceFilter::default()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.set_pass_through_after(head.len() as i64);
    parser.start_parse(b"http://example.com/large.html", false);
    parser.write(head);
    parser.flush();
    parser.write(tail);
    parser.finish();
    let mut expected = b"<html><body>one two<br>".to_vec();
    expected.extend_from_slice(tail);
    assert_eq!(sink.bytes(), expected);
}

const HTML4_DOCTYPE: &[u8] = b"<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\">";
const XHTML_DOCTYPE: &[u8] = b"<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">";
const HTML5_DOCTYPE: &[u8] = b"<!doctype html>";

fn with_doctype(doctype: &[u8], fragment: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(doctype.len() + fragment.len());
    out.extend_from_slice(doctype);
    out.extend_from_slice(fragment);
    out
}

#[test]
fn elide_attributes_cases() {
    // Ports ElideAttributesFilterTest (no body wrap; doctypes inline since
    // the C++ SetDoctype hook has no Rust counterpart yet).
    let plain: &[(&str, &[u8], &[u8])] = &[
        (
            "no_changes",
            b"<head><script src=\"foo.js\"></script></head><body><form method=\"post\"><input type=\"checkbox\" checked></form></body>",
            b"<head><script src=\"foo.js\"></script></head><body><form method=\"post\"><input type=\"checkbox\" checked></form></body>",
        ),
        (
            "remove_attr_with_default_value",
            b"<head></head><body><form method=get></form></body>",
            b"<head></head><body><form></form></body>",
        ),
    ];
    for (id, input, expected) in plain {
        let output = run_with_filters(input, |parser| {
            parser.add_filter(Box::new(ElideAttributesFilter));
        });
        assert_eq!(output, *expected, "case {id}");
    }
    let doctype_cases: &[(&str, &[u8], &[u8], &[u8])] = &[
        (
            "remove_value_from_attr",
            HTML4_DOCTYPE,
            b"<head></head><body><form><input type=checkbox checked=checked></form></body>",
            b"<head></head><body><form><input type=checkbox checked></form></body>",
        ),
        (
            "do_not_remove_value_from_attr_in_xhtml",
            XHTML_DOCTYPE,
            b"<head></head><body><form><input type=checkbox checked=checked></form></body>",
            b"<head></head><body><form><input type=checkbox checked=checked></form></body>",
        ),
        (
            "do_not_break_vbscript",
            HTML5_DOCTYPE,
            b"<head><script language=\"JavaScript\">var x=1;</script><script language=\"VBScript\">Sub foo(ByVal bar)\n  call baz(bar)\nend sub</script></head><body></body>",
            b"<head><script>var x=1;</script><script language=\"VBScript\">Sub foo(ByVal bar)\n  call baz(bar)\nend sub</script></head><body></body>",
        ),
        (
            "remove_script_type_in_html_5",
            HTML5_DOCTYPE,
            b"<head><script src=\"foo.js\" type=\"text/javascript\"></script></head><body></body>",
            b"<head><script src=\"foo.js\"></script></head><body></body>",
        ),
        (
            "do_not_remove_script_type_in_html_4",
            HTML4_DOCTYPE,
            b"<head><script src=\"foo.js\" type=\"text/javascript\"></script></head><body></body>",
            b"<head><script src=\"foo.js\" type=\"text/javascript\"></script></head><body></body>",
        ),
        (
            "do_not_remove_type_attribute_from_input",
            HTML4_DOCTYPE,
            b"<head></head><body><form><input type=text></form></body>",
            b"<head></head><body><form><input type=text></form></body>",
        ),
    ];
    for (id, doctype, input, expected) in doctype_cases {
        let output = run_with_filters(&with_doctype(doctype, input), |parser| {
            parser.add_filter(Box::new(ElideAttributesFilter));
        });
        assert_eq!(output, with_doctype(doctype, expected), "case {id}");
    }
}

#[test]
fn quote_removal_cases() {
    // Ports HtmlAttributeQuoteRemovalTest (body wrap).
    validate_filtered(
        "no_quotes_no_change",
        b"<div class=foo id=bar>foobar</div>",
        b"<div class=foo id=bar>foobar</div>",
        |parser| parser.add_filter(Box::new(QuoteRemovalFilter::default())),
    );
    validate_filtered(
        "do_not_remove_needed_quotes",
        b"<a href=\"http://www.example.com/\">foobar</a>",
        b"<a href=\"http://www.example.com/\">foobar</a>",
        |parser| parser.add_filter(Box::new(QuoteRemovalFilter::default())),
    );
    validate_filtered(
        "do_not_delete_empty_attrs",
        b"<div id=''></div>",
        b"<div id=''></div>",
        |parser| parser.add_filter(Box::new(QuoteRemovalFilter::default())),
    );
    validate_filtered(
        "remove_unneeded_quotes",
        b"<div class=\"foo\" id='bar'>foobar</div>",
        b"<div class=foo id=bar>foobar</div>",
        |parser| parser.add_filter(Box::new(QuoteRemovalFilter::default())),
    );
    validate_filtered(
        "no_value_no_change",
        b"<input checked type=checkbox>",
        b"<input checked type=checkbox>",
        |parser| parser.add_filter(Box::new(QuoteRemovalFilter::default())),
    );
    validate_filtered(
        "do_not_remove_quotes_in_xhtml",
        &with_doctype(XHTML_DOCTYPE, b"<div class=\"foo\" id='bar'>foobar</div>"),
        &with_doctype(XHTML_DOCTYPE, b"<div class=\"foo\" id='bar'>foobar</div>"),
        |parser| parser.add_filter(Box::new(QuoteRemovalFilter::default())),
    );
    validate_filtered(
        "keep_quotes_on_8bit_values",
        "<div class=\"muñecos\" id='muñecos'>foobar</div>".as_bytes(),
        "<div class=\"muñecos\" id='muñecos'>foobar</div>".as_bytes(),
        |parser| parser.add_filter(Box::new(QuoteRemovalFilter::default())),
    );
}

#[test]
fn strip_scripts_cases() {
    // Ports StripScriptsFilterTest (no body wrap).
    let cases: &[(&str, &[u8], &[u8])] = &[
        (
            "remove_script_src",
            b"<head><script src='http://www.google.com/javascript/ajax_apis.js'></script></head><body>Hello, world!</body>",
            b"<head></head><body>Hello, world!</body>",
        ),
        (
            "remove_script_inline",
            b"<head><script>alert('Alert, alert!')</script></head><body>Hello, world!</body>",
            b"<head></head><body>Hello, world!</body>",
        ),
    ];
    for (id, input, expected) in cases {
        let output = run_with_filters(input, |parser| {
            parser.add_filter(Box::new(StripScriptsFilter));
        });
        assert_eq!(output, *expected, "case {id}");
    }
}

#[test]
fn pedantic_cases() {
    // Ports PedanticFilterTest (no body wrap; HTML5 doctypes inline).
    let plain: &[(&str, &[u8], &[u8])] = &[
        (
            "change_style_with_no_type",
            b"<head><style>h1 {color : #ff0000;}</style></head>",
            b"<head><style type=\"text/css\">h1 {color : #ff0000;}</style></head>",
        ),
        (
            "do_not_break_style_type",
            b"<head><style type=\"text/css2\">h1 {color : #ff0000;}</style><head>",
            b"<head><style type=\"text/css2\">h1 {color : #ff0000;}</style><head>",
        ),
        (
            "change_script_with_no_type",
            b"<head><script>var x=1;</script></head>",
            b"<head><script type=\"text/javascript\">var x=1;</script></head>",
        ),
        (
            "do_not_break_script_type",
            b"<head><script type=\"text/ecmascript\">var x=1;</script></head>",
            b"<head><script type=\"text/ecmascript\">var x=1;</script></head>",
        ),
    ];
    for (id, input, expected) in plain {
        let output = run_with_filters(input, |parser| {
            parser.add_filter(Box::new(PedanticFilter));
        });
        assert_eq!(output, *expected, "case {id}");
    }
    let html5: &[(&str, &[u8])] = &[
        (
            "do_not_alter_html_5_style",
            b"<head><style>h1 {color : #ff0000;}</style></head>",
        ),
        (
            "do_not_alter_html_5_script",
            b"<head><script>var x=1;</script></head>",
        ),
    ];
    for (id, fragment) in html5 {
        let input = with_doctype(HTML5_DOCTYPE, fragment);
        let output = run_with_filters(&input, |parser| {
            parser.add_filter(Box::new(PedanticFilter));
        });
        assert_eq!(output, input, "case {id}");
    }
}

#[test]
fn base_tag_cases() {
    // Ports BaseTagFilterTest (no body wrap; base URL injected).
    let cases: &[(&str, &[u8], &[u8], &[u8])] = &[
        (
            "single_head",
            b"http://test.com/single_head.html",
            b"<head></head><body><img src=\"1.jpg\" /></body>",
            b"<head><base href=\"http://test.com/single_head.html\"></head><body><img src=\"1.jpg\"/></body>",
        ),
        (
            "multiple_heads",
            b"http://test.com/multiple_heads.html",
            b"<head></head><head></head><body></body>",
            b"<head><base href=\"http://test.com/multiple_heads.html\"></head><head></head><body></body>",
        ),
    ];
    for (id, base, input, expected) in cases {
        let output = run_with_filters(input, |parser| {
            parser.add_filter(Box::new(BaseTagFilter::new(base)));
        });
        assert_eq!(output, *expected, "case {id}");
    }
    // NoHeadTag runs through the driver chain: AddHead supplies the head
    // that BaseTag then fills (chain order: AddHead before BaseTag).
    let output = run_with_filters(b"<body><img src=\"1.jpg\" /></body>", |parser| {
        parser.add_filter(Box::new(AddHeadFilter::new(false)));
        parser.add_filter(Box::new(BaseTagFilter::new(
            b"http://test.com/no_head.html",
        )));
    });
    assert_eq!(
        output,
        b"<head><base href=\"http://test.com/no_head.html\"></head><body><img src=\"1.jpg\"/></body>"
    );
}

#[test]
fn add_head_cases() {
    // Hand-derived from the implementation (C++ has no unit test):
    // present heads are untouched, missing heads are inserted, empty
    // documents gain one at the end, and combine mode merges extras.
    let output = run_with_filters(b"<head><title>t</title></head><body>x</body>", |parser| {
        parser.add_filter(Box::new(AddHeadFilter::new(false)));
    });
    assert_eq!(output, b"<head><title>t</title></head><body>x</body>");

    let output = run_with_filters(b"<html><body>x</body></html>", |parser| {
        parser.add_filter(Box::new(AddHeadFilter::new(false)));
    });
    assert_eq!(output, b"<html><head/><body>x</body></html>");

    let output = run_with_filters(b"hello", |parser| {
        parser.add_filter(Box::new(AddHeadFilter::new(false)));
    });
    assert_eq!(output, b"hello<head/>");

    let input = b"<head><title>a</title></head><head><title>b</title></head><body></body>";
    let output = run_with_filters(input, |parser| {
        parser.add_filter(Box::new(AddHeadFilter::new(true)));
    });
    assert_eq!(
        output,
        b"<head><title>a</title><title>b</title></head><body></body>"
    );
    let output = run_with_filters(input, |parser| {
        parser.add_filter(Box::new(AddHeadFilter::new(false)));
    });
    assert_eq!(output, input);
}

#[test]
fn noscript_redirect_cases() {
    // Ports HandleNoscriptRedirectFilterTest (no body wrap; document URL
    // injected — the C++ names it after each test id).
    let cases: &[(&str, &[u8], &[u8], &[u8])] = &[
        (
            "one_head",
            b"http://test.com/one_head.html",
            b"<head></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
            b"<head><link rel=\"canonical\" href=\"http://test.com/one_head.html\"/></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
        ),
        (
            "multiple_heads",
            b"http://test.com/multiple_heads.html",
            b"<head></head><head></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
            b"<head><link rel=\"canonical\" href=\"http://test.com/multiple_heads.html\"/></head><head></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
        ),
        (
            "one_head_canonical",
            b"http://test.com/one_head_canonical.html",
            b"<head><link rel=\"canonical\" href=\"http://test.com/foo.html\"></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
            b"<head><link rel=\"canonical\" href=\"http://test.com/foo.html\"></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
        ),
        (
            "two_head_canonical",
            b"http://test.com/two_head_canonical.html",
            b"<head></head><head><link rel=\"canonical\" href=\"http://test.com/foo.html\"/></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
            b"<head><link rel=\"canonical\" href=\"http://test.com/two_head_canonical.html\"/></head><head><link rel=\"canonical\" href=\"http://test.com/foo.html\"/></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
        ),
        (
            "two_links_in_head",
            b"http://test.com/two_links_in_head.html",
            b"<head><link rel=\"canonical\" href=\"http://test.com/foo.html\"><link href=special.css rel=stylesheet type=text/css/></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
            b"<head><link rel=\"canonical\" href=\"http://test.com/foo.html\"><link href=special.css rel=stylesheet type=text/css/></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
        ),
    ];
    for (id, url, input, expected) in cases {
        let output = run_with_filters(input, |parser| {
            parser.add_filter(Box::new(HandleNoscriptRedirectFilter::new(url)));
        });
        assert_eq!(output, *expected, "case {id}");
    }
    // NoHead runs through the driver chain: AddHead supplies the head that
    // the redirect filter then fills.
    let output = run_with_filters(
        b"<body><img src=\"http://test.com/1.jpeg\"/></body>",
        |parser| {
            parser.add_filter(Box::new(AddHeadFilter::new(false)));
            parser.add_filter(Box::new(HandleNoscriptRedirectFilter::new(
                b"http://test.com/no_head.html",
            )));
        },
    );
    assert_eq!(
        output,
        b"<head><link rel=\"canonical\" href=\"http://test.com/no_head.html\"/></head><body><img src=\"http://test.com/1.jpeg\"/></body>"
    );
}

/// Strips generated ` id="PageSpeed-..."` attributes (`Unlabel`).
fn unlabel(labeled: &[u8]) -> Vec<u8> {
    let mut out = labeled.to_vec();
    let start = b" id=\"PageSpeed-";
    loop {
        let Some(pos) = out.windows(start.len()).position(|window| window == start) else {
            break;
        };
        let rest = pos + start.len();
        match out[rest..].iter().position(|byte| *byte == b'"') {
            Some(end) => out.drain(pos..=rest + end),
            None => break,
        };
    }
    out
}

#[test]
fn add_ids_cases() {
    // Ports AddIdsFilterTest (no body wrap; inputs unlabeled like C++).
    let no_divs = concat!(
        "<html><body>\n",
        "  <p>Today's top stories are:</p>\n",
        "  <ol>\n",
        "    <li><a href='http://www.example1.com/'>Website wins award for most boring URL.</a></li>\n",
        "    <li><a href='http://www.example2.com/'>Copycats quickly try to steal some spotlight.</a></li>\n",
        "    <li><a href='http://www.example3.com/'>Internet proves itself capable of spawning copycat copycats.</a></li>\n",
        "    <li><a href='http://www.example5.com/'>Embarrassed imitator ruins trend.</a></li>\n",
        "  </ol>\n",
        "</body></html>\n",
    );
    let output = run_with_filters(no_divs.as_bytes(), |parser| {
        parser.add_filter(Box::new(AddIdsFilter::default()));
    });
    assert_eq!(output, no_divs.as_bytes());

    let with_divs = concat!(
        "<html><body>\n",
        "  <div id='menu'>\n",
        "   <ul id=\"PageSpeed-menu-0\">\n",
        "    <li><a href='http://www.example.com/home'>HOME</a></li>\n",
        "    <li><a href='http://www.example.com/contact_us'>CONTACT US</a></li>\n",
        "    <li><a href='http://www.example.com/about'>ABOUT</a></li>\n",
        "    <li><div id=\"PageSpeed-menu-0-3-0\">Share this</div></li>\n",
        "   </ul>\n",
        "  </div>\n",
        "  <div id=\"PageSpeed-1\"></div>\n",
        "  <div id='content'>\n",
        "    <div class='top_story' id=\"PageSpeed-content-0\">\n",
        "      <div id=\"PageSpeed-content-0-0\">TOP STORY</div>\n",
        "    </div>\n",
        "    <div class='stories' id=\"PageSpeed-content-1\">\n",
        "      <div id=\"PageSpeed-content-1-0\">STORY ONE</div>\n",
        "      <div id=\"PageSpeed-content-1-1\">STORY TWO</div>\n",
        "      <div id=\"PageSpeed-content-1-2\">STORY THREE</div>\n",
        "    </div>\n",
        "  </div>\n",
        "</body>\n",
        "<div id=\"Pagespeed-2\">   Post-BODY content</div></html>\n",
    );
    let output = run_with_filters(&unlabel(with_divs.as_bytes()), |parser| {
        parser.add_filter(Box::new(AddIdsFilter::default()));
    });
    assert_eq!(output, with_divs.as_bytes());

    let body_id = with_divs
        .replacen("<html><body>", "<html><body id='body'>", 1)
        .replace(
            "<div id=\"PageSpeed-1\"></div>",
            "<div id=\"PageSpeed-body-1\"></div>",
        );
    let output = run_with_filters(&unlabel(body_id.as_bytes()), |parser| {
        parser.add_filter(Box::new(AddIdsFilter::default()));
    });
    assert_eq!(output, body_id.as_bytes());

    let table = concat!(
        "<table id=\"PageSpeed-0\">\n",
        "  <thead>\n",
        "    <tr id=\"PageSpeed-0-0-0\">\n",
        "      <th>Header content 1</th>\n",
        "      <th>Header content 2</th>\n",
        "    </tr>\n",
        "  </thead>\n",
        "  <tfoot>\n",
        "    <tr id=\"PageSpeed-0-1-0\">\n",
        "      <td>Footer content 1</td>\n",
        "      <td>Footer content 2</td>\n",
        "    </tr>\n",
        "  </tfoot>\n",
        "  <tbody>\n",
        "    <tr id=\"PageSpeed-0-2-0\">\n",
        "      <td>Body content 1</td>\n",
        "      <td>Body content 2</td>\n",
        "    </tr>\n",
        "  </tbody>\n",
        "</table>",
    );
    let output = run_with_filters(&unlabel(table.as_bytes()), |parser| {
        parser.add_filter(Box::new(AddIdsFilter::default()));
    });
    assert_eq!(output, table.as_bytes());

    for fragment in [
        "<header id='głowna'>Header</header>\n",
        "<header id=''>Header</header>\n<content id>Content</content>\n",
    ] {
        let output = run_with_filters(fragment.as_bytes(), |parser| {
            parser.add_filter(Box::new(AddIdsFilter::default()));
        });
        assert_eq!(output, fragment.as_bytes());
    }
}

#[test]
fn add_ids_two_digit_counts() {
    // Ports TwoDigitDivCountTest: multi-digit counters nest correctly.
    let mut expected = String::from("<html><body>\n  <menu id='menu'>\n");
    for link in 1..=10 {
        expected.push_str(&format!(
            "    <div id=\"PageSpeed-menu-{}\">Link {}</div>\n",
            link - 1,
            link
        ));
    }
    expected.push_str("    <div id=\"PageSpeed-menu-10\">Submenu 11\n");
    for nested in 0..=10 {
        expected.push_str(&format!(
            "      <div id=\"PageSpeed-menu-10-{nested}\">Nested {nested}</div>\n"
        ));
    }
    expected.push_str(
        "    </div>\n  </menu>\n  <div id=\"PageSpeed-1\">\n    This page contains a large menu of links.\n  </div>\n</body></html>\n",
    );
    let output = run_with_filters(&unlabel(expected.as_bytes()), |parser| {
        parser.add_filter(Box::new(AddIdsFilter::default()));
    });
    assert_eq!(output, expected.as_bytes());
}

#[test]
fn add_ids_mid_tag_flush() {
    // Ports MidTagFlushTest: explicit ids stay live across the flush so
    // later divs still label against them.
    let first = "<html><body>\n  <div id='a'>\n    <div id='b'>\n";
    let second = concat!(
        "      <div id=\"PageSpeed-b-0\">\n",
        "        <div id=\"PageSpeed-b-0-0\">\n",
        "        </div>\n",
        "      </div>\n",
        "    </div>\n",
        "    <div id=\"Pagespeed-a-0\">\n",
        "      <div id=\"Pagespeed-a-0-0\">\n",
        "      </div>\n",
        "    </div>\n",
        "  </div>\n",
        "</body></html>\n",
    );
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(AddIdsFilter::default()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/mid_tag_flush.html", false);
    parser.write(first.as_bytes());
    parser.flush();
    parser.write(&unlabel(second.as_bytes()));
    parser.finish();
    let mut expected = first.as_bytes().to_vec();
    expected.extend_from_slice(second.as_bytes());
    assert_eq!(sink.bytes(), expected);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn deterministic_js_cases() {
    // Ports DeterministicJsFilterTest (no body wrap; HTML mimetype means
    // no CDATA wrapper).
    let js = get_asset(StaticAsset::DeterministicJs, false);
    assert!(contains(js, b"Date"));
    assert!(contains(js, b"Math.random"));
    let mut expected_bare = Vec::new();
    expected_bare
        .extend_from_slice(b"<head><script type=\"text/javascript\" data-pagespeed-no-defer>");
    expected_bare.extend_from_slice(js);
    expected_bare.extend_from_slice(b"</script></head><body></body>");
    let mut expected_link = Vec::new();
    expected_link
        .extend_from_slice(b"<head><script type=\"text/javascript\" data-pagespeed-no-defer>");
    expected_link.extend_from_slice(js);
    expected_link.extend_from_slice(
        b"</script><link rel=\"stylesheet\" href=\"a.css\"></head><body></body>",
    );
    let cases: &[(&str, &[u8], &[u8])] = &[
        ("injection", b"<head></head><body></body>", &expected_bare),
        (
            "injection_with_head_content",
            b"<head><link rel=\"stylesheet\" href=\"a.css\"></head><body></body>",
            &expected_link,
        ),
    ];
    for (id, input, expected) in cases {
        let output = run_with_filters(input, |parser| {
            parser.add_filter(Box::new(DeterministicJsFilter::new(
                js,
                false,
                XhtmlStatus::IsNotXhtml,
            )));
        });
        assert_eq!(output, *expected, "case {id}");
    }
}

fn pagespeed_iframe_tag(src: &[u8]) -> Vec<u8> {
    [
        b"<pagespeed_iframe src=\"".as_slice(),
        src,
        b"\"><script type=\"text/javascript\">".as_slice(),
        DEFER_IFRAME_IFRAME_JS,
        b"</script></pagespeed_iframe>",
    ]
    .concat()
}

#[test]
fn defer_iframe_cases() {
    // Ports DeferIframeFilterTest (no body wrap).
    let js = get_asset(StaticAsset::DeferIframe, false);
    let support = [
        b"<script type=\"text/javascript\">".as_slice(),
        js,
        b"pagespeed.deferIframeInit();</script>",
    ]
    .concat();
    let one = b"<head></head><body><iframe src=\"http://test.com/1.html\"/></body>";
    let mut expected_one = b"<head></head><body>".to_vec();
    expected_one.extend_from_slice(&support);
    expected_one.extend_from_slice(&pagespeed_iframe_tag(b"http://test.com/1.html"));
    expected_one.extend_from_slice(b"</body>");
    let two = b"<head></head><body>\
        <iframe src=\"http://test.com/1.html\"/>\
        <iframe src=\"http://test.com/2.html\"/></body>";
    let mut expected_two = b"<head></head><body>".to_vec();
    expected_two.extend_from_slice(&support);
    expected_two.extend_from_slice(&pagespeed_iframe_tag(b"http://test.com/1.html"));
    expected_two.extend_from_slice(&pagespeed_iframe_tag(b"http://test.com/2.html"));
    expected_two.extend_from_slice(b"</body>");
    // Support script inserted once even for several iframes.
    let cases: &[(&str, &[u8], &[u8])] = &[
        ("defer_iframe", one, &expected_one),
        (
            "no_iframe",
            b"<head></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
            b"<head></head><body><img src=\"http://test.com/1.jpeg\"/></body>",
        ),
        (
            "iframe_in_noscript",
            b"<head></head><body><noscript><iframe src=\"http://test.com/1.html\"/></noscript></body>",
            b"<head></head><body><noscript><iframe src=\"http://test.com/1.html\"/></noscript></body>",
        ),
        ("multiple_iframes", two, &expected_two),
    ];
    for (id, input, expected) in cases {
        let output = run_with_filters(input, |parser| {
            parser.add_filter(Box::new(DeferIframeFilter::new(
                js,
                true,
                false,
                XhtmlStatus::IsNotXhtml,
            )));
        });
        assert_eq!(output, *expected, "case {id}");
    }
}

#[test]
fn iframe_guard_snippet_shape() {
    // Ports the DeferIframe row of InlineScriptGuardTest: guarded call,
    // XML-safe raw script text.
    assert_eq!(
        DEFER_IFRAME_IFRAME_JS,
        b"\nif(window.pagespeed){if(pagespeed.deferIframe){pagespeed.deferIframe.convertToIframe();}}"
    );
    let code = DEFER_IFRAME_IFRAME_JS;
    let start = code.iter().position(|byte| *byte != b'\n').unwrap();
    assert!(code[start..].starts_with(b"if(window.pagespeed){if(pagespeed."));
    assert!(!code
        .iter()
        .any(|byte| matches!(byte, b'<' | b'>' | b'"' | b'\'' | b'&')));
}

#[test]
fn fix_reflow_cases() {
    // Ports FixReflowFilterTest in isolation (no noscript/js_defer driver
    // companions): the property value is injected, misses pass through.
    let input = b"<body><div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div></body>";
    let output = run_with_filters(input, |parser| {
        parser.add_filter(Box::new(FixReflowFilter::new(None)));
    });
    assert_eq!(output, input);
    let output = run_with_filters(input, |parser| {
        parser.add_filter(Box::new(FixReflowFilter::new(Some(
            b"contentContainer:10px,",
        ))));
    });
    assert_eq!(
        output,
        b"<body><div id=\"contentContainer\" style=\"min-height:10px\" data-pagespeed-fix-reflow=\"\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div></body>"
    );
    let output = run_with_filters(input, |parser| {
        parser.add_filter(Box::new(FixReflowFilter::new(Some(
            b"middleFooter:10px,contentContainer:20px,",
        ))));
    });
    assert_eq!(
        output,
        b"<body><div id=\"contentContainer\" style=\"min-height:20px\" data-pagespeed-fix-reflow=\"\"><h1>Hello 1</h1><div id=\"middleFooter\" style=\"min-height:10px\" data-pagespeed-fix-reflow=\"\"><h3>Hello 3</h3></div></div></body>"
    );
}

#[test]
fn support_noscript_cases() {
    // Ports SupportNoscriptFilterTest (no body wrap; the escaped document
    // URL and enablement verdict are driver inputs, injected here).
    let url = b"http://test.com/support_noscript&#39;%22.html?PageSpeed=noscript";
    let input = b"<head></head><body><img src=\"http://test.com/1.jpeg\"/></body>";
    let mut expected =
        b"<head></head><body><noscript><meta HTTP-EQUIV=\"refresh\" content=\"0;url='".to_vec();
    expected.extend_from_slice(url);
    expected.extend_from_slice(b"'\" /><style><!--table,div,span,font,p{display:none} --></style><div style=\"display:block\">Please click <a href=\"");
    expected.extend_from_slice(url);
    expected.extend_from_slice(b"\">here</a> if you are not redirected within a few seconds.</div></noscript><img src=\"http://test.com/1.jpeg\"/></body>");
    let output = run_with_filters(input, |parser| {
        parser.add_filter(Box::new(SupportNoscriptFilter::new(url, true)));
    });
    assert_eq!(output, expected);
    // Second bodies are left alone.
    let two = b"<head></head><body><img src=\"http://test.com/1.jpeg\"/></body><body><img src=\"http://test.com/2.jpeg\"/></body>";
    let mut expected_two =
        b"<head></head><body><noscript><meta HTTP-EQUIV=\"refresh\" content=\"0;url='".to_vec();
    expected_two.extend_from_slice(url);
    expected_two.extend_from_slice(b"'\" /><style><!--table,div,span,font,p{display:none} --></style><div style=\"display:block\">Please click <a href=\"");
    expected_two.extend_from_slice(url);
    expected_two.extend_from_slice(b"\">here</a> if you are not redirected within a few seconds.</div></noscript><img src=\"http://test.com/1.jpeg\"/></body><body><img src=\"http://test.com/2.jpeg\"/></body>");
    let output = run_with_filters(two, |parser| {
        parser.add_filter(Box::new(SupportNoscriptFilter::new(url, true)));
    });
    assert_eq!(output, expected_two);
    // No body, or an incapable client, means no insertion.
    let output = run_with_filters(b"<head></head>", |parser| {
        parser.add_filter(Box::new(SupportNoscriptFilter::new(url, true)));
    });
    assert_eq!(output, b"<head></head>");
    let output = run_with_filters(input, |parser| {
        parser.add_filter(Box::new(SupportNoscriptFilter::new(url, false)));
    });
    assert_eq!(output, input);
}

#[test]
fn noscript_enablement_rules() {
    // Ports IsAnyFilterRequiringScriptExecutionEnabled over explicit
    // filter sets and UA capabilities.
    use ps_config::filter::Filter;
    use ps_config::options::RewriteOptions;
    let verdict = |filters: &[Filter], defer: bool, inline: bool| {
        let mut options = RewriteOptions::new();
        for filter in filters {
            options.filters_mut().enable(*filter);
        }
        should_insert_noscript(options.filters(), defer, inline)
    };
    // Defer pair follows UA defer support.
    assert!(verdict(&[Filter::DeferJavascript], true, false));
    assert!(verdict(&[Filter::DeferIframe], true, false));
    assert!(!verdict(&[Filter::DeferJavascript], false, false));
    // Image quartet follows inlining support.
    for filter in [
        Filter::DedupInlinedImages,
        Filter::DelayImages,
        Filter::LazyloadImages,
        Filter::LocalStorageCache,
    ] {
        assert!(verdict(&[filter], false, true), "{filter:?}");
        assert!(!verdict(&[filter], false, false), "{filter:?}");
    }
    // Mobilize never counts; anything else script-requiring always does.
    assert!(!verdict(&[Filter::Mobilize], true, true));
    assert!(verdict(&[Filter::FlushSubresources], false, false));
    assert!(verdict(&[Filter::CachePartialHtmlDeprecated], false, false));
    // Nothing enabled, or only plain filters, means no insertion.
    assert!(!verdict(&[], true, true));
    assert!(!verdict(&[Filter::CollapseWhitespace], true, true));
}

#[test]
fn strip_legacy_polyfills_cases() {
    // Ports StripLegacyPolyfillsFilterTest (no body wrap, no options).
    use std::collections::HashSet;
    let empty: HashSet<String> = HashSet::new();
    let run = |input: &[u8]| {
        run_with_filters(input, |parser| {
            parser.add_filter(Box::new(StripLegacyPolyfillsFilter::new(&empty)));
        })
    };
    let output = run(b"<head><script type='text/javascript' src='https://c0.wp.com/c/5.8.12/wp-includes/js/dist/vendor/wp-polyfill.min.js' id='wp-polyfill-js' data-pagespeed-remove-polyfill='true'></script></head><body>Hello</body>");
    assert_eq!(output, b"<head></head><body>Hello</body>");
    let output = run(b"<head><script src='https://polyfill.io/v3/polyfill.min.js?features=default' data-pagespeed-remove-polyfill='true'></script></head><body>Hello</body>");
    assert_eq!(output, b"<head></head><body>Hello</body>");
    let input = b"<head><script src='/app.js'></script><script>var x = 1;</script></head><body>Hello</body>";
    assert_eq!(run(input), input);
    for url in [
        "https://c0.wp.com/wp-polyfill.min.js",
        "https://polyfill.io/v3/polyfill.min.js",
        "/polyfill.min.js",
        "/polyfills.min.js",
        "/app.js?bundle=wp-polyfill",
        "https://polyfill.io.example.test/app.js",
    ] {
        let input = format!(
            "<script src=\"{url}\"></script><script>window.requiredDependencyUsed = true;</script>"
        );
        assert_eq!(run(input.as_bytes()), input.as_bytes(), "{url}");
    }
    for value in ["false", "", "yes", "1"] {
        let input = format!(
            "<script src=\"/polyfills.min.js\" data-pagespeed-remove-polyfill=\"{value}\"></script>"
        );
        assert_eq!(run(input.as_bytes()), input.as_bytes(), "{value}");
    }
    for input in [
        "<script data-pagespeed-remove-polyfill=\"true\">window.required = 1;</script>",
        "<script src=\"\" data-pagespeed-remove-polyfill=\"true\"></script>",
        "<script src=\" \" data-pagespeed-remove-polyfill=\"true\"></script>",
    ] {
        assert_eq!(run(input.as_bytes()), input.as_bytes(), "{input}");
    }
    for script_type in [
        "module",
        "importmap",
        "speculationrules",
        "application/ld+json",
        "application/json",
        "unknown",
    ] {
        let input = format!(
            "<script type=\"{script_type}\" src=\"/polyfills.min.js\" data-pagespeed-remove-polyfill=\"true\">{{}}</script>"
        );
        assert_eq!(run(input.as_bytes()), input.as_bytes(), "{script_type}");
    }
    for input in [
        "<script nomodule=\"\" src=\"/polyfills.min.js\" data-pagespeed-remove-polyfill=\"true\"></script>",
        "<script data-pagespeed-no-transform=\"\" src=\"/polyfills.min.js\" data-pagespeed-remove-polyfill=\"true\"></script>",
    ] {
        assert_eq!(run(input.as_bytes()), input.as_bytes(), "{input}");
    }
    let output = run(b"<script src=\"/compatibility.js\" data-pagespeed-remove-polyfill=\"true\"></script><p>Kept</p>");
    assert_eq!(output, b"<p>Kept</p>");
    let input = b"<script src=\"/polyfills.min.js\" nonce=\"abc\" integrity=\"sha256-test\" crossorigin=\"anonymous\" referrerpolicy=\"no-referrer\" async=\"\" fetchpriority=\"low\"></script>";
    assert_eq!(run(input), input);
}

#[test]
fn strip_legacy_polyfills_allowlist() {
    // Hand-derived (C++ has no allowlist tests): exact-name match, the
    // percent-escape guard, and query-only sources.
    use std::collections::HashSet;
    let approved: HashSet<String> = ["polyfills.min.js".to_string()].into_iter().collect();
    let run = |input: &[u8]| {
        run_with_filters(input, |parser| {
            parser.add_filter(Box::new(StripLegacyPolyfillsFilter::new(&approved)));
        })
    };
    // Exact basenames authorize removal, whatever the host or query.
    for src in [
        "/polyfills.min.js",
        "https://cdn.test/lib/polyfills.min.js?v=2",
        "polyfills.min.js",
    ] {
        let input = format!("<script src=\"{src}\"></script><p>Kept</p>");
        assert_eq!(run(input.as_bytes()), b"<p>Kept</p>", "{src}");
    }
    // Near misses stay.
    for src in [
        "/polyfill.min.js",
        "/app.js?bundle=polyfills.min.js",
        "?polyfills.min.js",
        "",
    ] {
        let input = format!("<script src=\"{src}\"></script>");
        assert_eq!(run(input.as_bytes()), input.as_bytes(), "{src}");
    }
    // Percent escapes must not smuggle names past the allowlist (rejected
    // at membership; the canonical leaf differs from the raw basename too).
    let input = b"<script src=\"/polyfills%2emin%2ejs\"></script>";
    assert_eq!(run(input), input);
    let input = b"<script src=\"/polyfills%2Emin%2Ejs\"></script>";
    assert_eq!(run(input), input);
}

const AMP_PREFIX: &str = "<html>\n<head>\n";
const AMP_NON_AMP_LINK: &str = "<link rel=stylesheet href=\"style.css\"/>\n";
const AMP_SUFFIX: &str = "</head>\n<body><p>Hello world!</p></body>\n</html>";
const AMP_DEFAULT_PATTERN: &str =
    "https://ampversion.com${path_no_trailing_slash}/amp${maybe_trailing_slash}${maybe_query}";

fn run_amp(doc_url: &str, pattern: &str, input: &[u8]) -> Vec<u8> {
    run_with_filters(input, |parser| {
        parser.add_filter(Box::new(InsertAmpLinkFilter::new(
            pattern.as_bytes(),
            doc_url,
        )));
    })
}

#[test]
fn amp_link_cases() {
    // Ports InsertAmpLinkFilterTest (no body wrap; per-case doc URLs).
    let unchanged = |url: &str, html: String| {
        assert_eq!(
            run_amp(url, AMP_DEFAULT_PATTERN, html.as_bytes()),
            html.as_bytes()
        );
    };
    unchanged(
        "http://test.com/a.html",
        format!("{AMP_PREFIX}<link rel=AMPHtml href=blah>{AMP_SUFFIX}"),
    );
    unchanged(
        "http://test.com/a.html",
        format!("{AMP_PREFIX}<link rel=\"AMPHtml\" href=blah>{AMP_SUFFIX}"),
    );
    let output = run_amp(
        "http://test.com/a/b/",
        AMP_DEFAULT_PATTERN,
        format!("{AMP_PREFIX}{AMP_NON_AMP_LINK}{AMP_SUFFIX}").as_bytes(),
    );
    assert_eq!(
        output,
        format!(
            "{AMP_PREFIX}{AMP_NON_AMP_LINK}<link rel=\"amphtml\" href=\"https://ampversion.com/a/b/amp/\">{AMP_SUFFIX}"
        )
        .as_bytes()
    );
    let output = run_amp(
        "http://test.com/a?q=3",
        AMP_DEFAULT_PATTERN,
        b"<html><head></head><head></head></html>",
    );
    assert_eq!(
        output,
        b"<html><head><link rel=\"amphtml\" href=\"https://ampversion.com/a/amp?q=3\"></head><head></head></html>"
    );
    unchanged(
        "http://test.com/a?q=3",
        "<html><body></body></html>".to_string(),
    );
    // Badly formed patterns stay literal.
    for (pattern, href) in [
        ("${url", "${url"),
        ("a${unknown_pattern}b", "a${unknown_pattern}b"),
        ("}${url}", "}http://test.com/a"),
    ] {
        let input = format!("{AMP_PREFIX}{AMP_SUFFIX}");
        let output = run_amp("http://test.com/a", pattern, input.as_bytes());
        assert_eq!(
            output,
            format!("{AMP_PREFIX}<link rel=\"amphtml\" href=\"{href}\">{AMP_SUFFIX}").as_bytes(),
            "{pattern}"
        );
    }
    // Default template across URL shapes.
    for (url, href) in [
        (
            "http://test.com/a.html",
            "https://ampversion.com/a.html/amp",
        ),
        ("http://test.com/a/b/", "https://ampversion.com/a/b/amp/"),
        ("http://test.com/a?q=3", "https://ampversion.com/a/amp?q=3"),
    ] {
        let input = format!("{AMP_PREFIX}{AMP_SUFFIX}");
        let output = run_amp(url, AMP_DEFAULT_PATTERN, input.as_bytes());
        assert_eq!(
            output,
            format!("{AMP_PREFIX}<link rel=\"amphtml\" href=\"{href}\">{AMP_SUFFIX}").as_bytes(),
            "{url}"
        );
    }
}

fn js_disable_config() -> ps_html::filters::js_disable::JsDisableConfig {
    use ps_config::wildcard::WildcardGroup;
    let mut allow = WildcardGroup::new();
    allow.disallow("*donotmove*");
    ps_html::filters::js_disable::JsDisableConfig {
        base_url: b"http://example.com/".to_vec(),
        allow,
        defer_applies: true,
        is_ie: false,
        defer_js_experimental: false,
        prioritizing_scripts: false,
    }
}

fn run_js_disable(input: &[u8], config: ps_html::filters::js_disable::JsDisableConfig) -> Vec<u8> {
    run_with_filters(input, |parser| {
        parser.add_filter(Box::new(JsDisableFilter::new(config)));
    })
}

#[test]
fn js_disable_cases() {
    // Ports JsDisableFilterTest except the Defer-chain case (ported as
    // `disable_chain` in js_defer_disabled.rs) and log assertions (no
    // logging channel yet).
    // NOTE on `onload`: the C++ test expects `onload=""CODE">`, which the
    // implementation cannot produce — `AddEscapedAttribute` stores CODE as
    // the value and the writer emits `="..."` for every non-null value
    // (proven by the `=""` round trip in the same test). The expectation
    // below pins the implementation-derived `onload="CODE"` instead.
    let onload_rewritten = format!(
        "<img src=\"abc.jpg\" data-pagespeed-onload=\"foo1('abc');foo2();\" onload=\"{}\"",
        String::from_utf8_lossy(ELEMENT_ONLOAD_CODE)
    );
    let input = "<body><noscript>This is original noscript tag</noscript><script></script><script src=\"blah1\" random=\"true\">hi1</script><div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div><img src=\"abc.jpg\" onload=\"foo1('abc');foo2();\"><script src=\"blah2\" random=\"false\">hi2</script><script src=\"blah3\" data-pagespeed-no-defer=\"\"></script><script src=\"blah4\" pagespeed_no_defer=\"\"></script><script src=\"something-donotmove\"></script></body>";
    let expected = format!(
        "<body><noscript>This is original noscript tag</noscript>\
        <script type=\"text/psajs\" data-pagespeed-orig-index=\"0\"></script>\
        <script src=\"blah1\" random=\"true\" type=\"text/psajs\" data-pagespeed-orig-index=\"1\">hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
        {onload_rewritten}>\
        <script src=\"blah2\" random=\"false\" type=\"text/psajs\" data-pagespeed-orig-index=\"2\">hi2</script>\
        <script src=\"blah3\" data-pagespeed-no-defer=\"\"></script>\
        <script src=\"blah4\" pagespeed_no_defer=\"\"></script>\
        <script src=\"something-donotmove\"></script></body>"
    );
    assert_eq!(
        run_js_disable(input.as_bytes(), js_disable_config()),
        expected.as_bytes()
    );
}

#[test]
fn js_disable_variants() {
    // Modules, data blocks, and non-JS types stay; plain scripts retag.
    let input = "<body><script type=\"module\" src=\"mod.js\"></script>\
        <script type=\"importmap\">{\"imports\":{}}</script>\
        <script type=\"application/json\">{\"a\":1}</script>\
        <script src=\"plain.js\"></script></body>";
    let expected = "<body><script type=\"module\" src=\"mod.js\"></script>\
        <script type=\"importmap\">{\"imports\":{}}</script>\
        <script type=\"application/json\">{\"a\":1}</script>\
        <script src=\"plain.js\" type=\"text/psajs\" data-pagespeed-orig-index=\"0\"></script></body>";
    assert_eq!(
        run_js_disable(input.as_bytes(), js_disable_config()),
        expected.as_bytes()
    );

    // Blacklisted UA disables the filter entirely.
    let mut config = js_disable_config();
    config.defer_applies = false;
    let input = "<head><script src='http://www.google.com/javascript/ajax_apis.js'></script>\
        <script> func();</script></head><body>Hello, world!</body>";
    assert_eq!(run_js_disable(input.as_bytes(), config), input.as_bytes());

    // Src queries (escaped or bare) and valueless src attributes survive.
    for (id, input, expected) in [
        (
            "query",
            "<noscript>This is original noscript tag</noscript>\
            <script src=\"x?a=b&amp;c=d\" random=\"true\">hi1</script>\
            <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
            <script src=\"y?a=b&amp;c=d\" random=\"false\">hi2</script>",
            "<noscript>This is original noscript tag</noscript>\
            <script src=\"x?a=b&amp;c=d\" random=\"true\" type=\"text/psajs\" data-pagespeed-orig-index=\"0\">hi1</script>\
            <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
            <script src=\"y?a=b&amp;c=d\" random=\"false\" type=\"text/psajs\" data-pagespeed-orig-index=\"1\">hi2</script>",
        ),
        (
            "unescaped_query",
            "<noscript>This is original noscript tag</noscript>\
            <script src=\"x?a=b&c=d\" random=\"true\">hi1</script>\
            <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
            <script src=\"y?a=b&c=d\" random=\"false\">hi2</script>",
            "<noscript>This is original noscript tag</noscript>\
            <script src=\"x?a=b&c=d\" random=\"true\" type=\"text/psajs\" data-pagespeed-orig-index=\"0\">hi1</script>\
            <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
            <script src=\"y?a=b&c=d\" random=\"false\" type=\"text/psajs\" data-pagespeed-orig-index=\"1\">hi2</script>",
        ),
        (
            "null_src",
            "<noscript>This is original noscript tag</noscript>\
            <script src random=\"true\">hi1</script>\
            <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
            <script src random=\"false\">hi2</script>",
            "<noscript>This is original noscript tag</noscript>\
            <script src random=\"true\" type=\"text/psajs\" data-pagespeed-orig-index=\"0\">hi1</script>\
            <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
            <script src random=\"false\" type=\"text/psajs\" data-pagespeed-orig-index=\"1\">hi2</script>",
        ),
    ] {
        assert_eq!(run_js_disable(input.as_bytes(), js_disable_config()), expected.as_bytes(), "{id}");
    }

    // Duplicate type attributes collapse into one orig-type + psajs tag.
    let input = "<noscript>This is original noscript tag</noscript>\
        <script src=\"x?a=b&amp;c=d\" type='text/javascript' type='a' type='b'>hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>";
    let expected = "<noscript>This is original noscript tag</noscript>\
        <script src=\"x?a=b&amp;c=d\" data-pagespeed-orig-type='text/javascript' type=\"text/psajs\" data-pagespeed-orig-index=\"0\">hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>";
    assert_eq!(
        run_js_disable(input.as_bytes(), js_disable_config()),
        expected.as_bytes()
    );
}

#[test]
fn js_disable_experimental_and_ie() {
    // Experimental flag appends the bootstrap script at body end.
    let mut config = js_disable_config();
    config.defer_js_experimental = true;
    let input = "<body><noscript>This is original noscript tag</noscript>\
        <script src=\"blah1\" random=\"true\">hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
        <script src=\"blah2\" random=\"false\">hi2</script></body>";
    let expected = format!(
        "<body><noscript>This is original noscript tag</noscript>\
        <script src=\"blah1\" random=\"true\" type=\"text/psajs\" data-pagespeed-orig-index=\"0\">hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
        <script src=\"blah2\" random=\"false\" type=\"text/psajs\" data-pagespeed-orig-index=\"1\">hi2</script>\
        <script type=\"text/javascript\" data-pagespeed-no-defer>{}</script></body>",
        "window.pagespeed = window.pagespeed || {};window.pagespeed.defer_js_experimental=true;"
    );
    assert_eq!(
        run_js_disable(input.as_bytes(), config),
        expected.as_bytes()
    );

    // IE clients gain a compatibility meta in a synthesized head.
    let mut config = js_disable_config();
    config.is_ie = true;
    let input = "<body><noscript>This is original noscript tag</noscript>\
        <script src=\"blah1\" random=\"true\">hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div></body>";
    let expected = "<head><meta http-equiv=\"X-UA-Compatible\" content=\"IE=edge\"></head><body>\
        <noscript>This is original noscript tag</noscript>\
        <script src=\"blah1\" random=\"true\" type=\"text/psajs\" data-pagespeed-orig-index=\"0\">hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div></body>";
    assert_eq!(
        run_js_disable(input.as_bytes(), config),
        expected.as_bytes()
    );
}

#[test]
fn js_disable_element_after_body_falls_back() {
    // Hand test pinning the body-end fallback: elements after `</body>`
    // drop the injection point, so the bootstrap lands past them instead
    // of inside the body.
    let mut config = js_disable_config();
    config.defer_js_experimental = true;
    let output = run_js_disable(b"<body><script src=\"a\">x</script></body><br>", config);
    let output = String::from_utf8_lossy(&output);
    let script = output
        .find("defer_js_experimental")
        .expect("bootstrap present");
    let body_end = output.find("</body>").expect("body end present");
    assert!(script > body_end, "bootstrap must follow </body>: {output}");
}

#[test]
fn js_disable_prioritize() {
    let mut config = js_disable_config();
    config.prioritizing_scripts = true;
    let onload_rewritten = format!(
        "<img src=\"abc.jpg\" data-pagespeed-onload=\"foo1();foo2();\" onload=\"{}\"",
        String::from_utf8_lossy(ELEMENT_ONLOAD_CODE)
    );
    let input = "<body><noscript>This is original noscript tag</noscript>\
        <script src=\"blah1\" random=\"true\">hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
        <img src=\"abc.jpg\" onload=\"foo1();foo2();\">\
        <script src=\"blah2\" random=\"false\" data-pagespeed-prioritize>hi2</script>\
        <script data-pagespeed-prioritize>hi5</script></body>";
    let expected = format!(
        "<body><noscript>This is original noscript tag</noscript>\
        <script src=\"blah1\" random=\"true\" type=\"text/psajs\" data-pagespeed-orig-index=\"0\">hi1</script>\
        <div id=\"contentContainer\"><h1>Hello 1</h1><div id=\"middleFooter\"><h3>Hello 3</h3></div></div>\
        {onload_rewritten}>\
        <script src=\"blah2\" random=\"false\" data-pagespeed-prioritize type=\"text/prioritypsajs\" data-pagespeed-orig-index=\"1\">hi2</script>\
        <script data-pagespeed-prioritize type=\"text/prioritypsajs\" data-pagespeed-orig-index=\"2\">hi5</script></body>"
    );
    assert_eq!(
        run_js_disable(input.as_bytes(), config),
        expected.as_bytes()
    );
}

#[test]
fn filter_names() {
    // C++ Name() strings.
    assert_eq!(
        <QuoteRemovalFilter as HtmlFilter>::name(&QuoteRemovalFilter::default()),
        "HtmlAttributeQuoteRemoval"
    );
    assert_eq!(QuoteRemovalFilter::default().total_quotes_removed(), 0);
    assert_eq!(
        <RemoveCommentsFilter as HtmlFilter>::name(&RemoveCommentsFilter::default()),
        "RemoveComments"
    );
    assert_eq!(
        <CollapseWhitespaceFilter as HtmlFilter>::name(&CollapseWhitespaceFilter::default()),
        "CollapseWhitespace"
    );
    assert_eq!(
        <ElideAttributesFilter as HtmlFilter>::name(&ElideAttributesFilter),
        "ElideAttributes"
    );
    assert_eq!(
        <StripScriptsFilter as HtmlFilter>::name(&StripScriptsFilter),
        "StripScripts"
    );
    assert_eq!(
        <PedanticFilter as HtmlFilter>::name(&PedanticFilter),
        "Pedantic"
    );
    assert_eq!(
        <BaseTagFilter as HtmlFilter>::name(&BaseTagFilter::new(b"http://x.test/")),
        "BaseTag"
    );
    assert_eq!(
        <AddHeadFilter as HtmlFilter>::name(&AddHeadFilter::new(false)),
        "AddHead"
    );
    assert_eq!(
        <AddIdsFilter as HtmlFilter>::name(&AddIdsFilter::default()),
        "AddIdsFilter"
    );
    assert_eq!(
        <FixReflowFilter as HtmlFilter>::name(&FixReflowFilter::new(None)),
        "FixReflowFilter"
    );
    assert_eq!(
        <SupportNoscriptFilter as HtmlFilter>::name(&SupportNoscriptFilter::new(b"", true)),
        "SupportNoscript"
    );
    assert_eq!(
        <StripLegacyPolyfillsFilter as HtmlFilter>::name(&StripLegacyPolyfillsFilter::new(
            &std::collections::HashSet::new()
        )),
        "StripLegacyPolyfills"
    );
    assert_eq!(
        <DeterministicJsFilter as HtmlFilter>::name(&DeterministicJsFilter::new(
            b"",
            false,
            XhtmlStatus::IsNotXhtml
        )),
        "DeterministicJs"
    );
    assert_eq!(
        <DeferIframeFilter as HtmlFilter>::name(&DeferIframeFilter::new(
            b"",
            true,
            false,
            XhtmlStatus::IsNotXhtml
        )),
        "DeferIframe"
    );
    assert_eq!(
        <HandleNoscriptRedirectFilter as HtmlFilter>::name(&HandleNoscriptRedirectFilter::new(
            b"http://x.test/"
        )),
        "HandleNoscriptRedirect"
    );
    assert_eq!(
        <InsertAmpLinkFilter as HtmlFilter>::name(&InsertAmpLinkFilter::new(b"", "http://x.test/")),
        "InsertAmpLink"
    );
    assert_eq!(
        <JsDisableFilter as HtmlFilter>::name(&JsDisableFilter::new(JsDisableConfig::default())),
        "JsDisableFilter"
    );
    assert_eq!(
        <FlushHtmlFilter as HtmlFilter>::name(&FlushHtmlFilter::new(
            &ps_config::options::RewriteOptions::new()
        )),
        "FlushHtmlFilter"
    );
}

#[test]
fn registry_maps_implemented_slots() {
    // Every implemented filter covers its chain slot; the rest wait.
    assert!(instantiate(ChainFilter::RemoveComments).is_some());
    assert!(instantiate(ChainFilter::CollapseWhitespace).is_some());
    assert!(instantiate(ChainFilter::ElideAttributes).is_some());
    assert!(instantiate(ChainFilter::RemoveQuotes).is_some());
    assert!(instantiate(ChainFilter::StripScripts).is_some());
    assert!(instantiate(ChainFilter::Pedantic).is_some());
    // BaseTag and AddHead need driver context (base URL, combine flag).
    assert!(instantiate(ChainFilter::BaseTag).is_none());
    assert!(instantiate(ChainFilter::AddHead).is_none());
    // MoveCssToHead needs its two option flags at construction.
    assert!(instantiate(ChainFilter::CssMoveToHead).is_none());
    // MetaTag needs shared driver headers at construction.
    assert!(instantiate(ChainFilter::MetaTag).is_none());
    // FixReflow and SupportNoscript need injected driver context.
    assert!(instantiate(ChainFilter::FixReflow).is_none());
    assert!(instantiate(ChainFilter::SupportNoscript).is_none());
    // DeterministicJs and DeferIframe need asset bytes plus driver flags.
    assert!(instantiate(ChainFilter::DeterministicJs).is_none());
    assert!(instantiate(ChainFilter::DeferIframe).is_none());
    // Listeners wire directly (options snapshot, verdict callback).
    assert!(instantiate(ChainFilter::FlushHtml).is_none());
    assert!(instantiate(ChainFilter::AmpDocument).is_none());
    // StripLegacyPolyfills needs the approved set at construction.
    assert!(instantiate(ChainFilter::StripLegacyPolyfills).is_none());
    // InsertAmpLink needs pattern plus document URL at construction.
    assert!(instantiate(ChainFilter::InsertAmpLink).is_none());
    // JsDisable needs its driver config at construction.
    assert!(instantiate(ChainFilter::JsDisable).is_none());
    assert!(instantiate(ChainFilter::AddIds).is_some());
    assert!(instantiate(ChainFilter::HandleNoscriptRedirect).is_none());
    // Minify subsets stand in until the driver-bound filters land.
    assert!(instantiate(ChainFilter::CssFilter).is_some());
    assert!(instantiate(ChainFilter::Javascript).is_some());
    assert!(instantiate(ChainFilter::ImageRewrite).is_none());
    assert!(instantiate(ChainFilter::HtmlWriter).is_none());
}

#[test]
fn css_minify_cases() {
    use ps_html::filters::CssMinifyFilter;
    // Goldens observed from C++ (`core-rich` compat case).
    let plain: &[(&str, &[u8], &[u8])] = &[
        (
            "minify_style_block",
            b"<head><style>body { color: red; }</style></head>",
            b"<head><style>body{color:red}</style></head>",
        ),
        (
            "minify_declarations",
            b"<head><style>p { margin: 0 auto; padding : 4px ; }</style></head>",
            b"<head><style>p{margin:0 auto;padding:4px}</style></head>",
        ),
        (
            "leaves_unparseable_css",
            b"<head><style>{</style></head>",
            b"<head><style>{</style></head>",
        ),
        (
            "ignores_non_style_text",
            b"<body><p>a  b</p></body>",
            b"<body><p>a  b</p></body>",
        ),
    ];
    for (id, input, expected) in plain {
        let output = run_with_filters(input, |parser| {
            parser.add_pre_render_filter(Box::new(CssMinifyFilter::default()));
        });
        assert_eq!(output, *expected, "case {id}");
    }
}

#[test]
fn js_minify_cases() {
    use ps_html::filters::JsMinifyFilter;
    // Goldens observed from C++ (`core-rich` compat case).
    let plain: &[(&str, &[u8], &[u8])] = &[
        (
            "minify_script_block",
            b"<body><script>function f() { return 1 + 2; }</script></body>",
            b"<body><script>function f(){return 1+2;}</script></body>",
        ),
        (
            "skips_external_script",
            b"<body><script src=\"a.js\">var  x  =  1;</script></body>",
            b"<body><script src=\"a.js\">var  x  =  1;</script></body>",
        ),
        (
            "skips_non_javascript_type",
            b"<body><script type=\"application/json\">{\"a\" : 1 }</script></body>",
            b"<body><script type=\"application/json\">{\"a\" : 1 }</script></body>",
        ),
        (
            "keeps_end_tag_unsafe_minification",
            b"<body><script>var  s  =  \"</script>\";</script></body>",
            b"<body><script>var  s  =  \"</script>\";</script></body>",
        ),
        (
            "skips_noscript_contents",
            b"<body><noscript><script>var  x  =  1;</script></noscript></body>",
            b"<body><noscript><script>var  x  =  1;</script></noscript></body>",
        ),
    ];
    for (id, input, expected) in plain {
        let output = run_with_filters(input, |parser| {
            parser.add_pre_render_filter(Box::new(JsMinifyFilter::default()));
        });
        assert_eq!(output, *expected, "case {id}");
    }
}

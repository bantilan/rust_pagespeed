//! Analytics rewrite tests ported from `google_analytics_filter_test.cc`.
//!
//! NOTE: inputs skip the body wrapper (it carries no scripts, so it
//! commutes with this filter).
#[path = "harness.rs"]
mod harness;

use harness::SharedSink;
use ps_html::filters::google_analytics::{
    GoogleAnalyticsFilter, GA_SNIPPET_PREFIX, GA_SNIPPET_SUFFIX, PAGE_LOAD_COUNT,
    REWRITTEN_COUNT,
};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;
use ps_stats::core::Statistics;

/// Test glue methods (`kAsyncGlueMethods`).
const ASYNC_GLUE_METHODS: &[u8] = b"        '_trackPageview',\n        '_setCookiePath'\n";

/// Typical synchronous init (`kSyncInit`).
const SYNC_INIT: &[u8] = b"<script type=\"text/javascript\">\ntry {\nvar pageTracker = _gat._getTracker(\"UA-XXXXX-X\");\npageTracker._trackPageview();} catch(err) {}</script>";

/// Init with the tracker call retargeted (`kAsyncGlueInit`).
const ASYNC_GLUE_INIT: &[u8] = b"<script type=\"text/javascript\">\ntry {\nvar pageTracker = _modpagespeed_getRewriteTracker(\"UA-XXXXX-X\");\npageTracker._trackPageview();} catch(err) {}</script>";

fn run_case(case: &str, input: &[u8]) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_pre_render_filter(Box::new(GoogleAnalyticsFilter::with_methods(
        vec!["_trackPageview".to_string(), "_setCookiePath".to_string()],
        vec!["_get".to_string(), "_getLinkerUrl".to_string()],
    )));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(input);
    parser.finish();
    sink.bytes()
}

/// Expected rewritten load + init (`GetAsyncLoadAndInit`).
fn async_load_and_init(load_prefix: &[u8], load_suffix: &[u8]) -> Vec<u8> {
    let mut out = load_prefix.to_vec();
    out.extend_from_slice(GA_SNIPPET_PREFIX);
    out.extend_from_slice(ASYNC_GLUE_METHODS);
    out.extend_from_slice(GA_SNIPPET_SUFFIX);
    out.extend_from_slice(load_suffix);
    out.extend_from_slice(ASYNC_GLUE_INIT);
    out
}

fn validate_expected(case: &str, input: &[u8], expected: &[u8]) {
    assert_eq!(run_case(case, input), expected, "case {case}");
}

#[test]
fn sync_script_src_made_async() {
    let mut input =
        b"<script src=\"http://www.google-analytics.com/ga.js\" type=\"text/javascript\">\n</script>\n".to_vec();
    input.extend_from_slice(SYNC_INIT);
    validate_expected(
        "sync_script_src_made_async",
        &input,
        &async_load_and_init(b"<script type=\"text/javascript\">", b"\n</script>\n"),
    );
}

#[test]
fn sync_script_src_no_characters_node_made_async() {
    let mut input =
        b"<script src=\"http://www.google-analytics.com/ga.js\" type=\"text/javascript\"></script>\n".to_vec();
    input.extend_from_slice(SYNC_INIT);
    validate_expected(
        "sync_script_src_no_characters_node_made_async",
        &input,
        &async_load_and_init(b"<script type=\"text/javascript\">", b"</script>\n"),
    );
}

#[test]
fn sync_document_write_made_async() {
    let mut input =
        b"<script type='text/javascript'>\nvar gaJsHost = ((\"https:\" == document.location.protocol) ? \"https://ssl.\" : \"http://www.\");\ndocument.write(unescape(\"%3Cscript src=\" + gaJsHost + \"google-analytics.com/ga.js' type='text/javascript'%3E%3C/script%3E\"));\n</script>\n".to_vec();
    input.extend_from_slice(SYNC_INIT);
    validate_expected(
        "sync_document_write_made_async",
        &input,
        &async_load_and_init(
            b"<script type='text/javascript'>\nvar gaJsHost = ((\"https:\" == document.location.protocol) ? \"https://ssl.\" : \"http://www.\");\n",
            b"\n</script>\n",
        ),
    );
}

#[test]
fn sync_document_write_at_position_zero() {
    let mut input =
        b"<script type='text/javascript'>document.write(unescape(\"%3Cscript src=\"http://www.google-analytics.com/ga.js' type='text/javascript'%3E%3C/script%3E\"));\n</script>\n".to_vec();
    input.extend_from_slice(SYNC_INIT);
    validate_expected(
        "sync_document_write_at_position_zero",
        &input,
        &async_load_and_init(b"<script type='text/javascript'>", b"\n</script>\n"),
    );
}

#[test]
fn multiple_load_reduced_to_single() {
    let mut input =
        b"<script type='text/javascript'>document.write(unescape(\"%3Cscript src=\"http://www.google-analytics.com/ga.js' type='text/javascript'%3E%3C/script%3E\"));\n</script>\n".to_vec();
    input.extend_from_slice(SYNC_INIT);
    input.extend_from_slice(
        b"<script type='text/javascript'>\nvar gaJsHost = ((\"https:\" == document.location.protocol) ? \"https://ssl.\" : \"http://www.\");\ndocument.write(unescape(\"%3Cscript src=\" + gaJsHost + \"google-analytics.com/ga.js' type='text/javascript'%3E%3C/script%3E\"));\n</script>\n",
    );
    input.extend_from_slice(SYNC_INIT);
    input.extend_from_slice(
        b"<script src=\"http://www.google-analytics.com/ga.js\" type=\"text/javascript\"></script>",
    );
    input.extend_from_slice(SYNC_INIT);

    let mut expected = async_load_and_init(b"<script type='text/javascript'>", b"\n</script>\n");
    // The second load (document.write) is removed completely.
    expected.extend_from_slice(
        b"<script type='text/javascript'>\nvar gaJsHost = ((\"https:\" == document.location.protocol) ? \"https://ssl.\" : \"http://www.\");\n\n</script>\n",
    );
    expected.extend_from_slice(ASYNC_GLUE_INIT);
    // The third load (script src) is removed completely.
    expected.extend_from_slice(ASYNC_GLUE_INIT);
    validate_expected("multiple_load_reduced_to_single", &input, &expected);
}

#[test]
fn async_give_no_changes() {
    let input = b"<script type=\"text/javascript\">\nvar _gaq = _gaq || [];\n_gaq.push(['_setAccount', 'UA-XXXXX-X']);\n_gaq.push(['_trackPageview']);\n(function() {\n  var ga = document.createElement('script');\n  ga.type = 'text/javascript'; ga.async = true;\n  ga.src = ('https:' == document.location.protocol ? 'https://ssl' : 'http://www') +\n    '.google-analytics.com/ga.js';\n  var s = document.getElementsByTagName('script')[0]\n  s.parentNode.insertBefore(ga, s);\n})();\n</script>\n";
    validate_expected("async_unchanged", input, input);
}

#[test]
fn nonstandard_init_give_no_changes() {
    let input = b"<script type=\"text/javascript\">\ngoogleAnalytics = new TFHtmlUtilsGoogleAnalytics({\n  'trackingcode': \"UA-XXXXX-X\"\n});\ngoogleAnalytics.trackPageview();\n} catch(err) {}\n</script>\n";
    validate_expected("nonstandard_init_unchanged", input, input);
}

#[test]
fn unhandled_call_causes_skipped_rewrite() {
    let mut input =
        b"<script src=\"http://www.google-analytics.com/ga.js\" type=\"text/javascript\"></script>\n".to_vec();
    input.extend_from_slice(SYNC_INIT);
    // `_getLinkerUrl` is listed as an unhandled call.
    input.extend_from_slice(
        b"<script type=\"text/javascript\">pageTracker._getLinkerUrl ();\n</script>\n",
    );
    validate_expected("unhandled_call_causes_skipped_rewrite", &input, &input);
}

#[test]
fn unknown_call_still_allows_rewrite() {
    let mut input =
        b"<script src=\"http://www.google-analytics.com/ga.js\" type=\"text/javascript\"></script>\n".to_vec();
    input.extend_from_slice(SYNC_INIT);
    // `_getFoo` is not listed as an unhandled call.
    input.extend_from_slice(b"<script type=\"text/javascript\">pageTracker._getFoo();</script>\n");
    let mut expected = async_load_and_init(b"<script type=\"text/javascript\">", b"</script>\n");
    expected
        .extend_from_slice(b"<script type=\"text/javascript\">pageTracker._getFoo();</script>\n");
    validate_expected("unknown_call_still_allows_rewrite", &input, &expected);
}

#[test]
fn publishes_page_counters() {
    let stats = Statistics::new();
    let mut filter = GoogleAnalyticsFilter::with_methods(
        vec!["_trackPageview".to_string(), "_setCookiePath".to_string()],
        vec!["_get".to_string(), "_getLinkerUrl".to_string()],
    );
    filter.init_stats(&stats);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_pre_render_filter(Box::new(filter));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/stats.html", false);
    let mut input =
        b"<script src=\"http://www.google-analytics.com/ga.js\" type=\"text/javascript\">\n</script>\n"
            .to_vec();
    input.extend_from_slice(SYNC_INIT);
    parser.write(&input);
    parser.finish();
    assert_eq!(stats.get_variable(PAGE_LOAD_COUNT).unwrap().get(), 1);
    assert_eq!(stats.get_variable(REWRITTEN_COUNT).unwrap().get(), 1);
}

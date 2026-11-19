//! GA insertion tests ported from `insert_ga_filter_test.cc`.
//!
//! NOTE: expected outputs omit the `<head/>` tags the C++ full chain
//! inserts via AddHead (single-filter harness here), and inputs skip the
//! body wrapper (it carries no scripts, so it commutes).
#[path = "harness.rs"]
mod harness;

use harness::SharedSink;
use ps_html::filters::insert_ga::{InsertGAConfig, InsertGAFilter};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

const GA_ID: &[u8] = b"UA-21111111-1";

fn base_config() -> InsertGAConfig {
    InsertGAConfig {
        ga_id: GA_ID.to_vec(),
        increase_speed_tracking: true,
        ..InsertGAConfig::default()
    }
}

fn custom_var_config(use_analytics_js: bool) -> InsertGAConfig {
    InsertGAConfig {
        ga_id: GA_ID.to_vec(),
        increase_speed_tracking: true,
        use_analytics_js,
        running_experiment: true,
        experiment_id: Some(2),
        experiment_string: "Experiment: 2".to_string(),
        experiment_ga_slot: 4,
        ..InsertGAConfig::default()
    }
}

fn content_experiment_config(use_analytics_js: bool, variant: &str) -> InsertGAConfig {
    InsertGAConfig {
        ga_id: GA_ID.to_vec(),
        increase_speed_tracking: true,
        use_analytics_js,
        running_experiment: true,
        is_content_experiment: true,
        experiment_id: Some(2),
        content_experiment_id: "123".to_string(),
        content_experiment_variant_id: variant.to_string(),
        ..InsertGAConfig::default()
    }
}

fn run_case(url: &[u8], input: &[u8], config: InsertGAConfig) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_pre_render_filter(Box::new(InsertGAFilter::new(config)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url, false);
    parser.write(input);
    parser.finish();
    sink.bytes()
}

fn validate_expected(case: &str, input: &[u8], expected: &[u8], config: InsertGAConfig) {
    let url = format!("http://test.com/{case}.html");
    assert_eq!(
        run_case(url.as_bytes(), input, config),
        expected,
        "case {case}"
    );
}

const HTML_INPUT: &[u8] = b"<head>\n<title>Something</title>\n</head><body> Hello World!</body>";

fn output_format(before_script: &[u8], first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut out = b"<head>\n<title>Something</title>\n</head><body> Hello World!".to_vec();
    out.extend_from_slice(before_script);
    out.extend_from_slice(b"<script>");
    out.extend_from_slice(first);
    out.extend_from_slice(second);
    out.extend_from_slice(b"</script></body>");
    out
}

const GA_SPEED_TRACKING: &[u8] = b"_gaq.push(['_setSiteSpeedSampleRate', 100]);";
const ANALYTICS_SPEED_TRACKING: &[u8] = b", {'siteSpeedSampleRate': 100}";
const ANALYTICS_SPEED_TRACKING_MINIMAL: &[u8] = b"'siteSpeedSampleRate': 100,";
const CX_API_URL: &[u8] = b"//www.google-analytics.com/cx/api.js";

fn ga_js_snippet(ga_id: &[u8], domain: &[u8], speed: &[u8]) -> Vec<u8> {
    let mut out =
        b"if (window.parent == window) {var _gaq = _gaq || [];_gaq.push(['_setAccount', '".to_vec();
    out.extend_from_slice(ga_id);
    out.extend_from_slice(b"']);_gaq.push(['_setDomainName', '");
    out.extend_from_slice(domain);
    out.extend_from_slice(b"']);_gaq.push(['_setAllowLinker', true]);");
    out.extend_from_slice(speed);
    out.extend_from_slice(b"_gaq.push(['_trackPageview']);(function() {var ga = document.createElement('script'); ga.type = 'text/javascript';ga.async = true;ga.src = 'https://ssl.google-analytics.com/ga.js';var s = document.getElementsByTagName('script')[0];s.parentNode.insertBefore(ga, s);})();}");
    out
}

fn ga_experiment_snippet(speed: &[u8], slot: u32, spec: &[u8]) -> Vec<u8> {
    let mut out = b"var _gaq = _gaq || [];".to_vec();
    out.extend_from_slice(speed);
    out.extend_from_slice(b"_gaq.push(['_setCustomVar', ");
    out.extend_from_slice(slot.to_string().as_bytes());
    out.extend_from_slice(b", 'ExperimentState', '");
    out.extend_from_slice(spec);
    out.extend_from_slice(b"']);");
    out
}

fn analytics_js_snippet(ga_id: &[u8], speed: &[u8], experiment: &[u8]) -> Vec<u8> {
    let mut out = b"if (window.parent == window) {(function(i,s,o,g,r,a,m){i['GoogleAnalyticsObject']=r;i[r]=i[r]||function(){(i[r].q=i[r].q||[]).push(arguments)},i[r].l=1*new Date();a=s.createElement(o), m=s.getElementsByTagName(o)[0];a.async=1;a.src=g;m.parentNode.insertBefore(a,m)})(window,document,'script','//www.google-analytics.com/analytics.js','ga');ga('create', '".to_vec();
    out.extend_from_slice(ga_id);
    out.extend_from_slice(b"', 'auto'");
    out.extend_from_slice(speed);
    out.extend_from_slice(b";");
    out.extend_from_slice(experiment);
    out
}

fn cx_exp_and_variant(exp: &[u8], variant: &[u8]) -> Vec<u8> {
    let mut out = b"ga('set', 'expId', '".to_vec();
    out.extend_from_slice(exp);
    out.extend_from_slice(b"');ga('set', 'expVar', '");
    out.extend_from_slice(variant);
    out.extend_from_slice(b"');");
    out
}

fn cx_chosen_variation(variant: i32, exp: &[u8]) -> Vec<u8> {
    let mut out = b"cxApi.setChosenVariation(".to_vec();
    out.extend_from_slice(variant.to_string().as_bytes());
    out.extend_from_slice(b", '");
    out.extend_from_slice(exp);
    out.extend_from_slice(b"');");
    out
}

fn cx_non_numeric_comment(variant: &[u8]) -> Vec<u8> {
    let mut out = b"\n/* mod_pagespeed cannot inject experiment variant '".to_vec();
    out.extend_from_slice(variant);
    out.extend_from_slice(b" because it's not a number */\n");
    out
}

fn generate_expected_html(domain: &[u8], slot: Option<u32>, spec: &[u8], speed: bool) -> Vec<u8> {
    let speed_tracking = if speed { GA_SPEED_TRACKING } else { b"" };
    let addition = match slot {
        Some(slot) if !spec.is_empty() => ga_experiment_snippet(b"", slot, spec),
        _ => Vec::new(),
    };
    output_format(
        b"",
        &addition,
        &ga_js_snippet(GA_ID, domain, speed_tracking),
    )
}

#[test]
fn simple_insert_ga_js() {
    let mut config = base_config();
    config.use_analytics_js = false;
    validate_expected(
        "simple_addition",
        HTML_INPUT,
        &generate_expected_html(b"test.com", None, b"", true),
        config.clone(),
    );
    assert_eq!(
        run_case(b"https://www.test1.com/index.html", HTML_INPUT, config,),
        generate_expected_html(b"www.test1.com", None, b"", true)
    );
}

#[test]
fn simple_insert_ga_js_id_unset() {
    let mut config = base_config();
    config.use_analytics_js = false;
    config.ga_id.clear();
    validate_expected(
        "cant_do_anything_without_a_ga_id",
        HTML_INPUT,
        HTML_INPUT,
        config,
    );
}

#[test]
fn simple_insert_analytics_js() {
    // The C++ default for use_analytics_js is true.
    let mut config = base_config();
    config.use_analytics_js = true;
    validate_expected(
        "simple_addition",
        HTML_INPUT,
        &output_format(
            b"",
            b"",
            &analytics_js_snippet(GA_ID, ANALYTICS_SPEED_TRACKING, b""),
        ),
        config,
    );
}

#[test]
fn no_increased_speed() {
    let mut config = base_config();
    config.use_analytics_js = false;
    config.increase_speed_tracking = false;
    validate_expected(
        "simple_addition_no_increased_speed",
        HTML_INPUT,
        &generate_expected_html(b"test.com", None, b"", false),
        config,
    );
}

#[test]
fn analytics_js_no_increased_speed() {
    let mut config = base_config();
    config.use_analytics_js = true;
    config.increase_speed_tracking = false;
    validate_expected(
        "simple_addition",
        HTML_INPUT,
        &output_format(b"", b"", &analytics_js_snippet(GA_ID, b"", b"")),
        config,
    );
}

#[test]
fn experiment_ga_js_cv() {
    validate_expected(
        "ga_js_cv_experiment",
        HTML_INPUT,
        &generate_expected_html(b"test.com", Some(4), b"Experiment: 2", true),
        custom_var_config(false),
    );
}

#[test]
fn experiment_ga_js_cx() {
    let mut before = b"<script src=\"".to_vec();
    before.extend_from_slice(CX_API_URL);
    before.extend_from_slice(b"\"></script>");
    let mut second = cx_chosen_variation(456, b"123");
    second.extend_from_slice(&ga_js_snippet(GA_ID, b"test.com", GA_SPEED_TRACKING));
    validate_expected(
        "ga_js_cx_experiment",
        HTML_INPUT,
        &output_format(&before, b"", &second),
        content_experiment_config(false, "456"),
    );
}

#[test]
fn experiment_ga_js_cx_string() {
    let mut before = b"<script src=\"".to_vec();
    before.extend_from_slice(CX_API_URL);
    before.extend_from_slice(b"\"></script>");
    let mut second = cx_non_numeric_comment(b"StringVariant");
    second.extend_from_slice(&ga_js_snippet(GA_ID, b"test.com", GA_SPEED_TRACKING));
    validate_expected(
        "ga_js_cx_experiment",
        HTML_INPUT,
        &output_format(&before, b"", &second),
        content_experiment_config(false, "StringVariant"),
    );
}

#[test]
fn experiment_analytics_js_cv() {
    // analytics.js cannot report custom variables: snippet only.
    validate_expected(
        "analytics_js_cx_experiment",
        HTML_INPUT,
        &output_format(
            b"",
            b"",
            &analytics_js_snippet(GA_ID, ANALYTICS_SPEED_TRACKING, b""),
        ),
        custom_var_config(true),
    );
}

#[test]
fn experiment_analytics_js_cx() {
    validate_expected(
        "analytics_js_cx_experiment",
        HTML_INPUT,
        &output_format(
            b"",
            b"",
            &analytics_js_snippet(
                GA_ID,
                ANALYTICS_SPEED_TRACKING,
                &cx_exp_and_variant(b"123", b"456"),
            ),
        ),
        content_experiment_config(true, "456"),
    );
}

#[test]
fn experiment_analytics_js_cx_string() {
    validate_expected(
        "analytics_js_cx_experiment",
        HTML_INPUT,
        &output_format(
            b"",
            b"",
            &analytics_js_snippet(
                GA_ID,
                ANALYTICS_SPEED_TRACKING,
                &cx_exp_and_variant(b"123", b"StringVariant"),
            ),
        ),
        content_experiment_config(true, "StringVariant"),
    );
}

#[test]
fn experiment_no_double() {
    let analytics_js = ga_js_snippet(GA_ID, b"test.com", b"");
    let mut input =
        b"<head>\n<title>Something</title>\n</head><body> Hello World!<script>".to_vec();
    input.extend_from_slice(&analytics_js);
    input.extend_from_slice(b"</script></body>");
    let experiment = ga_experiment_snippet(GA_SPEED_TRACKING, 4, b"Experiment: 2");
    validate_expected(
        "variable_added",
        &input,
        &output_format(b"", &experiment, &analytics_js),
        custom_var_config(false),
    );
}

#[test]
fn many_heads_and_bodies() {
    let input = b"<head></head><head></head><head></head></head><body></body><body></body>";
    let experiment = ga_experiment_snippet(b"", 4, b"Experiment: 2");
    let analytics_js = ga_js_snippet(GA_ID, b"test.com", GA_SPEED_TRACKING);
    let mut expected =
        b"<head></head><head></head><head></head></head><body></body><body><script>".to_vec();
    expected.extend_from_slice(&experiment);
    expected.extend_from_slice(&analytics_js);
    expected.extend_from_slice(b"</script></body>");
    validate_expected(
        "many_heads_and_bodies",
        input,
        &expected,
        custom_var_config(false),
    );
}

fn urchin_script(ga_id: &[u8]) -> Vec<u8> {
    let mut out =
        b"<script src=\"http://www.google-analytics.com/urchin.js\" type=\"text/javascript\"></script> <script>_uacct = \"".to_vec();
    out.extend_from_slice(ga_id);
    out.extend_from_slice(b"\"; urchinTracker();</script>");
    out
}

#[test]
fn existing_urchin_analytics() {
    let input = urchin_script(GA_ID);
    // No experiment: untouched.
    validate_expected("analytics_already_present", &input, &input, base_config());
    // Experiments cannot touch urchin snippets either.
    validate_expected(
        "urchin_not_supported_for_experiments",
        &input,
        &input,
        custom_var_config(false),
    );
    validate_expected(
        "urchin_not_supported_for_experiments",
        &input,
        &input,
        content_experiment_config(false, "456"),
    );
}

fn unusable_snippet(ga_id: &[u8]) -> Vec<u8> {
    let mut out = b"<script>var ga_id = '".to_vec();
    out.extend_from_slice(ga_id);
    out.extend_from_slice(b"';</script>");
    out
}

#[test]
fn unusable_snippet_cases() {
    let input = unusable_snippet(GA_ID);
    validate_expected("unusable_script", &input, &input, base_config());
    validate_expected("unusable_script", &input, &input, custom_var_config(false));
    validate_expected(
        "unusable_script",
        &input,
        &input,
        content_experiment_config(false, "456"),
    );
}

fn synchronous_ga(prefix: &[u8], infix: &[u8], ga_id: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(
        b"<script> var gaJsHost = ((\"https:\" == document.location.protocol) ?                \"https://ssl.\" : \"http://www.\"); document.write(unescape(\"%3Cscript src='\" + gaJsHost +                         \"google-analytics.com/ga.js'                         type='text/javascript'%3E%3C/script%3E\"));</script><script>",
    );
    out.extend_from_slice(infix);
    out.extend_from_slice(b" try { var pageTracker = _gat._getTracker(\"");
    out.extend_from_slice(ga_id);
    out.extend_from_slice(b"\");       pageTracker._trackPageview(); } catch(err) {}</script>");
    out
}

fn synchronous_dc(prefix: &[u8], infix: &[u8], ga_id: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(
        b"<script> var gaJsHost = ((\"https:\" == document.location.protocol) ?                \"https://\" : \"http://\"); document.write(unescape(\"%3Cscript src='\" + gaJsHost +                         \"stats.g.doubleclick.net/dc.js'                         type='text/javascript'%3E%3C/script%3E\"));</script><script>",
    );
    out.extend_from_slice(infix);
    out.extend_from_slice(b" try { var pageTracker = _gat._getTracker(\"");
    out.extend_from_slice(ga_id);
    out.extend_from_slice(b"\");       pageTracker._trackPageview(); } catch(err) {}</script>");
    out
}

#[test]
fn synchronous_ga_no_experiment() {
    let input = synchronous_ga(b"", b"", GA_ID);
    validate_expected("ga_js_no_experiment", &input, &input, base_config());
}

#[test]
fn synchronous_ga_custom_var_experiment() {
    let input = synchronous_ga(b"", b"", GA_ID);
    let experiment = ga_experiment_snippet(GA_SPEED_TRACKING, 4, b"Experiment: 2");
    validate_expected(
        "extend_sync_ga_js_for_cv_experiment",
        &input,
        &synchronous_ga(b"", &experiment, GA_ID),
        custom_var_config(false),
    );
}

#[test]
fn synchronous_dc_custom_var_experiment() {
    let input = synchronous_dc(b"", b"", GA_ID);
    let experiment = ga_experiment_snippet(GA_SPEED_TRACKING, 4, b"Experiment: 2");
    validate_expected(
        "extend_sync_dc_js_for_cv_experiment",
        &input,
        &synchronous_dc(b"", &experiment, GA_ID),
        custom_var_config(false),
    );
}

fn cx_rewrite_infix() -> Vec<u8> {
    let mut out = b"</script><script src=\"".to_vec();
    out.extend_from_slice(CX_API_URL);
    out.extend_from_slice(b"\"></script><script>");
    out.extend_from_slice(&cx_chosen_variation(456, b"123"));
    out
}

#[test]
fn synchronous_ga_content_experiment() {
    let input = synchronous_ga(b"", b"", GA_ID);
    validate_expected(
        "extend_sync_ga_js_for_content_experiment",
        &input,
        &synchronous_ga(b"", &cx_rewrite_infix(), GA_ID),
        content_experiment_config(false, "456"),
    );
}

fn async_ga(prefix: &[u8], infix: &[u8], ga_id: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(
        b"<script type='text/javascript'>document.write('another script');</script><script>",
    );
    out.extend_from_slice(infix);
    out.extend_from_slice(b"var _gaq = _gaq || [];_gaq.push(['_setAccount', '");
    out.extend_from_slice(ga_id);
    out.extend_from_slice(
        b"']);_gaq.push(['_trackPageview']);(function() {  var ga = document.createElement('script');  ga.src = ('https:' == document.location.protocol ?  'https://ssl' : 'http://www') +  '.google-analytics.com/ga.js';  ga.setAttribute('async', 'true');  document.documentElement.firstChild.appendChild(ga);})();</script>",
    );
    out
}

fn async_dc(prefix: &[u8], infix: &[u8], ga_id: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(
        b"<script type='text/javascript'>document.write('another script');</script><script>",
    );
    out.extend_from_slice(infix);
    out.extend_from_slice(b"var _gaq = _gaq || [];_gaq.push(['_setAccount', '");
    out.extend_from_slice(ga_id);
    out.extend_from_slice(
        b"']);_gaq.push(['_trackPageview']);(function() {  var ga = document.createElement('script');  ga.src = ('https:' == document.location.protocol ?  'https://' : 'http://') +  'stats.g.doubleclick.net/dc.js'';  ga.setAttribute('async', 'true');  document.documentElement.firstChild.appendChild(ga);})();</script>",
    );
    out
}

#[test]
fn asynchronous_ga_no_experiment() {
    let input = async_ga(b"", b"", GA_ID);
    validate_expected("async_ga_js_no_experiment", &input, &input, base_config());
}

#[test]
fn asynchronous_ga_custom_var_experiment() {
    let input = async_ga(b"", b"", GA_ID);
    let experiment = ga_experiment_snippet(GA_SPEED_TRACKING, 4, b"Experiment: 2");
    validate_expected(
        "extend_async_ga_js_for_cv_experiment",
        &input,
        &async_ga(b"", &experiment, GA_ID),
        custom_var_config(false),
    );
}

#[test]
fn asynchronous_dc_custom_var_experiment() {
    let input = async_dc(b"", b"", GA_ID);
    let experiment = ga_experiment_snippet(GA_SPEED_TRACKING, 4, b"Experiment: 2");
    validate_expected(
        "extend_async_dc_js_for_cv_experiment",
        &input,
        &async_dc(b"", &experiment, GA_ID),
        custom_var_config(false),
    );
}

#[test]
fn asynchronous_ga_content_experiment() {
    let input = async_ga(b"", b"", GA_ID);
    validate_expected(
        "extend_async_ga_js_for_content_experiment",
        &input,
        &async_ga(b"", &cx_rewrite_infix(), GA_ID),
        content_experiment_config(false, "456"),
    );
}

fn analytics_js(
    prefix: &[u8],
    ga_id: &[u8],
    create_extra: &[u8],
    send_extra: &[u8],
    send: &[u8],
) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(
        b"<script>(function(i,s,o,g,r,a,m){  i['GoogleAnalyticsObject']=r;  i[r]=i[r]||function(){    (i[r].q=i[r].q||[]).push(arguments)},i[r].l=1*new Date();    a=s.createElement(o), m=s.getElementsByTagName(o)[0];    a.async=1;a.src=g;m.parentNode.insertBefore(a,m)})(window,document,'script',   '//www.google-analytics.com/analytics.js','ga');ga('create', '",
    );
    out.extend_from_slice(ga_id);
    out.extend_from_slice(b"', 'auto'");
    out.extend_from_slice(create_extra);
    out.extend_from_slice(b");");
    out.extend_from_slice(send_extra);
    out.extend_from_slice(send);
    out.extend_from_slice(b"</script>");
    out
}

fn analytics_js_no_create(prefix: &[u8], create: &[u8], send_extra: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(
        b"<script>(function(i,s,o,g,r,a,m){  i['GoogleAnalyticsObject']=r;  i[r]=i[r]||function(){    (i[r].q=i[r].q||[]).push(arguments)},i[r].l=1*new Date();    a=s.createElement(o), m=s.getElementsByTagName(o)[0];    a.async=1;a.src=g;m.parentNode.insertBefore(a,m)})(window,document,'script',   '//www.google-analytics.com/analytics.js','ga');",
    );
    out.extend_from_slice(create);
    out.extend_from_slice(send_extra);
    out.extend_from_slice(b"ga('send', 'pageview');</script>");
    out
}

fn analytics_js_invalid(
    prefix: &[u8],
    ga_id: &[u8],
    create_extra: &[u8],
    send_extra: &[u8],
    send: &[u8],
) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(
        b"<script>(functioni,s,o,g,r,a,m){  i['GoogleAnalyticsObject']=r;  i[r]=i[r]||function(){    (i[r].q=i[r].q||[]).push(arguments)},i[r].l=1*new Date();    a=s.createElement(o), m=s.getElementsByTagName(o)[0];    a.async=1;a.src=g;m.parentNode.insertBefore(a,m)})(window,document,'script',   '//www.google-analytics.com/analytics.js','ga');ga('create', '",
    );
    out.extend_from_slice(ga_id);
    out.extend_from_slice(b"', 'auto'");
    out.extend_from_slice(create_extra);
    out.extend_from_slice(b");");
    out.extend_from_slice(send_extra);
    out.extend_from_slice(send);
    out.extend_from_slice(b"</script>");
    out
}

const SEND_PAGEVIEWS: &[&[u8]] = &[
    b"ga('send', 'pageview');",
    b"ga(\"send\", \"pageview\");",
    b"ga   (   'send'    ,        'pageview'    )    ;",
    b"ga(\n'send',\n'pageview'\n);",
    b"ga('MyTracker.send','pageview', 'foo', 'bar');",
];

const NOT_SEND_PAGEVIEWS: &[&[u8]] = &[
    b"ga('sendpageview');",
    b"ga('send''pageview');",
    b"ga('send' 'pageview');",
    b"a('send', 'pageview');",
    b"ga('send', 'pageview'[1]);",
    b"ga('send', 'event', 'link', 'click');",
];

#[test]
fn analytics_js_no_experiment() {
    let input = analytics_js(b"", GA_ID, b"", b"", SEND_PAGEVIEWS[0]);
    validate_expected("analytics_js_no_experiment", &input, &input, base_config());
}

#[test]
fn analytics_js_no_custom_var_experiment() {
    let input = analytics_js(b"", GA_ID, b"", b"", SEND_PAGEVIEWS[0]);
    validate_expected(
        "analytics_js_with_cv_experiment",
        &input,
        &input,
        custom_var_config(false),
    );
}

#[test]
fn analytics_js_content_experiment() {
    let experiment = cx_exp_and_variant(b"123", b"456");
    for (index, send) in SEND_PAGEVIEWS.iter().enumerate() {
        validate_expected(
            &format!("analytics_js_cx_insertion_{index}"),
            &analytics_js(b"", GA_ID, b"", b"", send),
            &analytics_js(b"", GA_ID, ANALYTICS_SPEED_TRACKING, &experiment, send),
            content_experiment_config(false, "456"),
        );
    }
    for (index, send) in NOT_SEND_PAGEVIEWS.iter().enumerate() {
        let input = analytics_js(b"", GA_ID, b"", b"", send);
        validate_expected(
            &format!("analytics_js_cx_non_insertion_{index}"),
            &input,
            &input,
            content_experiment_config(false, "456"),
        );
    }
}

#[test]
fn analytics_js_invalid_no_experiment() {
    let input = analytics_js_invalid(b"", GA_ID, b"", b"", SEND_PAGEVIEWS[0]);
    validate_expected("analytics_js_no_experiment", &input, &input, base_config());
}

#[test]
fn analytics_js_invalid_no_custom_var_experiment() {
    let input = analytics_js_invalid(b"", GA_ID, b"", b"", SEND_PAGEVIEWS[0]);
    validate_expected(
        "analytics_js_with_cv_experiment",
        &input,
        &input,
        custom_var_config(false),
    );
}

#[test]
fn analytics_js_invalid_content_experiment() {
    for (index, send) in SEND_PAGEVIEWS.iter().enumerate() {
        let input = analytics_js_invalid(b"", GA_ID, b"", b"", send);
        validate_expected(
            &format!("analytics_js_cx_insertion_{index}"),
            &input,
            &input,
            content_experiment_config(false, "456"),
        );
    }
    for (index, send) in NOT_SEND_PAGEVIEWS.iter().enumerate() {
        let input = analytics_js_invalid(b"", GA_ID, b"", b"", send);
        validate_expected(
            &format!("analytics_js_cx_non_insertion_{index}"),
            &input,
            &input,
            content_experiment_config(false, "456"),
        );
    }
}

fn format_create(template: &[u8], ga_id: &[u8], extra: &[u8]) -> Vec<u8> {
    // Templates carry one or two `%s` slots; the field-object template
    // uses positional slots instead (handled by its caller).
    let mut out = Vec::new();
    let mut rest = template;
    let mut replacements = [ga_id, extra].into_iter();
    while let Some(pos) = rest.windows(2).position(|window| window == b"%s") {
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(replacements.next().unwrap_or(b"".as_slice()));
        rest = &rest[pos + 2..];
    }
    out.extend_from_slice(rest);
    out
}

const NO_FIELD_OBJECT_CREATES: &[&[u8]] = &[
    b"ga('create', '%s', 'auto'%s);",
    b"ga(\"create\", \"%s\", \"auto\"%s);",
    b"ga('create','%s','auto'%s);",
    b"ga    (    'create'    ,    '%s'    ,    'auto'     %s);",
    b"ga(\n'create'\n,\n'%s'\n,\n'auto'\n%s);",
    b"ga('create', '%s'%s);",
    b"ga('create','%s','example.com', 'myTracker'%s);",
];

const YES_FIELD_OBJECT_CREATES: &[&[u8]] = &[
    b"ga('create', '%s', {%stransport: 'beacon'});",
    b"ga('create', '%s', {%stransport: \"beacon\"});",
    b"ga('create', '%s', {%stransport: 'beacon', cookieDomain: 'auto'});",
    b"ga('create','%s',{%stransport:'beacon'});",
    b"ga( 'create' , '%s' , {   %stransport   : 'beacon'  }  );",
    b"ga('create', {%2$strackingId: '%1$s'});",
    b"ga('create', '%s', 'auto', 'foo', {%stransport: 'beacon'});",
];

const GA_NO_CREATES: &[&[u8]] = &[
    b"ga('create \"%s\" auto');",
    b"ga('create, \"%s\", auto');",
    b"ga[0]('create', '%s', 'auto');",
    b"ga('create', ('%s', 'auto'));",
    b"ga('create'('%s', 'auto'));",
    b"ga('create, \"%s\", auto, {transport: \\'beacon\\'}');",
];

fn substitute_once(template: &[u8], needle: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(template.len() + value.len());
    let mut rest = template;
    while let Some(pos) = rest
        .windows(needle.len())
        .position(|window| window == needle)
    {
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(value);
        rest = &rest[pos + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
}

fn format_positional_create(template: &[u8], ga_id: &[u8], extra: &[u8]) -> Vec<u8> {
    // Only the trackingId template uses `%1$s`/`%2$s` positional slots.
    substitute_once(&substitute_once(template, b"%1$s", ga_id), b"%2$s", extra)
}

#[test]
fn analytics_js_content_experiment_speed_tracking() {
    let experiment = cx_exp_and_variant(b"123", b"456");
    for (index, template) in NO_FIELD_OBJECT_CREATES.iter().enumerate() {
        validate_expected(
            &format!("analytics_js_cx_insertion_speed_tracking_{index}"),
            &analytics_js_no_create(b"", &format_create(template, GA_ID, b""), b""),
            &analytics_js_no_create(
                b"",
                &format_create(template, GA_ID, ANALYTICS_SPEED_TRACKING),
                &experiment,
            ),
            content_experiment_config(false, "456"),
        );
    }
    for (index, template) in YES_FIELD_OBJECT_CREATES.iter().enumerate() {
        let format = |extra: &[u8]| {
            if template.contains(&b'%') && template.windows(4).any(|w| w == b"%1$s" || w == b"%2$s")
            {
                format_positional_create(template, GA_ID, extra)
            } else {
                format_create(template, GA_ID, extra)
            }
        };
        validate_expected(
            &format!("analytics_js_cx_insertion_field_object_speed_tracking_{index}"),
            &analytics_js_no_create(b"", &format(b""), b""),
            &analytics_js_no_create(b"", &format(ANALYTICS_SPEED_TRACKING_MINIMAL), &experiment),
            content_experiment_config(false, "456"),
        );
    }
    for (index, template) in GA_NO_CREATES.iter().enumerate() {
        let create = format_create(template, GA_ID, b"");
        validate_expected(
            &format!("analytics_js_cx_non_insertion_speed_tracking_{index}"),
            &analytics_js_no_create(b"", &create, b""),
            &analytics_js_no_create(b"", &create, &experiment),
            content_experiment_config(false, "456"),
        );
    }
}

#[test]
fn analytics_js_content_experiment_no_increase_speed() {
    let mut config = content_experiment_config(false, "456");
    config.increase_speed_tracking = false;
    let experiment = cx_exp_and_variant(b"123", b"456");
    validate_expected(
        "analytics_js_cx_insertion_no_increased_speed",
        &analytics_js(b"", GA_ID, b"", b"", SEND_PAGEVIEWS[0]),
        &analytics_js(b"", GA_ID, b"", &experiment, SEND_PAGEVIEWS[0]),
        config,
    );
}

#[test]
fn analytics_js_no_close_body() {
    let input = b"<head>\n<title>Something</title>\n</head><body> Hello World!";
    let snippet = analytics_js_snippet(
        GA_ID,
        ANALYTICS_SPEED_TRACKING,
        &cx_exp_and_variant(b"123", b"456"),
    );
    let mut expected = input.to_vec();
    expected.extend_from_slice(b"<script>");
    expected.extend_from_slice(&snippet);
    expected.extend_from_slice(b"</script>");
    validate_expected(
        "no_close_body",
        input,
        &expected,
        content_experiment_config(true, "456"),
    );
}

#[test]
fn asynchronous_ga_content_experiment_flush() {
    let part1 =
        b"<script type='text/javascript'>document.write('another script');</script><script>";
    let mut part2 = b"var _gaq = _gaq || [];_gaq.push(['_setAccount', '".to_vec();
    part2.extend_from_slice(GA_ID);
    part2.extend_from_slice(b"']);_gaq.push(['_trackPageview']);(function() {  var ga = document.createElement('script');  ga.src = ('https:' == document.location.protocol ?  'https://ssl' : 'http://www') +  '.google-analytics.com/ga.js';  ga.setAttribute('async', 'true');  document.documentElement.firstChild.appendChild(ga);})();");
    let part3 = b"</script>";

    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_pre_render_filter(Box::new(InsertGAFilter::new(content_experiment_config(
        false, "456",
    ))));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/flush.html", false);
    parser.write(b"<html>");
    parser.write(part1);
    parser.flush();
    parser.write(&part2);
    parser.flush();
    parser.write(part3);
    parser.finish();

    let mut expected = b"<html>".to_vec();
    expected.extend_from_slice(&async_ga(b"", &cx_rewrite_infix(), GA_ID));
    assert_eq!(sink.bytes(), expected);
}

#[test]
fn gtag_snippet_insertion() {
    // Hand test: GT-/G- ids take the gtag.js path (uncovered upstream).
    let mut config = base_config();
    config.ga_id = b"G-12345".to_vec();
    let mut script = b"if (window.parent == window) {window.dataLayer = window.dataLayer || [];function gtag(){dataLayer.push(arguments);}gtag('js', new Date());gtag('config', '".to_vec();
    script.extend_from_slice(b"G-12345");
    script.extend_from_slice(b"');(function(){var s = document.createElement('script');s.async = true;s.src = 'https://www.googletagmanager.com/gtag/js?id=");
    script.extend_from_slice(b"G-12345");
    script.extend_from_slice(b"';var f = document.getElementsByTagName('script')[0];if (f && f.parentNode) {f.parentNode.insertBefore(s, f);} else {(document.head || document.documentElement).appendChild(s);}})();}");
    validate_expected(
        "gtag_insertion",
        HTML_INPUT,
        &output_format(b"", b"", &script),
        config,
    );
}

#[test]
fn analytics_js_detection_quirk() {
    // Hand test documenting upstream behavior: the `else if (s.find(...))`
    // in snippet detection treats every position except 0 — including
    // not-found — as analytics.js, so a bare ga(create/send) snippet
    // without the loader URL still rewrites under a content experiment.
    let input = b"<script>var ga_id = 'UA-21111111-1';ga('create', 'UA-21111111-1', 'auto');ga('send', 'pageview');</script>";
    let experiment = cx_exp_and_variant(b"123", b"456");
    let mut expected =
        b"<script>var ga_id = 'UA-21111111-1';ga('create', 'UA-21111111-1', 'auto'".to_vec();
    expected.extend_from_slice(ANALYTICS_SPEED_TRACKING);
    expected.extend_from_slice(b");");
    expected.extend_from_slice(&experiment);
    expected.extend_from_slice(b"ga('send', 'pageview');</script>");
    validate_expected(
        "analytics_js_detection_quirk",
        input,
        &expected,
        content_experiment_config(false, "456"),
    );
}

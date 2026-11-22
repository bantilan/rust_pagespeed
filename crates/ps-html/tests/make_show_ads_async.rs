//! ShowAds tests ported from `make_show_ads_async_filter_test.cc`.
//!
//! NOTE: inputs are not body-wrapped (the C++ wrapper carries no scripts,
//! so it commutes with this filter). The `show_ads_snippets_not_converted`
//! statistic is not ported: upstream registers it but never increments it,
//! so every C++ assertion on it is a vacuous zero.
#[path = "harness.rs"]
mod harness;

use harness::{SharedFilter, SharedSink};
use ps_html::filters::MakeShowAdsAsyncFilter;
use ps_html::filters::make_show_ads_async::{API_CALLS_REPLACED, SNIPPETS_CONVERTED};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;
use ps_stats::core::Statistics;

const ADS_BY_GOOGLE_JS: &[u8] =
    b"<script async src=\"//pagead2.googlesyndication.com/pagead/js/adsbygoogle.js\"></script>";
const ADS_BY_GOOGLE_API_CALL: &[u8] =
    b"<script>(adsbygoogle = window.adsbygoogle || []).push({})</script>";
const SHOW_ADS_API_CALL: &[u8] = b"<script type=\"text/javascript\" src=\"http://pagead2.googlesyndication.com/pagead/show_ads.js\"> </script>";

fn page(content: &[u8]) -> Vec<u8> {
    let mut out = b"<head><title>Something</title></head><body>".to_vec();
    out.extend_from_slice(content);
    out.extend_from_slice(b"</body>");
    out
}

fn ins(content: &[u8]) -> Vec<u8> {
    let mut out = b"<ins class=\"adsbygoogle\" ".to_vec();
    out.extend_from_slice(content);
    out.extend_from_slice(b" </ins>");
    out
}

fn snippet(content: &[u8]) -> Vec<u8> {
    let mut out = b"<script type=\"text/javascript\"> ".to_vec();
    out.extend_from_slice(content);
    out.extend_from_slice(b" </script>");
    out
}

fn show_ads_content_1(before: &[u8], middle: &[u8], after: &[u8]) -> Vec<u8> {
    let mut out = before.to_vec();
    out.extend_from_slice(b"google_ad_client = \"test-publishercode-expected\"; ");
    out.extend_from_slice(middle);
    out.extend_from_slice(b"google_ad_channel = \"test-original-channel\"; ");
    out.extend_from_slice(b"google_ad_slot = \"1234567\";");
    out.extend_from_slice(b"google_ad_width = 728;");
    out.extend_from_slice(b"google_ad_height = 90;");
    out.extend_from_slice(after);
    out
}

fn show_ads_output_1() -> Vec<u8> {
    b"<ins class=\"adsbygoogle\" style=\"display:inline-block;width:728px;height:90px\" data-ad-channel=\"test-original-channel\" data-ad-client=\"test-publishercode-expected\" data-ad-slot=\"1234567\"></ins>".to_vec()
}

fn show_ads_format_1_output() -> Vec<u8> {
    let mut out = ADS_BY_GOOGLE_JS.to_vec();
    out.extend_from_slice(&show_ads_output_1());
    out.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    out
}

const SHOW_ADS_CONTENT_2: &[u8] = b"<!--google_ad_client = \"test-publishercode-expected\"; /**/google_ad_slot = \"1234562\";google_ad_width = 722;google_ad_height = 92;google_ad_format = \"722x92\";//-->";

fn show_ads_output_2() -> Vec<u8> {
    b"<ins class=\"adsbygoogle\" style=\"display:inline-block;width:722px;height:92px\" data-ad-client=\"test-publishercode-expected\" data-ad-format=\"722x92\" data-ad-slot=\"1234562\"></ins>".to_vec()
}

const ADS_BY_GOOGLE_CONTENT_1: &[u8] = b"style=\"display:inline-block;width:160px;height:600px\" data-ad-client=\"test-publishercode-expected\" data-ad-slot=\"1234567\" data-ad-channel=\"test-original-channel\">";
const ADS_BY_GOOGLE_CONTENT_2: &[u8] = b"style=\"display:inline-block;width:162px;height:602px\" data-ad-client=\"test-publishercode-expected\" data-ad-slot=\"1234562\">";

fn run_case(case: &str, input: &[u8]) -> (Vec<u8>, SharedFilter<MakeShowAdsAsyncFilter>) {
    run_writes(case, &[(input, false)])
}

fn run_writes(
    case: &str,
    writes: &[(&[u8], bool)],
) -> (Vec<u8>, SharedFilter<MakeShowAdsAsyncFilter>) {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(MakeShowAdsAsyncFilter::new());
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    for (chunk, flush) in writes {
        parser.write(chunk);
        if *flush {
            parser.flush();
        }
    }
    parser.finish();
    (sink.bytes(), filter)
}

fn validate_expected(
    case: &str,
    input: &[u8],
    expected: &[u8],
) -> SharedFilter<MakeShowAdsAsyncFilter> {
    let (output, filter) = run_case(case, input);
    assert_eq!(output, expected, "case {case}");
    filter
}

fn check_stats(filter: &SharedFilter<MakeShowAdsAsyncFilter>, converted: i64, replaced: i64) {
    assert_eq!(filter.with(|filter| filter.snippets_converted()), converted);
    assert_eq!(filter.with(|filter| filter.api_calls_replaced()), replaced);
}

#[test]
fn no_ads() {
    let filter = validate_expected(
        "no_ads",
        &page(b"HTML page with no ads"),
        &page(b"HTML page with no ads"),
    );
    check_stats(&filter, 0, 0);
}

#[test]
fn one_show_ads() {
    let mut input = snippet(&show_ads_content_1(b"", b"", b""));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let filter = validate_expected(
        "one_show_ads",
        &page(&input),
        &page(&show_ads_format_1_output()),
    );
    check_stats(&filter, 1, 1);
}

#[test]
fn one_show_ads_with_comments() {
    let mut input = snippet(&show_ads_content_1(b"", b"/* comment */", b""));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let filter = validate_expected(
        "one_show_ads_with_comments",
        &page(&input),
        &page(&show_ads_format_1_output()),
    );
    check_stats(&filter, 1, 1);
}

#[test]
fn one_show_ads_with_enclosing_comment_tags() {
    let mut input = snippet(&show_ads_content_1(b"<!--", b"", b"//-->"));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let filter = validate_expected(
        "one_show_ads_with_enclosing_comment_tags",
        &page(&input),
        &page(&show_ads_format_1_output()),
    );
    check_stats(&filter, 1, 1);
}

#[test]
fn one_show_ads_data2() {
    let mut input = snippet(SHOW_ADS_CONTENT_2);
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let mut expected = ADS_BY_GOOGLE_JS.to_vec();
    expected.extend_from_slice(&show_ads_output_2());
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    let filter = validate_expected("one_show_ads_data2", &page(&input), &page(&expected));
    check_stats(&filter, 1, 1);
}

#[test]
fn multiple_show_ads() {
    let mut input = snippet(&show_ads_content_1(b"", b"", b""));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    input.extend_from_slice(&snippet(SHOW_ADS_CONTENT_2));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let mut expected = show_ads_format_1_output();
    expected.extend_from_slice(&show_ads_output_2());
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    let filter = validate_expected("multiple_show_ads", &page(&input), &page(&expected));
    check_stats(&filter, 2, 2);
}

#[test]
fn shows_ads_html_google_ad_output() {
    let mut input = snippet(&show_ads_content_1(b"google_ad_output='html';", b"", b""));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    // The output flag splices before `data-ad-slot` (sorted position).
    let base = show_ads_output_1();
    let at = base
        .windows(b"data-ad-slot".len())
        .position(|window| window == b"data-ad-slot")
        .expect("slot present");
    let mut hacked = base[..at].to_vec();
    hacked.extend_from_slice(b"data-ad-output=\"html\" ");
    hacked.extend_from_slice(&base[at..]);
    let mut expected = ADS_BY_GOOGLE_JS.to_vec();
    expected.extend_from_slice(&hacked);
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    let filter = validate_expected(
        "shows_ads_html_google_ad_output",
        &page(&input),
        &page(&expected),
    );
    check_stats(&filter, 1, 1);
}

#[test]
fn mixed_ads() {
    let mut input = ADS_BY_GOOGLE_JS.to_vec();
    input.extend_from_slice(&ins(ADS_BY_GOOGLE_CONTENT_1));
    input.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    input.extend_from_slice(&snippet(&show_ads_content_1(b"", b"", b"")));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    input.extend_from_slice(&ins(ADS_BY_GOOGLE_CONTENT_2));
    input.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    input.extend_from_slice(&snippet(&show_ads_content_1(b"", b"", b"")));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let mut expected = ADS_BY_GOOGLE_JS.to_vec();
    expected.extend_from_slice(&ins(ADS_BY_GOOGLE_CONTENT_1));
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    expected.extend_from_slice(&show_ads_output_1());
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    expected.extend_from_slice(&ins(ADS_BY_GOOGLE_CONTENT_2));
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    expected.extend_from_slice(&show_ads_output_1());
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    let filter = validate_expected("mixed_ads", &page(&input), &page(&expected));
    check_stats(&filter, 2, 2);
}

#[test]
fn show_ads_missing_api_call_flag() {
    let input = snippet(&show_ads_content_1(b"", b"", b""));
    let mut expected = ADS_BY_GOOGLE_JS.to_vec();
    expected.extend_from_slice(&show_ads_output_1());
    let filter = validate_expected(
        "show_ads_missing_api_call_flag",
        &page(&input),
        &page(&expected),
    );
    check_stats(&filter, 1, 0);
}

#[test]
fn mispaired_show_ads_flag() {
    let mut input = SHOW_ADS_API_CALL.to_vec();
    input.extend_from_slice(&snippet(&show_ads_content_1(b"", b"", b"")));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let mut expected = SHOW_ADS_API_CALL.to_vec();
    expected.extend_from_slice(ADS_BY_GOOGLE_JS);
    expected.extend_from_slice(&show_ads_output_1());
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    expected.extend_from_slice(SHOW_ADS_API_CALL);
    let filter = validate_expected("mispaired_show_ads_flag", &page(&input), &page(&expected));
    check_stats(&filter, 1, 1);
}

#[test]
fn mixed_ads_with_misplaced_snippet() {
    let mut input = ADS_BY_GOOGLE_JS.to_vec();
    input.extend_from_slice(&ins(ADS_BY_GOOGLE_CONTENT_1));
    input.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    input.extend_from_slice(SHOW_ADS_API_CALL);
    input.extend_from_slice(&ins(ADS_BY_GOOGLE_CONTENT_2));
    input.extend_from_slice(&snippet(&show_ads_content_1(b"", b"", b"")));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let mut expected = ADS_BY_GOOGLE_JS.to_vec();
    expected.extend_from_slice(&ins(ADS_BY_GOOGLE_CONTENT_1));
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    expected.extend_from_slice(SHOW_ADS_API_CALL);
    expected.extend_from_slice(&ins(ADS_BY_GOOGLE_CONTENT_2));
    expected.extend_from_slice(&show_ads_output_1());
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    let filter = validate_expected(
        "mixed_ads_with_misplaced_snippet",
        &page(&input),
        &page(&expected),
    );
    check_stats(&filter, 1, 1);
}

#[test]
fn show_ads_missing_attribute() {
    let input = snippet(
        b"<!--google_ad_client = \"test-publishercode-expected\"; google_ad_slot = \"1234567\";google_ad_height = 90;//-->",
    );
    let filter = validate_expected("show_ads_missing_attribute", &page(&input), &page(&input));
    check_stats(&filter, 0, 0);
}

#[test]
fn show_ads_unexpected_statement() {
    let input = snippet(
        b"<!--google_ad_client = \"test-publishercode-expected\"; google_ad_slot = \"1234567\";google_ad_height = 90;if (a) google_ad_width = 180; else google_ad_width = 190;//-->",
    );
    let filter = validate_expected(
        "show_ads_unexpected_statement",
        &page(&input),
        &page(&input),
    );
    check_stats(&filter, 0, 0);
}

#[test]
fn show_ads_invalid_google_ad_format() {
    let input = snippet(
        b"<!--google_ad_client = \"test-publishercode-expected\"; google_ad_slot = \"1234567\";google_ad_width = 722;google_ad_height = 92;google_ad_format = \"weird_722x92_as_rimg\";//-->",
    );
    validate_expected(
        "show_ads_invalid_google_ad_format",
        &page(&input),
        &page(&input),
    );
}

#[test]
fn shows_ads_js_google_ad_output() {
    let mut input = snippet(&show_ads_content_1(b"google_ad_output='js';", b"", b""));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    validate_expected(
        "shows_ads_js_google_ad_output",
        &page(&input),
        &page(&input),
    );
}

#[test]
fn flush_in_the_middle_of_show_ads_data_script() {
    let head = b"<head><title>Something</title></head><body>";
    let part1 = b"<script type=\"text/javascript\"> google_ad_client = \"test-publishercode-expected\"; google_ad_channel = \"test-original-channel\"; ";
    let part2 = b"google_ad_slot = \"1234567\";google_ad_width = 728;google_ad_height = 90;";
    let part3 = b"</script>";
    let part4 = b"<script type=\"text/javascript\" src=\"http://pagead2.googlesyndication.com/pagead/show_ads.js\"> </script>";
    let part5 = b"</body>";
    // The flush splits the writes but the script element stays rewritable:
    // the lexer buffers it until `</script>`.
    let (output, filter) = run_writes(
        "flush_data",
        &[
            (head, false),
            (part1, true),
            (part2, false),
            (part3, false),
            (part4, false),
            (part5, false),
        ],
    );
    let mut expected = b"<head><title>Something</title></head><body>".to_vec();
    expected.extend_from_slice(ADS_BY_GOOGLE_JS);
    expected.extend_from_slice(
        b"<ins class=\"adsbygoogle\" style=\"display:inline-block;width:728px;height:90px\" data-ad-channel=\"test-original-channel\" data-ad-client=\"test-publishercode-expected\" data-ad-slot=\"1234567\"></ins>",
    );
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    expected.extend_from_slice(b"</body>");
    assert_eq!(output, expected);
    check_stats(&filter, 1, 1);
}

#[test]
fn flush_in_the_middle_of_show_ads_api_call() {
    let head = b"<head><title>Something</title></head><body>";
    let data = b"<script type=\"text/javascript\"> google_ad_client = \"test-publishercode-expected\"; google_ad_channel = \"test-original-channel\"; google_ad_slot = \"1234567\";google_ad_width = 728;google_ad_height = 90;</script>";
    let api_open = b"<script type=\"text/javascript\" src=\"http://pagead2.googlesyndication.com/pagead/show_ads.js\"> ";
    let api_close = b"</script>";
    let tail = b"</body>";
    let (output, filter) = run_writes(
        "flush_api",
        &[
            (head, false),
            (data, false),
            (api_open, true),
            (api_close, false),
            (tail, false),
        ],
    );
    let mut expected = b"<head><title>Something</title></head><body>".to_vec();
    expected.extend_from_slice(ADS_BY_GOOGLE_JS);
    expected.extend_from_slice(
        b"<ins class=\"adsbygoogle\" style=\"display:inline-block;width:728px;height:90px\" data-ad-channel=\"test-original-channel\" data-ad-client=\"test-publishercode-expected\" data-ad-slot=\"1234567\"></ins>",
    );
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    expected.extend_from_slice(b"</body>");
    assert_eq!(output, expected);
    check_stats(&filter, 1, 1);
}

#[test]
fn flush_in_the_middle_of_show_ads_data_and_api_call() {
    let head = b"<head><title>Something</title></head><body>";
    let part1 = b"<script type=\"text/javascript\"> google_ad_client = \"test-publishercode-expected\"; google_ad_channel = \"test-original-channel\"; ";
    let part2 = b"google_ad_slot = \"1234567\";google_ad_width = 728;google_ad_height = 90;";
    let part3 = b"</script>";
    let api_open = b"<script type=\"text/javascript\" src=\"http://pagead2.googlesyndication.com/pagead/show_ads.js\"> ";
    let api_close = b"</script>";
    let tail = b"</body>";
    let (output, filter) = run_writes(
        "flush_both",
        &[
            (head, false),
            (part1, false),
            (part2, true),
            (part3, false),
            (api_open, true),
            (api_close, false),
            (tail, false),
        ],
    );
    let mut expected = b"<head><title>Something</title></head><body>".to_vec();
    expected.extend_from_slice(ADS_BY_GOOGLE_JS);
    expected.extend_from_slice(
        b"<ins class=\"adsbygoogle\" style=\"display:inline-block;width:728px;height:90px\" data-ad-channel=\"test-original-channel\" data-ad-client=\"test-publishercode-expected\" data-ad-slot=\"1234567\"></ins>",
    );
    expected.extend_from_slice(ADS_BY_GOOGLE_API_CALL);
    expected.extend_from_slice(b"</body>");
    assert_eq!(output, expected);
    check_stats(&filter, 1, 1);
}

#[test]
fn show_ads_no_parent() {
    let mut input = snippet(&show_ads_content_1(b"", b"", b""));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    let (output, filter) = run_case("no_parent", &input);
    assert_eq!(output, show_ads_format_1_output());
    check_stats(&filter, 1, 1);
}

#[test]
fn publishes_conversion_counters() {
    let stats = Statistics::new();
    let mut filter = MakeShowAdsAsyncFilter::new();
    filter.init_stats(&stats);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/stats.html", false);
    let mut input = snippet(&show_ads_content_1(b"", b"", b""));
    input.extend_from_slice(SHOW_ADS_API_CALL);
    parser.write(&page(&input));
    parser.finish();
    assert_eq!(stats.get_variable(SNIPPETS_CONVERTED).unwrap().get(), 1);
    assert_eq!(stats.get_variable(API_CALLS_REPLACED).unwrap().get(), 1);
}

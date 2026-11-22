//! Driver tests: plan-driven phased execution end to end.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;

use harness::SharedSink;
use ps_config::filter::{Filter, RewriteLevel};
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::chain::{ChainContext, ChainFilter, Phase};
use ps_html::driver::Driver;
use ps_html::element::NodeId;
use ps_html::filter::HtmlFilter;
use ps_html::lexer::Lexer;
use ps_html::parser::{Dom, Parser};

fn ctx() -> ChainContext {
    ChainContext::new(true, true)
}

fn enable(options: &mut RewriteOptions, filters: &[Filter]) {
    for filter in filters {
        options.filters_mut().enable(*filter);
    }
}

#[test]
fn passthrough_round_trip() {
    let options = RewriteOptions::new();
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    assert_eq!(
        driver.skipped_slots(),
        [
            (Phase::EarlyPreRender, ChainFilter::StripSubresourceHints),
            (Phase::PostRender, ChainFilter::SupportNoscript),
        ]
    );
    let input = b"<div class=\"a\">Hello <b>world</b><!-- c --></div>";
    driver.start_parse(b"http://test.com/pt.html", false);
    driver.write(input);
    driver.finish();
    assert_eq!(sink.bytes(), input);
}

#[test]
fn executes_implemented_slots_across_phases() {
    // AddIds (early-pre) + StripScripts (pre) + RemoveComments and
    // CollapseWhitespace (post) all apply through the driver.
    let mut options = RewriteOptions::new();
    enable(
        &mut options,
        &[
            Filter::AddIds,
            Filter::StripScripts,
            Filter::RemoveComments,
            Filter::CollapseWhitespace,
        ],
    );
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    assert_eq!(
        driver.skipped_slots(),
        [
            (Phase::EarlyPreRender, ChainFilter::StripSubresourceHints),
            (Phase::PostRender, ChainFilter::SupportNoscript),
        ]
    );
    driver.start_parse(b"http://test.com/exec.html", false);
    driver.write(b"<div>  <!--c-->x  </div><script>y</script>");
    driver.finish();
    assert_eq!(sink.bytes(), b"<div id=\"PageSpeed-0\"> x </div>");
}

#[test]
fn core_plan_leaves_unplanned_content_alone() {
    // Core enables no comment/whitespace filters: content passes through
    // even though those filters exist (AddHead still inserts the
    // missing head, verbatim C++).
    let mut options = RewriteOptions::new();
    options.filters_mut().set_level(RewriteLevel::CoreFilters);
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    // Minify subsets cover the CSS/JS slots now.
    assert!(!driver
        .skipped_slots()
        .iter()
        .any(|(_, slot)| *slot == ChainFilter::CssFilter));
    assert!(!driver
        .skipped_slots()
        .iter()
        .any(|(_, slot)| *slot == ChainFilter::Javascript));
    assert!(driver
        .skipped_slots()
        .contains(&(Phase::PreRender, ChainFilter::ImageRewrite)));
    assert!(driver
        .skipped_slots()
        .contains(&(Phase::PostRender, ChainFilter::MetaTag)));
    assert!(!driver
        .skipped_slots()
        .iter()
        .any(|(_, slot)| *slot == ChainFilter::HtmlWriter));
    let input = b"<div><!--c-->x  y</div>";
    driver.start_parse(b"http://test.com/core.html", false);
    driver.write(input);
    driver.finish();
    assert_eq!(sink.bytes(), b"<head/><div><!--c-->x  y</div>".as_slice());
}

#[test]
fn size_limit_propagates() {
    let mut options = RewriteOptions::new();
    options
        .set_option("MaxHtmlParseBytes", "10", Scope::ProcessStrict)
        .unwrap();
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    assert_eq!(driver.plan().size_limit, Some(10));
    driver.start_parse(b"http://test.com/limit.html", false);
    driver.write(b"<html><body>\n<div>hello</div></body></html>\n");
    driver.finish();
    assert!(driver.size_limit_exceeded());
    assert_eq!(sink.bytes(), b"<html></html>");
}

#[test]
fn writer_disabled_means_no_output() {
    let mut options = RewriteOptions::new();
    options
        .filters_mut()
        .disabled_set_mut()
        .insert(Filter::HtmlWriterFilter);
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    assert!(!driver.plan().post.contains(&ChainFilter::HtmlWriter));
    driver.start_parse(b"http://test.com/nowriter.html", false);
    driver.write(b"<div>x</div>");
    driver.finish();
    assert!(sink.bytes().is_empty());
}

/// Records event visits per phase (`Rec`).
#[derive(Clone)]
struct Rec {
    tag: &'static str,
    log: Rc<RefCell<Vec<String>>>,
}

impl Rec {
    fn new(tag: &'static str, log: &Rc<RefCell<Vec<String>>>) -> Self {
        Rec {
            tag,
            log: log.clone(),
        }
    }

    fn push(&self, what: &str) {
        self.log.borrow_mut().push(format!("{}:{what}", self.tag));
    }
}

impl HtmlFilter for Rec {
    fn name(&self) -> &'static str {
        "Rec"
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let name = String::from_utf8_lossy(&dom.element_name(id)).into_owned();
        self.push(&format!("start({name})"));
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let name = String::from_utf8_lossy(&dom.element_name(id)).into_owned();
        self.push(&format!("end({name})"));
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let contents = String::from_utf8_lossy(&dom.leaf_contents(id)).into_owned();
        self.push(&format!("chars({contents})"));
    }

    fn render_done(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.push("done");
    }
}

fn flush_options() -> RewriteOptions {
    let mut options = RewriteOptions::new();
    options
        .set_option("FlushHtml", "true", ps_config::scope::Scope::ProcessStrict)
        .unwrap();
    options
}

fn css_link(name: &[u8]) -> Vec<u8> {
    [
        b"<link rel='stylesheet' href='".as_slice(),
        name,
        b"' type='text/css'/>",
    ]
    .concat()
}

#[test]
fn flush_html_no_extra_flushes() {
    // Ports NoExtraFlushes: score 12 stays quiet until finish.
    let options = flush_options();
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    let mut input = css_link(b"a.css");
    input.extend_from_slice(b"<img src='b.png'/>");
    driver.start_parse(b"http://example.com", false);
    driver.write(&input);
    assert!(sink.bytes().is_empty());
    driver.finish();
    assert_eq!(sink.bytes(), input);
}

#[test]
fn flush_html_induces_flushes() {
    // Ports InduceFlushes: 7 stylesheets + 1 script hit 80 mid-write.
    let options = flush_options();
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    let mut input = Vec::new();
    for _ in 0..7 {
        input.extend_from_slice(&css_link(b"a.css"));
    }
    input.extend_from_slice(b"<script type=text/javascript src='b.js'></script>");
    driver.start_parse(b"http://example.com", false);
    driver.write(&input);
    assert_eq!(sink.bytes(), input);
    driver.finish();
    assert_eq!(sink.bytes(), input);
}

#[test]
fn flush_html_not_enough() {
    // Ports NotEnoughToInduceFlushes: 72 stays quiet until finish.
    let options = flush_options();
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    let mut input = Vec::new();
    for _ in 0..7 {
        input.extend_from_slice(&css_link(b"a.css"));
    }
    input.extend_from_slice(b"<img src='b.png'/>");
    driver.start_parse(b"http://example.com", false);
    driver.write(&input);
    assert!(sink.bytes().is_empty());
    driver.finish();
    assert_eq!(sink.bytes(), input);
}

#[test]
fn amp_verdict_through_driver() {
    assert_eq!(
        driver_amp_verdict(b"<html amp><body>x</body></html>"),
        Some(true)
    );
    assert_eq!(
        driver_amp_verdict(b"<html><body>x</body></html>"),
        Some(false)
    );
}

fn driver_amp_verdict(html: &[u8]) -> Option<bool> {
    let options = RewriteOptions::new();
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    assert_eq!(driver.is_amp_document(), None);
    driver.start_parse(b"http://test.com/amp.html", false);
    driver.write(html);
    driver.finish();
    driver.is_amp_document()
}

#[test]
fn driver_bound_css_inlines_small_link() {
    use ps_cache::cache::MemoryCache;
    use ps_html::server_context::{ResourceDriver, ServerContext};
    use ps_http::mock::MockUrlFetcher;
    use ps_http::response::{HttpOptions, ResponseHeaders};
    use std::sync::Arc;

    let http_backend = Arc::new(MemoryCache::new());
    let metadata_backend = Arc::new(MemoryCache::new());
    let fetcher = Rc::new(MockUrlFetcher::new());
    let server = Rc::new(ServerContext::new_with_metadata(
        http_backend,
        metadata_backend,
        fetcher.clone(),
        1_700_000_000_000,
    ));
    let mut css_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    css_headers.set_status_code(200);
    css_headers
        .headers_mut()
        .add("Date", "Sun, 06 Nov 1994 08:49:37 GMT");
    css_headers.headers_mut().add("Content-Type", "text/css");
    css_headers
        .headers_mut()
        .add("Cache-Control", "max-age=300");
    css_headers.compute_caching();
    fetcher.set_response(
        "http://test.com/a.css",
        css_headers,
        b".a { color: red; }\n",
    );
    // Live passes one CoreFilters-leveled options object to both
    // the plan and the resource driver (no explicit enables).
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .set_option(
            "RewriteLevel",
            "CoreFilters",
            ps_config::scope::Scope::Directory,
        )
        .unwrap();
    let resource_driver = Rc::new(ResourceDriver::new(
        server,
        options.clone(),
        "http://test.com/index.html",
    ));
    let sink = SharedSink::default();
    let live_ctx = ps_html::chain::ChainContext::new(true, false);
    let mut driver = Driver::new(
        &options.borrow(),
        &live_ctx,
        sink.clone(),
        Some(resource_driver),
    );
    driver.start_parse(b"http://test.com/index.html", false);
    driver.write(
        b"<html><head><link rel=\"stylesheet\" href=\"a.css\"></head><body>hi</body></html>",
    );
    driver.finish();
    // Small stylesheets inline (verbatim C++); larger ones take
    // `.pagespeed.cf.` URLs.
    let out = String::from_utf8(sink.bytes()).unwrap();
    assert_eq!(
        "<html><head><style>.a{color:red}</style></head><body>hi</body></html>",
        out
    );
}

#[test]
fn phases_run_early_pre_render_post() {
    // FlushAsync order: early-pre, pre, render-done for both, post chain.
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut parser = Parser::new();
    parser.add_early_pre_render_filter(Box::new(Rec::new("early", &log)));
    parser.add_pre_render_filter(Box::new(Rec::new("pre", &log)));
    parser.add_filter(Box::new(Rec::new("post", &log)));
    parser.start_parse(b"http://test.com/phases.html", false);
    parser.write(b"<div>x</div>");
    parser.finish();
    assert_eq!(
        log.borrow().as_slice(),
        [
            "early:start(div)",
            "early:chars(x)",
            "early:end(div)",
            "pre:start(div)",
            "pre:chars(x)",
            "pre:end(div)",
            "early:done",
            "pre:done",
            "post:start(div)",
            "post:chars(x)",
            "post:end(div)",
        ]
    );
}

/// Builds a resource driver over scratch caches (the live path hands
/// both the plan options and the driver to `Driver::new`).
fn resource_driver_for(
    options: Rc<RefCell<RewriteOptions>>,
    base_url: &str,
) -> Rc<ps_html::server_context::ResourceDriver> {
    use ps_cache::cache::MemoryCache;
    use ps_html::server_context::ServerContext;
    use ps_http::mock::MockUrlFetcher;
    use std::sync::Arc;

    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        Rc::new(MockUrlFetcher::new()),
        1_700_000_000_000,
    ));
    Rc::new(ps_html::server_context::ResourceDriver::new(
        server, options, base_url,
    ))
}

#[test]
fn driver_binds_meta_tag_to_shared_headers() {
    use ps_http::response::{HttpOptions, ResponseHeaders};

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::ConvertMetaTags);
    let resource_driver = resource_driver_for(options.clone(), "http://test.com/meta.html");
    let mut staged = ResponseHeaders::new(HttpOptions::default_for_tests());
    staged.headers_mut().replace("Content-Type", "text/html");
    resource_driver.set_response_headers(staged);
    let sink = SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ctx(),
        sink.clone(),
        Some(resource_driver.clone()),
    );
    assert!(
        !driver
            .skipped_slots()
            .contains(&(Phase::PostRender, ChainFilter::MetaTag)),
        "MetaTag must bind, skipped: {:?}",
        driver.skipped_slots()
    );
    let input = b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html;  charset=UTF-8\"></head><body></body></html>";
    driver.start_parse(b"http://test.com/meta.html", false);
    driver.write(input);
    driver.finish();
    assert_eq!(sink.bytes(), input);
    // The filter mutated the driver's live headers through the share.
    let values = resource_driver
        .response_headers()
        .headers()
        .lookup("Content-Type")
        .unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html; charset=utf-8");
}

#[test]
fn driver_binds_support_noscript_with_driver_url() {
    // A script-requiring filter plus a defer-capable desktop UA
    // inserts the redirect; the URL is the driver's, with the
    // `?PageSpeed=noscript` param (C++ `DetermineEnabled`).
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::DeferJavascript);
    let resource_driver = resource_driver_for(options.clone(), "http://test.com/sn.html");
    resource_driver.set_user_agent(concat!(
        "Mozilla/5.0 (Windows; U; Windows NT 5.1; en-US) ",
        "AppleWebKit/525.13 (KHTML, like Gecko) Chrome/0.A.B.C Safari/525.13"
    ));
    let sink = SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ctx(),
        sink.clone(),
        Some(resource_driver),
    );
    assert!(
        !driver
            .skipped_slots()
            .contains(&(Phase::PostRender, ChainFilter::SupportNoscript)),
        "SupportNoscript must bind, skipped: {:?}",
        driver.skipped_slots()
    );
    let input = b"<head></head><body><p>x</p></body>";
    driver.start_parse(b"http://test.com/sn.html", false);
    driver.write(input);
    driver.finish();
    let url = b"http://test.com/sn.html?PageSpeed=noscript";
    let mut expected =
        b"<head></head><body><noscript><meta HTTP-EQUIV=\"refresh\" content=\"0;url='".to_vec();
    expected.extend_from_slice(url);
    expected.extend_from_slice(b"'\" /><style><!--table,div,span,font,p{display:none} --></style><div style=\"display:block\">Please click <a href=\"");
    expected.extend_from_slice(url);
    expected.extend_from_slice(b"\">here</a> if you are not redirected within a few seconds.</div></noscript><p>x</p></body>");
    assert_eq!(sink.bytes(), expected);
}

/// Reads a statistics counter through the resource driver.
fn stat(
    resource_driver: &Rc<ps_html::server_context::ResourceDriver>,
    name: &str,
) -> i64 {
    resource_driver
        .server_context()
        .statistics()
        .get_variable(name)
        .unwrap_or_else(|| panic!("missing stat {name}"))
        .get()
}

#[test]
fn show_ads_async_runs_through_driver() {
    use ps_html::filters::make_show_ads_async::{API_CALLS_REPLACED, SNIPPETS_CONVERTED};

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::MakeShowAdsAsync);
    let resource_driver = resource_driver_for(options.clone(), "http://test.com/ads.html");
    let sink = SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ctx(),
        sink.clone(),
        Some(resource_driver.clone()),
    );
    assert!(
        !driver
            .skipped_slots()
            .contains(&(Phase::PreRender, ChainFilter::MakeShowAdsAsync)),
        "MakeShowAdsAsync must bind, skipped: {:?}",
        driver.skipped_slots()
    );
    driver.start_parse(b"http://test.com/ads.html", false);
    driver.write(b"<head><title>t</title></head><body><script type=\"text/javascript\"> google_ad_client = \"pub\"; google_ad_width = 728; google_ad_height = 90; </script><script type=\"text/javascript\" src=\"http://pagead2.googlesyndication.com/pagead/show_ads.js\"> </script></body>");
    driver.finish();
    let output = String::from_utf8_lossy(&sink.bytes()).into_owned();
    assert!(output.contains("adsbygoogle"), "{output}");
    assert_eq!(stat(&resource_driver, SNIPPETS_CONVERTED), 1);
    assert_eq!(stat(&resource_driver, API_CALLS_REPLACED), 1);
}

#[test]
fn show_ads_async_runs_without_resource_driver() {
    // Driver-independent filters bind (but count locally) with no driver.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::MakeShowAdsAsync]);
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone(), None);
    assert!(
        !driver
            .skipped_slots()
            .contains(&(Phase::PreRender, ChainFilter::MakeShowAdsAsync)),
        "MakeShowAdsAsync must bind, skipped: {:?}",
        driver.skipped_slots()
    );
    driver.start_parse(b"http://test.com/ads.html", false);
    driver.write(b"<head><title>t</title></head><body><script type=\"text/javascript\"> google_ad_client = \"pub\"; google_ad_width = 728; google_ad_height = 90; </script><script type=\"text/javascript\" src=\"http://pagead2.googlesyndication.com/pagead/show_ads.js\"> </script></body>");
    driver.finish();
    let output = String::from_utf8_lossy(&sink.bytes()).into_owned();
    assert!(output.contains("adsbygoogle"), "{output}");
}

#[test]
fn google_analytics_runs_through_driver() {
    use ps_html::filters::google_analytics::{PAGE_LOAD_COUNT, REWRITTEN_COUNT};

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::MakeGoogleAnalyticsAsync);
    let resource_driver = resource_driver_for(options.clone(), "http://test.com/ga.html");
    let sink = SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ctx(),
        sink.clone(),
        Some(resource_driver.clone()),
    );
    assert!(
        !driver
            .skipped_slots()
            .contains(&(Phase::PreRender, ChainFilter::GoogleAnalytics)),
        "GoogleAnalytics must bind, skipped: {:?}",
        driver.skipped_slots()
    );
    driver.start_parse(b"http://test.com/ga.html", false);
    let mut input =
        b"<head><title>t</title></head><body><script src=\"http://www.google-analytics.com/ga.js\" type=\"text/javascript\">\n</script>\n".to_vec();
    input.extend_from_slice(b"<script type=\"text/javascript\">\ntry {\nvar pageTracker = _gat._getTracker(\"UA-XXXXX-X\");\npageTracker._trackPageview();} catch(err) {}</script></body>");
    driver.write(&input);
    driver.finish();
    let output = String::from_utf8_lossy(&sink.bytes()).into_owned();
    assert!(output.contains("_modpagespeed_getRewriteTracker"), "{output}");
    assert_eq!(stat(&resource_driver, PAGE_LOAD_COUNT), 1);
    assert_eq!(stat(&resource_driver, REWRITTEN_COUNT), 1);
}

#[test]
fn url_left_trim_runs_through_driver() {
    use ps_html::filters::url_left_trim::URL_TRIMS;

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::LeftTrimUrls);
    let resource_driver = resource_driver_for(options.clone(), "http://test.com/trim.html");
    let sink = SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ctx(),
        sink.clone(),
        Some(resource_driver.clone()),
    );
    assert!(
        !driver
            .skipped_slots()
            .contains(&(Phase::PostRender, ChainFilter::UrlLeftTrim)),
        "UrlLeftTrim must bind, skipped: {:?}",
        driver.skipped_slots()
    );
    driver.start_parse(b"http://test.com/trim.html", false);
    driver.write(b"<head><base href='http://foo.bar/baz/'/></head><body><img src='http://foo.bar/baz/nav.jpg'/></body>");
    driver.finish();
    let output = String::from_utf8_lossy(&sink.bytes()).into_owned();
    assert!(output.contains("src='nav.jpg'"), "{output}");
    assert_eq!(stat(&resource_driver, URL_TRIMS), 1);
}

#[test]
fn max_url_size_blocks_base_adoption_through_driver() {
    use ps_html::filters::url_left_trim::URL_TRIMS;

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .set_option("MaxUrlSize", "10", Scope::Server)
        .unwrap();
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::LeftTrimUrls);
    let resource_driver = resource_driver_for(options.clone(), "http://test.com/max.html");
    let sink = SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ctx(),
        sink.clone(),
        Some(resource_driver.clone()),
    );
    driver.start_parse(b"http://test.com/max.html", false);
    // The 24-char base href exceeds the cap, so the base stays the
    // document URL and the foreign-origin image keeps its absolute URL.
    driver.write(b"<head><base href='http://example.com/dir/'/></head><body><img src='http://foo.bar/baz/nav.jpg'/></body>");
    driver.finish();
    let output = String::from_utf8_lossy(&sink.bytes()).into_owned();
    assert!(output.contains("src='http://foo.bar/baz/nav.jpg'"), "{output}");
    assert_eq!(stat(&resource_driver, URL_TRIMS), 0);
}

#[test]
fn proxy_suffix_threads_lawyer_through_driver() {
    use ps_html::filters::url_left_trim::URL_TRIMS;

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .parse_and_set_1("ProxySuffix", ".sfx", Scope::Server)
        .unwrap();
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::LeftTrimUrls);
    let resource_driver =
        resource_driver_for(options.clone(), "http://example.com.sfx/page.html");
    let sink = SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ctx(),
        sink.clone(),
        Some(resource_driver.clone()),
    );
    driver.start_parse(b"http://example.com.sfx/page.html", false);
    // The lawyer suffixes the adopted base to the proxy host, so the
    // origin-host image no longer shares the base origin: no trim.
    driver.write(b"<head><base href='http://example.com/dir/'/></head><body><img src='http://example.com/dir/pic.png'/></body>");
    driver.finish();
    let output = String::from_utf8_lossy(&sink.bytes()).into_owned();
    assert!(
        output.contains("src='http://example.com/dir/pic.png'"),
        "{output}"
    );
    assert_eq!(stat(&resource_driver, URL_TRIMS), 0);
}

#[test]
fn domain_rewrite_runs_through_driver() {
    use ps_html::filters::domain_rewrite::DOMAIN_REWRITES;

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .parse_and_set_2("MapRewriteDomain", "to.com", "from.com")
        .unwrap();
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::RewriteDomains);
    let resource_driver = resource_driver_for(options.clone(), "http://test.com/rw.html");
    let sink = SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ctx(),
        sink.clone(),
        Some(resource_driver.clone()),
    );
    assert!(
        !driver
            .skipped_slots()
            .contains(&(Phase::PostRender, ChainFilter::DomainRewrite)),
        "DomainRewrite must bind, skipped: {:?}",
        driver.skipped_slots()
    );
    driver.start_parse(b"http://test.com/rw.html", false);
    driver.write(b"<head></head><body><img src='http://from.com/a.png'/></body>");
    driver.finish();
    let output = String::from_utf8_lossy(&sink.bytes()).into_owned();
    assert!(output.contains("http://to.com/a.png"), "{output}");
    assert_eq!(stat(&resource_driver, DOMAIN_REWRITES), 1);
}

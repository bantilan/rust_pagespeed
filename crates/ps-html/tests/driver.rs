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
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
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
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
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
    // even though those filters exist.
    let mut options = RewriteOptions::new();
    options.filters_mut().set_level(RewriteLevel::CoreFilters);
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
    assert!(driver
        .skipped_slots()
        .contains(&(Phase::PreRender, ChainFilter::CssFilter)));
    assert!(driver
        .skipped_slots()
        .contains(&(Phase::PreRender, ChainFilter::Javascript)));
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
    assert_eq!(sink.bytes(), input);
}

#[test]
fn size_limit_propagates() {
    let mut options = RewriteOptions::new();
    options
        .set_option("MaxHtmlParseBytes", "10", Scope::ProcessStrict)
        .unwrap();
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
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
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
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
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
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
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
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
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
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
    let mut driver = Driver::new(&options, &ctx(), sink.clone());
    assert_eq!(driver.is_amp_document(), None);
    driver.start_parse(b"http://test.com/amp.html", false);
    driver.write(html);
    driver.finish();
    driver.is_amp_document()
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

//! Resource scanner tests: translated table plus hand-ported specials.
#[path = "goldens/generated_scanner.rs"]
mod generated;

#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;

use generated::CASES;
use harness::{run_with_filters, wrap_body};
use ps_config::options::{Category, RewriteOptions};
use ps_html::element::NodeId;
use ps_html::filter::HtmlFilter;
use ps_html::lexer::Lexer;
use ps_html::parser::Dom;
use ps_html::resource_tag_scanner;

/// Collects scanned (url, category) pairs per element (`ResourceCollector`).
#[derive(Debug, Clone, Default)]
struct Collector {
    pairs: Rc<RefCell<Vec<(Vec<u8>, Category)>>>,
    overrides: Vec<(String, String, Category)>,
}

impl HtmlFilter for Collector {
    fn name(&self) -> &'static str {
        "ResourceCollector"
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        for (index, category) in resource_tag_scanner::scan_element(dom, id, &self.overrides) {
            let (decoded, _) = dom.decoded_attribute(id, index);
            self.pairs
                .borrow_mut()
                .push((decoded.unwrap_or_default(), category));
        }
    }
}

fn category_named(name: &str) -> Category {
    match name {
        "Script" => Category::Script,
        "Image" => Category::Image,
        "Stylesheet" => Category::Stylesheet,
        "OtherResource" => Category::OtherResource,
        "Hyperlink" => Category::Hyperlink,
        "Prefetch" => Category::Prefetch,
        other => panic!("unknown category {other}"),
    }
}

fn collect(
    fragment: &[u8],
    overrides: Vec<(String, String, Category)>,
) -> Vec<(Vec<u8>, Category)> {
    let wrapped = wrap_body(fragment);
    let collector = Collector {
        pairs: Rc::new(RefCell::new(Vec::new())),
        overrides,
    };
    let probe = collector.clone();
    let output = run_with_filters(&wrapped, |parser| {
        parser.add_filter(Box::new(probe));
    });
    assert_eq!(output, wrapped);
    let pairs = collector.pairs.borrow().to_vec();
    pairs
}

#[test]
fn translated_scanner_cases() {
    for (id, html, expected) in CASES {
        let pairs = collect(html, Vec::new());
        let expected: Vec<(Vec<u8>, Category)> = expected
            .iter()
            .map(|(url, category)| (url.to_vec(), category_named(category)))
            .collect();
        assert_eq!(pairs, expected, "case {id}");
    }
}

#[test]
fn many_attributes_limit() {
    // 256 attributes scan; the 257th opts the element out entirely.
    let mut at_limit = b"<img".to_vec();
    for _ in 0..256 {
        at_limit.extend_from_slice(b" src=a.jpg");
    }
    at_limit.extend_from_slice(b">");
    let pairs = collect(&at_limit, Vec::new());
    assert_eq!(pairs.len(), 256);
    assert!(pairs
        .iter()
        .all(|(_, category)| *category == Category::Image));
    let mut past_limit = b"<img".to_vec();
    for _ in 0..257 {
        past_limit.extend_from_slice(b" src=a.jpg");
    }
    past_limit.extend_from_slice(b">");
    assert!(collect(&past_limit, Vec::new()).is_empty());
}

fn overrides_for(
    element: &str,
    attribute: &str,
    category: &str,
) -> Vec<(String, String, Category)> {
    let mut options = RewriteOptions::new();
    options
        .parse_and_set_3("UrlValuedAttribute", element, attribute, category)
        .unwrap();
    options.url_valued_attributes().to_vec()
}

#[test]
fn url_valued_attribute() {
    let overrides = overrides_for("img", "data-src", "Image");
    let pairs = collect(
        b"<img src=find-image data-src=img2 longdesc=do-find-longdesc>",
        overrides.clone(),
    );
    assert_eq!(
        pairs,
        vec![
            (b"find-image".to_vec(), Category::Image),
            (b"img2".to_vec(), Category::Image),
            (b"do-find-longdesc".to_vec(), Category::Hyperlink),
        ]
    );
    let pairs = collect(b"<img data-src=img2 longdesc=do-find-longdesc>", overrides);
    assert_eq!(
        pairs,
        vec![
            (b"img2".to_vec(), Category::Image),
            (b"do-find-longdesc".to_vec(), Category::Hyperlink),
        ]
    );
}

#[test]
fn url_valued_attribute_override() {
    let overrides = overrides_for("a", "href", "Image");
    let pairs = collect(b"<a href=find-image>", overrides);
    assert_eq!(pairs, vec![(b"find-image".to_vec(), Category::Image)]);
}

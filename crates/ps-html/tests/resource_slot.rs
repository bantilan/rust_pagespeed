//! Slot tests ported from `resource_slot_test.cc`.
//!
//! NOTE: the C++ fixture hand-builds elements with explicit line
//! numbers (`AddElement`/`CloseElement` line args); here the elements
//! parse normally and the second link's lines are pinned to 2-3 the
//! same way. Slot operations run from a probe filter's end-document
//! hook so the trailing writer emits their effect, mirroring the
//! C++ `ApplyFilter(writer)` re-render.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use harness::SharedSink;
use ps_config::options::RewriteOptions;
use ps_html::element::NodeId;
use ps_html::filter::HtmlFilter;
use ps_html::lexer::Lexer;
use ps_html::name::Keyword;
use ps_html::parser::{Dom, Parser};
use ps_html::resource::{DataUrlInputResource, ResourceHandle};
use ps_html::resource_slot::{compare_html_slots, FetchResourceSlot, HtmlResourceSlot};
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::url_input_resource::make_url_input_resource;
use ps_html::writer::WriterFilter;
use ps_http::google_url::GoogleUrl;

const DOC_URL: &str = "http://html.parse.test/event_list_test.html";
const DRIVER_ID: &str = "resource_slot_test";
const INPUT: &str = "<link href=\"v1\" src=\"v2\"/><link href=\"v3\" src=\"v4\"/>";

/// Minimal driver for building URL resources (mirrors the C++
/// `rewrite_driver()` fixture half).
fn test_driver() -> Rc<ResourceDriver> {
    let backend: std::sync::Arc<dyn ps_cache::cache::Cache> =
        std::sync::Arc::new(ps_cache::cache::MemoryCache::new());
    let fetcher = Rc::new(ps_http::mock::MockUrlFetcher::new());
    let server = Rc::new(ServerContext::new(backend, fetcher, 1_700_000_000_000));
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    Rc::new(ResourceDriver::new(server, options, DOC_URL))
}

/// Slot operation for the probe filter.
#[derive(Clone, Copy)]
enum SlotOp {
    /// Build slots and check accessors/locations only.
    Inspect,
    /// Rewrite slot 0's URL, then render.
    Update,
    /// Delete slot 0's element, then render.
    Delete,
}

/// Collects links, then runs one slot operation at end of document.
struct Probe {
    links: Vec<NodeId>,
    op: SlotOp,
    options: RewriteOptions,
    slots_out: Option<Rc<RefCell<Vec<HtmlResourceSlot>>>>,
}

impl Probe {
    fn make_slot(&self, dom: &mut Dom, element: usize, attribute: usize) -> HtmlResourceSlot {
        HtmlResourceSlot::new(None, dom, self.links[element], attribute, DRIVER_ID)
    }
}

impl HtmlFilter for Probe {
    fn name(&self) -> &'static str {
        "SlotProbe"
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Link {
            self.links.push(id);
        }
    }

    fn end_document(&mut self, dom: &mut Dom, _lexer: &Lexer) {
        assert_eq!(self.links.len(), 2);
        // Mirror the C++ `AddElement`/`CloseElement` line arguments.
        dom.set_element_begin_line(self.links[1], 2);
        dom.set_element_end_line(self.links[1], 3);
        let base = GoogleUrl::new(DOC_URL);
        match self.op {
            SlotOp::Inspect => {
                let mut slots = [
                    self.make_slot(dom, 0, 0),
                    self.make_slot(dom, 0, 1),
                    self.make_slot(dom, 1, 0),
                    self.make_slot(dom, 1, 1),
                ];
                assert_eq!(slots[0].element(), Some(self.links[0]));
                assert_eq!(slots[0].attribute(), 0);
                assert_eq!(slots[1].element(), Some(self.links[0]));
                assert_eq!(slots[1].attribute(), 1);
                assert_eq!(slots[2].element(), Some(self.links[1]));
                assert_eq!(slots[2].attribute(), 0);
                assert_eq!(slots[3].element(), Some(self.links[1]));
                assert_eq!(slots[3].attribute(), 1);
                assert!(!slots[0].base().was_optimized());
                slots[0].base_mut().set_was_optimized(true);
                assert!(slots[0].base().was_optimized());
                assert_eq!(slots[0].location_string(), "resource_slot_test:1");
                assert_eq!(slots[2].location_string(), "resource_slot_test:2-3");

                let data: ResourceHandle =
                    Rc::new(DataUrlInputResource::make("data:text/plain,Huh").unwrap());
                let fetch = FetchResourceSlot::new(Some(data));
                assert_eq!(fetch.location_string(), "Fetch of data:text/plain,Huh");

                if let Some(out) = &self.slots_out {
                    out.borrow_mut().extend(slots);
                }
            }
            SlotOp::Update => {
                let updated: ResourceHandle = Rc::new(make_url_input_resource(
                    test_driver(),
                    None,
                    "http://html.parse.test/new_css.css",
                    true,
                ));
                let mut slot = self.make_slot(dom, 0, 0);
                slot.base_mut().set_resource(updated);
                slot.render(dom, &base, &self.options);
            }
            SlotOp::Delete => {
                let mut slot = self.make_slot(dom, 0, 0);
                assert!(!slot.base().should_delete_element());
                assert!(!slot.base().disable_further_processing());
                slot.base_mut().request_delete_element();
                assert!(slot.base().should_delete_element());
                assert!(slot.base().disable_further_processing());
                slot.render(dom, &base, &self.options);
            }
        }
    }
}

fn run_with_probe(op: SlotOp) -> String {
    run_with_probe_options(op, RewriteOptions::new(), None)
}

fn run_with_probe_options(
    op: SlotOp,
    options: RewriteOptions,
    slots_out: Option<Rc<RefCell<Vec<HtmlResourceSlot>>>>,
) -> String {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(Probe {
        links: Vec::new(),
        op,
        options,
        slots_out,
    }));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(DOC_URL.as_bytes(), false);
    parser.write(INPUT.as_bytes());
    parser.finish();
    String::from_utf8(sink.bytes()).unwrap()
}

#[test]
fn accessors() {
    // Assertions run inside the probe; the document is untouched.
    assert_eq!(run_with_probe(SlotOp::Inspect), INPUT);
}

#[test]
fn comparator() {
    /// Ordered slot wrapper (`HtmlResourceSlotSet` element).
    #[derive(Clone)]
    struct Ordered(Rc<HtmlResourceSlot>);
    impl PartialEq for Ordered {
        fn eq(&self, other: &Self) -> bool {
            compare_html_slots(&self.0, &other.0) == std::cmp::Ordering::Equal
        }
    }
    impl Eq for Ordered {}
    impl PartialOrd for Ordered {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(compare_html_slots(&self.0, &other.0))
        }
    }
    impl Ord for Ordered {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            compare_html_slots(&self.0, &other.0)
        }
    }

    let slots_out: Rc<RefCell<Vec<HtmlResourceSlot>>> = Rc::new(RefCell::new(Vec::new()));
    assert_eq!(
        run_with_probe_options(
            SlotOp::Inspect,
            RewriteOptions::new(),
            Some(slots_out.clone())
        ),
        INPUT
    );
    let slots = slots_out.borrow();
    assert_eq!(slots.len(), 4);
    let mut set = BTreeSet::new();
    for slot in slots.iter() {
        assert!(set.insert(Ordered(Rc::new(slot.clone()))));
    }
    assert_eq!(set.len(), 4);
    // An equivalent slot is rejected; the set is unchanged.
    assert!(!set.insert(Ordered(Rc::new(slots[3].clone()))));
    assert_eq!(set.len(), 4);
}

#[test]
fn render_update() {
    assert_eq!(
        run_with_probe(SlotOp::Update),
        "<link href=\"new_css.css\" src=\"v2\"/><link href=\"v3\" src=\"v4\"/>"
    );
}

#[test]
fn render_delete() {
    assert_eq!(
        run_with_probe(SlotOp::Delete),
        "<link href=\"v3\" src=\"v4\"/>"
    );
}

#[test]
fn context_tracking_rules() {
    use ps_html::cacheable_resource::InputInfo;
    use ps_html::resource_slot::SlotBase;

    let mut base = SlotBase::new(None);
    assert_eq!(base.last_context(), None);
    assert!(base.inputs().is_none());
    base.add_context(7);
    base.add_context(9);
    assert_eq!(base.last_context(), Some(9));
    // First and last attachments may leave, in any order.
    base.detach_context(7);
    assert_eq!(base.last_context(), Some(9));
    base.detach_context(9);
    assert_eq!(base.last_context(), None);
    // Inputs aggregate from empty.
    base.report_input(InputInfo::default());
    base.report_input(InputInfo::default());
    assert_eq!(base.inputs().unwrap().len(), 2);
}

#[test]
#[should_panic(expected = "first or last")]
fn detach_middle_context_panics() {
    use ps_html::resource_slot::SlotBase;

    let mut base = SlotBase::new(None);
    base.add_context(1);
    base.add_context(2);
    base.add_context(3);
    base.detach_context(2);
}

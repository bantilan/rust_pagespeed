//! Reproducible font-discovery callback/allocation profile. No network or live changes.
use ps_cache::cache::MemoryCache;
use ps_config::options::RewriteOptions;
use ps_html::{
    element::NodeId,
    filter::HtmlFilter,
    filters::preload_critical_fonts::PreloadCriticalFontsFilter,
    lexer::Lexer,
    parser::{Dom, Parser},
    server_context::{ResourceDriver, ServerContext},
    writer::WriterFilter,
};
use ps_http::mock::MockUrlFetcher;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
struct Alloc;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Alloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.realloc(p, l, n)
    }
}
#[global_allocator]
static ALLOC: Alloc = Alloc;
#[derive(Default)]
struct Profile {
    walk: u128,
    decide: u128,
    walk_alloc: usize,
    decide_alloc: usize,
}
struct Timed {
    filter: PreloadCriticalFontsFilter,
    profile: Rc<RefCell<Profile>>,
}
impl HtmlFilter for Timed {
    fn name(&self) -> &'static str {
        "ProfileFont"
    }
    fn start_document(&mut self, d: &mut Dom, l: &Lexer) {
        self.filter.start_document(d, l)
    }
    fn start_element(&mut self, d: &mut Dom, l: &Lexer, n: NodeId) {
        let a = ALLOCS.load(Ordering::Relaxed);
        let t = Instant::now();
        self.filter.start_element(d, l, n);
        let ns = t.elapsed().as_nanos();
        let alloc = ALLOCS.load(Ordering::Relaxed) - a;
        let mut p = self.profile.borrow_mut();
        p.walk += ns;
        p.walk_alloc += alloc;
    }
    fn end_element(&mut self, d: &mut Dom, l: &Lexer, n: NodeId) {
        self.filter.end_element(d, l, n)
    }
    fn characters(&mut self, d: &mut Dom, l: &Lexer, n: NodeId) {
        self.filter.characters(d, l, n)
    }
    fn end_document(&mut self, d: &mut Dom, l: &Lexer) {
        let a = ALLOCS.load(Ordering::Relaxed);
        let t = Instant::now();
        self.filter.end_document(d, l);
        let ns = t.elapsed().as_nanos();
        let alloc = ALLOCS.load(Ordering::Relaxed) - a;
        let mut p = self.profile.borrow_mut();
        p.decide += ns;
        p.decide_alloc += alloc;
    }
}
#[derive(Clone)]
struct Sink(Rc<RefCell<Vec<u8>>>);
impl ps_html::writer::ByteSink for Sink {
    fn write_bytes(&mut self, b: &[u8]) -> bool {
        self.0.borrow_mut().extend_from_slice(b);
        true
    }
}
fn server() -> Rc<ServerContext> {
    Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        Arc::new(MockUrlFetcher::new()),
        1_700_000_000_000,
    ))
}
fn render(
    s: &Rc<ServerContext>,
    o: &Rc<RefCell<RewriteOptions>>,
    html: &str,
    enabled: bool,
) -> (u128, usize, Rc<RefCell<Profile>>, Vec<u8>) {
    let count = ALLOCS.load(Ordering::Relaxed);
    let t = Instant::now();
    let p = Rc::new(RefCell::new(Profile::default()));
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let mut parser = Parser::new();
    if enabled {
        let driver = Rc::new(ResourceDriver::new(
            s.clone(),
            o.clone(),
            "https://test.com/page",
        ));
        parser.add_early_pre_render_filter(Box::new(Timed {
            filter: PreloadCriticalFontsFilter::new(driver),
            profile: p.clone(),
        }));
    }
    parser.add_filter(Box::new(WriterFilter::new(Sink(bytes.clone()))));
    parser.start_parse(b"https://test.com/page", false);
    parser.write(html.as_bytes());
    parser.finish();
    let elapsed = t.elapsed().as_nanos();
    let alloc = ALLOCS.load(Ordering::Relaxed) - count;
    let output = bytes.borrow().clone();
    (elapsed, alloc, p, output)
}
fn median(v: &mut [u128]) -> u128 {
    v.sort_unstable();
    v[v.len() / 2]
}
fn main() {
    html_hit();
    let o = Rc::new(RefCell::new(RewriteOptions::new()));
    println!("fixture,phase,total_us,walk_us,decision_us,allocations,walk_allocations,decision_allocations,font_preloads");
    for (fixture, unique) in [("repeated", false), ("unique", true)] {
        let mut html=String::from("<html><head><style>@font-face{font-family:Manrope;src:url(https://test.com/f.woff2)}body{font-family:Manrope}</style></head><body>");
        for i in 0..600 {
            let class = if unique {
                format!("card-{i}")
            } else {
                "card title active".into()
            };
            html.push_str(&format!("<div class=\"{class}\" style=\"font-family:Manrope;font-weight:400;color:red\"><span class=\"title\">text</span></div>"));
        }
        html.push_str("</body></html>");
        for phase in ["off", "cold", "warm"] {
            let mut s = server();
            if phase == "warm" {
                render(&s, &o, &html, true);
            }
            let (mut total, mut walk, mut decide, mut alloc, mut wa, mut da) =
                (vec![], vec![], vec![], vec![], vec![], vec![]);
            let mut fonts = 0;
            for i in 0..350 {
                if phase == "cold" {
                    s = server();
                }
                let (t, a, p, out) = render(&s, &o, &html, phase != "off");
                let text = String::from_utf8(out).unwrap();
                fonts = text.matches("as=\"font\"").count();
                assert_eq!(fonts, usize::from(phase != "off"));
                if i >= 50 {
                    let p = p.borrow();
                    total.push(t);
                    walk.push(p.walk);
                    decide.push(p.decide);
                    alloc.push(a as u128);
                    wa.push(p.walk_alloc as u128);
                    da.push(p.decide_alloc as u128);
                }
            }
            println!(
                "{fixture},{phase},{:.3},{:.3},{:.3},{},{},{},{fonts}",
                median(&mut total) as f64 / 1000.,
                median(&mut walk) as f64 / 1000.,
                median(&mut decide) as f64 / 1000.,
                median(&mut alloc),
                median(&mut wa),
                median(&mut da)
            );
        }
    }
}

#[allow(dead_code)]
fn html_hit() {
    use ps_config::{filter::Filter, scope::Scope};
    use ps_html::html_cache::HtmlCacheSession;
    use ps_http::{
        request::{Method, RequestHeaders},
        response::{HttpOptions, ResponseHeaders},
    };
    let now = 1_700_000_000_000;
    let page = "https://test.com/page";
    for enabled in [false, true] {
        let s = server();
        let mut options = RewriteOptions::new();
        options
            .set_option("HtmlCache", "on", Scope::Server)
            .unwrap();
        // Include publication-hint preparation in the ordinary HIT profile.
        options
            .set_option("HtmlCacheEarlyLookup", "on", Scope::Server)
            .unwrap();
        if enabled {
            options.filters_mut().enable(Filter::PreloadCriticalFonts);
        } else {
            options.filters_mut().disable(Filter::PreloadCriticalFonts);
        }
        let o = Rc::new(RefCell::new(options));
        let mut request = RequestHeaders::new();
        request.set_method(Method::Get);
        request.set_version(1, 1);
        request.headers_mut().add("Host", "test.com");
        request.headers_mut().add("Accept", "text/html");
        let mut origin = ResponseHeaders::new(HttpOptions::default_for_tests());
        origin.set_status_and_reason(200);
        origin.headers_mut().add("Content-Type", "text/html");
        origin
            .headers_mut()
            .add("Cache-Control", "public, max-age=300");
        origin
            .headers_mut()
            .add("Date", &ps_http::time::format_http_date(now));
        origin.compute_caching();
        let purge = ps_cache::purge::PurgeSet::new(25000);
        let driver = || Rc::new(ResourceDriver::new(s.clone(), o.clone(), page));
        let mut first = HtmlCacheSession::new(driver(), page);
        assert!(first.lookup(&origin, &request, &purge, None, now).is_none());
        first.write(b"<html><head></head><body>cached</body></html>");
        first.finish(&request, now);
        let (mut times, mut allocs) = (vec![], vec![]);
        for i in 0..1050 {
            let a = ALLOCS.load(Ordering::Relaxed);
            let t = Instant::now();
            let mut session = HtmlCacheSession::new(driver(), page);
            let hit = session
                .lookup(&origin, &request, &purge, None, now + 1)
                .expect("HTML HIT");
            std::hint::black_box(hit);
            let elapsed = t.elapsed().as_nanos();
            let alloc = ALLOCS.load(Ordering::Relaxed) - a;
            if i >= 50 {
                times.push(elapsed);
                allocs.push(alloc as u128);
            }
        }
        println!(
            "html-cache,hit-{}, {:.3},0,0,{},0,0,0",
            if enabled { "on" } else { "off" },
            median(&mut times) as f64 / 1000.,
            median(&mut allocs)
        );
    }
}

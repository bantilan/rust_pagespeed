//! Image spriting tests ported from `image_combine_filter_test.cc`.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use harness::SharedSink;
use ps_cache::cache::MemoryCache;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_html::filter::HtmlFilter;
use ps_html::filters::css::CssFilter;
use ps_html::filters::image_combine::{ImageCombineFilter, IMAGE_FILE_COUNT_REDUCTION};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::request::RequestHeaders;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

const BIKE_PNG: &str = "BikeCrashIcn.png";
const CUPPA_PNG: &str = "Cuppa.png";
const PUZZLE_JPG: &str = "Puzzle.jpg";
const CHEF_GIF: &str = "IronChef2.gif";

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    user_agent: RefCell<String>,
    request_headers: RefCell<RequestHeaders>,
}

impl Fixture {
    fn new() -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Rc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend,
            metadata_backend,
            fetcher.clone(),
            START_MS,
        );
        server.set_hasher(Rc::new(ps_cache::hash::MockHasher::new()));
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        // We setup the options before filters so the CSS filter is
        // created aware of these.
        options.borrow_mut().enable_filter(Filter::SpriteImages);
        options.borrow_mut().enable_filter(Filter::RewriteCss);
        options.borrow_mut().set_always_rewrite_css(true);
        Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            user_agent: RefCell::new(String::new()),
            request_headers: RefCell::new(RequestHeaders::new()),
        }
    }

    /// Mocks the four test images (`CssImageCombineTest::SetUp`).
    fn mock_images(&self) {
        self.add_file_to_mock_fetcher(
            &format!("{TEST_DOMAIN}{BIKE_PNG}"),
            BIKE_PNG,
            "image/png",
            100,
        );
        self.add_file_to_mock_fetcher(
            &format!("{TEST_DOMAIN}{CUPPA_PNG}"),
            CUPPA_PNG,
            "image/png",
            100,
        );
        self.add_file_to_mock_fetcher(
            &format!("{TEST_DOMAIN}{PUZZLE_JPG}"),
            PUZZLE_JPG,
            "image/jpeg",
            100,
        );
        self.add_file_to_mock_fetcher(
            &format!("{TEST_DOMAIN}{CHEF_GIF}"),
            CHEF_GIF,
            "image/gif",
            100,
        );
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        driver.set_user_agent(&self.user_agent.borrow());
        driver.set_request_headers(self.request_headers.borrow().clone());
        driver
    }

    /// Scripts a fetch with default headers
    /// (`SetResponseWithDefaultHeaders`).
    fn set_response_with_default_headers(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        ttl_sec: i64,
    ) {
        let url = if url.contains("://") {
            url.to_string()
        } else {
            format!("{TEST_DOMAIN}{url}")
        };
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type(content_type),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(self.server.now_ms(), ttl_sec * 1000, "");
        headers.compute_caching();
        self.fetcher.set_response(&url, headers, body);
    }

    /// Scripts a testdata image fetch (`AddFileToMockFetcher`).
    fn add_file_to_mock_fetcher(&self, url: &str, file: &str, content_type: &str, ttl_sec: i64) {
        let url = if url.contains("://") {
            url.to_string()
        } else {
            format!("{TEST_DOMAIN}{url}")
        };
        let body = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data")
                .join(file),
        )
        .unwrap_or_else(|_| panic!("missing image {file}"));
        self.set_response_with_default_headers(&url, content_type, &body, ttl_sec);
    }

    /// Parses through the CSS filter.
    fn parse(&self, url: &str, html: &str) -> String {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    /// Serves a URL: body plus headers on success.
    fn serve_url(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let driver = self.driver_for(url);
        driver.add_filter(Rc::new(CssFilter::new(driver.clone())));
        driver.add_filter(Rc::new(ImageCombineFilter::new(driver.clone())));
        driver.add_filter(Rc::new(
            ps_html::filters::cache_extender::CacheExtenderFilter::new(driver.clone()),
        ));
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(url, ctx);
        let handled = driver.fetch_resource(url, &mut fetch);
        assert_eq!(handled, fetch.done());
        if handled && fetch.success() {
            Some((fetch.body().to_vec(), fetch.response_headers().clone()))
        } else {
            None
        }
    }

    /// Serves through a fresh server sharing the fetcher and options.
    fn serve_from_new_context(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let mut fresh = ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            self.fetcher.clone(),
            START_MS,
        );
        fresh.set_hasher(Rc::new(ps_cache::hash::MockHasher::new()));
        let server = Rc::new(fresh);
        let options = Rc::new(RefCell::new(self.options.borrow().clone()));
        let driver = Rc::new(ResourceDriver::new(server, options, url));
        driver.set_user_agent(&self.user_agent.borrow());
        driver.set_request_headers(self.request_headers.borrow().clone());
        driver.add_filter(Rc::new(CssFilter::new(driver.clone())));
        driver.add_filter(Rc::new(ImageCombineFilter::new(driver.clone())));
        driver.add_filter(Rc::new(
            ps_html::filters::cache_extender::CacheExtenderFilter::new(driver.clone()),
        ));
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(url, ctx);
        let handled = driver.fetch_resource(url, &mut fetch);
        assert_eq!(handled, fetch.done());
        if handled && fetch.success() {
            Some((fetch.body().to_vec(), fetch.response_headers().clone()))
        } else {
            None
        }
    }

    fn stat(&self, name: &str) -> i64 {
        self.server.variable(name).get()
    }

    /// Parses and compares (`ValidateExpected`).
    fn validate_expected(&self, id: &str, before: &str, after: &str) {
        assert_eq!(
            self.parse(&format!("{TEST_DOMAIN}{id}"), before),
            after,
            "id: {id}"
        );
    }

    /// Parses expecting no changes (`ValidateNoChanges`).
    fn validate_no_changes(&self, id: &str, html: &str) {
        self.validate_expected(id, html, html);
    }
}

/// Joins sprite leaves (`MultiUrl`).
fn multi_url(leaves: &[&str]) -> String {
    leaves
        .iter()
        .map(|leaf| ps_cache::namer::encode_url_segment_bytes(leaf.as_bytes()))
        .collect::<Vec<_>>()
        .join("+")
}

/// Encodes a rewritten leaf (`Encode`).
fn encode(prefix: &str, filter_id: &str, hash: &str, leaf: &str, ext: &str) -> String {
    // The image encoder `x`-prefixes, the CSS encoder `A.`-prefixes;
    // cache extension passes through; the combiner takes a
    // pre-joined multipart segment.
    let segment = if filter_id == "ic" {
        format!(
            "x{}",
            ps_cache::namer::encode_url_segment_bytes(leaf.as_bytes())
        )
    } else if filter_id == "cf" {
        format!(
            "A.{}",
            ps_cache::namer::encode_url_segment_bytes(leaf.as_bytes())
        )
    } else {
        leaf.to_string()
    };
    format!("{prefix}{segment}.pagespeed.{filter_id}.{hash}.{ext}")
}

/// Sprites one position case (`TestSpriting`).
fn test_spriting(
    fixture: &Fixture,
    bike_position: &str,
    expected_position: &str,
    should_sprite: bool,
) {
    let sprite = encode("", "is", "0", &multi_url(&[CUPPA_PNG, BIKE_PNG]), "png");
    // The JPEG will not be included in the sprite because we only handle PNGs.
    let html = "<head><style>#div1{background-image:url(%s);background-position:0 0;width:10px;height:10px}#div2{background:transparent url(%s);background-position:%s;width:10px;height:10px}#div3{background-image:url(%s);width:10px;height:10px}</style></head>";
    let before = html
        .replacen("%s", CUPPA_PNG, 1)
        .replacen("%s", BIKE_PNG, 1)
        .replacen("%s", bike_position, 1)
        .replacen("%s", PUZZLE_JPG, 1);
    let after = html
        .replacen("%s", &sprite, 1)
        .replacen("%s", &sprite, 1)
        .replacen("%s", expected_position, 1)
        .replacen("%s", PUZZLE_JPG, 1);

    fixture.validate_expected(
        "sprites_images",
        &before,
        if should_sprite { &after } else { &before },
    );

    // Try it again, this time using the background shorthand with a couple
    // different orderings
    let html2 = "<head><style>#div1{background:0 0 url(%s) no-repeat transparent scroll;width:10px;height:10px}#div2{background:url(%s) %s repeat fixed;width:10px;height:10px}#div3{background-image:url(%s);width:10px;height:10px}</style></head>";

    let before = html2
        .replacen("%s", CUPPA_PNG, 1)
        .replacen("%s", BIKE_PNG, 1)
        .replacen("%s", bike_position, 1)
        .replacen("%s", PUZZLE_JPG, 1);
    let after = html2
        .replacen("%s", &sprite, 1)
        .replacen("%s", &sprite, 1)
        .replacen("%s", expected_position, 1)
        .replacen("%s", PUZZLE_JPG, 1);

    fixture.validate_expected(
        "sprites_images",
        &before,
        if should_sprite { &after } else { &before },
    );
}

#[test]
fn sprites_images() {
    // For each of these, expect the following:
    // If spriting is possible, the first image (Cuppa.png)
    // ends up on top and the second image (BikeCrashIcn.png) ends up on the
    // bottom.
    // Cuppa.png 65px wide by 70px high.
    // BikeCrashIcn.png is 100px wide by 100px high.
    // Therefore if you want to see just BikeCrashIcn.png, you need to
    // align the image 70px above the div (i.e. -70px).
    // All the divs are 10px by 10px (which affects the resulting
    // alignments).
    for (bike_position, expected_position) in [
        ("0 0", "0 -70px"),
        ("left top", "0 -70px"),
        ("top 10px", "10px -70px"),
        // TODO(nforman): Have spriting reject this since the 5px will
        // display part of the image above this one.
        ("-5px 5px", "-5px -65px"),
        // We want pixels 45 to 55 out of the image, therefore align the image
        // 45 pixels to the left of the div.
        ("center top", "-45px -70px"),
        // Same as above, but this time select the middle 10 pixels vertically,
        // as well (45 to 55, but offset by 70 for the image above).
        ("center center", "-45px -115px"),
        // We want the bottom, right corner of the image, i.e. pixels
        // 90 to 100 (both vertically and horizontally), so align the image
        // 90 pixels to the left and 160 pixels (70 from Cuppa.png) above.
        ("right bottom", "-90px -160px"),
        // Here we need the vertical center (45 to 55, plus the 70 offset),
        // and the horizontal right (90 to 100).
        ("center right", "-90px -115px"),
        // This is equivalent to "center right".
        ("right", "-90px -115px"),
        // This is equivalent to "top center".
        ("top", "-45px -70px"),
    ] {
        let fixture = Fixture::new();
        fixture.mock_images();
        test_spriting(&fixture, bike_position, expected_position, true);
    }
}

/// Metadata backend that rewrites every hit to carry an out-of-bounds
/// slot index (`OobSlotIndexCache`).
#[derive(Debug)]
struct OobSlotIndexCache {
    delegate: Arc<dyn ps_cache::cache::Cache>,
    num_corrupted: std::sync::Mutex<i64>,
}

impl OobSlotIndexCache {
    fn new(delegate: Arc<dyn ps_cache::cache::Cache>) -> Self {
        OobSlotIndexCache {
            delegate,
            num_corrupted: std::sync::Mutex::new(0),
        }
    }

    fn num_corrupted(&self) -> i64 {
        *self.num_corrupted.lock().unwrap()
    }
}

impl ps_cache::cache::Cache for OobSlotIndexCache {
    fn get(&self, key: &str) -> ps_cache::cache::Lookup {
        let mut lookup = self.delegate.get(key);
        if lookup.state == ps_cache::cache::KeyState::Available {
            if let Some(bytes) = lookup.value.take() {
                if let Some(mut table) = ps_html::metadata::decode_partitions(&bytes) {
                    let mut touched = false;
                    for partition in &mut table.partition {
                        for input in &mut partition.input {
                            input.index = Some(999_999);
                            touched = true;
                        }
                    }
                    if touched {
                        *self.num_corrupted.lock().unwrap() += 1;
                        lookup.value = Some(ps_html::metadata::encode_partitions(&table));
                    } else {
                        lookup.value = Some(bytes);
                    }
                }
            }
        }
        lookup
    }

    fn put(&self, key: &str, value: &[u8]) {
        self.delegate.put(key, value);
    }

    fn delete(&self, key: &str) {
        self.delegate.delete(key);
    }

    fn name(&self) -> String {
        "OobSlotIndexCache".to_string()
    }
}

#[test]
fn oob_slot_index_metadata_treated_as_miss() {
    let mut fixture = Fixture::new();
    fixture.mock_images();
    let input = format!(
        "<head><style>#div1{{background:url({CUPPA_PNG}) 0 0;width:10px;height:10px}}#div2{{background:url({BIKE_PNG}) 0 -70px;width:10px;height:10px}}</style></head>"
    );

    let sprited_output = fixture.parse(TEST_DOMAIN, &input);
    // Sanity: the two images were combined into a sprite.
    assert!(sprited_output.contains(".pagespeed.is."));

    // Interpose a backend that rewrites every metadata hit to carry a slot
    // index far outside the slot table. The wrapper does not own its
    // delegate.
    let corrupting = Arc::new(OobSlotIndexCache::new(
        fixture.server.metadata_cache().clone(),
    ));
    let corrupting_clone = corrupting.clone();
    Rc::get_mut(&mut fixture.server)
        .expect("unique server")
        .set_metadata_cache(corrupting);

    // The corrupted entry must degrade to a miss and be recomputed, yielding
    // byte-identical output.
    let output = fixture.parse(TEST_DOMAIN, &input);
    assert_eq!(sprited_output, output);
    assert!(corrupting_clone.num_corrupted() >= 1);
}

#[test]
fn unauthorized_domain() {
    let fixture = Fixture::new();
    fixture.mock_images();
    let bike_path = format!("http://unauth.com/{BIKE_PNG}");
    fixture.add_file_to_mock_fetcher(&bike_path, BIKE_PNG, "image/png", 100);
    fixture.options.borrow_mut().enable_filter(Filter::Debug);

    let debug_message = format!(
        "<!--Flattening failed: Cannot rewrite {bike_path} as it is on an unauthorized domain-->"
    );
    let html = "<head><style>#div2{background:transparent url(%s);background-position:0 0;width:10px;height:10px}</style>%s</head>%s";
    let before = html
        .replacen("%s", &bike_path, 1)
        .replacen("%s", "", 1)
        .replacen("%s", "", 1);
    let after = html
        .replacen("%s", &bike_path, 1)
        .replacen("%s", &debug_message, 1)
        .replacen("%s", "", 1);

    fixture.validate_expected("unauthorized_domain", &before, &after);
}

#[test]
fn dont_leak() {
    // Regression test for a leak: we had trouble when a single position was
    // merely "0%".
    let fixture = Fixture::new();
    fixture.mock_images();
    const HTML: &str = "<style>#div2{background:transparent url(Cuppa.png) no-repeat scroll 0%;background-position:0 0;width:10px;height:10px}</style>";

    fixture.validate_no_changes("single_pos", HTML);
}

const HTML_TEMPLATE_3_DIVS: &str = "<head><style>#div1{background:url(%s) 0 0;width:10px;height:10px}#div2{background:url(%s) 0 %s;width:%s;height:10px}#div3{background:url(%s) 0 %s;width:10px;height:10px}</style></head>";

fn fill_3_divs(url1: &str, url2: &str, pos2: &str, width2: &str, url3: &str, pos3: &str) -> String {
    HTML_TEMPLATE_3_DIVS
        .replacen("%s", url1, 1)
        .replacen("%s", url2, 1)
        .replacen("%s", pos2, 1)
        .replacen("%s", width2, 1)
        .replacen("%s", url3, 1)
        .replacen("%s", pos3, 1)
}

#[test]
fn sprites_multiple() {
    let fixture = Fixture::new();
    fixture.mock_images();
    // With the same image present 3 times, there should be no sprite.
    let before = fill_3_divs(BIKE_PNG, BIKE_PNG, "0", "10px", BIKE_PNG, "0");
    fixture.validate_no_changes("no_sprite_3_bikes", &before);

    // With 2 of the same and 1 different, there should be a sprite without
    // duplication.
    let before = fill_3_divs(BIKE_PNG, BIKE_PNG, "0", "10px", CUPPA_PNG, "0");
    let sprite = encode("", "is", "0", &multi_url(&[BIKE_PNG, CUPPA_PNG]), "png");
    let after = fill_3_divs(&sprite, &sprite, "0", "10px", &sprite, "-100px");
    fixture.validate_expected("sprite_2_bikes_1_cuppa", &before, &after);

    // If the second occurrence of the image is unspriteable (e.g. if the div is
    // larger than the image), then don't sprite anything.
    let before = fill_3_divs(BIKE_PNG, BIKE_PNG, "0", "999px", CUPPA_PNG, "0");
    fixture.validate_no_changes("sprite_none_dimensions", &before);
}

// Try the last test from SpritesMultiple with a cold cache.
#[test]
fn no_sprites_multiple() {
    // If the second occurrence of the image is unspriteable (e.g. if the div is
    // larger than the image), then don't sprite anything.
    let fixture = Fixture::new();
    fixture.mock_images();
    let in_text = fill_3_divs(BIKE_PNG, BIKE_PNG, "0", "999px", CUPPA_PNG, "0");
    fixture.validate_no_changes("no_sprite", &in_text);
}

#[test]
fn no_sprite_image_past_resolution_limit() {
    let fixture = Fixture::new();
    // 5000 pixels at 4 bytes per pixel. Cuppa.png is 65x70 (4550 pixels,
    // under the limit); BikeCrashIcn.png is 100x100 (10000 pixels, over it).
    fixture
        .options
        .borrow_mut()
        .set_image_resolution_limit_bytes(20000);
    fixture.mock_images();
    const BEFORE: &str = "<head><style>#div1 { background-image:url('Cuppa.png');width:10px;height:10px}#div2 { background-image:url('BikeCrashIcn.png');width:10px;height:10px}</style></head>";
    // The CSS parser normalizes quoting and whitespace on serialization, so
    // pin the exact serialized form.
    const AFTER: &str = "<head><style>#div1{background-image:url(Cuppa.png);width:10px;height:10px}#div2{background-image:url(BikeCrashIcn.png);width:10px;height:10px}</style></head>";
    fixture.validate_expected("sprite_resolution_limit", BEFORE, AFTER);
}

#[test]
fn no_sprite_when_canvas_exceeds_limit() {
    let fixture = Fixture::new();
    // 50000 pixels at 4 bytes per pixel. Each of Cuppa.png (65x70, 4550
    // pixels), BikeCrashIcn.png (100x100, 10000) and IronChef2.gif (192x256,
    // 49152) is under the limit, but their canvas is 192 x 426 = 81792.
    fixture
        .options
        .borrow_mut()
        .set_image_resolution_limit_bytes(200000);
    fixture.mock_images();
    const BEFORE: &str = "<head><style>#div1 { background-image:url('Cuppa.png');width:10px;height:10px}#div2 { background-image:url('BikeCrashIcn.png');width:10px;height:10px}#div3 { background-image:url('IronChef2.gif');width:10px;height:10px}</style></head>";
    const AFTER: &str = "<head><style>#div1{background-image:url(Cuppa.png);width:10px;height:10px}#div2{background-image:url(BikeCrashIcn.png);width:10px;height:10px}#div3{background-image:url(IronChef2.gif);width:10px;height:10px}</style></head>";
    fixture.validate_expected("sprite_canvas_limit", BEFORE, AFTER);
}

#[test]
fn no_crash_unknown_type() {
    // Make sure we don't crash trying to sprite an image with an unknown mimetype
    let fixture = Fixture::new();
    fixture.mock_images();
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("image/png"),
        "",
        "",
        &mut headers,
    );
    headers
        .headers_mut()
        .replace("Content-Type", "image/x-bewq");
    headers.compute_caching();
    fixture.fetcher.set_response(
        &format!("{TEST_DOMAIN}bar.bewq"),
        headers,
        b"unused payload",
    );
    fixture.set_response_with_default_headers("foo.png", "image/png", b"unused payload", 100);

    let before = "<head><style>#div1 { background-image:url('bar.bewq');width:10px;height:10px}#div2 { background:transparent url('foo.png');width:10px;height:10px}</style></head>";

    fixture.parse(&format!("{TEST_DOMAIN}unknown"), before);
}

#[test]
fn sprites_images_external() {
    // NOTE: the async staging (nothing loaded at first) is
    // unrepresentable in the sync model; this covers the sprited
    // outcome.
    let fixture = Fixture::new();
    fixture.mock_images();

    let before_css = format!(
        " #div1{{background-image:url({CUPPA_PNG});width:10px;height:10px}}#div2{{background:transparent url({BIKE_PNG});width:10px;height:10px}}"
    );
    fixture.set_response_with_default_headers(
        &format!("{TEST_DOMAIN}style.css"),
        "text/css",
        before_css.as_bytes(),
        100,
    );

    let sprite = encode("", "is", "0", &multi_url(&[CUPPA_PNG, BIKE_PNG]), "png");
    let sprite_css = format!(
        "#div1{{background-image:url({sprite});width:10px;height:10px;background-position:0 0}}#div2{{background:transparent url({sprite});width:10px;height:10px;background-position:0 -70px}}"
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}wip"),
            "<link rel='stylesheet' href='style.css'>"
        ),
        "<link rel='stylesheet' href='A.style.css.pagespeed.cf.0.css'>"
    );
    let (actual, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.style.css.pagespeed.cf.0.css"))
        .expect("serve css");
    assert_eq!(sprite_css.as_bytes(), actual.as_slice());
}

#[test]
fn sprites_ok_after_404() {
    // Make sure the handling of a 404 is correct, and doesn't interrupt spriting
    // (nor check fail, as it used to before).
    let fixture = Fixture::new();
    fixture.mock_images();
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}bike2.png"),
        BIKE_PNG,
        "image/png",
        100,
    );
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}bike3.png"),
        BIKE_PNG,
        "image/png",
        100,
    );
    fixture
        .fetcher
        .set_response(&format!("{TEST_DOMAIN}404.png"), not_found_headers(), b"");

    const TEMPLATE: &str = "<head><style>#div1{background:url(%s);width:10px;height:10px}#div2{background:url(%s);width:10px;height:10px}#div3{background:url(%s);width:10px;height:10px}#div4{background:url(%s);width:10px;height:10px}#div5{background:url(%s);width:10px;height:10px}</style></head>";

    let html = TEMPLATE
        .replacen("%s", BIKE_PNG, 1)
        .replacen("%s", CUPPA_PNG, 1)
        .replacen("%s", "404.png", 1)
        .replacen("%s", "bike2.png", 1)
        .replacen("%s", "bike3.png", 1);
    let output = fixture.parse(&format!("{TEST_DOMAIN}sprite_with_404"), &html);
    assert!(output.contains(&encode(
        "",
        "is",
        "0",
        &multi_url(&[BIKE_PNG, CUPPA_PNG, "bike2.png", "bike3.png"]),
        "png"
    )));
}

fn not_found_headers() -> ResponseHeaders {
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    headers.set_first_line(1, 1, 404, "Not Found");
    headers
}

#[test]
fn sprites_multi_site() {
    // Make sure we do something sensible when we're forced to split into multiple
    // partitions due to different host names -- at lest when it doesn't require
    // us to keep track of multiple partitions intelligently.
    const ALT_DOMAIN: &str = "http://images.example.com/";
    let fixture = Fixture::new();
    fixture.mock_images();
    fixture
        .options
        .borrow_mut()
        .add_domain("http://images.example.com/");

    fixture.add_file_to_mock_fetcher(
        &format!("{ALT_DOMAIN}{BIKE_PNG}"),
        BIKE_PNG,
        "image/png",
        100,
    );
    fixture.add_file_to_mock_fetcher(
        &format!("{ALT_DOMAIN}{CUPPA_PNG}"),
        CUPPA_PNG,
        "image/png",
        100,
    );

    const TEMPLATE: &str = "<head><style>#div1{background:url(%s);width:10px;height:10px%s}#div2{background:url(%s);width:10px;height:10px%s}#div3{background:url(%s);width:10px;height:10px%s}#div4{background:url(%s);width:10px;height:10px%s}</style></head>";

    let test_bike = format!("{TEST_DOMAIN}{BIKE_PNG}");
    let alt_bike = format!("{ALT_DOMAIN}{BIKE_PNG}");
    let test_cup = format!("{TEST_DOMAIN}{CUPPA_PNG}");
    let alt_cup = format!("{ALT_DOMAIN}{CUPPA_PNG}");
    let test_sprite = encode(
        TEST_DOMAIN,
        "is",
        "0",
        &multi_url(&[BIKE_PNG, CUPPA_PNG]),
        "png",
    );
    let alt_sprite = encode(
        ALT_DOMAIN,
        "is",
        "0",
        &multi_url(&[BIKE_PNG, CUPPA_PNG]),
        "png",
    );

    let before = TEMPLATE
        .replacen("%s", &test_bike, 1)
        .replacen("%s", "", 1)
        .replacen("%s", &alt_bike, 1)
        .replacen("%s", "", 1)
        .replacen("%s", &test_cup, 1)
        .replacen("%s", "", 1)
        .replacen("%s", &alt_cup, 1)
        .replacen("%s", "", 1);

    let after = TEMPLATE
        .replacen("%s", &test_sprite, 1)
        .replacen("%s", ";background-position:0 0", 1)
        .replacen("%s", &alt_sprite, 1)
        .replacen("%s", ";background-position:0 0", 1)
        .replacen("%s", &test_sprite, 1)
        .replacen("%s", ";background-position:0 -100px", 1)
        .replacen("%s", &alt_sprite, 1)
        .replacen("%s", ";background-position:0 -100px", 1);
    fixture.validate_expected("multi_site", &before, &after);

    // For this test, a partition should get created for the alt_bike image,
    // but it should end up getting canceled and deleted since the partition
    // will have only one image in it.
    let before = TEMPLATE
        .replacen("%s", &alt_bike, 1)
        .replacen("%s", "", 1)
        .replacen("%s", &test_bike, 1)
        .replacen("%s", "", 1)
        .replacen("%s", &test_cup, 1)
        .replacen("%s", "", 1)
        .replacen("%s", &test_bike, 1)
        .replacen("%s", "", 1);
    let after = TEMPLATE
        .replacen("%s", &alt_bike, 1)
        .replacen("%s", "", 1)
        .replacen("%s", &test_sprite, 1)
        .replacen("%s", ";background-position:0 0", 1)
        .replacen("%s", &test_sprite, 1)
        .replacen("%s", ";background-position:0 -100px", 1)
        .replacen("%s", &test_sprite, 1)
        .replacen("%s", ";background-position:0 0", 1);
    fixture.validate_expected("multi_site_one_sprite", &before, &after);
}

#[test]
fn serve_files() {
    let fixture = Fixture::new();
    fixture.mock_images();
    let sprite = encode(
        TEST_DOMAIN,
        "is",
        "0",
        &multi_url(&[CUPPA_PNG, BIKE_PNG]),
        "png",
    );
    let (output, _) = fixture.serve_url(&sprite).expect("serve sprite");
    assert!(!output.is_empty());
    let (output2, _) = fixture
        .serve_from_new_context(&sprite)
        .expect("reserve sprite");
    assert_eq!(output, output2);
}

#[test]
fn combine_many_files() {
    // Prepare an HTML fragment with too many image files to combine,
    // exceeding the char limit.
    const NUM_IMAGES: usize = 100;
    const IMAGES_IN_COMBINATION: usize = 47;
    let fixture = Fixture::new();
    fixture.mock_images();
    let mut html = String::from("<head><style>");
    for i in 0..NUM_IMAGES {
        let url = format!("{TEST_DOMAIN}{i:02}{BIKE_PNG}");
        fixture.add_file_to_mock_fetcher(&url, BIKE_PNG, "image/png", 100);
        html.push_str(&format!(
            "#div{i}{{background:url({url}) 0 0;width:10px;height:10px}}"
        ));
    }
    html.push_str("</style></head>");

    // We expect 3 combinations: 0-46, 47-93, 94-99
    let mut combinations = Vec::new();
    let mut image_index = 0;
    while image_index < NUM_IMAGES {
        let mut combo = Vec::new();
        let end_index = (image_index + IMAGES_IN_COMBINATION).min(NUM_IMAGES);
        while image_index < end_index {
            combo.push(format!("{image_index:02}{BIKE_PNG}"));
            image_index += 1;
        }
        // Original URL is absolute, so rewritten one is as well.
        let refs: Vec<&str> = combo.iter().map(String::as_str).collect();
        combinations.push(encode(TEST_DOMAIN, "is", "0", &multi_url(&refs), "png"));
    }

    image_index = 0;
    let mut combo_index = 0;
    let mut result = String::from("<head><style>");
    while image_index < NUM_IMAGES {
        let offset = (image_index as i64 - (combo_index * IMAGES_IN_COMBINATION) as i64) * -100;
        let offset_str = if offset == 0 {
            // Minification artifact.
            "0".to_string()
        } else {
            format!("{offset}px")
        };

        result.push_str(&format!(
            "#div{image_index}{{background:url({}) 0 {offset_str};width:10px;height:10px}}",
            combinations[combo_index]
        ));
        image_index += 1;
        if image_index % IMAGES_IN_COMBINATION == 0 {
            combo_index += 1;
        }
    }
    result.push_str("</style></head>");

    fixture.validate_expected("manymanyimages", &html, &result);
}

// Once a sprite is full, later images are not offered to it again: each
// full sprite refuses one image, however many images follow it.
#[test]
fn full_sprite_is_not_offered_later_images() {
    // Same layout as CombineManyFiles: 100 images fill two sprites of 47 and
    // leave 6 for a third.
    const NUM_IMAGES: usize = 100;
    let fixture = Fixture::new();
    fixture.mock_images();
    let mut html = String::from("<head><style>");
    for i in 0..NUM_IMAGES {
        let url = format!("{TEST_DOMAIN}{i:02}{BIKE_PNG}");
        fixture.add_file_to_mock_fetcher(&url, BIKE_PNG, "image/png", 100);
        html.push_str(&format!(
            "#div{i}{{background:url({url}) 0 0;width:10px;height:10px}}"
        ));
    }
    html.push_str("</style></head>");

    let output = fixture.parse(&format!("{TEST_DOMAIN}full_sprites"), &html);

    // Three sprites were written.
    let mut sprites = std::collections::HashSet::new();
    let mut pos = 0;
    while let Some(found) = output[pos..].find(".pagespeed.is.") {
        let pos_abs = pos + found;
        let Some(start) = output[..pos_abs].rfind("url(") else {
            panic!("sprite url start");
        };
        sprites.insert(output[start..pos_abs].to_string());
        pos = pos_abs + 1;
    }
    assert_eq!(3, sprites.len());
    // NOTE: the refusal message counts wait for the message handler.
}

#[test]
fn sprites_broken_up() {
    // Make sure we include all spritable images, even if there are
    // un-spritable images in between.
    let fixture = Fixture::new();
    fixture.mock_images();

    let before = fill_3_divs(BIKE_PNG, PUZZLE_JPG, "0", "10px", CUPPA_PNG, "0");

    let sprite = encode("", "is", "0", &multi_url(&[BIKE_PNG, CUPPA_PNG]), "png");

    let after = fill_3_divs(&sprite, PUZZLE_JPG, "0", "10px", &sprite, "-100px");
    fixture.validate_expected("sprite_broken_up", &before, &after);
}

#[test]
fn sprites_gifs_with_pngs() {
    // Make sure we include all spritable images, even if there are
    // un-spritable images in between.
    let fixture = Fixture::new();
    fixture.mock_images();

    let before = fill_3_divs(BIKE_PNG, CHEF_GIF, "0", "10px", CUPPA_PNG, "0");

    let sprite = encode(
        "",
        "is",
        "0",
        &multi_url(&[BIKE_PNG, CHEF_GIF, CUPPA_PNG]),
        "png",
    );

    // The BikePng is 100px tall, the ChefGif is 256px tall, so we
    // expect the Chef to be offset by -100, and the CuppaPng to be
    // offset by -356.
    let after = fill_3_divs(&sprite, &sprite, "-100px", "10px", &sprite, "-356px");
    fixture.validate_expected("sprite_with_gif", &before, &after);
}

#[test]
fn sprite_wrong_mime() {
    // Make sure that a server messing up the content-type doesn't prevent
    // spriting.
    let fixture = Fixture::new();
    fixture.mock_images();
    let wrong_bike = format!("{TEST_DOMAIN}w{BIKE_PNG}");
    let wrong_cuppa = format!("{TEST_DOMAIN}w{CUPPA_PNG}");

    fixture.add_file_to_mock_fetcher(&wrong_bike, BIKE_PNG, "image/jpeg", 100);
    fixture.add_file_to_mock_fetcher(&wrong_cuppa, CUPPA_PNG, "image/jpeg", 100);

    let rel_sprite = encode(
        "",
        "is",
        "0",
        &multi_url(&[&format!("w{BIKE_PNG}"), &format!("w{CUPPA_PNG}"), CUPPA_PNG]),
        "png",
    );
    let abs_sprite = format!("{TEST_DOMAIN}{rel_sprite}");

    let before = fill_3_divs(&wrong_bike, &wrong_cuppa, "0", "10px", CUPPA_PNG, "0");

    // The BikePng is 100px tall, the cuppa is 70px tall, so we
    // expect the cuppa to be offset by -100, and the right-path cuppa to be
    // offset by -170.
    //
    // First 2 original URLs were absolute, so rewritten ones are as well.
    // Last was relative, so it is preserved as relative.
    let after = fill_3_divs(
        &abs_sprite,
        &abs_sprite,
        "-100px",
        "10px",
        &rel_sprite,
        "-170px",
    );
    fixture.validate_expected("wrong_mime", &before, &after);
}

#[test]
fn sprites_and_non_sprites() {
    // We setup the options before the upcall so that the
    // CSS filter is created aware of these.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ExtendCacheImages);
    fixture.mock_images();

    // With the same image present 3 times, there should be no sprite.
    let before = fill_3_divs(BIKE_PNG, BIKE_PNG, "0", "10px", BIKE_PNG, "0");
    let encoded = encode("", "ce", "0", BIKE_PNG, "png");
    let after = fill_3_divs(&encoded, &encoded, "0", "10px", &encoded, "0");
    fixture.validate_expected("no_sprite_3_bikes", &before, &after);

    // With 2 of the same and 1 different, there should be a sprite without
    // duplication.
    let before = fill_3_divs(BIKE_PNG, BIKE_PNG, "0", "10px", CUPPA_PNG, "0");
    let sprite = encode("", "is", "0", &multi_url(&[BIKE_PNG, CUPPA_PNG]), "png");
    let after = fill_3_divs(&sprite, &sprite, "0", "10px", &sprite, "-100px");
    fixture.validate_expected("sprite_2_bikes_1_cuppa", &before, &after);

    // If the second occurrence of the image is unspriteable (e.g. if the div is
    // larger than the image), we shouldn't sprite any of them.
    let before = fill_3_divs(BIKE_PNG, BIKE_PNG, "0", "999px", CUPPA_PNG, "0");
    let cuppa_encoded = encode("", "ce", "0", CUPPA_PNG, "png");
    let after = fill_3_divs(&encoded, &encoded, "0", "999px", &cuppa_encoded, "0");
    fixture.validate_expected("sprite_none_dimmensions", &before, &after);
}

// A test in which base URL inside CSS is different than inside HTML.
// Specifically CSS base URL is inside subdir/.
// This might also be the only test for external stylesheets.
#[test]
fn css_different_base() {
    // Set up resources.
    let fixture = Fixture::new();
    fixture.mock_images();
    fixture.add_file_to_mock_fetcher("subdir/Cuppa.png", CUPPA_PNG, "image/png", 100);
    fixture.add_file_to_mock_fetcher("subdir/BikeCrashIcn.png", BIKE_PNG, "image/png", 100);
    const CSS_BEFORE: &str = ".a {background: 0 0 url(Cuppa.png) no-repeat; width:10px; height:10px}.b {background: 0 0 url(BikeCrashIcn.png) no-repeat; width:10px; height:10px}";
    fixture.set_response_with_default_headers(
        "subdir/foo.css",
        "text/css",
        CSS_BEFORE.as_bytes(),
        100,
    );

    let sprite = encode("", "is", "0", &multi_url(&[CUPPA_PNG, BIKE_PNG]), "png");
    let expected_css_after = format!(
        ".a{{background:0 0 url({sprite}) no-repeat;width:10px;height:10px}}.b{{background:0 -70px url({sprite}) no-repeat;width:10px;height:10px}}"
    );

    let rewritten_url = encode("subdir/", "cf", "0", "foo.css", "css");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}diff_base"),
            &format!("<link rel='stylesheet' href='subdir/foo.css'>")
        ),
        format!("<link rel='stylesheet' href='{rewritten_url}'>")
    );

    let (actual_css_after, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}{rewritten_url}"))
        .expect("serve css");
    assert_eq!(expected_css_after.as_bytes(), actual_css_after.as_slice());
}

#[test]
fn with_flattening() {
    // We setup the options before the upcall so that the
    // CSS filter is created aware of these.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::FlattenCssImports);
    fixture.mock_images();
    fixture.add_file_to_mock_fetcher("dir/Cuppa.png", CUPPA_PNG, "image/png", 100);
    fixture.add_file_to_mock_fetcher("dir/BikeCrashIcn.png", BIKE_PNG, "image/png", 100);

    const LEAF_CSS: &str = ".a {background: 0 0 url(Cuppa.png) no-repeat; width:10px; height:10px}.b {background: 0 0 url(BikeCrashIcn.png) no-repeat; width:10px; height:10px}";
    fixture.set_response_with_default_headers("dir/a.css", "text/css", LEAF_CSS.as_bytes(), 100);

    const BEFORE_HTML: &str = "<style>@import url(dir/a.css);</style>";
    // Note: This is flattened and combined.
    // TODO(sligocki): Perhaps http://test.com/dir/Cuppa.png should be relative
    // given that the original URL in the original stylesheet was relative.
    let sprite = encode(
        &format!("{TEST_DOMAIN}dir/"),
        "is",
        "0",
        &multi_url(&[CUPPA_PNG, BIKE_PNG]),
        "png",
    );
    let after_html = format!(
        "<style>.a{{background:0 0 url({sprite}) no-repeat;width:10px;height:10px}}.b{{background:0 -70px url({sprite}) no-repeat;width:10px;height:10px}}</style>"
    );

    fixture.validate_expected("with_flattening", BEFORE_HTML, &after_html);
}

#[test]
fn no_combine_across_flattening() {
    // We setup the options before the upcall so that the
    // CSS filter is created aware of these.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::FlattenCssImports);
    fixture.mock_images();
    fixture.add_file_to_mock_fetcher("dir/Cuppa.png", CUPPA_PNG, "image/png", 100);

    const LEAF_CSS: &str = ".a {background: 0 0 url(Cuppa.png) no-repeat; width:10px; height:10px}";
    fixture.set_response_with_default_headers("dir/a.css", "text/css", LEAF_CSS.as_bytes(), 100);

    const BEFORE_HTML: &str = "<style>\n@import url(dir/a.css);\n.b {background: 0 0 url(BikeCrashIcn.png) no-repeat; width:10px; height:10px}\n</style>";
    // TODO(sligocki): Any reason not to combine images across flattening
    // boundaries? Currently we don't seem to.
    // TODO(sligocki): Perhaps http://test.com/dir/Cuppa.png should be relative
    // given that the original URL in the original stylesheet was relative.
    const AFTER_HTML: &str = "<style>.a{background:0 0 url(http://test.com/dir/Cuppa.png) no-repeat;width:10px;height:10px}.b{background:0 0 url(BikeCrashIcn.png) no-repeat;width:10px;height:10px}</style>";

    fixture.validate_expected("with_flattening", BEFORE_HTML, AFTER_HTML);
}

/// Serves one fallback case (`ValidateFallbackHeaderSanitizationHelper`).
fn validate_fallback_header_sanitization_helper(
    fixture: &Fixture,
    filter_id: &str,
    origin_content_type: &str,
    expect_load: bool,
) {
    // Mangle the content type to make a url name by removing '/'s.
    let leafable = origin_content_type.replace('/', "-");

    let leaf = format!("leaf-{leafable}");
    let origin_contents = "this isn't a real file";

    let url = format!("{TEST_DOMAIN}{leaf}");
    let mut origin_response_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    origin_response_headers.set_first_line(1, 1, 200, "OK");
    origin_response_headers
        .headers_mut()
        .add("Content-Type", origin_content_type);

    // This is a case where we do need to make some changes for security and we
    // want to be sure we make them even if no-transform is set.
    origin_response_headers.set_date_and_caching(
        fixture.server.now_ms(),
        0, /* ttl */
        "; no-transform",
    );
    origin_response_headers.compute_caching();

    fixture
        .fetcher
        .set_response(&url, origin_response_headers, origin_contents.as_bytes());

    let resource = format!(
        "{TEST_DOMAIN}{}",
        encode("", filter_id, "0", &leaf, "ignored")
    );

    match fixture.serve_url(&resource) {
        Some((response_content, response_headers)) => {
            assert!(expect_load, "loaded {origin_content_type}");
            assert_eq!(origin_contents.as_bytes(), response_content.as_slice());
            assert_eq!(
                Some(origin_content_type.to_string()),
                response_headers.headers().lookup1("Content-Type"),
            );

            assert_eq!(
                Some("nosniff".to_string()),
                response_headers.headers().lookup1("X-Content-Type-Options"),
            );
        }
        None => assert!(!expect_load, "failed {origin_content_type}"),
    }
}

#[test]
fn content_type_validation() {
    let fixture = Fixture::new();
    // These content types will all be preserved.
    for content_type in [
        "text/css",
        "text/javascript",
        "application/javascript",
        "image/jpg",
        "image/jpeg",
        "image/png",
        "image/gif",
        "image/webp",
        "application/pdf",
    ] {
        validate_fallback_header_sanitization_helper(&fixture, "is", content_type, true);
    }

    // All other content types will be stripped.
    for content_type in [
        "text/html",
        "text/plain",
        "text/xml",
        "application/xml",
        "image/svg",
        "image/svg+xml",
        "audio/mp3",
        "video/mp4",
        "",
        "invalid",
    ] {
        validate_fallback_header_sanitization_helper(&fixture, "is", content_type, false);
    }
}

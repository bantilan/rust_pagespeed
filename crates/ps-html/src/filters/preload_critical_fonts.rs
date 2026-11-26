//! Automatic critical font preloads (`PreloadCriticalFonts`).
//!
//! Discovers `@font-face` fonts a page needs and injects at most two
//! `<link rel=preload as=font>` hints at the start of `<head>`,
//! cutting the late-discovery chain (HTML → CSS → font) that delays
//! text rendering. Core filter: on by default, capped conservatively.
//!
//! Sources, in order: inline `<style>` blocks (always parsed, bounded),
//! then external stylesheets via parsed-faces cache plus inline-completing
//! input loads (local mapped files complete synchronously; remote URLs
//! serve from HTTP cache or wait for a later render — discovery never
//! blocks the response on network). Nested `@import`s resolve
//! breadth-first with cycle and depth caps. Beacon-confirmed fonts
//! (learned over visits) sort first and lift icon/subset pruning.
//!
//! Emission: absolute resolved URLs, correct MIME by extension,
//! always `crossorigin="anonymous"` (required for the preload/use
//! cache match). Author font preloads (elements and Link headers),
//! duplicate URLs, data: sources, icon-only and non-Latin-unconfirmed
//! faces are never emitted. Decisions cache per page+options+inputs
//! so warm renders skip re-parsing; beacon merges bump the HTML
//! generation so learned changes refresh cached pages.
//!
//! Detection: `var()` refs resolve against custom properties merged
//! from every sheet (order-independent); selectors verify against
//! document tags/classes/ids so unused utility classes never
//! preload; page-wide (`body`), structural (headings/nav/titles),
//! and verified usages rank above unverifiable ones; variable
//! weight ranges overlap-match. With `Debug`, a head comment
//! explains every selected/skipped/rejected family.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
use crate::css_tag_scanner::{is_alternate_stylesheet, is_stylesheet_or_alternate};
use crate::element::{LeafKind, NodeId};
use crate::filter::HtmlFilter;
use crate::font_discovery::{self, RawFontUsage};
use crate::font_store;
use crate::js_escape::escape_to_js_string_literal;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::server_context::{InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver};
use crate::static_assets::{get_asset, StaticAsset};

/// Discovered faces (`preload_critical_fonts_discovered`).
pub const FONT_PRELOAD_DISCOVERED: &str = "preload_critical_fonts_discovered";
/// Emitted preloads (`preload_critical_fonts_preloaded`).
pub const FONT_PRELOAD_PRELOADED: &str = "preload_critical_fonts_preloaded";
/// Considered-but-unemitted faces (`preload_critical_fonts_skipped`).
pub const FONT_PRELOAD_SKIPPED: &str = "preload_critical_fonts_skipped";
/// Beacon-confirmed triples loaded (`preload_critical_fonts_beacon_confirmed`).
pub const FONT_PRELOAD_BEACON_CONFIRMED: &str = "preload_critical_fonts_beacon_confirmed";
/// Decision cache hits (`preload_critical_fonts_cache_hit`).
pub const FONT_PRELOAD_CACHE_HIT: &str = "preload_critical_fonts_cache_hit";
/// Decision cache misses (`preload_critical_fonts_cache_miss`).
pub const FONT_PRELOAD_CACHE_MISS: &str = "preload_critical_fonts_cache_miss";
/// Accepted font beacons (`preload_critical_fonts_beacon_accepted`).
pub const FONT_PRELOAD_BEACON_ACCEPTED: &str = "preload_critical_fonts_beacon_accepted";
/// Rejected font beacons (`preload_critical_fonts_beacon_rejected`).
pub const FONT_PRELOAD_BEACON_REJECTED: &str = "preload_critical_fonts_beacon_rejected";

/// Max inline style blocks collected.
const MAX_STYLE_BLOCKS: usize = 32;
/// Max inline style bytes total.
const MAX_STYLE_BYTES: usize = 256 * 1024;
/// Max external stylesheets followed.
const MAX_EXTERNAL_SHEETS: usize = 16;
/// Max `@import` depth past the referring sheet.
const MAX_IMPORT_DEPTH: u32 = 2;
/// Max inline `style=` usages collected (document order).
const MAX_ATTR_USAGES: usize = 128;
/// Max bytes fetched per external sheet.
const MAX_SHEET_BYTES: usize = 256 * 1024;
/// Max document tags/classes/ids kept for selector verification.
const MAX_DOM_TAGS: usize = 512;
/// Max document classes kept for selector verification.
const MAX_DOM_CLASSES: usize = 2048;
/// Max document ids kept for selector verification.
const MAX_DOM_IDS: usize = 2048;
/// Max token bytes kept per tag/class/id.
const MAX_DOM_TOKEN_BYTES: usize = 64;
/// Max diagnostic comment bytes (Debug filter only).
const MAX_DEBUG_COMMENT_BYTES: usize = 1536;

/// What one external stylesheet load produced.
enum SheetLoad {
    Ready(Vec<u8>),
    /// Fetch in flight: nothing blocked, nothing learned.
    Pending,
    /// Failed, unsafe, or unusable bytes.
    Failed,
}

/// What one external sheet's discovery resolved to.
enum SheetOutcome {
    Ready(font_store::CachedCssFaces),
    Pending,
    Failed,
}
/// Default beacon endpoint (mirrors the image beacon default).
const DEFAULT_BEACON_URL: &str = "/ngx_pagespeed_beacon";

/// Driver-bound preload filter: collects stylesheets, usages, and
/// author preloads during the parse; decides and injects at end of
/// document (post phase, after CSS inlining lands in the DOM).
pub struct PreloadCriticalFontsFilter {
    driver: Rc<ResourceDriver>,
    head_element: Option<NodeId>,
    body_element: Option<NodeId>,
    in_style: Option<NodeId>,
    style_blocks: Vec<Vec<u8>>,
    style_bytes: usize,
    external_sheets: Vec<String>,
    attr_usages: Vec<RawFontUsage>,
    author_preloads: HashSet<String>,
    link_headers_scanned: bool,
    dom_tags: HashSet<String>,
    dom_classes: HashSet<String>,
    dom_ids: HashSet<String>,
}

impl PreloadCriticalFontsFilter {
    /// Uses the request's resource driver (base URL, caches, stats).
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        Self {
            driver,
            head_element: None,
            body_element: None,
            in_style: None,
            style_blocks: Vec::new(),
            style_bytes: 0,
            external_sheets: Vec::new(),
            attr_usages: Vec::new(),
            author_preloads: HashSet::new(),
            link_headers_scanned: false,
            dom_tags: HashSet::new(),
            dom_classes: HashSet::new(),
            dom_ids: HashSet::new(),
        }
    }

    fn bump(&self, name: &str, count: i64) {
        self.driver.server_context().variable(name).add(count);
    }

    /// Whether a `media` attribute rules out screen rendering.
    /// Absent or screen/all-bearing values stay eligible.
    fn media_is_screen(media: Option<Vec<u8>>) -> bool {
        let Some(media) = media else {
            return true;
        };
        let text = media.to_ascii_lowercase();
        let text = String::from_utf8_lossy(&text);
        if text.contains("screen") || text.contains("all") {
            return true;
        }
        !(text.contains("print")
            || text.contains("speech")
            || text.contains("tty")
            || text.contains("projection")
            || text.contains("handheld")
            || text.contains("embossed")
            || text.contains("braille")
            || text.contains("aural"))
    }

    fn decoded_attr(dom: &mut Dom, id: NodeId, keyword: Keyword) -> Option<Vec<u8>> {
        let index = dom.find_attribute(id, keyword)?;
        dom.decoded_attribute(id, index).0
    }

    fn note_existing_preload(&mut self, dom: &mut Dom, id: NodeId) {
        let rel = match Self::decoded_attr(dom, id, Keyword::Rel) {
            Some(rel) => rel,
            None => return,
        };
        if !has_rel_token(&rel, "preload") {
            return;
        }
        let is_font = Self::decoded_attr(dom, id, Keyword::As)
            .is_some_and(|value| value.eq_ignore_ascii_case(b"font"));
        if !is_font {
            return;
        }
        if let Some(href) = Self::decoded_attr(dom, id, Keyword::Href) {
            let base = self.driver.decoded_base_url();
            if let Ok(text) = std::str::from_utf8(&href) {
                let resolved = ps_http::google_url::GoogleUrl::resolve(&base, text);
                if resolved.is_web_valid() {
                    self.author_preloads.insert(resolved.spec().to_string());
                }
            }
        }
    }

    /// Loads one stylesheet URL's bytes when they complete inline
    /// (cache hits, mapped files, mock fetcher); pending network
    /// fetches report [`SheetLoad::Pending`] and never block.
    fn load_stylesheet_bytes(&self, url: &ps_http::google_url::GoogleUrl) -> SheetLoad {
        let (resource, _) = self.driver.create_input_resource(
            url,
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Style,
        );
        let mut input = match resource {
            Some(input) => input,
            None => return SheetLoad::Failed,
        };
        let mut ctx = self.driver.request_context().borrow().clone();
        let mut callback = MockLoadCallback::new();
        input.load(
            NotCacheablePolicy::ReportFailureIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        // Pending arrives two ways: the async pool leaves the
        // callback uncalled; the sync path reports done with the
        // inconclusive flag set.
        if !callback.done() || input.load_pending() {
            return SheetLoad::Pending;
        }
        if !callback.success() || !input.loaded() || !input.http_status_ok() {
            return SheetLoad::Failed;
        }
        let (safe, _) = input.is_safe_to_rewrite(false);
        if !safe {
            return SheetLoad::Failed;
        }
        let bytes = input.contents();
        if bytes.is_empty() || bytes.len() > MAX_SHEET_BYTES {
            return SheetLoad::Failed;
        }
        SheetLoad::Ready(bytes)
    }
}

fn has_rel_token(rel_value: &[u8], token: &str) -> bool {
    String::from_utf8_lossy(rel_value)
        .split([' ', '\t'])
        .any(|part| part.eq_ignore_ascii_case(token))
}

impl PreloadCriticalFontsFilter {
    /// Cache identity: page URL, options hash, device suffix (mirrors
    /// the beacon keying so learned fonts stay scoped).
    fn cache_identity(&self) -> (String, String, String) {
        let server = self.driver.server_context();
        let page_url = self.driver.base_url().spec().to_string();
        let options_hash = server
            .hasher()
            .hash(self.driver.options_signature().as_bytes());
        let properties = self.driver.request_properties();
        let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
            properties.device_type(),
        )
        .to_string();
        (page_url, options_hash, suffix)
    }

    /// Loads beacon-confirmed font triples for this page.
    fn load_confirmed(&self) -> HashSet<(String, u32, font_discovery::FontStyle)> {
        use ps_cache::cache::KeyState;
        let (page_url, options_hash, suffix) = self.cache_identity();
        let cache = self.driver.server_context().metadata_cache();
        match cache.get(&font_store::confirmed_key(
            &page_url,
            &options_hash,
            &suffix,
        )) {
            ps_cache::cache::Lookup {
                state: KeyState::Available,
                value: Some(blob),
            } => String::from_utf8(blob)
                .map(|text| font_store::decode_confirmed(&text))
                .unwrap_or_default(),
            _ => HashSet::new(),
        }
    }

    /// Loads one external sheet's discovery: fresh metadata cache
    /// first, else an inline-completing load whose parse is cached.
    /// Pending network fetches report [`SheetOutcome::Pending`]
    /// (nothing blocks; the HTTP cache warms them for later
    /// renders); failures report [`SheetOutcome::Failed`].
    fn external_sheet(
        &self,
        css_url: &ps_http::google_url::GoogleUrl,
        now_ms: i64,
    ) -> SheetOutcome {
        use ps_cache::cache::KeyState;
        let cache = self.driver.server_context().metadata_cache();
        let key = font_store::css_faces_key(css_url.spec());
        if let ps_cache::cache::Lookup {
            state: KeyState::Available,
            value: Some(blob),
        } = cache.get(&key)
        {
            if let Ok(text) = std::str::from_utf8(&blob) {
                if let Some(cached) = font_store::decode_css_faces(text) {
                    if now_ms - cached.fetched_ms <= font_store::CSS_FACES_TTL_MS {
                        return SheetOutcome::Ready(cached);
                    }
                }
            }
        }
        match self.load_stylesheet_bytes(css_url) {
            SheetLoad::Ready(bytes) => {
                let scanned = font_discovery::scan_stylesheet(&bytes);
                let usages = font_discovery::scan_stylesheet_usages(&bytes);
                let custom_props = font_discovery::collect_custom_properties(&bytes);
                let cached = font_store::CachedCssFaces {
                    fetched_ms: now_ms,
                    faces: scanned.faces,
                    imports: scanned.imports,
                    usages,
                    custom_props,
                };
                cache.put(&key, font_store::encode_css_faces(&cached).as_bytes());
                SheetOutcome::Ready(cached)
            }
            SheetLoad::Pending => SheetOutcome::Pending,
            SheetLoad::Failed => SheetOutcome::Failed,
        }
    }

    /// Retry window past the longest remembered fetch failure, so a
    /// revalidation actually re-fetches instead of hitting the HTTP
    /// cache's short-circuit.
    fn failed_sheets_retry_ms(&self) -> i64 {
        use ps_cache::ttl::FetchStatus;
        let http_cache = self.driver.server_context().http_cache();
        let worst = [
            FetchStatus::NotSet,
            FetchStatus::Ok,
            FetchStatus::Uncacheable200,
            FetchStatus::UncacheableError,
            FetchStatus::Error4xx,
            FetchStatus::OtherError,
            FetchStatus::Dropped,
            FetchStatus::Empty,
        ]
        .iter()
        .map(|status| http_cache.failure_caching_ttl_sec(*status))
        .max()
        .unwrap_or(300);
        i64::from(worst.max(0))
            .saturating_mul(1000)
            .saturating_add(10_000)
    }

    /// Resolves face source URLs against a stylesheet base, dropping
    /// unresolvable and non-web entries.
    fn resolve_faces(
        faces: Vec<font_discovery::FontFace>,
        base: &ps_http::google_url::GoogleUrl,
    ) -> Vec<font_discovery::FontFace> {
        faces
            .into_iter()
            .map(|mut face| {
                face.sources.retain(|source| {
                    let resolved = ps_http::google_url::GoogleUrl::resolve(base, &source.url);
                    resolved.is_web_valid()
                });
                for source in &mut face.sources {
                    let resolved = ps_http::google_url::GoogleUrl::resolve(base, &source.url);
                    source.url = resolved.spec().to_string();
                }
                face
            })
            .filter(|face| !face.sources.is_empty())
            .collect()
    }

    /// Emits one preload link at the start of `<head>`.
    fn emit_preload(&self, dom: &mut Dom, preload: &font_discovery::FontPreload) {
        let Some(head) = self.head_element else {
            return;
        };
        if !dom.can_append_child(head) {
            return;
        }
        let link = dom.new_element(None, b"link");
        dom.add_attribute(link, b"rel", b"preload");
        dom.add_attribute(link, b"as", b"font");
        dom.add_attribute(link, b"type", preload.mime.as_bytes());
        dom.add_attribute(link, b"href", preload.url.as_bytes());
        // Anonymous mode is required for the preload/use match even
        // for same-origin fonts (CORS-mode cache partitioning).
        dom.add_attribute(link, b"crossorigin", b"anonymous");
        dom.prepend_child(head, link);
    }

    /// Appends a diagnostic head comment (Debug filter only).
    fn emit_debug_comment(&self, dom: &mut Dom, text: &str) {
        let debug = self
            .driver
            .options()
            .filters()
            .enabled(ps_config::filter::Filter::Debug);
        if !debug {
            return;
        }
        let Some(head) = self.head_element else {
            return;
        };
        if !dom.can_append_child(head) {
            return;
        }
        let mut clean = text.replace("--", "- -");
        clean.truncate(MAX_DEBUG_COMMENT_BYTES);
        let mut escaped = Vec::new();
        crate::keywords::escape(clean.as_bytes(), &mut escaped);
        let node = dom.new_leaf(None, LeafKind::Comment, escaped);
        dom.append_child(Some(head), node);
    }

    /// Explains every considered family (Debug filter only).
    fn emit_debug_notes(&self, dom: &mut Dom, notes: &[font_discovery::FontNote]) {
        if notes.is_empty() {
            return;
        }
        let mut text = String::from("font-preloads:");
        for note in notes {
            let outcome = match note.outcome {
                font_discovery::FontOutcome::Selected => "selected",
                font_discovery::FontOutcome::Skipped => "skipped",
                font_discovery::FontOutcome::Rejected => "rejected",
            };
            let piece = format!(" {}={outcome}({})", note.family, note.reason);
            if text.len() + piece.len() > MAX_DEBUG_COMMENT_BYTES {
                break;
            }
            text.push_str(&piece);
        }
        self.emit_debug_comment(dom, &text);
    }

    /// Records one element's tag/classes/id for selector
    /// verification (bounded; tags lowercased, classes/ids exact).
    fn note_dom_token(&mut self, dom: &mut Dom, id: NodeId) {
        if self.dom_tags.len() < MAX_DOM_TAGS {
            let tag = dom.element_name(id).to_ascii_lowercase();
            if !tag.is_empty() && tag.len() <= MAX_DOM_TOKEN_BYTES {
                if let Ok(tag) = String::from_utf8(tag) {
                    self.dom_tags.insert(tag);
                }
            }
        }
        if self.dom_classes.len() < MAX_DOM_CLASSES {
            if let Some(index) = dom.find_attribute(id, Keyword::Class) {
                if let (Some(value), _) = dom.decoded_attribute(id, index) {
                    for class in value.split(|byte| byte.is_ascii_whitespace()) {
                        if class.is_empty() || class.len() > MAX_DOM_TOKEN_BYTES {
                            continue;
                        }
                        if self.dom_classes.len() >= MAX_DOM_CLASSES {
                            break;
                        }
                        if let Ok(class) = String::from_utf8(class.to_vec()) {
                            self.dom_classes.insert(class);
                        }
                    }
                }
            }
        }
        if self.dom_ids.len() < MAX_DOM_IDS {
            if let Some(index) = dom.find_attribute(id, Keyword::Id) {
                if let (Some(value), _) = dom.decoded_attribute(id, index) {
                    let clean = !value.is_empty()
                        && value.len() <= MAX_DOM_TOKEN_BYTES
                        && !value.iter().any(|byte| byte.is_ascii_whitespace());
                    if clean {
                        if let Ok(id) = String::from_utf8(value.to_vec()) {
                            self.dom_ids.insert(id);
                        }
                    }
                }
            }
        }
    }

    /// Scans Link response headers for author font preloads.
    fn scan_link_headers(&mut self) {
        if self.link_headers_scanned {
            return;
        }
        self.link_headers_scanned = true;
        let headers = self.driver.response_headers();
        let Some(values) = headers.headers().lookup("Link") else {
            return;
        };
        let base = self.driver.decoded_base_url();
        for value in values {
            for entry in value.split(',') {
                let mut url = None;
                let mut rel_preload = false;
                let mut as_font = false;
                for (index, part) in entry.split(';').enumerate() {
                    let part = part.trim();
                    if index == 0 {
                        url = part
                            .strip_prefix('<')
                            .and_then(|rest| rest.strip_suffix('>'));
                    } else if part.eq_ignore_ascii_case("rel=preload")
                        || part.eq_ignore_ascii_case("rel=\"preload\"")
                    {
                        rel_preload = true;
                    } else if part.eq_ignore_ascii_case("as=font")
                        || part.eq_ignore_ascii_case("as=\"font\"")
                    {
                        as_font = true;
                    }
                }
                if rel_preload && as_font {
                    if let Some(url) = url {
                        let resolved = ps_http::google_url::GoogleUrl::resolve(&base, url);
                        if resolved.is_web_valid() {
                            self.author_preloads.insert(resolved.spec().to_string());
                        }
                    }
                }
            }
        }
    }

    /// Whether learning could change future decisions: some
    /// matched face is still unconfirmed.
    fn learnable(
        faces: &[font_discovery::FontFace],
        usages: &[font_discovery::FontUsage],
        confirmed: &HashSet<(String, u32, font_discovery::FontStyle)>,
    ) -> bool {
        usages.iter().any(|usage| {
            let key = (usage.family.clone(), usage.weight, usage.style);
            if confirmed.contains(&key) {
                return false;
            }
            faces.iter().any(|face| {
                font_discovery::face_matches(face, usage)
                    && font_discovery::best_source(face).is_some()
            })
        })
    }

    /// Issues a nonce into the page's outstanding set and appends
    /// the instrumented beacon script at end of body. Both the miss
    /// path and a still-learnable cache hit call this: a hit skips
    /// rediscovery but must not stall learning.
    fn inject_beacon(
        &self,
        dom: &mut Dom,
        lexer: &Lexer,
        page_url: &str,
        options_hash: &str,
        suffix: &str,
    ) {
        let Some(body) = self.body_element else {
            return;
        };
        if !dom.can_append_child(body) {
            return;
        }
        let server = self.driver.server_context();
        let nonce_value =
            ((server.simple_random_next() as u64) << 32) | server.simple_random_next() as u64;
        let nonce = crate::beacon_finder::format_nonce(nonce_value);
        let cache = server.metadata_cache();
        let nonces_key = font_store::nonces_key(page_url, options_hash, suffix);
        let mut nonces = match cache.get(&nonces_key) {
            ps_cache::cache::Lookup {
                state: ps_cache::cache::KeyState::Available,
                value: Some(blob),
            } => String::from_utf8(blob)
                .map(|text| font_store::decode_nonces(&text))
                .unwrap_or_default(),
            _ => font_store::FontNonces::default(),
        };
        font_store::issue_nonce(&mut nonces, nonce.clone(), server.now_ms());
        cache.put(&nonces_key, font_store::encode_nonces(&nonces).as_bytes());
        let options = self.driver.options();
        let is_https = self.driver.base_url().scheme() == "https";
        let beacon_url = options
            .beacon_url()
            .map(|urls| if is_https { urls.https } else { urls.http })
            .unwrap_or_else(|| DEFAULT_BEACON_URL.to_string());
        let debug = options.filters().enabled(ps_config::filter::Filter::Debug);
        let mut js = get_asset(StaticAsset::FontPreloadBeaconJs, debug).to_vec();
        js.extend_from_slice(b"\npagespeed.FontPreloads.Run('");
        js.extend_from_slice(&escape_to_js_string_literal(beacon_url.as_bytes(), false));
        js.extend_from_slice(b"','");
        js.extend_from_slice(&escape_to_js_string_literal(page_url.as_bytes(), false));
        js.extend_from_slice(b"','");
        js.extend_from_slice(&escape_to_js_string_literal(options_hash.as_bytes(), false));
        js.extend_from_slice(b"','");
        js.extend_from_slice(&escape_to_js_string_literal(nonce.as_bytes(), false));
        js.extend_from_slice(b"');");
        let script = dom.new_element(None, b"script");
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        let xhtml = match self.driver.mime_type_xhtml_status() {
            crate::server_context::XhtmlStatus::IsXhtml => {
                crate::filters::common::XhtmlStatus::IsXhtml
            }
            _ => crate::filters::common::XhtmlStatus::IsNotXhtml,
        };
        crate::filters::common::add_js_to_element(dom, lexer, script, &js, false, xhtml);
        dom.append_child(Some(body), script);
    }

    /// Decides and emits: cached decision on input match, else full
    /// discovery over inline blocks, external sheets (and nested
    /// imports), usages, and beacon confirmations.
    fn decide_and_emit(&mut self, dom: &mut Dom, lexer: &Lexer) {
        use ps_cache::cache::KeyState;
        let Some(_) = self.head_element else {
            return;
        };
        self.scan_link_headers();
        let confirmed = self.load_confirmed();
        self.bump(FONT_PRELOAD_BEACON_CONFIRMED, confirmed.len() as i64);
        // Fingerprint over inline CSS plus external sheet URLs; the
        // confirmed digest scopes reuse to learned state. Selector
        // verification also reads the document, so fold in a digest
        // of the element sets (cheap sorted hash, no parsing).
        let mut fingerprinted = Vec::new();
        for block in &self.style_blocks {
            fingerprinted.extend_from_slice(block);
            fingerprinted.push(b'\n');
        }
        for href in &self.external_sheets {
            fingerprinted.extend_from_slice(href.as_bytes());
            fingerprinted.push(b'\n');
        }
        let mut dom_digest_input = Vec::new();
        for set in [&self.dom_tags, &self.dom_classes, &self.dom_ids] {
            let mut items: Vec<&str> = set.iter().map(String::as_str).collect();
            items.sort_unstable();
            dom_digest_input.extend_from_slice(&(items.len() as u64).to_be_bytes());
            for item in items {
                dom_digest_input.extend_from_slice(item.as_bytes());
                dom_digest_input.push(0);
            }
        }
        fingerprinted
            .extend_from_slice(&font_store::fingerprint_bytes(&dom_digest_input).to_be_bytes());
        let css_fingerprint = font_store::fingerprint_bytes(&fingerprinted);
        let mut confirmed_digest_input = Vec::new();
        let mut confirmed_list: Vec<(&str, u32)> = confirmed
            .iter()
            .map(|(family, weight, _)| (family.as_str(), *weight))
            .collect();
        confirmed_digest_input.extend_from_slice(
            &u64::try_from(confirmed.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        confirmed_list.sort_unstable();
        for (family, weight) in confirmed_list {
            confirmed_digest_input.extend_from_slice(family.as_bytes());
            confirmed_digest_input.extend_from_slice(&weight.to_be_bytes());
        }
        let confirmed_digest = font_store::fingerprint_bytes(&confirmed_digest_input);
        let (page_url, options_hash, suffix) = self.cache_identity();
        let cache = self.driver.server_context().metadata_cache();
        let decision_key = font_store::decision_key(&page_url, &options_hash, &suffix);
        let now_ms = self.driver.server_context().now_ms();
        if let ps_cache::cache::Lookup {
            state: KeyState::Available,
            value: Some(blob),
        } = cache.get(&decision_key)
        {
            if let Ok(text) = String::from_utf8(blob) {
                if let Some(decision) = font_store::decode_decision(&text) {
                    if decision.css_fingerprint == css_fingerprint
                        && decision.beacon_generation == confirmed_digest
                        && now_ms <= decision.revalidate_after_ms
                    {
                        self.bump(FONT_PRELOAD_CACHE_HIT, 1);
                        // Reversed: each prepend lands ahead of the last.
                        for preload in decision.preloads.iter().rev() {
                            if !self.author_preloads.contains(&preload.url) {
                                self.emit_preload(dom, preload);
                                self.bump(FONT_PRELOAD_PRELOADED, 1);
                            }
                        }
                        // A hit reuses the learned generation, so the
                        // stored flag is still accurate: keep the
                        // beacon shipping until nothing is learnable.
                        if decision.needs_beacon {
                            self.inject_beacon(dom, lexer, &page_url, &options_hash, &suffix);
                        }
                        self.emit_debug_comment(
                            dom,
                            &format!(
                                "font-preloads: cached decision replayed ({} preloads)",
                                decision.preloads.len()
                            ),
                        );
                        return;
                    }
                }
            }
        }
        self.bump(FONT_PRELOAD_CACHE_MISS, 1);
        let doc_base = self.driver.decoded_base_url();
        let mut faces = Vec::new();
        // Phase one: raw usages plus custom properties from every
        // sheet; props merge before anything resolves, so discovery
        // never depends on declaration order.
        let mut sheet_raw: Vec<RawFontUsage> = Vec::new();
        let mut props: HashMap<String, Vec<u8>> = HashMap::new();
        // Inline blocks (author plus inlined external) resolve against
        // the document base; their imports seed the external queue.
        let mut queue: Vec<(String, u32)> = Vec::new();
        for block in &self.style_blocks {
            let scanned = font_discovery::scan_stylesheet(block);
            sheet_raw.extend(font_discovery::scan_stylesheet_usages(block));
            for (name, value) in font_discovery::collect_custom_properties(block) {
                props.insert(name, value.into_bytes());
            }
            faces.extend(Self::resolve_faces(scanned.faces, &doc_base));
            for import in scanned.imports {
                queue.push((import, 1));
            }
        }
        for href in self.external_sheets.iter().take(MAX_EXTERNAL_SHEETS) {
            queue.push((href.clone(), 0));
        }
        // External sheets depth-first with visited guard; each
        // sheet's faces resolve against itself, imports re-resolve
        // against the importing sheet.
        let mut seen_sheets = HashSet::new();
        let mut sheets_pending = false;
        let mut sheets_failed = false;
        while let Some((href, depth)) = queue.pop() {
            if !seen_sheets.insert(href.clone()) || depth > MAX_IMPORT_DEPTH {
                continue;
            }
            let css_url = ps_http::google_url::GoogleUrl::resolve(&doc_base, &href);
            if !css_url.is_web_valid() {
                continue;
            }
            let cached = match self.external_sheet(&css_url, now_ms) {
                SheetOutcome::Ready(cached) => cached,
                // Still loading: skip the store below so the next
                // visit retries instead of replaying a partial call.
                SheetOutcome::Pending => {
                    sheets_pending = true;
                    continue;
                }
                // Failed: store with a retry window past the HTTP
                // failure TTL so the recheck really re-fetches.
                SheetOutcome::Failed => {
                    sheets_failed = true;
                    continue;
                }
            };
            faces.extend(Self::resolve_faces(cached.faces, &css_url));
            sheet_raw.extend(cached.usages);
            for (name, value) in cached.custom_props {
                props.insert(name, value.into_bytes());
            }
            if depth < MAX_IMPORT_DEPTH {
                for import in cached.imports {
                    let resolved = ps_http::google_url::GoogleUrl::resolve(&css_url, &import);
                    if resolved.is_web_valid() {
                        queue.push((resolved.spec().to_string(), depth + 1));
                    }
                }
            }
        }
        // Phase two: resolve `var()` refs against the merged map
        // and verify selectors; element-ordered inline usages first,
        // stylesheet order after.
        let dom_sets = font_discovery::DomSets {
            tags: self.dom_tags.clone(),
            classes: self.dom_classes.clone(),
            ids: self.dom_ids.clone(),
        };
        let mut usages =
            font_discovery::resolve_raw_usages(&self.attr_usages, &props, &dom_sets, true);
        let mut seen_usages: HashSet<(String, u32, u32, u8, u8, String)> = usages
            .iter()
            .map(|usage| {
                (
                    usage.family.clone(),
                    usage.weight,
                    usage.weight_max,
                    usage.style as u8,
                    usage.confidence as u8,
                    usage.selector.clone(),
                )
            })
            .collect();
        for usage in font_discovery::resolve_raw_usages(&sheet_raw, &props, &dom_sets, false) {
            if seen_usages.insert((
                usage.family.clone(),
                usage.weight,
                usage.weight_max,
                usage.style as u8,
                usage.confidence as u8,
                usage.selector.clone(),
            )) {
                usages.push(usage);
            }
        }
        self.bump(FONT_PRELOAD_DISCOVERED, faces.len() as i64);
        let (preloads, notes) = font_discovery::select_preloads_explained(
            &faces,
            &usages,
            &confirmed,
            &self.author_preloads,
        );
        let mut emitted = 0i64;
        // Reversed: each prepend lands ahead of the last.
        for preload in preloads.iter().rev() {
            self.emit_preload(dom, preload);
            emitted += 1;
        }
        self.bump(FONT_PRELOAD_PRELOADED, emitted);
        let skipped = notes
            .iter()
            .filter(|note| note.outcome == font_discovery::FontOutcome::Skipped)
            .count() as i64;
        self.bump(FONT_PRELOAD_SKIPPED, skipped);
        self.emit_debug_notes(dom, &notes);
        let needs_beacon = Self::learnable(&faces, &usages, &confirmed);
        if !sheets_pending {
            let revalidate_after_ms = if sheets_failed {
                now_ms.saturating_add(self.failed_sheets_retry_ms())
            } else {
                i64::MAX
            };
            cache.put(
                &decision_key,
                font_store::encode_decision(&font_store::FontDecision {
                    css_fingerprint,
                    beacon_generation: confirmed_digest,
                    preloads: preloads.clone(),
                    needs_beacon,
                    revalidate_after_ms,
                })
                .as_bytes(),
            );
        }
        if needs_beacon {
            self.inject_beacon(dom, lexer, &page_url, &options_hash, &suffix);
        }
    }
}

impl HtmlFilter for PreloadCriticalFontsFilter {
    fn name(&self) -> &'static str {
        "PreloadCriticalFontsFilter"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.head_element = None;
        self.body_element = None;
        self.in_style = None;
        self.style_blocks.clear();
        self.style_bytes = 0;
        self.external_sheets.clear();
        self.attr_usages.clear();
        self.author_preloads.clear();
        self.link_headers_scanned = false;
        self.dom_tags.clear();
        self.dom_classes.clear();
        self.dom_ids.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        match dom.element_keyword(id) {
            Keyword::Head => {
                self.head_element = Some(id);
            }
            Keyword::Body => {
                self.body_element = Some(id);
            }
            Keyword::Style => {
                let media = dom
                    .find_attribute_by_name(id, b"media")
                    .and_then(|index| dom.decoded_attribute(id, index).0);
                if Self::media_is_screen(media) && self.style_blocks.len() < MAX_STYLE_BLOCKS {
                    self.in_style = Some(id);
                    self.style_blocks.push(Vec::new());
                }
            }
            Keyword::Link => {
                let rel = dom
                    .find_attribute(id, Keyword::Rel)
                    .and_then(|index| dom.decoded_attribute(id, index).0)
                    .unwrap_or_default();
                if has_rel_token(&rel, "preload") {
                    self.note_existing_preload(dom, id);
                } else if is_stylesheet_or_alternate(&rel) && !is_alternate_stylesheet(&rel) {
                    let media = dom
                        .find_attribute_by_name(id, b"media")
                        .and_then(|index| dom.decoded_attribute(id, index).0);
                    if Self::media_is_screen(media)
                        && self.external_sheets.len() < MAX_EXTERNAL_SHEETS
                    {
                        if let Some(href) = dom
                            .find_attribute(id, Keyword::Href)
                            .and_then(|index| dom.decoded_attribute(id, index).0)
                        {
                            let href = String::from_utf8_lossy(&href).into_owned();
                            if !href.is_empty() && !self.external_sheets.contains(&href) {
                                self.external_sheets.push(href);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        self.note_dom_token(dom, id);
        // Inline `style=` font usage in element order (bounded).
        if self.attr_usages.len() < MAX_ATTR_USAGES {
            if let Some(index) = dom.find_attribute_by_name(id, b"style") {
                if let (Some(value), _) = dom.decoded_attribute(id, index) {
                    self.attr_usages
                        .extend(font_discovery::raw_usage_from_declarations(&value, ""));
                    self.attr_usages.truncate(MAX_ATTR_USAGES);
                }
            }
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.in_style.is_none() || self.style_bytes >= MAX_STYLE_BYTES {
            return;
        }
        let text = dom.leaf_contents(id);
        let room = MAX_STYLE_BYTES - self.style_bytes;
        let take = text.len().min(room);
        self.style_bytes += take;
        // A fresh buffer was pushed on `<style>` entry.
        if let Some(block) = self.style_blocks.last_mut() {
            block.extend_from_slice(&text[..take]);
        }
    }

    fn end_element(&mut self, _dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.in_style == Some(id) {
            self.in_style = None;
        }
    }

    fn end_document(&mut self, dom: &mut Dom, lexer: &Lexer) {
        self.decide_and_emit(dom, lexer);
    }
}

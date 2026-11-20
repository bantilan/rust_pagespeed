//! CSS rewriting (`CssFilter`, `cf`).
//!
//! Minifies external stylesheets and inline `<style>` blocks plus
//! `style` attributes, refusing output that could read as markup.
//! Nested image/`@import` contexts, fallback URL rewriting, and
//! absolutification land in later batches; this batch minifies only,
//! matching C++ whenever image rewriting, flattening, and domain
//! mapping are all off.
//!
//! NOTE: CSP gating waits for the CSP subsystem.

use std::rc::Rc;

use ps_cache::cache::KeyState;
use ps_cache::hash::Hasher;
use ps_config::filter::Filter;
use ps_config::options::{Category, RewriteOptions};
use ps_css::ast::{Declarations, Stylesheet, NO_ERROR};
use ps_css::minify::Minifier;
use ps_css::parser::Parser;
use ps_css::value::ValueType;
use ps_http::google_url::GoogleUrl;

use super::css_hierarchy::CssHierarchy;
use super::meta_tag::extract_meta_tag_details;
use crate::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::metadata::{CachedResult, OutputPartitions};
use crate::name::Keyword;
use crate::output_resource::{OutputResource, OutputResourceKind};
use crate::parser::Dom;
use crate::resource::{InputResource, Resource};
use crate::resource_tag_scanner::scan_element;
use crate::rewrite_context::{
    ContextSlot, RewriteResult, SingleRewriteBehavior, SingleRewriteContext,
};
use crate::rewrite_filter::RewriteFilter;
use crate::script_scanner::ScriptTagScanner;
use crate::server_context::{InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver};
use crate::url_segment_encoder::{CssUrlEncoder, UrlSegmentEncoder};

/// Shared CSS codec (`CssUrlEncoder`, `A.`-prefixed).
static CSS_ENCODER: CssUrlEncoder = CssUrlEncoder;

/// Filter id (`RewriteOptions::kCssFilterId`).
pub const FILTER_ID: &str = "cf";

/// Rewritten-block counter (`CssFilter::kBlocksRewritten`).
pub const BLOCKS_REWRITTEN: &str = "css_filter_blocks_rewritten";
/// Parse-failure counter (`CssFilter::kParseFailures`).
pub const PARSE_FAILURES: &str = "css_filter_parse_failures";
/// Over-bound counter (`CssFilter::kParseMaxBytesExceeded`).
pub const PARSE_MAX_BYTES_EXCEEDED: &str = "css_filter_parse_max_bytes_exceeded";
/// Fallback-rewrite counter (`CssFilter::kFallbackRewrites`).
pub const FALLBACK_REWRITES: &str = "css_filter_fallback_rewrites";
/// Fallback-failure counter (`CssFilter::kFallbackFailures`).
pub const FALLBACK_FAILURES: &str = "css_filter_fallback_failures";
/// Dropped-rewrite counter (`CssFilter::kRewritesDropped`).
pub const REWRITES_DROPPED: &str = "css_filter_rewrites_dropped";
/// Bytes-saved counter (`CssFilter::kTotalBytesSaved`).
pub const TOTAL_BYTES_SAVED: &str = "css_filter_total_bytes_saved";
/// Original-bytes counter (`CssFilter::kTotalOriginalBytes`).
pub const TOTAL_ORIGINAL_BYTES: &str = "css_filter_total_original_bytes";
/// Use counter (`CssFilter::kUses`).
pub const USES: &str = "css_filter_uses";
/// Flatten charset-mismatch counter (`CssFilter::kCharsetMismatch`).
pub const FLATTEN_CHARSET_MISMATCH: &str = "flatten_imports_charset_mismatch";
/// Flatten invalid-URL counter (`CssFilter::kInvalidUrl`).
pub const FLATTEN_INVALID_URL: &str = "flatten_imports_invalid_url";
/// Flatten limit counter (`CssFilter::kLimitExceeded`).
pub const FLATTEN_LIMIT_EXCEEDED: &str = "flatten_imports_limit_exceeded";
/// Flatten minify-failure counter (`CssFilter::kMinifyFailed`).
pub const FLATTEN_MINIFY_FAILED: &str = "flatten_imports_minify_failed";
/// Flatten recursion counter (`CssFilter::kRecursion`).
pub const FLATTEN_RECURSION: &str = "flatten_imports_recursion";
/// Flatten complex-query counter (`CssFilter::kComplexQueries`).
pub const FLATTEN_COMPLEX_QUERIES: &str = "flatten_imports_complex_queries";

/// Whether a top-level stylesheet is over the parse bound
/// (`CssFilter::OverParseMaxBytes`; a bound <= 0 means none). Counts
/// the skip and reports the message; the once-per-URL log waits for
/// the message handler.
pub fn over_parse_max_bytes(
    driver: &ResourceDriver,
    size: i64,
    url: &str,
    what: &str,
) -> Option<String> {
    let bound = driver.options().css_parse_max_bytes();
    if bound <= 0 || size <= bound {
        return None;
    }
    driver
        .server_context()
        .variable(PARSE_MAX_BYTES_EXCEEDED)
        .add(1);
    Some(format!(
        "{what}: {size} bytes is more than CssParseMaxBytes ({bound}) in {url}"
    ))
}

/// Parses a stylesheet (`RewriteCssText` setup): stylesheet plus
/// the unparseable-sections verdict (`None` on parse errors).
fn parse_stylesheet(input: &[u8]) -> Option<(Stylesheet, bool)> {
    let mut parser = Parser::new(input);
    parser.set_preservation_mode(true);
    parser.set_quirks_mode(false);
    let stylesheet = parser.parse_raw_stylesheet();
    if parser.errors_seen_mask() != NO_ERROR {
        return None;
    }
    let unparseable = parser.unparseable_sections_seen_mask() != NO_ERROR;
    Some((stylesheet, unparseable))
}

/// Parses declarations (`RewriteCssText` for attributes).
fn parse_declarations(input: &[u8]) -> Option<(Declarations, bool)> {
    let mut parser = Parser::new(input);
    parser.set_preservation_mode(true);
    parser.set_quirks_mode(false);
    let declarations = parser.parse_raw_declarations();
    if parser.errors_seen_mask() != NO_ERROR {
        return None;
    }
    let unparseable = parser.unparseable_sections_seen_mask() != NO_ERROR;
    Some((declarations, unparseable))
}

/// Minifies a parsed stylesheet (`SerializeCss` core). Returns the
/// output bytes plus raw `url()` values.
fn minify_sheet(stylesheet: &Stylesheet) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let uris = collect_stylesheet_uris(stylesheet);
    let mut minifier = Minifier::new(false);
    minifier.minify_stylesheet(stylesheet);
    let (out, ok) = minifier.finish();
    if ok {
        Some((out, uris))
    } else {
        None
    }
}

/// Minifies parsed declarations.
fn minify_decls(declarations: &Declarations) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let uris = collect_uris(declarations);
    let mut minifier = Minifier::new(false);
    minifier.minify_declarations(declarations);
    let (out, ok) = minifier.finish();
    if ok {
        Some((out, uris))
    } else {
        None
    }
}

/// Collects raw `url()` values from declarations (every URI value,
/// whatever the property).
fn collect_uris(declarations: &Declarations) -> Vec<Vec<u8>> {
    let mut uris = Vec::new();
    for declaration in declarations.iter() {
        for value in declaration.values().iter() {
            if value.lexical_type() == ValueType::Uri {
                uris.push(value.string_value().as_bytes().to_vec());
            }
        }
    }
    uris
}

/// Collects raw `url()` values from a stylesheet (imports plus every
/// ruleset; the AST flattens `@media` nesting).
fn collect_stylesheet_uris(stylesheet: &Stylesheet) -> Vec<Vec<u8>> {
    let mut uris = Vec::new();
    for import in stylesheet.imports().iter() {
        uris.push(import.link().as_bytes().to_vec());
    }
    for ruleset in stylesheet.rulesets().iter() {
        if ruleset.kind() != ps_css::ast::RulesetType::Ruleset {
            continue;
        }
        uris.extend(collect_uris(ruleset.declarations()));
    }
    uris
}

/// Builds the inline-rewrite cache key (input plus `CacheKeySuffix`
/// shape): raw bytes, options signature, and base-resolved URLs
/// (plus the page directory for style tags, whose key always
/// carries it).
fn inline_cache_key(
    driver: &Rc<ResourceDriver>,
    raw: &[u8],
    uris: &[Vec<u8>],
    kind: InlineCssKind,
) -> String {
    let mut payload = raw.to_vec();
    payload.push(b'\n');
    payload.extend_from_slice(driver.options().signature().as_bytes());
    let mut resolved: Vec<String> = uris
        .iter()
        .map(|uri| {
            GoogleUrl::resolve(driver.base_url(), &String::from_utf8_lossy(uri))
                .spec()
                .to_string()
        })
        .collect();
    resolved.sort();
    for url in resolved {
        payload.push(b'\n');
        payload.extend_from_slice(url.as_bytes());
    }
    if kind == InlineCssKind::InsideStyleTag {
        payload.push(b'\n');
        payload.extend_from_slice(driver.base_url().all_except_leaf().as_bytes());
    }
    // Nested image bytes vary by UA (webp/avif support plus the
    // inlining verdict), so the key does too (`UserAgentCacheKey`).
    payload.push(b'\n');
    payload.extend_from_slice(user_agent_cache_key(driver).as_bytes());
    format!(
        "cf-inline:{}",
        driver.server_context().lock_hasher().hash(&payload)
    )
}

/// UA cache-key fragment for CSS (`Context::UserAgentCacheKey`):
/// webp/avif support, plus `I` when CSS image inlining may run.
fn user_agent_cache_key(driver: &Rc<ResourceDriver>) -> String {
    use super::image_rewrite::encode_user_agent_into_resource_context;
    use crate::image_url_encoder::{ImageResourceContext, ImageUrlEncoder};
    let mut context = ImageResourceContext::default();
    encode_user_agent_into_resource_context(driver, &mut context);
    let mut key = ImageUrlEncoder::cache_key_from_resource_context(&context);
    // The cache key we get from the image codec is not sufficient, as
    // it does not produce different results if CSS image inlining is
    // on, but of course the css rewriter does.
    if driver.options().css_image_inline_max_bytes() != 0
        && driver.request_properties().supports_image_inlining()
    {
        key.push('I');
    } else {
        key.push('A');
    }
    key
}

/// Whether `@import` flattening runs (`FlattenCssImportsEnabled`):
/// the filter itself, or critical-CSS computation without a forbid.
fn flatten_css_imports_enabled(options: &RewriteOptions) -> bool {
    options.enabled(Filter::FlattenCssImports)
        || (!options.filters().forbidden(Filter::FlattenCssImports)
            && (options.enabled(Filter::PrioritizeCriticalCss)
                || options.enabled(Filter::ComputeCriticalCss)))
}

/// Import-failure reason (`CannotImportMessage`).
fn cannot_import_message(action: &str, url: &str, is_authorized: bool) -> String {
    format!(
        "Cannot {action} {url} {}",
        if is_authorized {
            "for an unknown reason"
        } else {
            "as it is on an unauthorized domain"
        }
    )
}

/// Per-context behavior.
struct CssBehavior {
    driver: Rc<ResourceDriver>,
    /// Root applicable media (link/style element).
    root_media: Vec<Vec<u8>>,
    /// Root charset (page/element agreement).
    root_charset: Vec<u8>,
    /// Root charset failure reason, if any.
    root_charset_failure: Option<String>,
}

impl CssBehavior {
    /// Rewrites one input (`RewriteSingle` + `Harvest` for the
    /// minify-only path).
    fn rewrite_css(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        output: &mut OutputResource,
        partition: &mut CachedResult,
    ) -> RewriteResult {
        let server = driver.server_context().clone();
        // NOTE: info messages wait for the message handler.
        let drop = driver.options().rewrite_random_drop_percentage();
        if drop > 0 && drop > (server.simple_random_next() % 100) as i64 {
            return RewriteResult::TooBusy;
        }
        let input_type = input
            .response_headers()
            .determine_content_type()
            .filter(|content_type| content_type.is_css());
        if input_type.is_none() {
            return RewriteResult::Failed;
        }
        let contents = input.contents();
        let in_text_size = contents.len() as i64;
        let (text, has_bom) = ps_css::combine::strip_utf8_bom(&contents);
        if let Some(message) =
            over_parse_max_bytes(driver, in_text_size, input.url(), "CSS not rewritten")
        {
            partition.debug_message.push(message);
            return RewriteResult::Failed;
        }
        let Some((mut stylesheet, unparseable)) = parse_stylesheet(text) else {
            server.variable(PARSE_FAILURES).add(1);
            partition.debug_message.push(format!(
                "CSS rewrite failed: Parse error in {}",
                input.url()
            ));
            if !driver.options().enabled(Filter::FallbackRewriteCssUrls) {
                return RewriteResult::Failed;
            }
            // Fallback to rewriting URLs with the tag scanner (no
            // flattening during fallback processing).
            let css_base = GoogleUrl::new(input.url());
            let css_trim = GoogleUrl::new(output.url());
            let parent_debug = Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut scratch_dom = Dom::new();
            let out_text = match super::css_image_rewriter::fallback_rewrite_urls(
                driver,
                &mut scratch_dom,
                &css_base,
                &css_trim,
                text,
                driver.options().css_image_inline_max_bytes(),
                parent_debug.clone(),
            ) {
                super::css_image_rewriter::FallbackOutcome::Rewritten(out) => out,
                super::css_image_rewriter::FallbackOutcome::Unparseable => {
                    return RewriteResult::Failed;
                }
                super::css_image_rewriter::FallbackOutcome::TransformError => {
                    server.variable(FALLBACK_FAILURES).add(1);
                    partition.debug_message.push(format!(
                        "CSS rewrite failed: Fallback transformer error in {}",
                        input.url()
                    ));
                    return RewriteResult::Failed;
                }
            };
            super::css_image_rewriter::merge_parent_debug(partition, parent_debug);
            server.variable(FALLBACK_REWRITES).add(1);
            driver.server_context().merge_non_caching_response_headers(
                input.response_headers(),
                output.response_headers_mut(),
            );
            let inputs = [input.clone()];
            if !driver.write_output(
                &inputs,
                &out_text,
                ps_http::content_type::mime_to_content_type("text/css"),
                input.charset(),
                output,
                partition,
            ) {
                return RewriteResult::Failed;
            }
            return RewriteResult::Ok;
        };
        let css_base = GoogleUrl::new(input.url());
        let css_trim = GoogleUrl::new(output.url());
        let mut stylesheet = stylesheet;
        let mut sprited = false;
        // Disabled flattening marks hierarchies failed (prefixing
        // image debug); a flatten attempt reports its own outcome.
        let mut flattening_failed = !flatten_css_imports_enabled(&driver.options());
        if flatten_css_imports_enabled(&driver.options()) {
            let mut hierarchy = CssHierarchy::new();
            hierarchy.initialize_root(
                css_base.clone(),
                css_trim.clone(),
                text.to_vec(),
                unparseable,
                driver.options().css_flatten_max_bytes(),
                stylesheet,
            );
            hierarchy.set_media(self.root_media.clone());
            hierarchy.set_charset(self.root_charset.clone());
            if let Some(reason) = &self.root_charset_failure {
                hierarchy.set_flattening_succeeded(false);
                hierarchy.add_flattening_failure_reason(reason);
            }
            sprited |= expand_and_flatten(driver, &mut hierarchy, partition);
            sprited |= sprite_hierarchy_node(driver, &mut hierarchy);
            let server_ref = driver.server_context();
            if hierarchy.flattening_succeeded() && hierarchy.flattened_result_limit() > 0 {
                hierarchy.roll_up_contents(server_ref);
            }
            hierarchy.roll_up_stylesheets(server_ref);
            if !hierarchy.flattening_failure_reason().is_empty() {
                partition
                    .debug_message
                    .push(hierarchy.flattening_failure_reason().to_string());
            }
            flattening_failed = !hierarchy.flattening_succeeded();
            stylesheet = hierarchy.into_stylesheet().expect("rolled-up sheet");
        } else if driver.options().enabled(Filter::SpriteImages) {
            let dir = css_base.all_except_leaf().to_string();
            sprited |= !super::image_combine::sprite_stylesheet_level(
                driver,
                &mut stylesheet,
                &css_base,
                &css_base,
                text,
                &dir,
            )
            .is_empty();
        }
        // Nested image contexts rewrite `url()`s in the rolled-up
        // sheet (per-level bases collapse here; no test combines
        // imports with image rewriting). The scratch DOM is never
        // touched: CSS slots have no elements.
        // NOTE: per-hierarchy-level image bases wait for a test that
        // needs them.
        let parent_debug = Rc::new(std::cell::RefCell::new(Vec::new()));
        let sheet = Rc::new(std::cell::RefCell::new(stylesheet));
        let mut scratch_dom = Dom::new();
        let css_rewritten = super::css_image_rewriter::rewrite_css_images(
            driver,
            &mut scratch_dom,
            &sheet,
            &css_base,
            &css_trim,
            driver.options().css_image_inline_max_bytes(),
            parent_debug.clone(),
            flattening_failed,
        );
        let mut stylesheet = match Rc::try_unwrap(sheet) {
            Ok(sheet) => sheet.into_inner(),
            Err(sheet) => sheet.borrow().clone(),
        };
        super::css_image_rewriter::merge_parent_debug(partition, parent_debug);
        // Nested image work counts as prior optimization even when
        // nothing needed absolutification (`Harvest`).
        let previously_optimized = css_rewritten
            || sprited
            || absolutify_harvest(
                driver,
                &css_base,
                &css_trim,
                &mut stylesheet,
                unparseable,
                css_rewritten || sprited,
            );
        let Some((mut out_text, _)) = minify_sheet(&stylesheet) else {
            server.variable(PARSE_FAILURES).add(1);
            return RewriteResult::Failed;
        };
        if has_bom {
            let mut bom = b"\xef\xbb\xbf".to_vec();
            bom.append(&mut out_text);
            out_text = bom;
        }
        let bytes_saved = in_text_size - out_text.len() as i64;
        if !driver.options().always_rewrite_css() && !previously_optimized && bytes_saved <= 0 {
            server.variable(REWRITES_DROPPED).add(1);
            partition.debug_message.push(format!(
                "CSS rewrite failed: Cannot improve {}",
                input.url()
            ));
            return RewriteResult::Failed;
        }
        // `SerializeCss` records before the write lands.
        server.variable(BLOCKS_REWRITTEN).add(1);
        server.up_down_counter(TOTAL_BYTES_SAVED).add(bytes_saved);
        server.variable(TOTAL_ORIGINAL_BYTES).add(in_text_size);
        driver.server_context().merge_non_caching_response_headers(
            input.response_headers(),
            output.response_headers_mut(),
        );
        let inputs = [input.clone()];
        if !driver.write_output(
            &inputs,
            &out_text,
            ps_http::content_type::mime_to_content_type("text/css"),
            input.charset(),
            output,
            partition,
        ) {
            return RewriteResult::Failed;
        }
        RewriteResult::Ok
    }
}

/// Builds a nested-flatten cache key: input URL plus content hash,
/// media suffix (`CacheKeySuffix`), and options signature.
fn flatten_cache_key(driver: &ResourceDriver, input: &InputResource, media: &[Vec<u8>]) -> String {
    let suffix = if media.is_empty() {
        "all".to_string()
    } else {
        media
            .iter()
            .map(|entry| String::from_utf8_lossy(entry).into_owned())
            .collect::<Vec<_>>()
            .join("_")
    };
    format!(
        "cf-flatten:{}:{}:{}:{}",
        input.url(),
        suffix,
        driver.options().signature(),
        input.contents_hash(),
    )
}

/// Expands one hierarchy level and flattens each child
/// (`RewriteCss` flatten half, sync form).
/// Sprites one hierarchy level, returning whether any value
/// sprited (`CssImageRewriter::RewriteCss` sprite half, per level).
fn sprite_hierarchy_node(driver: &Rc<ResourceDriver>, node: &mut CssHierarchy) -> bool {
    if !driver.options().enabled(Filter::SpriteImages) {
        return false;
    }
    if node.stylesheet().is_none() {
        return false;
    }
    let base = node.css_base_url().clone();
    let resolution_base = node.css_resolution_base().clone();
    let text = node.input_contents().to_vec();
    let dir = base.all_except_leaf().to_string();
    let Some(sheet) = node.stylesheet_mut() else {
        return false;
    };
    let sprited = super::image_combine::sprite_stylesheet_level(
        driver,
        sheet,
        &base,
        &resolution_base,
        &text,
        &dir,
    );
    !sprited.is_empty()
}

fn expand_and_flatten(
    driver: &Rc<ResourceDriver>,
    node: &mut CssHierarchy,
    partition: &mut CachedResult,
) -> bool {
    if !node.flattening_succeeded() {
        return false;
    }
    let server = driver.server_context().clone();
    if !node.expand_children(&server) {
        return false;
    }
    let mut sprited = false;
    for index in 0..node.children().len() {
        if !node.children()[index].needs_rewriting() {
            continue;
        }
        let (parent_charset, parent_source, parent_human) = (
            node.charset().to_vec(),
            node.charset_source().to_string(),
            node.url_for_humans(),
        );
        let child = &mut node.children_mut()[index];
        sprited |= rewrite_import(
            driver,
            &parent_charset,
            &parent_source,
            &parent_human,
            child,
            partition,
        );
    }
    sprited
}

/// Flattens one imported stylesheet
/// (`CssFlattenImportsContext::RewriteSingle` + `Harvest`, sync form).
fn rewrite_import(
    driver: &Rc<ResourceDriver>,
    parent_charset: &[u8],
    parent_source: &str,
    parent_human: &str,
    child: &mut CssHierarchy,
    partition: &mut CachedResult,
) -> bool {
    let server = driver.server_context().clone();
    let url = GoogleUrl::new(child.url());
    let (resource, authorized) = driver.create_input_resource(
        &url,
        InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
        IntendedFor::General,
        InputRole::Style,
    );
    let Some(mut input) = resource else {
        child.set_flattening_succeeded(false);
        child.add_flattening_failure_reason(&cannot_import_message(
            "import",
            &child.url_for_humans(),
            authorized,
        ));
        return false;
    };
    let mut ctx = driver.request_context().borrow().clone();
    let mut callback = MockLoadCallback::new();
    input.load(
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    let (safe, reason) = input.is_safe_to_rewrite(false);
    if !input.loaded() || !input.http_status_ok() || !safe {
        // Load failures propagate their reason (Debug-gated at
        // render) but record no flattening statistic.
        child.set_flattening_succeeded(false);
        if !reason.is_empty() {
            partition.debug_message.push(reason);
        }
        return false;
    }
    // NOTE: nested `if` outputs stay unwritten (no test serves
    // them); only the flattened bytes are cached.
    let key = flatten_cache_key(driver, &input, child.media());
    let cached = server.metadata_cache().get(&key);
    if cached.state == KeyState::Available {
        if let Some(stored) = cached.value {
            if stored.first() == Some(&1) {
                child.set_minified_contents(stored[1..].to_vec());
                child.set_input_contents(stored[1..].to_vec());
                // Re-parse the cached bytes (re-derives
                // complex-query failures, exactly like `Render`).
                child.parse(&server);
            } else {
                // Cached failure: silent, like `Render`.
                child.set_flattening_succeeded(false);
            }
            return false;
        }
    }
    // Resolve child URLs against the child/trim bases before parsing.
    // NOTE: `trim_urls_in_css()` is hardcoded true upstream (test seam).
    let contents = input.contents();
    let trim_urls = driver.server_context().url_namer().proxy_mode()
        != crate::url_namer::ProxyExtent::Full
        && driver.options().enabled(Filter::LeftTrimUrls);
    let options = driver.options();
    let namer = server.url_namer().clone();
    let base = child.css_base_url().clone();
    let trim = child.css_trim_url().clone();
    let mut transformer = crate::server_context::RewriteDomainTransformer::new(
        &base,
        &trim,
        driver.lawyer(),
        &options,
        &*namer,
        server.hasher().hash_size_in_chars(),
        driver.signature_length(),
        trim_urls,
    );
    match ps_css::scanner::transform_urls(&contents, &mut transformer) {
        Some(resolved) => {
            child.set_input_contents(resolved);
            child.set_input_contents_resolved(true);
        }
        None => child.set_input_contents(contents),
    }
    drop(options);
    if !child.parse(&server) {
        server.variable(FLATTEN_MINIFY_FAILED).add(1);
        child.set_flattening_succeeded(false);
        let human = child.url_for_humans();
        child.add_flattening_failure_reason(&format!("Cannot parse the CSS in {human}"));
        server.metadata_cache().put(&key, &[0]);
        return false;
    }
    let header_charset = input.response_headers().determine_charset();
    if let Err(reason) = child.check_charset_ok(
        header_charset.as_deref(),
        parent_charset,
        parent_source,
        parent_human,
    ) {
        server.variable(FLATTEN_CHARSET_MISMATCH).add(1);
        child.set_flattening_succeeded(false);
        child.add_flattening_failure_reason(&reason);
        server.metadata_cache().put(&key, &[0]);
        return false;
    }
    let sprited =
        expand_and_flatten(driver, child, partition) || sprite_hierarchy_node(driver, child);
    child.roll_up_contents(&server);
    let mut value = vec![1];
    value.extend_from_slice(child.minified_contents());
    server.metadata_cache().put(&key, &value);
    sprited
}

impl SingleRewriteBehavior for CssBehavior {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &CSS_ENCODER
    }

    fn kind(&self) -> OutputResourceKind {
        OutputResourceKind::Rewritten
    }

    fn key_suffix_extra(&self) -> String {
        user_agent_cache_key(&self.driver)
    }

    fn policy_permits_rendering(
        &self,
        _driver: &Rc<ResourceDriver>,
        _outputs: &[Option<OutputResource>],
    ) -> bool {
        // NOTE: `AreOutputsAllowedByCsp(kStyleSrc)` waits for CSP.
        true
    }

    fn note_render(
        &self,
        driver: &Rc<ResourceDriver>,
        _dom: &mut Dom,
        partitions: &OutputPartitions,
        slots: &[ContextSlot],
    ) {
        if partitions.partition.len() != 1 || slots.is_empty() {
            return;
        }
        if partitions.partition[0].optimizable {
            driver.server_context().variable(USES).add(1);
        }
    }

    fn rewrite_single(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        output: &mut OutputResource,
        partition: &mut CachedResult,
    ) -> RewriteResult {
        self.rewrite_css(driver, input, output, partition)
    }

    fn send_fallback_response(
        &self,
        driver: &Rc<ResourceDriver>,
        inputs: &[InputResource],
        original_url: &str,
        contents: &[u8],
        fetch: &mut dyn ps_http::fetch::AsyncFetch,
    ) -> bool {
        let allowed = fetch
            .response_headers()
            .determine_content_type()
            .is_some_and(|content_type| content_type.is_css());
        if !allowed {
            return false;
        }
        fetch.headers_complete();
        let Some(input) = inputs.first() else {
            return fetch.handle_write(contents);
        };
        let input_base = GoogleUrl::new(input.url());
        let (_, resolved) = driver.resolve_css_urls(&input_base, original_url, contents);
        fetch.handle_write(&resolved)
    }
}

/// Absolutifies a parsed stylesheet at harvest (`Harvest`
/// absolutification): imports first, then URLs, returning whether
/// anything changed (which counts as prior optimization).
fn absolutify_harvest(
    driver: &ResourceDriver,
    css_base: &GoogleUrl,
    css_trim: &GoogleUrl,
    stylesheet: &mut Stylesheet,
    unparseable_detected: bool,
    css_rewritten: bool,
) -> bool {
    // Must invoke ShouldAbsolutifyUrl first (proxying verdict).
    let (should_absolutify, proxying) = driver.should_absolutify_url(css_base, css_trim);
    let mut absolutified = false;
    if should_absolutify {
        absolutified = ps_css::absolutify::absolutify_imports(stylesheet, &|link| {
            let resolved = GoogleUrl::resolve(css_base, &String::from_utf8_lossy(link));
            if resolved.is_web_valid() {
                Some(resolved.spec().as_bytes().to_vec())
            } else {
                None
            }
        });
    }
    if should_absolutify || proxying {
        let options = driver.options();
        let namer = driver.server_context().url_namer().clone();
        let mut transformer = crate::server_context::RewriteDomainTransformer::new(
            css_base,
            css_base,
            driver.lawyer(),
            &options,
            &*namer,
            driver.server_context().hasher().hash_size_in_chars(),
            driver.signature_length(),
            false,
        );
        absolutified |= ps_css::absolutify::absolutify_urls(
            stylesheet,
            &mut transformer,
            !css_rewritten,
            unparseable_detected,
        );
    }
    absolutified
}

/// Inline CSS kind (`InlineCssKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineCssKind {
    InsideStyleTag,
    AttributeWithoutUrls,
    AttributeWithUrls,
}

/// CSS rewriting filter (`CssFilter`).
pub struct CssFilter {
    driver: Rc<ResourceDriver>,
    in_style_element: bool,
    rewrite_style_attributes: bool,
    rewrite_style_attributes_with_url: bool,
    html_path_active: bool,
    meta_tag_charset: Vec<u8>,
}

impl CssFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        CssFilter {
            driver,
            in_style_element: false,
            rewrite_style_attributes: false,
            rewrite_style_attributes_with_url: false,
            html_path_active: true,
            meta_tag_charset: Vec::new(),
        }
    }

    /// Applicable media of an element (`GetApplicableMedia`).
    fn applicable_media(dom: &mut Dom, id: NodeId) -> Vec<Vec<u8>> {
        let Some(index) = dom.find_attribute(id, Keyword::Media) else {
            return Vec::new();
        };
        let (decoded, _) = dom.decoded_attribute(id, index);
        decoded
            .map(|value| ps_css::util::vectorize_media_attribute(&value))
            .unwrap_or_default()
    }

    /// Applicable HTML charset (`GetApplicableCharset`): headers, else
    /// meta tag, else ISO-8859-1; `Err` when the element's charset
    /// attribute disagrees.
    fn applicable_charset(&self, dom: &mut Dom, id: Option<NodeId>) -> Result<Vec<u8>, String> {
        let mut our_charset = b"iso-8859-1".to_vec();
        let mut source = "the default";
        let mut headers_charset = Vec::new();
        if let Some(headers) = self.driver.response_headers().as_ref() {
            if let Some(charset) = headers.determine_charset() {
                headers_charset = charset.into_bytes();
                our_charset = headers_charset.clone();
                source = "from headers";
            }
        }
        if headers_charset.is_empty() && !self.meta_tag_charset.is_empty() {
            our_charset = self.meta_tag_charset.clone();
            source = "from a meta tag";
        }
        if let Some(id) = id {
            if let Some(index) = dom.find_attribute(id, Keyword::Charset) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                let elements_charset = decoded.unwrap_or_default();
                if our_charset != elements_charset {
                    return Err(format!(
                        "The charset of the HTML ({}, {}) is different from the charset attribute on the preceding element ({})",
                        String::from_utf8_lossy(&our_charset),
                        source,
                        if elements_charset.is_empty() {
                            "not set".to_string()
                        } else {
                            String::from_utf8_lossy(&elements_charset).into_owned()
                        },
                    ));
                }
            }
        }
        Ok(our_charset)
    }

    /// Rewrites an external stylesheet (`StartExternalRewrite`).
    fn rewrite_external_style(&self, dom: &mut Dom, id: NodeId, href: usize) {
        if ScriptTagScanner::has_integrity_attribute(dom, id) {
            self.driver.insert_debug_comment(
                dom,
                "CSS not rewritten: integrity attribute pins the stylesheet bytes",
                id,
            );
            return;
        }
        let (decoded, _) = dom.decoded_attribute(id, href);
        let Some(value) = decoded else {
            return;
        };
        let url = ps_http::google_url::GoogleUrl::resolve(
            self.driver.base_url(),
            &String::from_utf8_lossy(&value),
        );
        if !url.is_web_valid() {
            return;
        }
        let (resource, _authorized) = self.driver.create_input_resource(
            &url,
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Style,
        );
        let Some(input) = resource else {
            return;
        };
        let handle: Rc<dyn Resource> = Rc::new(input.clone());
        let slot = self.driver.get_slot(Some(handle), dom, id, href, "cf");
        if self.driver.options().css_preserve_urls() {
            slot.borrow_mut().base_mut().set_preserve_urls(true);
        }
        let root_media = Self::applicable_media(dom, id);
        let (root_charset, root_charset_failure) = match self.applicable_charset(dom, Some(id)) {
            Ok(charset) => (charset, None),
            Err(reason) => {
                self.driver
                    .server_context()
                    .variable(FLATTEN_CHARSET_MISMATCH)
                    .add(1);
                (Vec::new(), Some(reason))
            }
        };
        let behavior = CssBehavior {
            driver: self.driver.clone(),
            root_media,
            root_charset,
            root_charset_failure,
        };
        let mut context = SingleRewriteContext::new(self.driver.clone(), behavior);
        context.add_slot(slot, input);
        context.start(dom);
    }

    /// Rewrites an inline `<style>` body (`StartInlineRewrite`).
    fn rewrite_inline_style(&self, dom: &mut Dom, node: NodeId) {
        // NOTE: `CanModifyInlineStyle` waits for the CSP subsystem.
        let style = dom.leaf_contents(node);
        if style.is_empty() {
            // Empty blocks never reach the filter in C++ (no
            // CharactersNode), so they record nothing.
            return;
        }
        let server = self.driver.server_context().clone();
        // Debug notes anchor on the style element, like C++ slot
        // rendering (never inside the style body).
        let anchor = dom.arena_node(node).parent().unwrap_or(node);
        let in_text_size = style.len() as i64;
        if let Some(message) = over_parse_max_bytes(
            &self.driver,
            in_text_size,
            self.driver.base_url().spec(),
            "CSS not rewritten",
        ) {
            self.driver.insert_debug_comment(dom, &message, anchor);
            return;
        }
        // Fallback results (and remembered failures) resolve before
        // parsing, so repeats skip the reparse.
        let fallback_key =
            inline_cache_key(&self.driver, &style, &[], InlineCssKind::InsideStyleTag);
        let cached_fallback = self
            .driver
            .server_context()
            .metadata_cache()
            .get(&fallback_key);
        if cached_fallback.state == KeyState::Available {
            if let Some(stored) = cached_fallback.value {
                *dom.leaf_contents_mut(node) = stored;
                server.variable(USES).add(1);
                return;
            }
        }
        let Some((mut stylesheet, unparseable)) = parse_stylesheet(&style) else {
            server.variable(PARSE_FAILURES).add(1);
            self.driver.insert_debug_comment(
                dom,
                &format!(
                    "CSS rewrite failed: Parse error in {}",
                    self.driver.base_url().spec()
                ),
                anchor,
            );
            if self
                .driver
                .options()
                .enabled(Filter::FallbackRewriteCssUrls)
            {
                self.rewrite_inline_style_fallback(dom, node, anchor, &style);
            }
            return;
        };
        let css_base = self.driver.base_url().clone();
        // Inline styles flatten (attributes do not); the scratch
        // partition carries load-failure reasons to debug output.
        let mut stylesheet = stylesheet;
        let mut scratch = CachedResult::new();
        let mut sprited = false;
        let mut flattening_failed = !flatten_css_imports_enabled(&self.driver.options());
        if flatten_css_imports_enabled(&self.driver.options()) {
            let mut hierarchy = CssHierarchy::new();
            hierarchy.initialize_root(
                css_base.clone(),
                css_base.clone(),
                style.clone(),
                unparseable,
                self.driver.options().css_flatten_max_bytes(),
                stylesheet,
            );
            hierarchy.set_media(Self::applicable_media(dom, anchor));
            match self.applicable_charset(dom, None) {
                Ok(charset) => hierarchy.set_charset(charset),
                Err(reason) => {
                    // Unreachable: no element means no mismatch.
                    hierarchy.set_flattening_succeeded(false);
                    hierarchy.add_flattening_failure_reason(&reason);
                }
            }
            sprited |= expand_and_flatten(&self.driver, &mut hierarchy, &mut scratch);
            sprited |= sprite_hierarchy_node(&self.driver, &mut hierarchy);
            if hierarchy.flattening_succeeded() && hierarchy.flattened_result_limit() > 0 {
                hierarchy.roll_up_contents(&server);
            }
            hierarchy.roll_up_stylesheets(&server);
            if !hierarchy.flattening_failure_reason().is_empty() {
                self.driver.insert_debug_comment(
                    dom,
                    &hierarchy.flattening_failure_reason().to_string(),
                    anchor,
                );
            }
            let flatten_messages: Vec<String> = scratch.debug_message.drain(..).collect();
            self.driver
                .insert_debug_comments(dom, &flatten_messages, anchor);
            flattening_failed = !hierarchy.flattening_succeeded();
            stylesheet = hierarchy.into_stylesheet().expect("rolled-up sheet");
        } else if self.driver.options().enabled(Filter::SpriteImages) {
            let dir = css_base.all_except_leaf().to_string();
            sprited |= !super::image_combine::sprite_stylesheet_level(
                &self.driver,
                &mut stylesheet,
                &css_base,
                &css_base,
                &style,
                &dir,
            )
            .is_empty();
        }
        // Metadata hits skip nested work and stats (`kExpectCached`).
        // The key carries the input URLs so nested bytes resolve
        // before any rewriting runs.
        let key_uris = collect_stylesheet_uris(&stylesheet);
        let key = inline_cache_key(
            &self.driver,
            &style,
            &key_uris,
            InlineCssKind::InsideStyleTag,
        );
        let cached = self.driver.server_context().metadata_cache().get(&key);
        if cached.state == KeyState::Available {
            if let Some(stored) = cached.value {
                *dom.leaf_contents_mut(node) = stored;
                server.variable(USES).add(1);
                return;
            }
        }
        // Nested image contexts (`ImageInlineMaxBytes` for inline
        // elements is the tighter of the HTML and CSS budgets).
        let options = self.driver.options();
        let image_inline_max = options
            .image_inline_max_bytes()
            .min(options.css_image_inline_max_bytes());
        drop(options);
        let parent_debug = Rc::new(std::cell::RefCell::new(Vec::new()));
        let sheet = Rc::new(std::cell::RefCell::new(stylesheet));
        let css_rewritten = super::css_image_rewriter::rewrite_css_images(
            &self.driver,
            dom,
            &sheet,
            &css_base,
            &css_base,
            image_inline_max,
            parent_debug.clone(),
            flattening_failed,
        );
        let mut stylesheet = match Rc::try_unwrap(sheet) {
            Ok(sheet) => sheet.into_inner(),
            Err(sheet) => sheet.borrow().clone(),
        };
        super::css_image_rewriter::merge_parent_debug(&mut scratch, parent_debug);
        // NOTE: cached inline blocks re-render stored debug in batch 5.
        let nested_messages: Vec<String> = scratch.debug_message.drain(..).collect();
        self.driver
            .insert_debug_comments(dom, &nested_messages, anchor);
        let previously_optimized = css_rewritten
            || sprited
            || absolutify_harvest(
                &self.driver,
                &css_base,
                &css_base,
                &mut stylesheet,
                unparseable,
                css_rewritten || sprited,
            );
        let Some((out_text, _)) = minify_sheet(&stylesheet) else {
            server.variable(PARSE_FAILURES).add(1);
            return;
        };
        let bytes_saved = in_text_size - out_text.len() as i64;
        if !self.driver.options().always_rewrite_css() && !previously_optimized && bytes_saved <= 0
        {
            server.variable(REWRITES_DROPPED).add(1);
            self.driver.insert_debug_comment(
                dom,
                &format!(
                    "CSS rewrite failed: Cannot improve {}",
                    self.driver.base_url().spec()
                ),
                anchor,
            );
            return;
        }
        // `SerializeCss` records before the markup gate refuses.
        server.variable(BLOCKS_REWRITTEN).add(1);
        server.up_down_counter(TOTAL_BYTES_SAVED).add(bytes_saved);
        server.variable(TOTAL_ORIGINAL_BYTES).add(in_text_size);
        if ps_css::inline::may_contain_markup(&out_text) {
            self.driver.insert_debug_comment(
                dom,
                "CSS not rewritten since it contains a less-than sign",
                anchor,
            );
            return;
        }
        self.driver
            .server_context()
            .metadata_cache()
            .put(&key, &out_text);
        *dom.leaf_contents_mut(node) = out_text;
        server.variable(USES).add(1);
    }

    /// Rewrites an unparseable `<style>` body with the tag scanner
    /// (fallback path).
    fn rewrite_inline_style_fallback(
        &self,
        dom: &mut Dom,
        node: NodeId,
        anchor: NodeId,
        style: &[u8],
    ) {
        let server = self.driver.server_context().clone();
        let css_base = self.driver.base_url().clone();
        let options = self.driver.options();
        let image_inline_max = options
            .image_inline_max_bytes()
            .min(options.css_image_inline_max_bytes());
        drop(options);
        let parent_debug = Rc::new(std::cell::RefCell::new(Vec::new()));
        let out_text = match super::css_image_rewriter::fallback_rewrite_urls(
            &self.driver,
            dom,
            &css_base,
            &css_base,
            style,
            image_inline_max,
            parent_debug.clone(),
        ) {
            super::css_image_rewriter::FallbackOutcome::Rewritten(out) => out,
            super::css_image_rewriter::FallbackOutcome::Unparseable => return,
            super::css_image_rewriter::FallbackOutcome::TransformError => {
                server.variable(FALLBACK_FAILURES).add(1);
                self.driver.insert_debug_comment(
                    dom,
                    &format!(
                        "CSS rewrite failed: Fallback transformer error in {}",
                        self.driver.base_url().spec()
                    ),
                    anchor,
                );
                return;
            }
        };
        super::css_image_rewriter::insert_parent_debug(&self.driver, dom, parent_debug, anchor);
        server.variable(FALLBACK_REWRITES).add(1);
        let key = inline_cache_key(&self.driver, style, &[], InlineCssKind::InsideStyleTag);
        self.driver
            .server_context()
            .metadata_cache()
            .put(&key, &out_text);
        *dom.leaf_contents_mut(node) = out_text;
        server.variable(USES).add(1);
    }

    /// Rewrites a `style` attribute (`StartAttributeRewrite`).
    fn rewrite_style_attribute(
        &self,
        dom: &mut Dom,
        id: NodeId,
        index: usize,
        kind: InlineCssKind,
    ) {
        // NOTE: `HasDirectiveOrDefaultSrc(kStyleSrc)` waits for CSP.
        let (decoded, _) = dom.decoded_attribute(id, index);
        let Some(value) = decoded else {
            return;
        };
        if value.is_empty() {
            return;
        }
        let server = self.driver.server_context().clone();
        let in_text_size = value.len() as i64;
        if let Some(message) = over_parse_max_bytes(
            &self.driver,
            in_text_size,
            self.driver.base_url().spec(),
            "CSS not rewritten",
        ) {
            self.driver.insert_debug_comment(dom, &message, id);
            return;
        }
        let Some((mut declarations, unparseable)) = parse_declarations(&value) else {
            server.variable(PARSE_FAILURES).add(1);
            self.driver.insert_debug_comment(
                dom,
                &format!(
                    "CSS rewrite failed: Parse error in {}",
                    self.driver.base_url().spec()
                ),
                id,
            );
            if self
                .driver
                .options()
                .enabled(Filter::FallbackRewriteCssUrls)
            {
                self.rewrite_style_attribute_fallback(dom, id, index, kind, &value);
            }
            return;
        };
        let css_base = self.driver.base_url().clone();
        // Metadata hits skip nested work and stats.
        let key_uris = collect_uris(&declarations);
        let key = inline_cache_key(&self.driver, &value, &key_uris, kind);
        let cached = self.driver.server_context().metadata_cache().get(&key);
        if cached.state == KeyState::Available {
            if let Some(stored) = cached.value {
                dom.element_attributes_mut(id)[index].set_value(&stored);
                server.variable(USES).add(1);
                return;
            }
        }
        // Nested image contexts (inline elements take the tighter of
        // the HTML and CSS inline budgets).
        let options = self.driver.options();
        let image_inline_max = options
            .image_inline_max_bytes()
            .min(options.css_image_inline_max_bytes());
        drop(options);
        let parent_debug = Rc::new(std::cell::RefCell::new(Vec::new()));
        let decls = Rc::new(std::cell::RefCell::new(declarations));
        let css_rewritten = super::css_image_rewriter::rewrite_attribute_images(
            &self.driver,
            dom,
            &decls,
            &css_base,
            &css_base,
            image_inline_max,
            parent_debug.clone(),
            !flatten_css_imports_enabled(&self.driver.options()),
        );
        let mut declarations = match Rc::try_unwrap(decls) {
            Ok(decls) => decls.into_inner(),
            Err(decls) => decls.borrow().clone(),
        };
        super::css_image_rewriter::insert_parent_debug(&self.driver, dom, parent_debug, id);
        let (should_absolutify, proxying) = self.driver.should_absolutify_url(&css_base, &css_base);
        let mut previously_optimized = css_rewritten;
        if should_absolutify || proxying {
            let options = self.driver.options();
            let namer = self.driver.server_context().url_namer().clone();
            let mut transformer = crate::server_context::RewriteDomainTransformer::new(
                &css_base,
                &css_base,
                self.driver.lawyer(),
                &options,
                &*namer,
                self.driver.server_context().hasher().hash_size_in_chars(),
                self.driver.signature_length(),
                false,
            );
            previously_optimized |= ps_css::absolutify::absolutify_declarations(
                &mut declarations,
                &mut transformer,
                !css_rewritten,
                unparseable,
            );
        }
        let Some((out_text, _)) = minify_decls(&declarations) else {
            server.variable(PARSE_FAILURES).add(1);
            return;
        };
        let bytes_saved = in_text_size - out_text.len() as i64;
        if !self.driver.options().always_rewrite_css() && !previously_optimized && bytes_saved <= 0
        {
            server.variable(REWRITES_DROPPED).add(1);
            return;
        }
        server.variable(BLOCKS_REWRITTEN).add(1);
        server.up_down_counter(TOTAL_BYTES_SAVED).add(bytes_saved);
        server.variable(TOTAL_ORIGINAL_BYTES).add(in_text_size);
        if ps_css::inline::may_contain_markup(&out_text) {
            self.driver.insert_debug_comment(
                dom,
                "CSS not rewritten since it contains a less-than sign",
                id,
            );
            return;
        }
        self.driver
            .server_context()
            .metadata_cache()
            .put(&key, &out_text);
        dom.element_attributes_mut(id)[index].set_value(&out_text);
        server.variable(USES).add(1);
    }

    /// Rewrites an unparseable `style` attribute with the tag scanner
    /// (fallback path).
    fn rewrite_style_attribute_fallback(
        &self,
        dom: &mut Dom,
        id: NodeId,
        index: usize,
        kind: InlineCssKind,
        value: &[u8],
    ) {
        let server = self.driver.server_context().clone();
        let css_base = self.driver.base_url().clone();
        let options = self.driver.options();
        let image_inline_max = options
            .image_inline_max_bytes()
            .min(options.css_image_inline_max_bytes());
        drop(options);
        let parent_debug = Rc::new(std::cell::RefCell::new(Vec::new()));
        let out_text = match super::css_image_rewriter::fallback_rewrite_urls(
            &self.driver,
            dom,
            &css_base,
            &css_base,
            value,
            image_inline_max,
            parent_debug.clone(),
        ) {
            super::css_image_rewriter::FallbackOutcome::Rewritten(out) => out,
            super::css_image_rewriter::FallbackOutcome::Unparseable => return,
            super::css_image_rewriter::FallbackOutcome::TransformError => {
                server.variable(FALLBACK_FAILURES).add(1);
                self.driver.insert_debug_comment(
                    dom,
                    &format!(
                        "CSS rewrite failed: Fallback transformer error in {}",
                        self.driver.base_url().spec()
                    ),
                    id,
                );
                return;
            }
        };
        super::css_image_rewriter::insert_parent_debug(&self.driver, dom, parent_debug, id);
        server.variable(FALLBACK_REWRITES).add(1);
        let key = inline_cache_key(&self.driver, value, &[], kind);
        self.driver
            .server_context()
            .metadata_cache()
            .put(&key, &out_text);
        dom.element_attributes_mut(id)[index].set_value(&out_text);
        server.variable(USES).add(1);
    }
}

impl HtmlFilter for CssFilter {
    fn name(&self) -> &'static str {
        "Css"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.in_style_element = false;
        self.meta_tag_charset.clear();
        let options = self.driver.options();
        self.rewrite_style_attributes = options.enabled(Filter::RewriteStyleAttributes);
        self.rewrite_style_attributes_with_url =
            options.enabled(Filter::RewriteStyleAttributesWithUrl);
        // `AddFilters` skips the filter on the HTML path when URLs
        // are preserved without preemptive IPRO rewrites.
        self.html_path_active =
            !(options.css_preserve_urls() && !options.in_place_preemptive_rewrite_css());
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if !self.html_path_active {
            return;
        }
        if dom.element_keyword(id) == Keyword::Style {
            self.in_style_element = true;
            return;
        }
        if !self.driver.can_rewrite_resources() {
            return;
        }
        let check_for_url =
            !self.rewrite_style_attributes && self.rewrite_style_attributes_with_url;
        if !self.rewrite_style_attributes && !check_for_url {
            return;
        }
        let Some(index) = dom.find_attribute(id, Keyword::Style) else {
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, index);
        let has_url = decoded
            .as_deref()
            .is_some_and(|value| value.windows(4).any(|window| window == b"url("));
        if !check_for_url || has_url {
            let kind = if has_url {
                InlineCssKind::AttributeWithUrls
            } else {
                InlineCssKind::AttributeWithoutUrls
            };
            self.rewrite_style_attribute(dom, id, index, kind);
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, node: NodeId) {
        if self.in_style_element && self.driver.can_rewrite_resources() {
            self.rewrite_inline_style(dom, node);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.in_style_element {
            self.in_style_element = false;
        }
        if !self.html_path_active {
            return;
        }
        // NOTE: `IsRewritable` collapses to the DOM verdict.
        if !dom.is_rewritable(id) {
            return;
        }
        let url_valued = self.driver.options().url_valued_attributes().to_vec();
        for (index, category) in scan_element(dom, id, &url_valued) {
            if category == Category::Stylesheet {
                self.rewrite_external_style(dom, id, index);
            }
        }
        if self.meta_tag_charset.is_empty() && dom.element_keyword(id) == Keyword::Meta {
            if let Some((_, _, charset)) = extract_meta_tag_details(dom, id, None) {
                self.meta_tag_charset = charset;
            }
        }
    }
}

impl RewriteFilter for CssFilter {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn rewrite_in_place(
        &self,
        _driver: &Rc<ResourceDriver>,
        input: &[u8],
        content_type: &ps_http::content_type::ContentType,
    ) -> Option<crate::rewrite_filter::InPlaceOutput> {
        // Minify-only subset: nested @import flattening and
        // image-URL rewriting need slot contexts (the full
        // `.pagespeed.` path stays authoritative for those).
        let bytes = ps_css::minify::minify_css(input)?;
        Some(crate::rewrite_filter::InPlaceOutput {
            bytes,
            content_type: *content_type,
            optimized_image_type: None,
        })
    }

    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &CSS_ENCODER
    }

    fn fetch(
        &self,
        driver: &Rc<ResourceDriver>,
        output: OutputResource,
        fetch: &mut dyn ps_http::fetch::AsyncFetch,
    ) -> bool {
        let behavior = CssBehavior {
            driver: driver.clone(),
            root_media: Vec::new(),
            root_charset: Vec::new(),
            root_charset_failure: None,
        };
        let mut context = SingleRewriteContext::new(driver.clone(), behavior);
        context.fetch(output, fetch)
    }
}

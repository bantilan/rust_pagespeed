//! `@import` flattening (`css_hierarchy.{h,cc}` pure core).
//!
//! Builds the import tree with a caller-supplied fetcher, reduces
//! media, checks charsets and recursion, enforces
//! `css_flatten_max_bytes`, and merges the surviving stylesheets.
//! Async fetching, image slots, and output plumbing live in later
//! steps; URL resolution/trimming arrives via callbacks.
#![forbid(unsafe_code)]

use crate::ast::Stylesheet;
use crate::minify::Minifier;
use crate::parser::Parser;
use crate::stats::CssStats;
use crate::util::{
    clear_vector_if_contains_media_all, eliminate_elements_not_in, media_queries_to_strings,
    strings_to_media_queries,
};

/// Failure prefix (`kFailureReasonPrefix`).
pub const FAILURE_REASON_PREFIX: &str = "Flattening failed: ";

/// Fetched stylesheet (`CssHierarchy` input).
pub struct FetchedCss {
    /// Stylesheet bytes.
    pub bytes: Vec<u8>,
    /// Charset from response headers (may be empty).
    pub charset_from_headers: Vec<u8>,
}

/// Fetches nested stylesheets by resolved URL.
pub trait Fetch {
    /// Returns the stylesheet, or `None` when it cannot be fetched.
    fn fetch(&mut self, url: &[u8]) -> Option<FetchedCss>;
}

/// Resolves and rebases URLs (URL-layer callback).
pub trait Resolve {
    /// Resolves `link` against `base`; `None` means invalid.
    fn resolve(&self, base: &[u8], link: &[u8]) -> Option<Vec<u8>>;

    /// Whether two bases have different directories
    /// (`AllExceptLeaf` comparison for absolutification).
    fn dir_differs(&self, first: &[u8], second: &[u8]) -> bool;

    /// Rewrites nested contents' URLs from `base` toward `trim`
    /// (nested `RewriteDomainTransformer` pass); `None` keeps the
    /// raw contents, as on scanner failure.
    fn resolve_nested_contents(&self, contents: &[u8], base: &[u8], trim: &[u8])
        -> Option<Vec<u8>>;
}

/// Flattening outcome.
#[derive(Debug)]
pub struct FlattenOutcome {
    /// Minified output text.
    pub text: Vec<u8>,
    /// Whether imports were fully flattened.
    pub flattened: bool,
    /// Human-readable failure reason (empty on success).
    pub failure_reason: Vec<u8>,
    /// Whether unparseable sections were seen.
    pub unparseable_detected: bool,
}

/// Flattens a stylesheet (`CssFilter` flatten flow, pure core).
///
/// - `base_url`: the entry stylesheet's URL (resolution base).
/// - `trim_url`: the output URL (trim target, usually same dir).
/// - `contents`: entry bytes (must already be BOM-stripped).
/// - `root_charset`: entry charset from HTML context (may be empty).
/// - `root_media`: entry media from HTML context (empty means all).
/// - `max_bytes`: `css_flatten_max_bytes` (<= 0 disables the limit).
#[allow(clippy::too_many_arguments)]
pub fn flatten(
    base_url: &[u8],
    trim_url: &[u8],
    contents: &[u8],
    root_charset: &[u8],
    root_media: &[Vec<u8>],
    max_bytes: i64,
    resolve: &dyn Resolve,
    fetch: &mut dyn Fetch,
    stats: &mut CssStats,
) -> FlattenOutcome {
    let mut flattener = Flattener {
        trim_url: trim_url.to_vec(),
        max_bytes,
        resolve,
        fetch,
        stats,
    };
    flattener.flatten_root(base_url, contents, root_charset, root_media)
}

struct Flattener<'a> {
    trim_url: Vec<u8>,
    max_bytes: i64,
    resolve: &'a dyn Resolve,
    fetch: &'a mut dyn Fetch,
    stats: &'a mut CssStats,
}

/// One tree node (`CssHierarchy`).
struct Node {
    url: Vec<u8>,
    base_url: Vec<u8>,
    contents: Vec<u8>,
    stylesheet: Option<Stylesheet>,
    media: Vec<Vec<u8>>,
    children: Vec<Node>,
    charset: Vec<u8>,
    charset_source: Vec<u8>,
    flattening_succeeded: bool,
    unparseable_detected: bool,
    failure_reason: Vec<u8>,
}

impl Node {
    fn new(url: Vec<u8>, base_url: Vec<u8>) -> Node {
        Node {
            url,
            base_url,
            contents: Vec::new(),
            stylesheet: None,
            media: Vec::new(),
            children: Vec::new(),
            charset: Vec::new(),
            charset_source: b"from unknown".to_vec(),
            flattening_succeeded: true,
            unparseable_detected: false,
            failure_reason: Vec::new(),
        }
    }

    /// Human URL for messages (`url_for_humans`).
    fn url_for_humans(&self) -> &[u8] {
        if self.url.is_empty() {
            b"inline"
        } else if self.url.starts_with(b"data:") {
            b"data URL"
        } else {
            &self.url
        }
    }

    /// Appends a deduplicated failure reason
    /// (`AddFlatteningFailureReason`).
    fn add_failure_reason(&mut self, reason: &[u8]) {
        if reason.is_empty() {
            return;
        }
        let mut trimmed = reason;
        if let Some(rest) = trimmed.strip_prefix(FAILURE_REASON_PREFIX.as_bytes()) {
            trimmed = rest;
        }
        if contains_ignore_case(&self.failure_reason, trimmed) {
            return;
        }
        if self.flattening_succeeded {
            if !self.failure_reason.is_empty() {
                self.failure_reason.extend_from_slice(b" AND ");
            }
        } else if self.failure_reason.is_empty() {
            self.failure_reason
                .extend_from_slice(FAILURE_REASON_PREFIX.as_bytes());
        } else {
            if !contains_ignore_case(&self.failure_reason, FAILURE_REASON_PREFIX.as_bytes()) {
                let mut prefixed = FAILURE_REASON_PREFIX.as_bytes().to_vec();
                prefixed.extend_from_slice(&self.failure_reason);
                self.failure_reason = prefixed;
            }
            self.failure_reason.extend_from_slice(b" AND ");
        }
        self.failure_reason.extend_from_slice(trimmed);
    }
}

fn contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Parses preservation/no-quirks, failing closed on any error.
fn parse_stylesheet(contents: &[u8]) -> Option<(Stylesheet, bool)> {
    let mut parser = Parser::new(contents);
    parser.set_preservation_mode(true);
    parser.set_quirks_mode(false);
    let stylesheet = parser.parse_raw_stylesheet();
    if parser.errors_seen_mask() != 0 {
        return None;
    }
    let unparseable = parser.unparseable_sections_seen_mask() != 0;
    Some((stylesheet, unparseable))
}

/// Minified own stylesheet, or original contents on failure.
fn minify_own(node: &Node) -> Vec<u8> {
    let stylesheet = node.stylesheet.as_ref().expect("parsed");
    minify_stylesheet(stylesheet).unwrap_or_else(|| node.contents.clone())
}

/// Minifies a stylesheet; `None` on failure.
fn minify_stylesheet(stylesheet: &Stylesheet) -> Option<Vec<u8>> {
    let mut minifier = Minifier::new(false);
    minifier.minify_stylesheet(stylesheet);
    let (out, ok) = minifier.finish();
    if ok {
        Some(out)
    } else {
        None
    }
}

impl Flattener<'_> {
    fn flatten_root(
        &mut self,
        base_url: &[u8],
        contents: &[u8],
        root_charset: &[u8],
        root_media: &[Vec<u8>],
    ) -> FlattenOutcome {
        let Some((stylesheet, unparseable)) = parse_stylesheet(contents) else {
            self.stats.add(crate::stats::PARSE_FAILURES, 1);
            return FlattenOutcome {
                text: contents.to_vec(),
                flattened: false,
                failure_reason: b"CSS rewrite failed: Parse error".to_vec(),
                unparseable_detected: false,
            };
        };
        let mut root = Node::new(Vec::new(), base_url.to_vec());
        root.contents = contents.to_vec();
        root.stylesheet = Some(stylesheet);
        root.unparseable_detected = unparseable;
        root.charset = root_charset.to_vec();
        root.media = root_media.to_vec();
        // The root reduces its own ruleset media like every node.
        if !self.reduce_node_media(&mut root) {
            root.flattening_succeeded = false;
        }
        self.expand(&mut root, &[]);
        // Limit check through contents rollup (only when flattening
        // holds and a limit is configured).
        if root.flattening_succeeded && self.max_bytes > 0 {
            let _ = self.rollup_contents(&mut root);
        }
        // Merge stylesheets (or keep the root as-is on failure).
        self.rollup_stylesheets(&mut root);
        // Absolutify the merged output when the trim dir differs
        // (Harvest absolutification against the root base).
        let resolve = self.resolve;
        if resolve.dir_differs(base_url, &self.trim_url) {
            struct Absolutize<'a> {
                resolve: &'a dyn Resolve,
                base: Vec<u8>,
            }
            impl crate::scanner::Transformer for Absolutize<'_> {
                fn transform(&mut self, url: &mut Vec<u8>) -> crate::scanner::TransformStatus {
                    if url.is_empty() || url[0] == b'#' {
                        return crate::scanner::TransformStatus::NoChange;
                    }
                    match self.resolve.resolve(&self.base, url) {
                        Some(resolved) if resolved != *url => {
                            *url = resolved;
                            crate::scanner::TransformStatus::Success
                        }
                        _ => crate::scanner::TransformStatus::NoChange,
                    }
                }
            }
            let mut transformer = Absolutize {
                resolve,
                base: base_url.to_vec(),
            };
            let stylesheet = root.stylesheet.as_mut().expect("parsed");
            crate::absolutify::absolutify_imports(stylesheet, &|link| {
                if link.is_empty() || link[0] == b'#' {
                    return None;
                }
                resolve.resolve(base_url, link)
            });
            crate::absolutify::absolutify_urls(
                root.stylesheet.as_mut().expect("parsed"),
                &mut transformer,
                true,
                true,
            );
        }
        let stylesheet = root.stylesheet.as_ref().expect("root stylesheet");
        let text = minify_stylesheet(stylesheet).unwrap_or_else(|| contents.to_vec());
        let flattened = root.flattening_succeeded;
        FlattenOutcome {
            text,
            flattened,
            failure_reason: std::mem::take(&mut root.failure_reason),
            unparseable_detected: root.unparseable_detected,
        }
    }

    /// Expands one level (`ExpandChildren` + nested fetch/parse).
    fn expand(&mut self, node: &mut Node, ancestors: &[Vec<u8>]) {
        let stylesheet = node.stylesheet.as_ref().expect("parsed");
        // Snapshot import links/media (borrowed stylesheet).
        let mut imports = Vec::new();
        for import in stylesheet.imports() {
            imports.push((
                import.link().as_bytes().to_vec(),
                import.media_queries().clone(),
            ));
        }
        node.children = Vec::new();
        for (link, media_queries) in imports {
            let mut child = Node::new(Vec::new(), node.base_url.clone());
            let Some(import_url) = self.resolve.resolve(&node.base_url, &link) else {
                self.stats.add(crate::stats::INVALID_URL, 1);
                child.flattening_succeeded = false;
                let mut reason = b"Invalid import URL ".to_vec();
                reason.extend_from_slice(&link);
                reason.extend_from_slice(b" in ");
                reason.extend_from_slice(node.url_for_humans());
                child.add_failure_reason(&reason);
                node.children.push(child);
                continue;
            };
            let Some(mut media_types) = media_queries_to_strings(&media_queries) else {
                self.stats.add(crate::stats::COMPLEX_QUERIES, 1);
                child.flattening_succeeded = false;
                let mut reason = b"Complex media queries in the @import of ".to_vec();
                reason.extend_from_slice(&import_url);
                child.add_failure_reason(&reason);
                node.children.push(child);
                continue;
            };
            if !determine_import_media(&node.media, &mut media_types) {
                // Media reduced to nothing: skip silently (child left
                // default with empty URL, never fetched).
                node.children.push(child);
                continue;
            }
            child.url = import_url.clone();
            child.base_url = import_url.clone();
            child.media = media_types;
            if ancestors.contains(&import_url) || node.url == import_url {
                // Matches `IsRecursive` over initialized ancestors;
                // the root has no URL so top-level self-imports fail
                // one level down, exactly as in C++.
                self.stats.add(crate::stats::RECURSION, 1);
                child.flattening_succeeded = false;
                let mut reason = b"Recursive @import of ".to_vec();
                reason.extend_from_slice(&import_url);
                child.add_failure_reason(&reason);
                node.children.push(child);
                continue;
            }
            let Some(fetched) = self.fetch.fetch(&import_url) else {
                child.flattening_succeeded = false;
                let mut reason = b"Cannot import ".to_vec();
                reason.extend_from_slice(&import_url);
                child.add_failure_reason(&reason);
                node.children.push(child);
                continue;
            };
            // Resolve nested URLs before parsing (nested transformer).
            let resolved =
                self.resolve
                    .resolve_nested_contents(&fetched.bytes, &import_url, &self.trim_url);
            let headers_charset = fetched.charset_from_headers.clone();
            child.contents = resolved.unwrap_or(fetched.bytes);
            let Some((child_stylesheet, unparseable)) = parse_stylesheet(&child.contents) else {
                self.stats.add(crate::stats::MINIFY_FAILED, 1);
                child.flattening_succeeded = false;
                let mut reason = b"Cannot parse the CSS in ".to_vec();
                reason.extend_from_slice(&import_url);
                child.add_failure_reason(&reason);
                node.children.push(child);
                continue;
            };
            if unparseable {
                child.unparseable_detected = true;
            }
            child.stylesheet = Some(child_stylesheet);
            // Charset check (headers, then @charset, then inherit).
            if !self.check_charset(&mut child, node, &headers_charset) {
                node.children.push(child);
                continue;
            }
            // Reduce child ruleset media; complex queries fail.
            if !self.reduce_node_media(&mut child) {
                node.children.push(child);
                continue;
            }
            let mut child_ancestors = ancestors.to_vec();
            if !node.url.is_empty() {
                child_ancestors.push(node.url.clone());
            }
            self.expand(&mut child, &child_ancestors);
            node.children.push(child);
        }
    }

    /// Charset agreement (`CheckCharsetOk`).
    fn check_charset(&mut self, child: &mut Node, parent: &Node, headers_charset: &[u8]) -> bool {
        if child.charset.is_empty() {
            child.charset = headers_charset.to_vec();
            child.charset_source = b"from headers".to_vec();
        }
        if child.charset.is_empty() {
            if let Some(stylesheet) = child.stylesheet.as_ref() {
                if !stylesheet.charsets().is_empty() {
                    child.charset = stylesheet.charset(0).as_bytes().to_vec();
                    child.charset_source = b"from an @charset".to_vec();
                }
            }
        }
        if child.charset.is_empty() {
            child.charset = parent.charset.clone();
            child.charset_source = b"from the enclosing CSS".to_vec();
            return true;
        }
        if !child.charset.eq_ignore_ascii_case(&parent.charset) {
            self.stats.add(crate::stats::CHARSET_MISMATCH, 1);
            child.flattening_succeeded = false;
            let mut reason = b"The charset of ".to_vec();
            reason.extend_from_slice(child.url_for_humans());
            reason.extend_from_slice(b" (");
            reason.extend_from_slice(&child.charset);
            reason.extend_from_slice(b" ");
            reason.extend_from_slice(&child.charset_source);
            reason.extend_from_slice(b") is different from that of its parent (");
            reason.extend_from_slice(parent.url_for_humans());
            reason.extend_from_slice(b"): ");
            reason.extend_from_slice(&parent.charset);
            reason.extend_from_slice(b" ");
            reason.extend_from_slice(&parent.charset_source);
            child.add_failure_reason(&reason);
            return false;
        }
        true
    }

    /// Child ruleset media reduction (`CssHierarchy::Parse` loop).
    fn reduce_node_media(&mut self, child: &mut Node) -> bool {
        let stylesheet = child.stylesheet.as_mut().expect("parsed");
        let mut kept = Vec::new();
        let rulesets = std::mem::take(stylesheet.rulesets_mut());
        let mut remaining = rulesets.into_iter().peekable();
        while let Some(mut ruleset) = remaining.next() {
            let Some(mut ruleset_media) = media_queries_to_strings(ruleset.media_queries()) else {
                self.stats.add(crate::stats::COMPLEX_QUERIES, 1);
                child.flattening_succeeded = false;
                let mut reason = b"A media query is too complex in ".to_vec();
                reason.extend_from_slice(child.url_for_humans());
                child.add_failure_reason(&reason);
                // C++ keeps partial results here (harmless: failed).
                kept.push(ruleset);
                kept.extend(remaining);
                break;
            };
            if determine_ruleset_media(&child.media, &mut ruleset_media) {
                *ruleset.media_queries_mut() = strings_to_media_queries(&ruleset_media);
                kept.push(ruleset);
            }
        }
        *child.stylesheet.as_mut().expect("parsed").rulesets_mut() = kept;
        child.flattening_succeeded
    }

    /// Contents rollup for the limit check (`RollUpContents`).
    /// Returns the minified concatenation; on failure the node keeps
    /// its unflattened minified text (or original contents).
    /// Stylesheet-less children (media-skipped) contribute nothing.
    fn rollup_contents(&mut self, node: &mut Node) -> Vec<u8> {
        let children = std::mem::take(&mut node.children);
        let mut rebuilt = Vec::new();
        let mut child_texts = Vec::new();
        for mut child in children {
            if child.stylesheet.is_none() {
                // Skipped (media) or uninitialized: nothing to roll.
                rebuilt.push(child);
                continue;
            }
            let text = self.rollup_contents(&mut child);
            if !child.flattening_succeeded {
                node.flattening_succeeded = false;
            }
            let reason = std::mem::take(&mut child.failure_reason);
            node.add_failure_reason(&reason);
            if child.unparseable_detected {
                node.unparseable_detected = true;
            }
            child_texts.push(text);
            rebuilt.push(child);
        }
        node.children = rebuilt;
        if !node.flattening_succeeded {
            node.children.clear();
            return minify_own(node);
        }
        let mut out = Vec::new();
        for text in &child_texts {
            out.extend_from_slice(text);
        }
        // Strip charsets/imports, minify, then restore on failure.
        let stylesheet = node.stylesheet.as_mut().expect("parsed");
        let saved_charsets = std::mem::take(stylesheet.charsets_mut());
        let saved_imports = std::mem::take(stylesheet.imports_mut());
        let mut minifier = Minifier::new(false);
        minifier.minify_stylesheet(stylesheet);
        let (own, minified_ok) = minifier.finish();
        if !minified_ok {
            self.stats.add(crate::stats::MINIFY_FAILED, 1);
            node.flattening_succeeded = false;
            let mut reason = b"Minification failed for ".to_vec();
            reason.extend_from_slice(node.url_for_humans());
            node.add_failure_reason(&reason);
        } else {
            out.extend_from_slice(&own);
            if self.max_bytes > 0 && out.len() as i64 >= self.max_bytes {
                self.stats.add(crate::stats::LIMIT_EXCEEDED, 1);
                node.flattening_succeeded = false;
                let reason = format!(
                    "Flattening limit ({}) exceeded ({})",
                    self.max_bytes,
                    out.len()
                );
                node.add_failure_reason(reason.as_bytes());
            }
        }
        if !node.flattening_succeeded {
            node.children.clear();
            let stylesheet = node.stylesheet.as_mut().expect("parsed");
            *stylesheet.charsets_mut() = saved_charsets;
            *stylesheet.imports_mut() = saved_imports;
            return minify_own(node);
        }
        out
    }

    /// Stylesheet merge (`RollUpStylesheets`).
    fn rollup_stylesheets(&mut self, node: &mut Node) {
        if node.stylesheet.is_none() {
            // Media-skipped child: nothing to merge.
            return;
        }
        let children = std::mem::take(&mut node.children);
        let mut rebuilt = Vec::new();
        for mut child in children {
            self.rollup_stylesheets(&mut child);
            if !child.flattening_succeeded {
                node.flattening_succeeded = false;
            }
            let reason = std::mem::take(&mut child.failure_reason);
            node.add_failure_reason(&reason);
            if child.unparseable_detected {
                node.unparseable_detected = true;
            }
            rebuilt.push(child);
        }
        node.children = rebuilt;
        let failed = !node.flattening_succeeded;
        if failed {
            node.flattening_succeeded = false;
        }
        if node.flattening_succeeded {
            let stylesheet = node.stylesheet.as_mut().expect("parsed");
            stylesheet.charsets_mut().clear();
            stylesheet.imports_mut().clear();
            // Prepend children in reverse order (rulesets + fonts),
            // skipping media-skipped children without stylesheets.
            for child in node.children.iter_mut().rev() {
                if child.stylesheet.is_none() {
                    continue;
                }
                if let Some(child_stylesheet) = child.stylesheet.as_mut() {
                    let mut child_rulesets = std::mem::take(child_stylesheet.rulesets_mut());
                    let mut child_fonts = std::mem::take(child_stylesheet.font_faces_mut());
                    let stylesheet = node.stylesheet.as_mut().expect("parsed");
                    let target = stylesheet.rulesets_mut();
                    let mut merged = std::mem::take(target);
                    child_rulesets.append(&mut merged);
                    *target = child_rulesets;
                    let fonts_target = stylesheet.font_faces_mut();
                    let mut merged_fonts = std::mem::take(fonts_target);
                    child_fonts.append(&mut merged_fonts);
                    *fonts_target = child_fonts;
                }
            }
        }
        node.children.clear();
    }
}

/// Import media reduction (`DetermineImportMedia`).
fn determine_import_media(containing: &[Vec<u8>], media: &mut Vec<Vec<u8>>) -> bool {
    clear_vector_if_contains_media_all(media);
    if media.is_empty() {
        *media = containing.to_vec();
    } else {
        media.sort();
        eliminate_elements_not_in(media, containing);
        if media.is_empty() {
            return false;
        }
    }
    true
}

/// Ruleset media reduction (`DetermineRulesetMedia`).
fn determine_ruleset_media(node_media: &[Vec<u8>], ruleset_media: &mut Vec<Vec<u8>>) -> bool {
    clear_vector_if_contains_media_all(ruleset_media);
    ruleset_media.sort();
    if !node_media.is_empty() {
        eliminate_elements_not_in(ruleset_media, node_media);
        if ruleset_media.is_empty() {
            return false;
        }
    }
    true
}

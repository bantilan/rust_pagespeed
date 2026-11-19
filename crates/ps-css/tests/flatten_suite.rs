//! Flattening end-to-end suite.
//!
//! Fixtures extracted mechanically from `css_hierarchy_test.cc` and
//! `css_flatten_imports_test.cc`; control flow hand-written around
//! map-fetch and string-ops doubles (exact for these URL shapes; full
//! URL semantics arrive with the URL layer).

use ps_css::flatten::{flatten, Fetch, FetchedCss, Resolve};
use ps_css::scanner::{transform_urls, TransformStatus, Transformer};
use ps_css::stats::CssStats;
use std::collections::HashMap;

struct MapFetch {
    map: HashMap<Vec<u8>, (Vec<u8>, Vec<u8>)>,
}

impl MapFetch {
    fn new(entries: &[(&[u8], &[u8])]) -> MapFetch {
        MapFetch {
            map: entries
                .iter()
                .map(|(url, bytes)| (url.to_vec(), (bytes.to_vec(), Vec::new())))
                .collect(),
        }
    }

    fn with_charset(mut self, url: &[u8], charset: &[u8]) -> MapFetch {
        if let Some(entry) = self.map.get_mut(url) {
            entry.1 = charset.to_vec();
        }
        self
    }
}

impl Fetch for MapFetch {
    fn fetch(&mut self, url: &[u8]) -> Option<FetchedCss> {
        self.map.get(url).map(|(bytes, charset)| FetchedCss {
            bytes: bytes.clone(),
            charset_from_headers: charset.clone(),
        })
    }
}

struct TestResolve;

impl TestResolve {
    fn split_host(url: &[u8]) -> Option<(&[u8], &[u8])> {
        let rest = url
            .strip_prefix(b"http://")
            .or_else(|| url.strip_prefix(b"https://"))?;
        match rest.iter().position(|byte| *byte == b'/') {
            Some(index) => Some((&url[..url.len() - rest.len() + index], &rest[index..])),
            None => Some((url, b"/")),
        }
    }

    fn dir_of(url: &[u8]) -> Option<Vec<u8>> {
        let (host, path) = Self::split_host(url)?;
        let mut dir = host.to_vec();
        match path.iter().rposition(|byte| *byte == b'/') {
            Some(index) => dir.extend_from_slice(&path[..index + 1]),
            None => dir.push(b'/'),
        }
        Some(dir)
    }
}

impl Resolve for TestResolve {
    fn resolve(&self, base: &[u8], link: &[u8]) -> Option<Vec<u8>> {
        if link.is_empty() || link.iter().any(|byte| *byte == b' ' || *byte == b'\n') {
            return None;
        }
        if link.starts_with(b"http://") || link.starts_with(b"https://") {
            TestResolve::split_host(link)?;
            return Some(link.to_vec());
        }
        if link.iter().any(|byte| {
            !(byte.is_ascii_alphanumeric()
                || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'%' | b'?' | b'&' | b'='))
        }) {
            return None;
        }
        let (host, _) = TestResolve::split_host(base)?;
        if let Some(path) = link.strip_prefix(b"/") {
            let mut out = host.to_vec();
            out.push(b'/');
            out.extend_from_slice(path);
            return Some(out);
        }
        let mut out = TestResolve::dir_of(base)?;
        out.extend_from_slice(link);
        Some(out)
    }

    fn dir_differs(&self, first: &[u8], second: &[u8]) -> bool {
        TestResolve::dir_of(first) != TestResolve::dir_of(second)
    }

    fn resolve_nested_contents(
        &self,
        contents: &[u8],
        base: &[u8],
        trim: &[u8],
    ) -> Option<Vec<u8>> {
        struct Absolutize<'a> {
            base: &'a [u8],
            trim_dir: Option<Vec<u8>>,
        }
        impl Transformer for Absolutize<'_> {
            fn transform(&mut self, url: &mut Vec<u8>) -> TransformStatus {
                if url.is_empty() || url[0] == b'#' || url.contains(&b':') {
                    return TransformStatus::NoChange;
                }
                // Resolve against the nested base, trimming back when
                // the result still sits under the trim dir (same-dir
                // nesting is a no-op, as with UrlLeftTrimFilter).
                let base_dir = TestResolve::dir_of(self.base).unwrap_or_default();
                let mut absolute = base_dir.clone();
                absolute.extend_from_slice(url.strip_prefix(b"/").unwrap_or(url));
                if let Some(trim_dir) = &self.trim_dir {
                    if let Some(relative) = absolute.strip_prefix(trim_dir.as_slice()) {
                        if relative != url.as_slice() {
                            *url = relative.to_vec();
                            return TransformStatus::Success;
                        }
                        return TransformStatus::NoChange;
                    }
                }
                if absolute == *url {
                    TransformStatus::NoChange
                } else {
                    *url = absolute;
                    TransformStatus::Success
                }
            }
        }
        let mut transformer = Absolutize {
            base,
            trim_dir: TestResolve::dir_of(trim),
        };
        transform_urls(contents, &mut transformer)
    }
}

fn new_stats() -> CssStats {
    CssStats::new()
}

fn flatten_simple(
    base: &[u8],
    contents: &[u8],
    fetch: &mut MapFetch,
    max_bytes: i64,
    stats: &mut CssStats,
) -> ps_css::flatten::FlattenOutcome {
    flatten(
        base,
        base,
        contents,
        b"",
        &[],
        max_bytes,
        &TestResolve,
        fetch,
        stats,
    )
}

#[test]
fn flat_no_imports() {
    let mut fetch = MapFetch::new(&[]);
    let mut stats = new_stats();
    let outcome = flatten_simple(b"http://test.com/a.css", KTOPCSS, &mut fetch, 0, &mut stats);
    assert!(outcome.flattened);
    assert!(outcome.failure_reason.is_empty());
    assert_eq!(outcome.text, KTOPCSS);
}

#[test]
fn nested_full_flatten() {
    let top = [
        b"@import url(http://test.com/nested1.css);".as_slice(),
        b"@import url(http://test.com/nested2.css);".as_slice(),
        KTOPCSS,
    ]
    .concat();
    let child1 = [
        b"@import url(http://test.com/nested/nested1.css);".as_slice(),
        KTOPCHILD1CSS,
    ]
    .concat();
    let child2 = [
        b"@import url(http://test.com/nested/nested2.css);".as_slice(),
        KTOPCHILD2CSS,
    ]
    .concat();
    let mut fetch = MapFetch::new(&[
        (b"http://test.com/nested1.css".as_slice(), &child1),
        (b"http://test.com/nested2.css".as_slice(), &child2),
        (
            b"http://test.com/nested/nested1.css".as_slice(),
            KTOPCHILD1CHILD1CSS,
        ),
        (
            b"http://test.com/nested/nested2.css".as_slice(),
            KTOPCHILD2CHILD1CSS,
        ),
    ]);
    let mut stats = new_stats();
    let outcome = flatten_simple(b"http://test.com/a.css", &top, &mut fetch, 0, &mut stats);
    assert!(outcome.flattened);
    let expected = [
        KTOPCHILD1CHILD1CSS,
        KTOPCHILD1CSS,
        KTOPCHILD2CHILD1CSS,
        KTOPCHILD2CSS,
        KTOPCSS,
    ]
    .concat();
    assert_eq!(outcome.text, expected);
}

#[test]
fn flatten_limits() {
    let top = [
        b"@import url(http://test.com/nested1.css);".as_slice(),
        b"@import url(http://test.com/nested2.css);".as_slice(),
        KTOPCSS,
    ]
    .concat();
    let child1 = [
        b"@import url(http://test.com/nested/nested1.css);".as_slice(),
        KTOPCHILD1CSS,
    ]
    .concat();
    let child2 = [
        b"@import url(http://test.com/nested/nested2.css);".as_slice(),
        KTOPCHILD2CSS,
    ]
    .concat();
    let entries: &[(&[u8], &[u8])] = &[
        (b"http://test.com/nested1.css".as_slice(), &child1),
        (b"http://test.com/nested2.css".as_slice(), &child2),
        (
            b"http://test.com/nested/nested1.css".as_slice(),
            KTOPCHILD1CHILD1CSS,
        ),
        (
            b"http://test.com/nested/nested2.css".as_slice(),
            KTOPCHILD2CHILD1CSS,
        ),
    ];
    let flattened_len = KTOPCHILD1CHILD1CSS.len()
        + KTOPCHILD1CSS.len()
        + KTOPCHILD2CHILD1CSS.len()
        + KTOPCHILD2CSS.len()
        + KTOPCSS.len();
    // Under the limit: flattened.
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        &top,
        &mut MapFetch::new(entries),
        2048,
        &mut stats,
    );
    assert!(outcome.flattened);
    assert_eq!(outcome.text.len(), flattened_len);
    // Exactly at the limit: fails (>= trips), unflattened minified.
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        &top,
        &mut MapFetch::new(entries),
        flattened_len as i64,
        &mut stats,
    );
    assert!(!outcome.flattened);
    assert_eq!(outcome.text, top);
    assert_eq!(stats.get(ps_css::stats::LIMIT_EXCEEDED), 1);
    // Over the limit: same fallback.
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        &top,
        &mut MapFetch::new(entries),
        10,
        &mut stats,
    );
    assert!(!outcome.flattened);
    assert_eq!(outcome.text, top);
}

#[test]
fn recursion_fails() {
    // Direct self-import: caught one level down; output stays as-is.
    let css_in = b"@import url(http://test.com/recursive.css);";
    let mut fetch = MapFetch::new(&[(b"http://test.com/recursive.css".as_slice(), css_in)]);
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/recursive.css",
        css_in,
        &mut fetch,
        0,
        &mut stats,
    );
    assert!(!outcome.flattened);
    assert_eq!(outcome.text, css_in);
    assert_eq!(stats.get(ps_css::stats::RECURSION), 1);
    assert!(String::from_utf8_lossy(&outcome.failure_reason)
        .contains("Recursive @import of http://test.com/recursive.css"));
}

#[test]
fn media_elision_and_grouping() {
    // screen/print cross-imports elide; survivors group by media.
    let styles = b"@import url(print.css) print;@import url(screen.css) screen;@media all{.background_red{background-color:red}.foreground_yellow{color:#ff0}}";
    let print = b"@import url(screen.css) screen;.background_cyan{background-color:#0ff}.foreground_pink{color:#ffc0cb}@media all{.background_green{background-color:#0f0}.foreground_rose{color:rose}}";
    let screen = b"@import url(print.css) print;.background_blue{background-color:#00f}.foreground_gray{color:gray}@media all{.background_white{background-color:#fff}.foreground_black{color:#000}}";
    let mut fetch = MapFetch::new(&[
        (b"http://test.com/print.css".as_slice(), print),
        (b"http://test.com/screen.css".as_slice(), screen),
    ]);
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/styles.css",
        styles,
        &mut fetch,
        0,
        &mut stats,
    );
    assert!(outcome.flattened);
    assert_eq!(
        outcome.text,
        b"@media print{.background_cyan{background-color:#0ff}.foreground_pink{color:#ffc0cb}.background_green{background-color:#0f0}.foreground_rose{color:rose}}@media screen{.background_blue{background-color:#00f}.foreground_gray{color:gray}.background_white{background-color:#fff}.foreground_black{color:#000}}.background_red{background-color:red}.foreground_yellow{color:#ff0}".as_slice()
    );
}

#[test]
fn media_all_is_unconstrained() {
    let top = b"@import url(http://test.com/b.css) all;.a{color:red}";
    let mut fetch = MapFetch::new(&[(b"http://test.com/b.css".as_slice(), b".b{color:blue}")]);
    let mut stats = new_stats();
    let outcome = flatten_simple(b"http://test.com/a.css", top, &mut fetch, 0, &mut stats);
    assert!(outcome.flattened);
    assert_eq!(outcome.text, b".b{color:#00f}.a{color:red}");
}

#[test]
fn charset_mismatch_fails() {
    let top = b"@import url(http://test.com/nested1.css);.a{color:red}";
    let child = b".b{color:blue}";
    let fetch_entries: &[(&[u8], &[u8])] = &[(b"http://test.com/nested1.css".as_slice(), child)];
    // Compatible: both empty.
    let mut stats = new_stats();
    let outcome = flatten(
        b"http://test.com/a.css",
        b"http://test.com/a.css",
        top,
        b"",
        &[],
        0,
        &TestResolve,
        &mut MapFetch::new(fetch_entries),
        &mut stats,
    );
    assert!(outcome.flattened);
    // Incompatible headers charsets.
    let mut stats = new_stats();
    let outcome = flatten(
        b"http://test.com/a.css",
        b"http://test.com/a.css",
        top,
        b"iso-8859-1",
        &[],
        0,
        &TestResolve,
        &mut MapFetch::new(fetch_entries).with_charset(b"http://test.com/nested1.css", b"utf-8"),
        &mut stats,
    );
    assert!(!outcome.flattened);
    assert_eq!(stats.get(ps_css::stats::CHARSET_MISMATCH), 1);
    assert_eq!(
        outcome.failure_reason,
        b"Flattening failed: The charset of http://test.com/nested1.css (utf-8 from headers) is different from that of its parent (inline): iso-8859-1 from unknown".as_slice()
    );
}

#[test]
fn invalid_url_fails() {
    let top = b"@import url(\"ht!tp://[bad\");.a{color:red}";
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        top,
        &mut MapFetch::new(&[]),
        0,
        &mut stats,
    );
    assert!(!outcome.flattened);
    assert_eq!(stats.get(ps_css::stats::INVALID_URL), 1);
}

#[test]
fn complex_queries_fail() {
    // Complex query on the @import itself.
    let top = b"@import url(http://test.com/b.css) screen and (min-width: 10px);.a{color:red}";
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        top,
        &mut MapFetch::new(&[(b"http://test.com/b.css".as_slice(), b".b{color:blue}")]),
        0,
        &mut stats,
    );
    assert!(!outcome.flattened);
    assert_eq!(stats.get(ps_css::stats::COMPLEX_QUERIES), 1);
    // Complex query on a nested ruleset.
    let top = b"@import url(http://test.com/b.css);.a{color:red}";
    let child = b"@media screen and (min-width: 10px){.b{color:blue}}";
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        top,
        &mut MapFetch::new(&[(b"http://test.com/b.css".as_slice(), child)]),
        0,
        &mut stats,
    );
    assert!(!outcome.flattened);
    assert_eq!(stats.get(ps_css::stats::COMPLEX_QUERIES), 1);
}

#[test]
fn unparseable_sections_survive() {
    let top = b"@import url(http://test.com/b.css);@foobar {.a{color:red}}";
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        top,
        &mut MapFetch::new(&[(b"http://test.com/b.css".as_slice(), b".b{color:blue}")]),
        0,
        &mut stats,
    );
    assert!(outcome.flattened);
    assert!(outcome.unparseable_detected);
    assert_eq!(outcome.text, b".b{color:#00f}@foobar {.a{color:red}}");
}

#[test]
fn parse_error_fails_closed() {
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        b"a { color: red; } {",
        &mut MapFetch::new(&[]),
        0,
        &mut stats,
    );
    assert!(!outcome.flattened);
    assert_eq!(outcome.text, b"a { color: red; } {");
    assert_eq!(stats.get(ps_css::stats::PARSE_FAILURES), 1);
}

#[test]
fn nested_relative_urls_rebase() {
    // A relative image URL in a nested file resolves against the
    // nested base; outside the trim dir it stays absolute.
    let top = b"@import url(http://test.com/sub/b.css);.a{color:red}";
    let child = b".b{background-image:url(i.png)}";
    let mut stats = new_stats();
    let outcome = flatten(
        b"http://test.com/sub/b.css",
        b"http://test.com/css/a.css",
        top,
        b"",
        &[],
        0,
        &TestResolve,
        &mut MapFetch::new(&[(b"http://test.com/sub/b.css".as_slice(), child)]),
        &mut stats,
    );
    assert!(outcome.flattened);
    assert_eq!(
        outcome.text,
        b".b{background-image:url(http://test.com/sub/i.png)}.a{color:red}"
    );
    // Under the trim dir it trims back to root-relative (UrlLeftTrim).
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        top,
        &mut MapFetch::new(&[(b"http://test.com/sub/b.css".as_slice(), child)]),
        0,
        &mut stats,
    );
    assert!(outcome.flattened);
    assert_eq!(
        outcome.text,
        b".b{background-image:url(sub/i.png)}.a{color:red}"
    );
}

#[test]
fn same_dir_nesting_is_stable() {
    // Same-dir nesting trims back to relative (no visible change).
    let top = b"@import url(b.css);.a{color:red}";
    let child = b".b{background-image:url(i.png)}";
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        top,
        &mut MapFetch::new(&[(b"http://test.com/b.css".as_slice(), child)]),
        0,
        &mut stats,
    );
    assert!(outcome.flattened);
    assert_eq!(
        outcome.text,
        b".b{background-image:url(i.png)}.a{color:red}"
    );
}

#[test]
fn root_output_absolutifies_when_trim_differs() {
    // Inline-style flow: trim URL differs from the CSS base, so the
    // merged output's relative URLs absolutify against the base.
    let top = b".a{background:url(i.png)}";
    let mut stats = new_stats();
    let outcome = flatten(
        b"http://test.com/css/a.css",
        b"http://test.com/index.html",
        top,
        b"",
        &[],
        0,
        &TestResolve,
        &mut MapFetch::new(&[]),
        &mut stats,
    );
    assert!(outcome.flattened);
    assert_eq!(
        outcome.text,
        b".a{background:url(http://test.com/css/i.png)}"
    );
    // Same dir: untouched.
    let mut stats = new_stats();
    let outcome = flatten(
        b"http://test.com/css/a.css",
        b"http://test.com/css/out.css",
        top,
        b"",
        &[],
        0,
        &TestResolve,
        &mut MapFetch::new(&[]),
        &mut stats,
    );
    assert!(outcome.flattened);
    assert_eq!(outcome.text, top);
}

#[test]
fn missing_child_fails() {
    let top = b"@import url(http://test.com/missing.css);.a{color:red}";
    let mut stats = new_stats();
    let outcome = flatten_simple(
        b"http://test.com/a.css",
        top,
        &mut MapFetch::new(&[]),
        0,
        &mut stats,
    );
    assert!(!outcome.flattened);
    // Falls back to the minified unflattened root.
    assert_eq!(
        outcome.text,
        b"@import url(http://test.com/missing.css);.a{color:red}"
    );
}
const KTOPCSS: &[u8] = b".background_red{background-color:red}@foobar { font-family: 'Magellan'; font-style: normal }.foreground_yellow{color:#ff0}";
const KTOPCHILD1CSS: &[u8] = b".background_blue{background-color:#00f}.foreground_gray{color:gray}";
const KTOPCHILD1CHILD1CSS: &[u8] =
    b".background_cyan{background-color:#0ff}.foreground_pink{color:#ffc0cb}";
const KTOPCHILD2CSS: &[u8] = b".background_white{background-color:#fff}@foobar { font-family: 'Cook'; font-style: normal }.foreground_black{color:#000}";
const KTOPCHILD2CHILD1CSS: &[u8] =
    b".background_green{background-color:#0f0}.foreground_rose{color:rose}";

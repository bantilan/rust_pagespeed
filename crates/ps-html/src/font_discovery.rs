//! Critical font discovery for preload hints (pure core).
//!
//! Scans stylesheets for `@font-face` rules (plus `@import` edges
//! for the caller to follow), extracts family/weight/style/src/
//! format/unicode-range, resolves `var()` usages against merged
//! custom properties, verifies selectors against document element
//! sets, matches faces against page font usage (weight ranges
//! overlap), prunes icon-only and unnecessary-subset faces, and
//! selects at most two preloads with per-family diagnostic notes.
//! Self-contained scanner: the shared CSS parser splinters
//! `unicode-range` syntax, so block scanning and the descriptor
//! parsers live here. No I/O; fully unit-tested.

use std::collections::{HashMap, HashSet};

/// Max stylesheet bytes scanned (per file).
pub const MAX_SCAN_BYTES: usize = 256 * 1024;
/// Max faces collected per stylesheet.
pub const MAX_FACES_PER_STYLESHEET: usize = 64;
/// Max imports followed per stylesheet.
pub const MAX_IMPORTS_PER_STYLESHEET: usize = 16;
/// Max font preloads per page (conservative default).
pub const MAX_FONT_PRELOADS: usize = 2;

/// Latin basic coverage: faces whose ranges miss it entirely are
/// unnecessary subsets unless beacon-confirmed.
const LATIN_MAX: u32 = 0x00FF;
/// Private-use planes (icon fonts live here).
const PRIVATE_USE_RANGES: [(u32, u32); 3] =
    [(0xE000, 0xF8FF), (0xF0000, 0xFFFFD), (0x100000, 0x10FFFD)];

/// Font style (`font-style`, angle folded into obliqueness).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

/// One `src` entry: URL plus optional format hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontSource {
    /// Source URL as authored (caller resolves against the CSS base).
    pub url: String,
    /// Lowercased `format()` hint (`None` when absent).
    pub format: Option<String>,
}

/// One parsed `@font-face` rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFace {
    /// First family name, unquoted, lowercased for matching.
    pub family: String,
    /// Inclusive weight range (descriptor default 400).
    pub weight_min: u32,
    /// Inclusive weight range (descriptor default 400).
    pub weight_max: u32,
    /// Declared style (descriptor default normal).
    pub style: FontStyle,
    /// Source list in authored order.
    pub sources: Vec<FontSource>,
    /// Inclusive unicode ranges; empty means full coverage.
    /// Malformed input degrades to full rather than pruning blindly.
    pub ranges: Vec<(u32, u32)>,
}

/// One stylesheet scan: faces plus import edges for the caller.
#[derive(Debug, Default)]
pub struct StylesheetFonts {
    /// Parsed faces in document order.
    pub faces: Vec<FontFace>,
    /// Raw `@import` URLs in document order (caller resolves).
    pub imports: Vec<String>,
}

/// Skips ASCII whitespace and `/* */` comments, reporting the first
/// unconsumed byte. Unterminated comments consume to the end.
fn skip_trivia(bytes: &[u8], mut pos: usize) -> usize {
    loop {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos + 1 < bytes.len() && bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
            pos += 2;
            while pos + 1 < bytes.len() && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
                pos += 1;
            }
            pos = (pos + 2).min(bytes.len());
        } else {
            return pos;
        }
    }
}

/// Reads one CSS identifier-ish word (letters, digits, `-`, `_`,
/// non-ASCII) or quoted string starting at `pos`.
fn read_word(bytes: &[u8], pos: usize) -> (Vec<u8>, usize) {
    if pos < bytes.len() && (bytes[pos] == b'"' || bytes[pos] == b'\'') {
        let quote = bytes[pos];
        let mut out = Vec::new();
        let mut i = pos + 1;
        while i < bytes.len() && bytes[i] != quote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 1;
            }
            out.push(bytes[i]);
            i += 1;
        }
        return (out, (i + 1).min(bytes.len()));
    }
    let mut end = pos;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b'_' | b'.'))
    {
        end += 1;
    }
    (bytes[pos..end].to_vec(), end)
}

/// Splits declarations on top-level `;` (paren/string aware).
fn split_declarations(block: &[u8]) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut depth = 0u32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < block.len() {
        match block[i] {
            b'"' | b'\'' => {
                let quote = block[i];
                i += 1;
                while i < block.len() && block[i] != quote {
                    i += if block[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b';' if depth == 0 => {
                parts.push(&block[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < block.len() {
        parts.push(&block[start..]);
    }
    parts
}

/// Splits on top-level commas (paren/string aware).
fn split_top_commas(text: &[u8]) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut depth = 0u32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < text.len() {
        match text[i] {
            b'"' | b'\'' => {
                let quote = text[i];
                i += 1;
                while i < text.len() && text[i] != quote {
                    i += if text[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&text[start..]);
    parts
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

fn unquote(bytes: &[u8]) -> Vec<u8> {
    let text = trim_ascii(bytes);
    if text.len() >= 2 && (text[0] == b'"' || text[0] == b'\'') && text[text.len() - 1] == text[0] {
        text[1..text.len() - 1].to_vec()
    } else {
        text.to_vec()
    }
}

/// First family name, unquoted and lowercased for matching.
/// CSS-wide keywords are invalid here (no face is built).
fn parse_family(value: &[u8]) -> Option<String> {
    let first = split_top_commas(value).first()?.to_vec();
    let name = unquote(&first);
    if name.is_empty() {
        return None;
    }
    let name = String::from_utf8(name.to_ascii_lowercase()).ok()?;
    if is_generic_family(&name) || is_css_wide_keyword(&name) {
        return None;
    }
    Some(name)
}

/// Weight descriptor: `normal`/`bold`/number/`a b` range.
fn parse_weight(value: &[u8]) -> Option<(u32, u32)> {
    let text = String::from_utf8(trim_ascii(value).to_ascii_lowercase()).ok()?;
    let mut parts = text.split_whitespace();
    let first = parts.next()?;
    let second = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let single = |word: &str| -> Option<u32> {
        match word {
            "normal" => Some(400),
            "bold" => Some(700),
            _ => word.parse::<u32>().ok().filter(|w| (1..=1000).contains(w)),
        }
    };
    match (single(first)?, second.map(single)) {
        (a, None) => Some((a, a)),
        (a, Some(Some(b))) => Some((a.min(b), a.max(b))),
        _ => None,
    }
}

/// Style descriptor (angle folds into obliqueness).
fn parse_style(value: &[u8]) -> Option<FontStyle> {
    let text = String::from_utf8(trim_ascii(value).to_ascii_lowercase()).ok()?;
    let first = text.split_whitespace().next()?;
    match first {
        "normal" => Some(FontStyle::Normal),
        "italic" => Some(FontStyle::Italic),
        "oblique" => Some(FontStyle::Oblique),
        _ => None,
    }
}

/// Extracts the URL from a `url(...)` token (quoted, unquoted).
/// The wrapper match is case-insensitive; the URL itself keeps case.
fn parse_url_token(token: &[u8]) -> Option<String> {
    let text = trim_ascii(token);
    if text.len() < 5 || !text[..4].eq_ignore_ascii_case(b"url(") || text[text.len() - 1] != b')' {
        return None;
    }
    let url = unquote(&text[4..text.len() - 1]);
    if url.is_empty() {
        return None;
    }
    String::from_utf8(url).ok()
}

/// Extracts the string from a `format(...)` token.
fn parse_format_token(token: &[u8]) -> Option<String> {
    let text = trim_ascii(token);
    let lower = text.to_ascii_lowercase();
    let body = lower.strip_prefix(b"format(")?.strip_suffix(b")")?;
    let name = unquote(body);
    if name.is_empty() {
        return None;
    }
    String::from_utf8(name.to_ascii_lowercase()).ok()
}

/// Parses a `src` descriptor into (url, format) sources in order.
fn parse_src(value: &[u8]) -> Vec<FontSource> {
    let mut sources = Vec::new();
    for item in split_top_commas(value) {
        let mut url = None;
        let mut format = None;
        // Tokens within one comma item split on top-level spaces.
        let mut depth = 0u32;
        let mut start = 0usize;
        let bytes: &[u8] = item;
        let flush =
            |start: usize, end: usize, url: &mut Option<String>, format: &mut Option<String>| {
                let token = trim_ascii(&bytes[start..end]);
                if token.is_empty() {
                    return;
                }
                if url.is_none() {
                    if let Some(found) = parse_url_token(token) {
                        *url = Some(found);
                        return;
                    }
                }
                if format.is_none() {
                    *format = parse_format_token(token);
                }
            };
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'"' | b'\'' => {
                    let quote = bytes[i];
                    i += 1;
                    while i < bytes.len() && bytes[i] != quote {
                        i += if bytes[i] == b'\\' { 2 } else { 1 };
                    }
                    i += 1;
                    continue;
                }
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                b if b.is_ascii_whitespace() && depth == 0 => {
                    flush(start, i, &mut url, &mut format);
                    start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        flush(start, bytes.len(), &mut url, &mut format);
        // Bare local() entries carry no URL and are skipped.
        if let Some(url) = url {
            sources.push(FontSource { url, format });
        }
    }
    sources
}

/// Parses one hex run (1..=6 digits, no `0x`).
fn parse_hex_run(text: &[u8]) -> Option<u32> {
    if text.is_empty() || text.len() > 6 || !text.iter().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(std::str::from_utf8(text).ok()?, 16).ok()
}

/// Parses one `U+` item into an inclusive range (`?` wildcards expand).
fn parse_urange_item(item: &[u8]) -> Option<(u32, u32)> {
    let text = trim_ascii(item);
    if text.len() < 3 || !text[..2].eq_ignore_ascii_case(b"u+") {
        return None;
    }
    let body = &text[2..];
    if body.is_empty() || body.len() > 13 {
        return None;
    }
    if let Some(dash) = body.iter().position(|b| *b == b'-') {
        let (lo, hi) = (&body[..dash], &body[dash + 1..]);
        if lo.is_empty() || hi.is_empty() {
            return None;
        }
        let (lo, hi) = (parse_hex_run(lo)?, parse_hex_run(hi)?);
        if lo > hi || hi > 0x10FFFF {
            return None;
        }
        return Some((lo, hi));
    }
    if body.contains(&b'?') {
        if body.len() > 6 || !body.iter().all(|b| b.is_ascii_hexdigit() || *b == b'?') {
            return None;
        }
        let mut lo = String::with_capacity(6);
        let mut hi = String::with_capacity(6);
        for _ in 0..(6 - body.len()) {
            lo.push('0');
            hi.push('0');
        }
        for b in body {
            if *b == b'?' {
                lo.push('0');
                hi.push('F');
            } else {
                lo.push(*b as char);
                hi.push(*b as char);
            }
        }
        let (lo, hi) = (
            u32::from_str_radix(&lo, 16).ok()?,
            u32::from_str_radix(&hi, 16).ok()?,
        );
        if hi > 0x10FFFF {
            return None;
        }
        return Some((lo, hi));
    }
    let v = parse_hex_run(body)?;
    if v > 0x10FFFF {
        return None;
    }
    Some((v, v))
}

/// Parses a `unicode-range` descriptor; `None` (fail-open to full
/// coverage) when any item is malformed.
fn parse_unicode_ranges(value: &[u8]) -> Option<Vec<(u32, u32)>> {
    let mut ranges = Vec::new();
    for item in split_top_commas(value) {
        ranges.push(parse_urange_item(item)?);
    }
    if ranges.is_empty() {
        return None;
    }
    Some(ranges)
}

/// Splits one declaration into name/value at the top-level colon.
fn split_declaration(decl: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut depth = 0u32;
    for (i, b) in decl.iter().enumerate() {
        match b {
            b'"' | b'\'' => return None,
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => return Some((trim_ascii(&decl[..i]), &decl[i + 1..])),
            _ => {}
        }
    }
    None
}

/// Builds one face from its declaration block; `None` without a
/// usable family or any URL source.
fn parse_face(block: &[u8]) -> Option<FontFace> {
    let mut family = None;
    let mut weight = (400u32, 400u32);
    let mut style = FontStyle::Normal;
    let mut sources = Vec::new();
    let mut ranges = Vec::new();
    for decl in split_declarations(block) {
        let Some((name, value)) = split_declaration(decl) else {
            continue;
        };
        if name.eq_ignore_ascii_case(b"font-family") {
            family = parse_family(value);
        } else if name.eq_ignore_ascii_case(b"font-weight") {
            if let Some(parsed) = parse_weight(value) {
                weight = parsed;
            }
        } else if name.eq_ignore_ascii_case(b"font-style") {
            if let Some(parsed) = parse_style(value) {
                style = parsed;
            }
        } else if name.eq_ignore_ascii_case(b"src") {
            sources = parse_src(value);
        } else if name.eq_ignore_ascii_case(b"unicode-range") {
            if let Some(parsed) = parse_unicode_ranges(value) {
                ranges = parsed;
            }
        }
    }
    Some(FontFace {
        family: family?,
        weight_min: weight.0,
        weight_max: weight.1,
        style,
        sources: if sources.is_empty() {
            return None;
        } else {
            sources
        },
        ranges,
    })
}

/// Finds a balanced `{...}` block starting at the brace; strings,
/// escapes, and comments are honored. Returns the inner bytes.
fn balanced_block(bytes: &[u8], brace: usize) -> Option<&[u8]> {
    let mut depth = 0u32;
    let mut i = brace;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&bytes[brace + 1..i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Scans one stylesheet for `@font-face` blocks and `@import` URLs.
/// Bounded by byte/faces/import caps; malformed input truncates.
#[must_use]
pub fn scan_stylesheet(css: &[u8]) -> StylesheetFonts {
    let mut out = StylesheetFonts::default();
    let css = &css[..css.len().min(MAX_SCAN_BYTES)];
    let mut i = 0usize;
    while i < css.len() {
        i = skip_trivia(css, i);
        if i >= css.len() {
            break;
        }
        if css[i] == b'@' {
            let (word, after) = read_word(css, i + 1);
            if word.eq_ignore_ascii_case(b"font-face") {
                let brace = skip_trivia(css, after);
                if brace < css.len() && css[brace] == b'{' {
                    if let Some(block) = balanced_block(css, brace) {
                        if out.faces.len() < MAX_FACES_PER_STYLESHEET {
                            if let Some(face) = parse_face(block) {
                                out.faces.push(face);
                            }
                        }
                        i = brace + block.len() + 2;
                        continue;
                    }
                }
                i = after;
                continue;
            }
            if word.eq_ignore_ascii_case(b"import")
                && out.imports.len() < MAX_IMPORTS_PER_STYLESHEET
            {
                let mut j = skip_trivia(css, after);
                // url(...) or bare string, up to `;`.
                let mut url = Vec::new();
                if j < css.len() && css[j..].starts_with(b"url(")
                    || j < css.len() && css[j..].starts_with(b"URL(")
                {
                    j += 4;
                    let start = j;
                    let mut depth = 1u32;
                    while j < css.len() && depth > 0 {
                        match css[j] {
                            b'"' | b'\'' => {
                                let quote = css[j];
                                j += 1;
                                while j < css.len() && css[j] != quote {
                                    j += if css[j] == b'\\' { 2 } else { 1 };
                                }
                                j += 1;
                                continue;
                            }
                            b'(' => depth += 1,
                            b')' => depth -= 1,
                            _ => {}
                        }
                        j += 1;
                    }
                    url = unquote(&css[start..j.saturating_sub(1)]);
                } else if j < css.len() && (css[j] == b'"' || css[j] == b'\'') {
                    let (text, _) = read_word(css, j);
                    url = text;
                }
                if !url.is_empty() {
                    if let Ok(text) = String::from_utf8(url) {
                        out.imports.push(text);
                    }
                }
                while i < css.len() && css[i] != b';' && css[i] != b'}' {
                    i += 1;
                }
                continue;
            }
            // Print-only grouping: skip without recursing (print
            // faces never need preloads); screen grouping falls
            // through to the generic recursing skipper below.
            if word.eq_ignore_ascii_case(b"media") || word.eq_ignore_ascii_case(b"supports") {
                let mut k = after;
                while k < css.len() && css[k] != b'{' && css[k] != b';' && css[k] != b'}' {
                    k += 1;
                }
                if k < css.len() && css[k] == b'{' && media_is_print_only(&css[i..k]) {
                    if let Some(block) = balanced_block(css, k) {
                        i = k + block.len() + 2;
                        continue;
                    }
                }
            }
        }
        // Skip anything else: balanced block or one statement.
        let mut j = i;
        let mut saw_brace = false;
        while j < css.len() {
            match css[j] {
                b'"' | b'\'' => {
                    let quote = css[j];
                    j += 1;
                    while j < css.len() && css[j] != quote {
                        j += if css[j] == b'\\' { 2 } else { 1 };
                    }
                    j += 1;
                    continue;
                }
                b'{' => {
                    saw_brace = true;
                    break;
                }
                b';' | b'}' => break,
                _ => {}
            }
            j += 1;
        }
        if saw_brace {
            if let Some(block) = balanced_block(css, j) {
                // Recurse into grouping rules (@media, @supports).
                let inner = scan_stylesheet(block);
                for face in inner.faces {
                    if out.faces.len() < MAX_FACES_PER_STYLESHEET {
                        out.faces.push(face);
                    }
                }
                for import in inner.imports {
                    if out.imports.len() < MAX_IMPORTS_PER_STYLESHEET {
                        out.imports.push(import);
                    }
                }
                i = j + block.len() + 2;
            } else {
                i = j + 1;
            }
        } else {
            i = j + 1;
        }
    }
    out
}

/// Confidence that a usage reflects rendered above-the-fold text.
/// Lower sorts first in selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsageConfidence {
    /// Inline `style=` on a seen element, beacon-observed, or a
    /// page-wide (`body`/`html`/`*`/`:root`) rule.
    Certain,
    /// Headings, header/nav, site titles.
    Structural,
    /// Simple selector verified against the document.
    Verified,
    /// Complex selector that cannot be checked (assumed used).
    Assumed,
}

/// One observed font usage (stylesheet rule, inline style, or
/// beacon), matched against faces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontUsage {
    /// Family name, lowercased (matches face normalization).
    pub family: String,
    /// Requested weight range, min (400 when unknown).
    pub weight: u32,
    /// Requested weight range, max (equals min for single weights).
    pub weight_max: u32,
    /// Requested style (normal when unknown).
    pub style: FontStyle,
    /// How confidently this usage reflects rendered text.
    pub confidence: UsageConfidence,
    /// Stylesheet selector (empty for inline styles and beacons).
    pub selector: String,
}

/// One ruleset's font declarations before `var()` resolution and
/// selector verification (cached per stylesheet, resolved per page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFontUsage {
    /// Raw `font-family` value (may hold `var()` references).
    pub families: String,
    /// Raw `font-weight` value (empty when absent).
    pub weight: String,
    /// Raw `font-style` value (empty when absent).
    pub style: String,
    /// Rule selector list (empty for inline styles).
    pub selector: String,
}

/// Document element sets for selector verification (built by the
/// caller during its element walk; tags lowercased, classes/ids exact).
#[derive(Debug, Default)]
pub struct DomSets {
    pub tags: HashSet<String>,
    pub classes: HashSet<String>,
    pub ids: HashSet<String>,
}

/// Max raw declaration bytes kept per ruleset (adversarial cap).
const MAX_RAW_VALUE_BYTES: usize = 1024;
/// Max custom properties collected per stylesheet.
pub const MAX_CUSTOM_PROPS_PER_SHEET: usize = 256;
/// Max `var()` substitution depth (cycle-safe).
const MAX_VAR_DEPTH: u32 = 4;

/// Walks qualified rules, visiting (selector, block) pairs in
/// document order; recurses into screen grouping rules, skips
/// `@font-face`/`@import`/`@keyframes` and print-only `@media`.
/// Bounded; malformed input truncates.
fn walk_style_rules(css: &[u8], budget: &mut usize, visit: &mut impl FnMut(&str, &[u8])) {
    fn walk(css: &[u8], budget: &mut usize, visit: &mut impl FnMut(&str, &[u8])) {
        let mut i = 0usize;
        while i < css.len() && *budget > 0 {
            i = skip_trivia(css, i);
            if i >= css.len() {
                break;
            }
            // Find the next block or statement end.
            let mut j = i;
            let mut saw_brace = false;
            while j < css.len() {
                match css[j] {
                    b'"' | b'\'' => {
                        let quote = css[j];
                        j += 1;
                        while j < css.len() && css[j] != quote {
                            j += if css[j] == b'\\' { 2 } else { 1 };
                        }
                        j += 1;
                        continue;
                    }
                    b'{' => {
                        saw_brace = true;
                        break;
                    }
                    b';' | b'}' => break,
                    _ => {}
                }
                j += 1;
            }
            if !saw_brace {
                i = j + 1;
                continue;
            }
            let prelude = trim_ascii(&css[i..j]);
            let Some(block) = balanced_block(css, j) else {
                i = j + 1;
                continue;
            };
            i = j + block.len() + 2;
            if prelude.first() == Some(&b'@') {
                let (word, _) = read_word(prelude, 1);
                if (word.eq_ignore_ascii_case(b"media") || word.eq_ignore_ascii_case(b"supports"))
                    && !media_is_print_only(prelude)
                {
                    walk(block, budget, visit);
                }
                continue;
            }
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            let selector = String::from_utf8_lossy(prelude).into_owned();
            visit(&selector, block);
        }
    }
    walk(&css[..css.len().min(MAX_SCAN_BYTES)], budget, visit);
}

/// Whether a `@media` prelude can only match print: every
/// comma-branch mentions print and none mentions screen.
fn media_is_print_only(prelude: &[u8]) -> bool {
    let mut branches = 0u32;
    for branch in split_top_commas(prelude) {
        // Skip the `@media` keyword itself on the first branch.
        let text = trim_ascii(branch).to_ascii_lowercase();
        let text = text
            .strip_prefix(b"@media")
            .map(trim_ascii)
            .unwrap_or(text.as_slice());
        if text.is_empty() {
            continue;
        }
        branches += 1;
        let mentions_print = text
            .windows(5)
            .any(|window| window.eq_ignore_ascii_case(b"print"));
        let mentions_screen = text
            .windows(6)
            .any(|window| window.eq_ignore_ascii_case(b"screen"));
        if !mentions_print || mentions_screen {
            return false;
        }
    }
    branches > 0
}

/// Collects one ruleset's raw font declarations (unresolved).
/// Shared with the filter for inline `style` attributes.
pub(crate) fn raw_usage_from_declarations(block: &[u8], selector: &str) -> Option<RawFontUsage> {
    let mut families = None;
    let mut weight = String::new();
    let mut style = String::new();
    for decl in split_declarations(block) {
        let Some((name, value)) = split_declaration(decl) else {
            continue;
        };
        let mut text = String::from_utf8_lossy(trim_ascii(value)).into_owned();
        text.truncate(MAX_RAW_VALUE_BYTES);
        if name.eq_ignore_ascii_case(b"font-family") {
            if !text.is_empty() {
                families = Some(text);
            }
        } else if name.eq_ignore_ascii_case(b"font-weight") {
            weight = text;
        } else if name.eq_ignore_ascii_case(b"font-style") {
            style = text;
        }
    }
    families.map(|families| {
        let mut kept = selector.to_string();
        kept.truncate(MAX_RAW_VALUE_BYTES);
        RawFontUsage {
            families,
            weight,
            style,
            selector: kept,
        }
    })
}

/// Collects custom property definitions (`--name: value`) from every
/// qualified rule; names stay case-sensitive per CSS. Last wins at
/// merge time (CSS cascade order).
#[must_use]
pub fn collect_custom_properties(css: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut budget = 4096usize;
    walk_style_rules(css, &mut budget, &mut |_, block| {
        if out.len() >= MAX_CUSTOM_PROPS_PER_SHEET {
            return;
        }
        for decl in split_declarations(block) {
            let Some((name, value)) = split_declaration(decl) else {
                continue;
            };
            if !name.starts_with(b"--") || name.len() <= 2 {
                continue;
            }
            let mut key = String::from_utf8_lossy(trim_ascii(name)).into_owned();
            key.truncate(128);
            let mut text = String::from_utf8_lossy(trim_ascii(value)).into_owned();
            text.truncate(MAX_RAW_VALUE_BYTES);
            if key.len() > 2 && !text.is_empty() {
                out.push((key, text));
            }
            if out.len() >= MAX_CUSTOM_PROPS_PER_SHEET {
                break;
            }
        }
    });
    out
}

/// Strips one trailing `!important` (case-insensitive).
fn strip_important(value: &[u8]) -> &[u8] {
    let text = trim_ascii(value);
    let lower = text.to_ascii_lowercase();
    let stripped = lower
        .strip_suffix(b"!important")
        .map(|head| &text[..head.len()])
        .unwrap_or(text);
    trim_ascii(stripped)
}

/// Finds a top-level `var(` outside strings; returns its byte offset.
fn find_var_call(text: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    while i + 4 <= text.len() {
        match text[i] {
            b'"' | b'\'' => {
                let quote = text[i];
                i += 1;
                while i < text.len() && text[i] != quote {
                    i += if text[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            _ => {}
        }
        if text[i..].len() >= 4 && text[i..i + 4].eq_ignore_ascii_case(b"var(") {
            // Not part of a longer identifier (`myvar(`).
            if i == 0 || !(text[i - 1].is_ascii_alphanumeric() || text[i - 1] == b'-') {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Splits `var(` args into (name, fallback?) at the top-level comma.
fn split_var_args(args: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    let mut depth = 0u32;
    let mut i = 0usize;
    while i < args.len() {
        match args[i] {
            b'"' | b'\'' => {
                let quote = args[i];
                i += 1;
                while i < args.len() && args[i] != quote {
                    i += if args[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                return (
                    args[..i].to_vec(),
                    Some(trim_ascii(&args[i + 1..]).to_vec()),
                );
            }
            _ => {}
        }
        i += 1;
    }
    (args.to_vec(), None)
}

/// Resolves `var(--name[, fallback])` refs; `None` when a ref is
/// undefined without fallback, cyclic, or over the depth budget
/// (the declaration is then invalid and dropped).
#[must_use]
pub fn resolve_vars(value: &[u8], props: &HashMap<String, Vec<u8>>) -> Option<Vec<u8>> {
    fn inner(
        value: &[u8],
        props: &HashMap<String, Vec<u8>>,
        depth: u32,
        stack: &mut Vec<String>,
    ) -> Option<Vec<u8>> {
        let Some(at) = find_var_call(value) else {
            return Some(value.to_vec());
        };
        // Match the closing paren (string-aware).
        let mut j = at + 4;
        let mut paren = 1u32;
        while j < value.len() && paren > 0 {
            match value[j] {
                b'"' | b'\'' => {
                    let quote = value[j];
                    j += 1;
                    while j < value.len() && value[j] != quote {
                        j += if value[j] == b'\\' { 2 } else { 1 };
                    }
                    j += 1;
                    continue;
                }
                b'(' => paren += 1,
                b')' => paren -= 1,
                _ => {}
            }
            j += 1;
        }
        if paren > 0 {
            return None;
        }
        let (name_raw, fallback) = split_var_args(&value[at + 4..j - 1]);
        let name = String::from_utf8(trim_ascii(&name_raw).to_vec()).ok()?;
        if !name.starts_with("--") {
            return None;
        }
        if depth >= MAX_VAR_DEPTH || stack.contains(&name) {
            return None;
        }
        let mut replacement = if let Some(found) = props.get(&name) {
            stack.push(name);
            let resolved = inner(found, props, depth + 1, stack);
            stack.pop();
            resolved?
        } else if let Some(fallback) = fallback {
            inner(&fallback, props, depth + 1, stack)?
        } else {
            return None;
        };
        let mut out = value[..at].to_vec();
        out.append(&mut replacement);
        // Continue after the substituted span (it is already resolved).
        let mut rest = inner(&value[j..], props, depth, stack)?;
        out.append(&mut rest);
        Some(out)
    }
    if find_var_call(value).is_none() {
        return Some(value.to_vec());
    }
    inner(value, props, 0, &mut Vec::new())
}

/// CSS-wide keywords are not families (`inherit`, `unset`, ...).
fn is_css_wide_keyword(name: &str) -> bool {
    matches!(
        name,
        "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    )
}

/// Generic families never have faces (`serif`, `cursive`, ...).
fn is_generic_family(name: &str) -> bool {
    matches!(
        name,
        "serif"
            | "sans-serif"
            | "monospace"
            | "cursive"
            | "fantasy"
            | "system-ui"
            | "ui-serif"
            | "ui-sans-serif"
            | "ui-monospace"
            | "ui-rounded"
            | "emoji"
            | "math"
            | "fangsong"
    )
}

/// Scans qualified rules (plus nested screen grouping rules) for
/// raw font usages in document order; print-only `@media` is
/// skipped. Bounded; malformed input truncates. Resolve with
/// [`resolve_raw_usages`] once every sheet's custom properties and
/// the document element sets are known (order-independent).
#[must_use]
pub fn scan_stylesheet_usages(css: &[u8]) -> Vec<RawFontUsage> {
    let mut out = Vec::new();
    // At most one usage scan per ruleset, capped for adversarial CSS.
    let mut budget = 4096usize;
    walk_style_rules(css, &mut budget, &mut |selector, block| {
        if out.len() < MAX_CACHED_RAW_USAGES {
            out.extend(raw_usage_from_declarations(block, selector));
        }
    });
    // Dedup preserving first-use order.
    let mut seen = HashSet::new();
    out.into_iter()
        .filter(|raw| {
            seen.insert((
                raw.families.clone(),
                raw.weight.clone(),
                raw.style.clone(),
                raw.selector.clone(),
            ))
        })
        .collect()
}

/// Max raw usages kept per stylesheet scan.
const MAX_CACHED_RAW_USAGES: usize = 256;

/// Selector check outcome against the document element sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorVerdict {
    /// Page-wide rule (`body`, `html`, `*`, `:root`).
    PageWide,
    /// Headings, header/nav, or site title.
    Structural,
    /// Simple selector with every part present in the document.
    Verified,
    /// Too complex to check (assumed used; fail-open).
    Assumed,
    /// Simple selector with no possible match (safe to drop).
    Unmatched,
}

/// Structural tags: headings, header/nav chrome.
fn is_structural_tag(tag: &str) -> bool {
    matches!(
        tag,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "nav" | "header"
    )
}

/// Checks one comma-part: the last compound must match (ancestry is
/// ignored, fail-open). Returns the confidence when matched.
fn verify_compound(part: &[u8], dom: &DomSets) -> Option<UsageConfidence> {
    let text = trim_ascii(part);
    if text.is_empty() {
        return None;
    }
    // Attribute selectors and pseudo-classes (except bare `:root`)
    // are beyond the checker: assumed used.
    if text.contains(&b'[') {
        return Some(UsageConfidence::Assumed);
    }
    if text.contains(&b':') {
        if text.eq_ignore_ascii_case(b":root") {
            return Some(UsageConfidence::Certain);
        }
        return Some(UsageConfidence::Assumed);
    }
    if text == b"*" {
        return Some(UsageConfidence::Certain);
    }
    if text.eq_ignore_ascii_case(b"body") || text.eq_ignore_ascii_case(b"html") {
        return Some(UsageConfidence::Certain);
    }
    // Last compound after combinators.
    let mut start = 0usize;
    for (i, b) in text.iter().enumerate() {
        if b.is_ascii_whitespace() || matches!(b, b'>' | b'+' | b'~' | b',' | b'|') {
            start = i + 1;
        }
    }
    let compound = trim_ascii(&text[start..]);
    if compound.is_empty() {
        return None;
    }
    // Optional type selector, then `#id`/`.class` steps.
    let mut rest = compound;
    let mut tag: Option<Vec<u8>> = None;
    if rest[0] == b'*' {
        rest = &rest[1..];
    } else if rest[0].is_ascii_alphabetic() {
        let mut end = 0usize;
        while end < rest.len()
            && (rest[end].is_ascii_alphanumeric() || rest[end] == b'-' || rest[end] == b'_')
        {
            end += 1;
        }
        tag = Some(rest[..end].to_ascii_lowercase());
        rest = &rest[end..];
    }
    let mut id: Option<Vec<u8>> = None;
    let mut classes = Vec::new();
    while !rest.is_empty() {
        let (kind, after) = (rest[0], &rest[1..]);
        if kind != b'#' && kind != b'.' {
            return Some(UsageConfidence::Assumed);
        }
        let mut end = 0usize;
        while end < after.len()
            && (after[end].is_ascii_alphanumeric() || after[end] == b'-' || after[end] == b'_')
        {
            end += 1;
        }
        if end == 0 {
            return Some(UsageConfidence::Assumed);
        }
        let name = after[..end].to_vec();
        if kind == b'#' {
            if id.replace(name).is_some() {
                return Some(UsageConfidence::Assumed);
            }
        } else {
            classes.push(name);
        }
        rest = &after[end..];
    }
    if tag.is_none() && id.is_none() && classes.is_empty() {
        return None;
    }
    let tag_ok = tag
        .as_deref()
        .is_none_or(|tag| std::str::from_utf8(tag).is_ok_and(|tag| dom.tags.contains(tag)));
    let id_ok = id
        .as_deref()
        .is_none_or(|id| std::str::from_utf8(id).is_ok_and(|id| dom.ids.contains(id)));
    let classes_ok = classes
        .iter()
        .all(|class| std::str::from_utf8(class).is_ok_and(|class| dom.classes.contains(class)));
    if !(tag_ok && id_ok && classes_ok) {
        return None;
    }
    let structural = tag
        .as_deref()
        .is_some_and(|tag| std::str::from_utf8(tag).is_ok_and(is_structural_tag))
        || classes
            .iter()
            .any(|class| class.eq_ignore_ascii_case(b"site-title"));
    Some(if structural {
        UsageConfidence::Structural
    } else {
        UsageConfidence::Verified
    })
}

/// Verifies a selector list: the best verdict across comma-parts wins.
#[must_use]
pub fn verify_selector(selector: &str, dom: &DomSets) -> SelectorVerdict {
    let mut best = SelectorVerdict::Unmatched;
    for part in split_top_commas(selector.as_bytes()) {
        let verdict = match verify_compound(part, dom) {
            Some(UsageConfidence::Certain) => SelectorVerdict::PageWide,
            Some(UsageConfidence::Structural) => SelectorVerdict::Structural,
            Some(UsageConfidence::Verified) => SelectorVerdict::Verified,
            Some(UsageConfidence::Assumed) => SelectorVerdict::Assumed,
            None => continue,
        };
        let rank = |verdict: SelectorVerdict| match verdict {
            SelectorVerdict::PageWide => 0,
            SelectorVerdict::Structural => 1,
            SelectorVerdict::Verified => 2,
            SelectorVerdict::Assumed => 3,
            SelectorVerdict::Unmatched => 4,
        };
        if rank(verdict) < rank(best) {
            best = verdict;
        }
    }
    best
}

/// Resolves raw usages: substitutes `var()` refs from the merged
/// custom-property map, drops invalid declarations and generics,
/// and verifies selectors (`inline` usages are certain by
/// construction). Dedups preserving first-use order.
#[must_use]
pub fn resolve_raw_usages(
    raw: &[RawFontUsage],
    props: &HashMap<String, Vec<u8>>,
    dom: &DomSets,
    inline: bool,
) -> Vec<FontUsage> {
    let mut out = Vec::new();
    for item in raw {
        let confidence = if inline {
            UsageConfidence::Certain
        } else {
            match verify_selector(&item.selector, dom) {
                SelectorVerdict::PageWide => UsageConfidence::Certain,
                SelectorVerdict::Structural => UsageConfidence::Structural,
                SelectorVerdict::Verified => UsageConfidence::Verified,
                SelectorVerdict::Assumed => UsageConfidence::Assumed,
                SelectorVerdict::Unmatched => continue,
            }
        };
        let Some(resolved) = resolve_vars(strip_important(item.families.as_bytes()), props) else {
            continue;
        };
        let (wmin, wmax) = resolve_vars(item.weight.as_bytes(), props)
            .and_then(|raw| parse_weight(&raw))
            .unwrap_or((400, 400));
        let style = resolve_vars(item.style.as_bytes(), props)
            .and_then(|raw| parse_style(&raw))
            .unwrap_or(FontStyle::Normal);
        for part in split_top_commas(&resolved) {
            let name = unquote(part);
            let Ok(name) = String::from_utf8(name.to_ascii_lowercase()) else {
                continue;
            };
            if name.is_empty() || is_generic_family(&name) || is_css_wide_keyword(&name) {
                continue;
            }
            out.push(FontUsage {
                family: name,
                weight: wmin,
                weight_max: wmax,
                style,
                confidence,
                selector: item.selector.clone(),
            });
        }
    }
    let mut seen = HashSet::new();
    out.into_iter()
        .filter(|usage| {
            seen.insert((
                usage.family.clone(),
                usage.weight,
                usage.weight_max,
                usage.style as u8,
                usage.confidence as u8,
            ))
        })
        .collect()
}

/// Whether a face can satisfy a usage: family equality, weight
/// ranges overlapping (variable fonts on either side), and style
/// equality — except normal faces, which also cover synthesized
/// italic/oblique requests.
#[must_use]
pub fn face_matches(face: &FontFace, usage: &FontUsage) -> bool {
    if face.family != usage.family {
        return false;
    }
    if usage.weight > face.weight_max || usage.weight_max < face.weight_min {
        return false;
    }
    face.style == usage.style || face.style == FontStyle::Normal
}

/// Whether any range touches Latin basic (empty means full coverage).
fn covers_latin(ranges: &[(u32, u32)]) -> bool {
    ranges.is_empty() || ranges.iter().any(|(lo, _)| *lo <= LATIN_MAX)
}

/// Whether every range sits inside private-use planes (icon fonts).
/// Empty ranges are full fonts, never icons.
fn is_private_use_only(ranges: &[(u32, u32)]) -> bool {
    !ranges.is_empty()
        && ranges.iter().all(|(lo, hi)| {
            PRIVATE_USE_RANGES
                .iter()
                .any(|(plo, phi)| *lo >= *plo && *hi <= *phi)
        })
}

/// Whether a face is preload-eligible: full or Latin-covering
/// ranges, never icon-only — unless the beacon confirmed it.
/// Malformed-range faces fail open (parsed as full above).
#[must_use]
pub fn face_eligible(face: &FontFace, beacon_confirmed: bool) -> bool {
    if beacon_confirmed {
        return true;
    }
    if is_private_use_only(&face.ranges) {
        return false;
    }
    covers_latin(&face.ranges)
}

/// MIME type for a font URL, by extension (lowercased, query stripped).
#[must_use]
pub fn font_mime_for_url(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    match ext.as_str() {
        "woff2" => Some("font/woff2"),
        "woff" => Some("font/woff"),
        "ttf" => Some("font/ttf"),
        "otf" => Some("font/otf"),
        _ => None,
    }
}

/// Format preference rank (lower first): modern, smaller containers win.
fn format_rank(format: Option<&str>, url: &str) -> u32 {
    let name = format.unwrap_or("").to_ascii_lowercase();
    if name.contains("woff2") {
        return 0;
    }
    if name.contains("woff") {
        return 1;
    }
    if name.contains("truetype") || name.contains("opentype") {
        return 2;
    }
    match font_mime_for_url(url) {
        Some("font/woff2") => 0,
        Some("font/woff") => 1,
        Some("font/ttf") | Some("font/otf") => 2,
        _ => 3,
    }
}

/// Best downloadable source: preferred format, http(s) or root path,
/// never `data:` (already inlined). Shared with the learning gate.
pub(crate) fn best_source(face: &FontFace) -> Option<&FontSource> {
    face.sources
        .iter()
        .filter(|source| {
            let url = source.url.trim();
            !url.is_empty()
                && !url.to_ascii_lowercase().starts_with("data:")
                && format_rank(source.format.as_deref(), url) < 3
        })
        .min_by_key(|source| format_rank(source.format.as_deref(), &source.url))
}

/// One decided preload: absolute URL plus MIME (`crossorigin` is
/// always anonymous at emission).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontPreload {
    /// Absolute font file URL (caller-resolved).
    pub url: String,
    /// MIME type for the `type` attribute.
    pub mime: String,
}

/// Per-family selection outcome (diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontOutcome {
    /// A preload was emitted for this family.
    Selected,
    /// Eligible but not emitted (cap, duplicate, author preload).
    Skipped,
    /// Never preloadable under current evidence.
    Rejected,
}

/// Why a family was selected, skipped, or rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontNote {
    /// Lowercased family name.
    pub family: String,
    /// Selection outcome.
    pub outcome: FontOutcome,
    /// Short human-readable reason.
    pub reason: String,
}

/// Selection basis for a usage (diagnostic fragment).
fn usage_basis(usage: &FontUsage, is_confirmed: bool) -> String {
    if is_confirmed {
        return "beacon-confirmed usage".to_string();
    }
    match usage.confidence {
        UsageConfidence::Certain if usage.selector.is_empty() => {
            "inline style on a rendered element".to_string()
        }
        UsageConfidence::Certain => format!("page-wide rule ({})", usage.selector),
        UsageConfidence::Structural => format!("structural selector ({})", usage.selector),
        UsageConfidence::Verified => format!("used by `{}`", usage.selector),
        UsageConfidence::Assumed => format!("unverifiable selector ({})", usage.selector),
    }
}

/// Selects up to `MAX_FONT_PRELOADS` preloads: beacon-confirmed
/// usages first, then confidence, then document order; identical
/// URLs and `skip_urls` (author preloads) deduped. `confirmed`
/// holds beacon-verified (family, weight, style) triples. Returns
/// per-family diagnostic notes alongside.
#[must_use]
pub fn select_preloads_explained(
    faces: &[FontFace],
    usages: &[FontUsage],
    confirmed: &HashSet<(String, u32, FontStyle)>,
    skip_urls: &HashSet<String>,
) -> (Vec<FontPreload>, Vec<FontNote>) {
    let mut ordered: Vec<(bool, UsageConfidence, usize)> = usages
        .iter()
        .enumerate()
        .map(|(index, usage)| {
            (
                confirmed.contains(&(usage.family.clone(), usage.weight, usage.style)),
                usage.confidence,
                index,
            )
        })
        .collect();
    ordered.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    // Family faces in document order; each usage takes its first match.
    let mut by_family: HashMap<&str, Vec<&FontFace>> = HashMap::new();
    for face in faces {
        by_family
            .entry(face.family.as_str())
            .or_default()
            .push(face);
    }
    // Outcome precedence: a family keeps its best result.
    let mut notes: HashMap<String, FontNote> = HashMap::new();
    let mut note = |family: &str, outcome: FontOutcome, reason: String| {
        let rank = |outcome: FontOutcome| match outcome {
            FontOutcome::Selected => 0,
            FontOutcome::Skipped => 1,
            FontOutcome::Rejected => 2,
        };
        notes
            .entry(family.to_string())
            .and_modify(|kept| {
                if rank(outcome) < rank(kept.outcome) {
                    *kept = FontNote {
                        family: family.to_string(),
                        outcome,
                        reason: reason.clone(),
                    };
                }
            })
            .or_insert(FontNote {
                family: family.to_string(),
                outcome,
                reason,
            });
    };
    let mut seen_urls = HashSet::new();
    let mut out = Vec::new();
    for (is_confirmed, _, index) in &ordered {
        let usage = &usages[*index];
        let Some(candidates) = by_family.get(usage.family.as_str()) else {
            note(
                &usage.family,
                FontOutcome::Rejected,
                "no @font-face declared".to_string(),
            );
            continue;
        };
        let mut matched: Option<&FontFace> = None;
        let mut blocked = None;
        for face in candidates {
            if !face_matches(face, usage) {
                blocked = Some("weight/style mismatch".to_string());
                continue;
            }
            if !face_eligible(face, *is_confirmed) {
                blocked = Some(if is_private_use_only(&face.ranges) {
                    "icon-only face without beacon confirmation".to_string()
                } else {
                    "non-Latin subset without beacon confirmation".to_string()
                });
                continue;
            }
            matched = Some(face);
            break;
        }
        let Some(face) = matched else {
            note(
                &usage.family,
                FontOutcome::Rejected,
                blocked.unwrap_or_else(|| "no face matches usage".to_string()),
            );
            continue;
        };
        let Some(source) = best_source(face) else {
            note(
                &usage.family,
                FontOutcome::Rejected,
                "no downloadable source".to_string(),
            );
            continue;
        };
        let Some(mime) = font_mime_for_url(&source.url) else {
            note(
                &usage.family,
                FontOutcome::Rejected,
                "unsupported font container".to_string(),
            );
            continue;
        };
        if skip_urls.contains(&source.url) {
            note(
                &usage.family,
                FontOutcome::Skipped,
                "page already preloads this URL".to_string(),
            );
            continue;
        }
        if !seen_urls.insert(source.url.clone()) {
            note(
                &usage.family,
                FontOutcome::Skipped,
                "duplicate URL".to_string(),
            );
            continue;
        }
        if out.len() >= MAX_FONT_PRELOADS {
            note(
                &usage.family,
                FontOutcome::Skipped,
                format!("over the {MAX_FONT_PRELOADS}-preload cap"),
            );
            continue;
        }
        let basis = usage_basis(usage, *is_confirmed);
        let reason = if face.weight_min == face.weight_max {
            format!("{basis}; weight {}", face.weight_min)
        } else {
            format!(
                "{basis}; usage {}-{} overlaps face {}-{}",
                usage.weight, usage.weight_max, face.weight_min, face.weight_max
            )
        };
        note(&usage.family, FontOutcome::Selected, reason);
        out.push(FontPreload {
            url: source.url.clone(),
            mime: mime.to_string(),
        });
    }
    // Declared but never used families.
    for face in faces {
        if !usages.iter().any(|usage| usage.family == face.family) {
            note(
                &face.family,
                FontOutcome::Rejected,
                "no matching usage".to_string(),
            );
        }
    }
    let mut notes: Vec<FontNote> = notes.into_values().collect();
    notes.sort_by(|a, b| a.family.cmp(&b.family));
    (out, notes)
}

/// Selects up to `MAX_FONT_PRELOADS` preloads (see
/// [`select_preloads_explained`]; no author-URL skips).
#[must_use]
pub fn select_preloads(
    faces: &[FontFace],
    usages: &[FontUsage],
    confirmed: &HashSet<(String, u32, FontStyle)>,
) -> Vec<FontPreload> {
    select_preloads_explained(faces, usages, confirmed, &HashSet::new()).0
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE_FONTS_CSS: &str = "@font-face {\n  font-family: 'Manrope';\n  font-style: normal;\n  font-weight: 400;\n  font-display: swap;\n  src: url(https://fonts.gstatic.com/s/manrope/v15/xn7gYHE41ni1AdIRggexSg.woff2) format('woff2');\n  unicode-range: U+0000-00FF;\n}\n@font-face {\n  font-family: 'Manrope';\n  font-style: normal;\n  font-weight: 700;\n  src: url(https://fonts.gstatic.com/s/manrope/v15/bold.woff2) format('woff2'),\n       url(/fonts/manrope-bold.woff) format('woff');\n}\n@font-face {\n  font-family: 'Icons';\n  src: url(/fonts/icons.woff2) format('woff2');\n  unicode-range: U+E000-F8FF;\n}\n@import url(https://fonts.googleapis.com/css2?family=Other);\n";

    fn usage(family: &str, weight: u32, style: FontStyle) -> FontUsage {
        FontUsage {
            family: family.to_string(),
            weight,
            weight_max: weight,
            style,
            confidence: UsageConfidence::Verified,
            selector: String::new(),
        }
    }

    fn dom_with(tags: &[&str], classes: &[&str], ids: &[&str]) -> DomSets {
        DomSets {
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            classes: classes.iter().map(|class| class.to_string()).collect(),
            ids: ids.iter().map(|id| id.to_string()).collect(),
        }
    }

    fn resolve_all(css: &str, dom: &DomSets) -> Vec<FontUsage> {
        let raw = scan_stylesheet_usages(css.as_bytes());
        let props: HashMap<String, Vec<u8>> = collect_custom_properties(css.as_bytes())
            .into_iter()
            .map(|(key, value)| (key, value.into_bytes()))
            .collect();
        resolve_raw_usages(&raw, &props, dom, false)
    }

    #[test]
    fn scan_extracts_faces_and_imports() {
        let scanned = scan_stylesheet(GOOGLE_FONTS_CSS.as_bytes());
        assert_eq!(scanned.faces.len(), 3);
        assert_eq!(scanned.imports.len(), 1);
        assert_eq!(
            scanned.imports[0],
            "https://fonts.googleapis.com/css2?family=Other"
        );
        let first = &scanned.faces[0];
        assert_eq!(first.family, "manrope");
        assert_eq!((first.weight_min, first.weight_max), (400, 400));
        assert_eq!(first.style, FontStyle::Normal);
        assert_eq!(first.sources.len(), 1);
        assert_eq!(
            first.sources[0].url,
            "https://fonts.gstatic.com/s/manrope/v15/xn7gYHE41ni1AdIRggexSg.woff2"
        );
        assert_eq!(first.sources[0].format.as_deref(), Some("woff2"));
        assert_eq!(first.ranges, vec![(0x0000, 0x00FF)]);
        let bold = &scanned.faces[1];
        assert_eq!((bold.weight_min, bold.weight_max), (700, 700));
        assert_eq!(bold.sources.len(), 2);
        assert!(bold.ranges.is_empty());
    }

    #[test]
    fn scan_handles_ranges_comments_and_media() {
        let css = "/* lead */ @FONT-FACE { font-family: A; src: url(a.woff2); \
         unicode-range: U+4??, U+2000-206F; } \
         @media screen { @font-face { font-family: B; src: url(b.woff); } } \
         p { color: red; }";
        let scanned = scan_stylesheet(css.as_bytes());
        assert_eq!(scanned.faces.len(), 2);
        assert_eq!(scanned.faces[0].family, "a");
        assert_eq!(
            scanned.faces[0].ranges,
            vec![(0x0400, 0x04FF), (0x2000, 0x206F)]
        );
        assert_eq!(scanned.faces[1].family, "b");
        // Defaults: 400/normal.
        assert_eq!(
            (scanned.faces[1].weight_min, scanned.faces[1].weight_max),
            (400, 400)
        );
        assert_eq!(scanned.faces[1].style, FontStyle::Normal);
    }

    #[test]
    fn scan_skips_malformed_without_aborting() {
        // No family, no URL source, unterminated block: all skipped,
        // the valid face still collected.
        let css = "@font-face { src: url(a.woff2); } \
         @font-face { font-family: X; src: local('X'); } \
         @font-face { font-family: 'Good'; src: url(g.woff2); } \
         @font-face { font-family: 'Broken'; src: url(";
        let scanned = scan_stylesheet(css.as_bytes());
        assert_eq!(scanned.faces.len(), 1);
        assert_eq!(scanned.faces[0].family, "good");
    }

    #[test]
    fn weight_and_style_forms() {
        let face = |css: &str| {
            scan_stylesheet(
                format!("@font-face {{ font-family: F; src: url(f.woff2); {css} }}").as_bytes(),
            )
            .faces
            .pop()
            .unwrap()
        };
        assert_eq!((face("font-weight: bold;").weight_min, 700), (700, 700));
        let variable = face("font-weight: 100 900;");
        assert_eq!((variable.weight_min, variable.weight_max), (100, 900));
        assert_eq!(face("font-style: italic;").style, FontStyle::Italic);
        assert_eq!(face("font-style: oblique 10deg;").style, FontStyle::Oblique);
        // Garbage keeps spec defaults.
        assert_eq!((face("font-weight: bogus;").weight_min, 400), (400, 400));
    }

    #[test]
    fn matching_covers_ranges_and_synthesis() {
        let scanned = scan_stylesheet(GOOGLE_FONTS_CSS.as_bytes());
        let rate = |weight: u32, style: FontStyle| FontUsage {
            family: "manrope".to_string(),
            weight,
            weight_max: weight,
            style,
            confidence: UsageConfidence::Verified,
            selector: String::new(),
        };
        assert!(face_matches(
            &scanned.faces[0],
            &rate(400, FontStyle::Normal)
        ));
        assert!(face_matches(
            &scanned.faces[1],
            &rate(700, FontStyle::Normal)
        ));
        assert!(!face_matches(
            &scanned.faces[0],
            &rate(700, FontStyle::Normal)
        ));
        // Normal faces cover synthesized italic requests.
        assert!(face_matches(
            &scanned.faces[0],
            &rate(400, FontStyle::Italic)
        ));
        assert!(!face_matches(
            &scanned.faces[0],
            &FontUsage {
                family: "other".to_string(),
                weight: 400,
                weight_max: 400,
                style: FontStyle::Normal,
                confidence: UsageConfidence::Verified,
                selector: String::new(),
            }
        ));
    }

    #[test]
    fn eligibility_prunes_icons_and_subsets() {
        let scanned = scan_stylesheet(GOOGLE_FONTS_CSS.as_bytes());
        assert!(face_eligible(&scanned.faces[0], false));
        assert!(face_eligible(&scanned.faces[1], false));
        // Icon-only PUA face refused unless beacon-confirmed.
        assert!(!face_eligible(&scanned.faces[2], false));
        assert!(face_eligible(&scanned.faces[2], true));
        // Non-latin subset refused unless confirmed.
        let css = "@font-face { font-family: C; src: url(c.woff2); \
             unicode-range: U+0400-04FF; }";
        let face = &scan_stylesheet(css.as_bytes()).faces[0];
        assert!(!face_eligible(face, false));
        assert!(face_eligible(face, true));
    }

    #[test]
    fn mime_mapping_by_extension() {
        assert_eq!(
            font_mime_for_url("https://x.test/f.woff2?v=2"),
            Some("font/woff2")
        );
        assert_eq!(font_mime_for_url("/f/W.FF2"), None);
        assert_eq!(font_mime_for_url("/fonts/a.WOFF"), Some("font/woff"));
        assert_eq!(font_mime_for_url("/fonts/a.ttf"), Some("font/ttf"));
        assert_eq!(font_mime_for_url("/fonts/a.otf"), Some("font/otf"));
        assert_eq!(font_mime_for_url("/fonts/a.eot"), None);
        assert_eq!(font_mime_for_url("/fonts/noext"), None);
    }

    #[test]
    fn selection_prefers_confirmed_caps_two_and_dedups() {
        let scanned = scan_stylesheet(GOOGLE_FONTS_CSS.as_bytes());
        let usages = vec![
            usage("manrope", 400, FontStyle::Normal),
            usage("manrope", 700, FontStyle::Normal),
            usage("icons", 400, FontStyle::Normal),
        ];
        // Unconfirmed: icon face pruned, two manrope preloads.
        let out = select_preloads(&scanned.faces, &usages, &HashSet::new());
        assert_eq!(out.len(), 2);
        assert!(out[0].url.contains("ggexSg.woff2"));
        assert_eq!(out[0].mime, "font/woff2");
        assert!(out[1].url.contains("bold.woff2"));
        // Confirmed icon usage jumps the queue (cap still two).
        let mut confirmed = HashSet::new();
        confirmed.insert(("icons".to_string(), 400, FontStyle::Normal));
        let out = select_preloads(&scanned.faces, &usages, &confirmed);
        assert_eq!(out.len(), 2);
        assert!(out[0].url.contains("icons.woff2"));
        // Duplicate URLs dedup.
        let dup = vec![
            usage("manrope", 400, FontStyle::Normal),
            usage("manrope", 400, FontStyle::Normal),
        ];
        let out = select_preloads(&scanned.faces, &dup, &HashSet::new());
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn usage_scan_orders_and_dedups() {
        let css = "body { font-family: Manrope, sans-serif; font-weight: 400; } \
             h1 { font-family: 'Manrope'; font-weight: 700; } \
             p { font-family: Manrope; } \
             @media screen { span { font-family: Icons; } } \
             div { color: red; }";
        let dom = dom_with(&["body", "h1", "p", "span", "div"], &[], &[]);
        let usages = resolve_all(css, &dom);
        assert_eq!(
            usages
                .iter()
                .map(|usage| (
                    usage.family.as_str(),
                    usage.weight,
                    usage.style,
                    usage.confidence
                ))
                .collect::<Vec<_>>(),
            vec![
                ("manrope", 400, FontStyle::Normal, UsageConfidence::Certain),
                (
                    "manrope",
                    700,
                    FontStyle::Normal,
                    UsageConfidence::Structural
                ),
                ("manrope", 400, FontStyle::Normal, UsageConfidence::Verified),
                ("icons", 400, FontStyle::Normal, UsageConfidence::Verified),
            ]
        );
    }

    #[test]
    fn usage_scan_skips_generic_and_malformed() {
        let css = "a { font-family: serif; } b { font-weight: 700; } \
             c { font-family: ; } d { font-family: 'Good', fantasy; \
             font-style: italic; }";
        let dom = dom_with(&["a", "b", "c", "d"], &[], &[]);
        let usages = resolve_all(css, &dom);
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].family, "good");
        assert_eq!(usages[0].style, FontStyle::Italic);
    }

    #[test]
    fn var_resolution_substitutes_fallbacks_and_nests() {
        let props: HashMap<String, Vec<u8>> = [
            ("--a".to_string(), b"Manrope , sans-serif".to_vec()),
            ("--b".to_string(), b"var(--a)".to_vec()),
            ("--c".to_string(), b"var(--missing, \"Fira Code\")".to_vec()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            resolve_vars(b"var(--a)", &props).as_deref(),
            Some(b"Manrope , sans-serif".as_slice())
        );
        assert_eq!(
            resolve_vars(b"var(--b)", &props).as_deref(),
            Some(b"Manrope , sans-serif".as_slice())
        );
        assert_eq!(
            resolve_vars(b"var(--c)", &props).as_deref(),
            Some(b"\"Fira Code\"".as_slice())
        );
        // Undefined without fallback invalidates the declaration.
        assert_eq!(resolve_vars(b"var(--nope)", &props), None);
        // Cycles fail closed.
        let cyclic: HashMap<String, Vec<u8>> = [("--x".to_string(), b"var(--x)".to_vec())]
            .into_iter()
            .collect();
        assert_eq!(resolve_vars(b"var(--x)", &cyclic), None);
        // Names are case-sensitive.
        assert_eq!(resolve_vars(b"var(--A)", &props), None);
        // Plain values pass through untouched.
        assert_eq!(
            resolve_vars(b"Manrope, serif", &props).as_deref(),
            Some(b"Manrope, serif".as_slice())
        );
    }

    #[test]
    fn wordpress_preset_var_body_resolves_to_manrope() {
        let css = ":root{--wp--preset--font-family--manrope:Manrope , sans-serif;}\
             body{font-family: var(--wp--preset--font-family--manrope);font-weight:300;}";
        let dom = dom_with(&["html", "body", "p"], &[], &[]);
        let usages = resolve_all(css, &dom);
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].family, "manrope");
        assert_eq!(usages[0].weight, 300);
        assert_eq!(usages[0].confidence, UsageConfidence::Certain);
    }

    #[test]
    fn discovery_is_declaration_order_independent() {
        let forward = "body{font-family:var(--f);} :root{--f:Manrope;}";
        let backward = ":root{--f:Manrope;} body{font-family:var(--f);}";
        let dom = dom_with(&["body"], &[], &[]);
        // Props merge across blocks before any usage resolves, so the
        // winning value does not depend on scan order.
        for css in [forward, backward] {
            let usages = resolve_all(css, &dom);
            assert_eq!(usages.len(), 1);
            assert_eq!(usages[0].family, "manrope");
        }
        // Faces match usages regardless of relative order too.
        let faces_first = "@font-face{font-family:A;src:url(a.woff2);} p{font-family:A;}";
        let usage_first = "p{font-family:A;} @font-face{font-family:A;src:url(a.woff2);}";
        for css in [faces_first, usage_first] {
            let faces = scan_stylesheet(css.as_bytes()).faces;
            let usages = resolve_all(css, &dom_with(&["p"], &[], &[]));
            assert!(face_matches(&faces[0], &usages[0]));
        }
    }

    #[test]
    fn selector_verification_ranks_and_drops() {
        let dom = dom_with(&["h1", "p"], &["used", "site-title"], &["main"]);
        assert_eq!(verify_selector("body", &dom), SelectorVerdict::PageWide);
        assert_eq!(verify_selector(":root", &dom), SelectorVerdict::PageWide);
        assert_eq!(verify_selector("h1", &dom), SelectorVerdict::Structural);
        assert_eq!(
            verify_selector("nav a", &dom_with(&["nav", "a"], &[], &[])),
            SelectorVerdict::Verified
        );
        assert_eq!(
            verify_selector(".site-title", &dom),
            SelectorVerdict::Structural
        );
        assert_eq!(verify_selector(".used", &dom), SelectorVerdict::Verified);
        assert_eq!(verify_selector("p.used", &dom), SelectorVerdict::Verified);
        assert_eq!(verify_selector("#main", &dom), SelectorVerdict::Verified);
        // Absent parts drop the usage.
        assert_eq!(
            verify_selector(".missing", &dom),
            SelectorVerdict::Unmatched
        );
        assert_eq!(verify_selector("h2", &dom), SelectorVerdict::Unmatched);
        assert_eq!(
            verify_selector("p.missing", &dom),
            SelectorVerdict::Unmatched
        );
        // Complex selectors fail open; lists take the best verdict.
        assert_eq!(verify_selector("a:hover", &dom), SelectorVerdict::Assumed);
        assert_eq!(verify_selector("[lang] p", &dom), SelectorVerdict::Assumed);
        assert_eq!(
            verify_selector(".missing, p", &dom),
            SelectorVerdict::Verified
        );
    }

    #[test]
    fn print_only_media_is_skipped_everywhere() {
        let css = "@media print { p { font-family: Print; } \
             @font-face { font-family: Print; src: url(p.woff2); } } \
             @media screen { p { font-family: Screen; } }";
        let faces = scan_stylesheet(css.as_bytes()).faces;
        assert!(faces.is_empty());
        let usages = resolve_all(css, &dom_with(&["p"], &[], &[]));
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].family, "screen");
        // Mixed screen+print branches stay (fail-open).
        let mixed = "@media screen, print { p { font-family: Both; } }";
        let usages = resolve_all(mixed, &dom_with(&["p"], &[], &[]));
        assert_eq!(usages.len(), 1);
    }

    #[test]
    fn css_wide_keywords_are_not_families() {
        let css = "p{font-family:inherit;} q{font-family:Manrope;} \
             @font-face{font-family:initial;src:url(x.woff2);}";
        let usages = resolve_all(css, &dom_with(&["p", "q"], &[], &[]));
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].family, "manrope");
        let faces = scan_stylesheet(css.as_bytes()).faces;
        assert!(faces.is_empty());
    }

    #[test]
    fn variable_weight_ranges_overlap_both_directions() {
        let face = FontFace {
            family: "manrope".to_string(),
            weight_min: 200,
            weight_max: 800,
            style: FontStyle::Normal,
            sources: Vec::new(),
            ranges: Vec::new(),
        };
        let rate = |min: u32, max: u32| FontUsage {
            family: "manrope".to_string(),
            weight: min,
            weight_max: max,
            style: FontStyle::Normal,
            confidence: UsageConfidence::Verified,
            selector: String::new(),
        };
        assert!(face_matches(&face, &rate(300, 300)));
        assert!(face_matches(&face, &rate(200, 200)));
        assert!(face_matches(&face, &rate(800, 800)));
        assert!(face_matches(&face, &rate(300, 500)));
        assert!(!face_matches(&face, &rate(100, 100)));
        assert!(!face_matches(&face, &rate(900, 900)));
        assert!(!face_matches(&face, &rate(100, 199)));
    }

    #[test]
    fn explained_selection_reports_reasons() {
        let scanned = scan_stylesheet(GOOGLE_FONTS_CSS.as_bytes());
        let usages = vec![
            usage("manrope", 400, FontStyle::Normal),
            usage("icons", 400, FontStyle::Normal),
            usage("undeclared", 400, FontStyle::Normal),
        ];
        let (out, notes) =
            select_preloads_explained(&scanned.faces, &usages, &HashSet::new(), &HashSet::new());
        assert_eq!(out.len(), 1);
        let by_family: HashMap<&str, &FontNote> = notes
            .iter()
            .map(|note| (note.family.as_str(), note))
            .collect();
        assert_eq!(by_family["manrope"].outcome, FontOutcome::Selected);
        assert_eq!(by_family["icons"].outcome, FontOutcome::Rejected);
        assert!(by_family["icons"].reason.contains("icon-only"));
        assert_eq!(by_family["undeclared"].outcome, FontOutcome::Rejected);
        // Cap overflow reports Skipped with a reason.
        let mut faces = scanned.faces.clone();
        faces.push(FontFace {
            family: "extra".to_string(),
            weight_min: 400,
            weight_max: 400,
            style: FontStyle::Normal,
            sources: vec![FontSource {
                url: "https://x.test/e.woff2".to_string(),
                format: Some("woff2".to_string()),
            }],
            ranges: Vec::new(),
        });
        let many = vec![
            usage("manrope", 400, FontStyle::Normal),
            usage("icons", 400, FontStyle::Normal),
            usage("extra", 400, FontStyle::Normal),
        ];
        let mut confirmed = HashSet::new();
        confirmed.insert(("icons".to_string(), 400, FontStyle::Normal));
        let (out, notes) = select_preloads_explained(&faces, &many, &confirmed, &HashSet::new());
        assert_eq!(out.len(), 2);
        let by_family: HashMap<&str, &FontNote> = notes
            .iter()
            .map(|note| (note.family.as_str(), note))
            .collect();
        assert_eq!(by_family["extra"].outcome, FontOutcome::Skipped);
        assert!(by_family["extra"].reason.contains("cap"));
    }

    #[test]
    fn best_source_prefers_woff2_over_woff() {
        let scanned = scan_stylesheet(GOOGLE_FONTS_CSS.as_bytes());
        let out = select_preloads(
            &scanned.faces,
            &[usage("manrope", 700, FontStyle::Normal)],
            &HashSet::new(),
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].url.ends_with("bold.woff2"));
    }
}

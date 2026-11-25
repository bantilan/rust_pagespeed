//! Critical font discovery for preload hints (pure core).
//!
//! Scans stylesheets for `@font-face` rules (plus `@import` edges
//! for the caller to follow), extracts family/weight/style/src/
//! format/unicode-range, matches faces against page font usage,
//! prunes icon-only and unnecessary-subset faces, and selects at
//! most two preloads. Self-contained scanner: the shared CSS parser
//! splinters `unicode-range` syntax, so block scanning and the six
//! descriptor parsers live here. No I/O; fully unit-tested.

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
fn parse_family(value: &[u8]) -> Option<String> {
    let first = split_top_commas(value).first()?.to_vec();
    let name = unquote(&first);
    if name.is_empty() {
        return None;
    }
    String::from_utf8(name.to_ascii_lowercase()).ok()
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

/// One observed font usage (DOM scan or beacon), matched against faces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontUsage {
    /// Family name, lowercased (matches face normalization).
    pub family: String,
    /// Requested weight (400 when unknown).
    pub weight: u32,
    /// Requested style (normal when unknown).
    pub style: FontStyle,
}

/// Collects font usages from one ruleset's declarations: every
/// `font-family` name paired with the rule's weight/style (or
/// 400/normal defaults). Generic families are skipped (no face).
/// Shared with the filter for inline `style` attributes.
pub(crate) fn usages_from_declarations(block: &[u8]) -> Vec<FontUsage> {
    let mut family: Option<Vec<Vec<u8>>> = None;
    let mut weight = (400u32, 400u32);
    let mut style = FontStyle::Normal;
    for decl in split_declarations(block) {
        let Some((name, value)) = split_declaration(decl) else {
            continue;
        };
        if name.eq_ignore_ascii_case(b"font-family") {
            let names: Vec<Vec<u8>> = split_top_commas(value)
                .iter()
                .map(|part| unquote(part))
                .filter(|part| !part.is_empty())
                .collect();
            if !names.is_empty() {
                family = Some(names);
            }
        } else if name.eq_ignore_ascii_case(b"font-weight") {
            if let Some(parsed) = parse_weight(value) {
                weight = parsed;
            }
        } else if name.eq_ignore_ascii_case(b"font-style") {
            if let Some(parsed) = parse_style(value) {
                style = parsed;
            }
        }
    }
    let Some(families) = family else {
        return Vec::new();
    };
    families
        .into_iter()
        .filter_map(|raw| {
            let name = String::from_utf8(raw.to_ascii_lowercase()).ok()?;
            if name.is_empty() || is_generic_family(&name) {
                return None;
            }
            Some(FontUsage {
                family: name,
                weight: weight.0,
                style,
            })
        })
        .collect()
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

/// Scans qualified rules (plus nested grouping rules) for font
/// usages in document order. Bounded; malformed input truncates.
#[must_use]
pub fn scan_stylesheet_usages(css: &[u8]) -> Vec<FontUsage> {
    fn walk(css: &[u8], out: &mut Vec<FontUsage>, budget: &mut usize) {
        let mut i = 0usize;
        while i < css.len() && *budget > 0 {
            i = skip_trivia(css, i);
            if i >= css.len() {
                break;
            }
            // Find the next block or statement end.
            let mut j = i;
            let mut saw_brace = false;
            let mut is_at_rule = false;
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
                    b'@' if j == i => {
                        is_at_rule = true;
                    }
                    _ => {}
                }
                j += 1;
            }
            if !saw_brace {
                i = j + 1;
                continue;
            }
            let Some(block) = balanced_block(css, j) else {
                i = j + 1;
                continue;
            };
            if is_at_rule {
                // Grouping rules (@media/@supports): recurse for usages.
                // @font-face/@import carry no element usages.
                walk(block, out, budget);
            } else if *budget > 0 {
                *budget -= 1;
                out.extend(usages_from_declarations(block));
            }
            i = j + block.len() + 2;
        }
    }
    let mut out = Vec::new();
    // At most one usage scan per ruleset, capped for adversarial CSS.
    let mut budget = 4096usize;
    walk(&css[..css.len().min(MAX_SCAN_BYTES)], &mut out, &mut budget);
    // Dedup preserving first-use order.
    let mut seen = HashSet::new();
    out.into_iter()
        .filter(|usage| seen.insert((usage.family.clone(), usage.weight, usage.style as u8)))
        .collect()
}

/// Whether a face can satisfy a usage: family equality, weight in
/// range, and style equality — except normal faces, which also cover
/// synthesized italic/oblique requests.
#[must_use]
pub fn face_matches(face: &FontFace, usage: &FontUsage) -> bool {
    if face.family != usage.family {
        return false;
    }
    if usage.weight < face.weight_min || usage.weight > face.weight_max {
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

/// Selects up to `MAX_FONT_PRELOADS` preloads: beacon-confirmed
/// usages first, then document order; identical URLs deduped.
/// `confirmed` holds beacon-verified (family, weight, style) triples.
#[must_use]
pub fn select_preloads(
    faces: &[FontFace],
    usages: &[FontUsage],
    confirmed: &HashSet<(String, u32, FontStyle)>,
) -> Vec<FontPreload> {
    let mut ordered: Vec<(bool, usize)> = usages
        .iter()
        .enumerate()
        .map(|(index, usage)| {
            (
                confirmed.contains(&(usage.family.clone(), usage.weight, usage.style)),
                index,
            )
        })
        .collect();
    ordered.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    // Family faces in document order; each usage takes its first match.
    let mut by_family: HashMap<&str, Vec<&FontFace>> = HashMap::new();
    for face in faces {
        by_family
            .entry(face.family.as_str())
            .or_default()
            .push(face);
    }
    let mut seen_urls = HashSet::new();
    let mut out = Vec::new();
    for (is_confirmed, index) in ordered {
        if out.len() >= MAX_FONT_PRELOADS {
            break;
        }
        let usage = &usages[index];
        let matched = by_family.get(usage.family.as_str()).and_then(|candidates| {
            candidates.iter().find_map(|face| {
                if face_matches(face, usage) && face_eligible(face, is_confirmed) {
                    Some(*face)
                } else {
                    None
                }
            })
        });
        let Some(face) = matched else {
            continue;
        };
        let Some(source) = best_source(face) else {
            continue;
        };
        let Some(mime) = font_mime_for_url(&source.url) else {
            continue;
        };
        if !seen_urls.insert(source.url.clone()) {
            continue;
        }
        out.push(FontPreload {
            url: source.url.clone(),
            mime: mime.to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE_FONTS_CSS: &str = "@font-face {\n  font-family: 'Manrope';\n  font-style: normal;\n  font-weight: 400;\n  font-display: swap;\n  src: url(https://fonts.gstatic.com/s/manrope/v15/xn7gYHE41ni1AdIRggexSg.woff2) format('woff2');\n  unicode-range: U+0000-00FF;\n}\n@font-face {\n  font-family: 'Manrope';\n  font-style: normal;\n  font-weight: 700;\n  src: url(https://fonts.gstatic.com/s/manrope/v15/bold.woff2) format('woff2'),\n       url(/fonts/manrope-bold.woff) format('woff');\n}\n@font-face {\n  font-family: 'Icons';\n  src: url(/fonts/icons.woff2) format('woff2');\n  unicode-range: U+E000-F8FF;\n}\n@import url(https://fonts.googleapis.com/css2?family=Other);\n";

    fn usage(family: &str, weight: u32, style: FontStyle) -> FontUsage {
        FontUsage {
            family: family.to_string(),
            weight,
            style,
        }
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
            style,
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
                style: FontStyle::Normal
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
        let usages = scan_stylesheet_usages(css.as_bytes());
        assert_eq!(
            usages,
            vec![
                usage("manrope", 400, FontStyle::Normal),
                usage("manrope", 700, FontStyle::Normal),
                usage("icons", 400, FontStyle::Normal),
            ]
        );
    }

    #[test]
    fn usage_scan_skips_generic_and_malformed() {
        let css = "a { font-family: serif; } b { font-weight: 700; } \
             c { font-family: ; } d { font-family: 'Good', fantasy; \
             font-style: italic; }";
        let usages = scan_stylesheet_usages(css.as_bytes());
        assert_eq!(usages, vec![usage("good", 400, FontStyle::Italic)]);
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

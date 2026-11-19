#!/usr/bin/env python3
"""Translate css_tag_scanner_test.cc Transform goldens to Rust tests.

Usage: translate_scanner_tests.py <css_tag_scanner_test.cc> <out.rs>

Translates RewriteDomainTransformerTest Transform()/TransformStreaming()
cases with a prefix-mapping test double (relative URLs gain the
old-base prefix, exactly as the goldens show; absolute URLs pass
through). HTML-parser tests (ParseCssElement) belong to ps-html and
are skipped; the streaming char-by-char loop and timing case are
hand-written in the preamble. Fails loudly on unrecognized shapes.
"""

import re
import sys

SKIP_STREAMING_HAND = {"StreamingCharByChar",
                       "UnquotedUrlLongWhitespaceIsLinear"}


def strip_comments(body):
    out, i, n = [], 0, len(body)
    while i < n:
        c = body[i]
        if c == '"':
            out.append(c)
            i += 1
            while i < n:
                out.append(body[i])
                if body[i] == "\\":
                    i += 1
                    if i < n:
                        out.append(body[i])
                    i += 1
                elif body[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
        elif c == "/" and i + 1 < n and body[i + 1] == "/":
            while i < n and body[i] != "\n":
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


CXX_ESCAPES = {
    "n": 10, "r": 13, "t": 9, "f": 12, "v": 11, "a": 7, "b": 8,
    "\\": 92, '"': 34, "?": 63, "'": 39,
}


def cxx_decode(literal):
    assert literal.startswith('"') and literal.endswith('"'), literal
    inner, out, i = literal[1:-1], bytearray(), 0
    while i < len(inner):
        c = inner[i]
        if c != "\\":
            out.extend(c.encode("utf-8"))
            i += 1
            continue
        i += 1
        e = inner[i]
        if e == "x":
            i += 1
            digits = ""
            while i < len(inner) and inner[i] in "0123456789ABCDEFabcdef":
                digits += inner[i]
                i += 1
            assert len(digits) == 2, literal
            out.append(int(digits, 16))
        elif e in "01234567":
            digits = ""
            while i < len(inner) and len(digits) < 3 \
                    and inner[i] in "01234567":
                digits += inner[i]
                i += 1
            out.append(int(digits, 8) & 0xFF)
        else:
            assert e in CXX_ESCAPES, literal
            out.append(CXX_ESCAPES[e])
            i += 1
    return bytes(out)


def rust_bytes(data):
    parts = []
    for byte in data:
        if byte == 10:
            parts.append("\\n")
        elif byte == 13:
            parts.append("\\r")
        elif byte == 9:
            parts.append("\\t")
        elif byte == 34:
            parts.append('\\"')
        elif byte == 92:
            parts.append("\\\\")
        elif 32 <= byte < 127:
            parts.append(chr(byte))
        else:
            parts.append(f"\\x{byte:02x}")
    return 'b"' + "".join(parts) + '"'


def concat_arg(arg):
    lits = re.findall(r'"(?:[^"\\]|\\.)*"', arg)
    joined = "".join(lits)
    assert re.sub(r"\s+", "", joined) == re.sub(r"\s+", "", arg), arg
    return b"".join(cxx_decode(lit) for lit in lits)


def snake(name):
    out = re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()
    out = re.sub(r"^2_", "two_", out)
    return out


PREAMBLE = '''//! Ported CSS scanner suite.
//!
//! Mechanically translated from `css_tag_scanner_test.cc` (see
//! `tool/translate_scanner_tests.py`); do not hand-edit expectations.
//! A prefix-mapping test double stands in for the domain machinery
//! (relative URLs gain the old-base prefix, exactly as the goldens
//! show); the scanner under test is production code.

use ps_css::scanner::{
    Scanner, TransformStatus, Transformer, has_import, has_url,
    is_alternate_stylesheet, is_stylesheet_or_alternate, transform_urls,
};

const OLD_BASE: &[u8] = b"http://old-base.com/";

/// Test double matching the golden mapping: relative URLs gain the
/// old-base prefix (with `"` percent-encoded, as GoogleUrl does);
/// absolute, fragment, and data URLs pass through. Full URL
/// canonicalization belongs to the URL layer; this double covers
/// exactly the mappings the goldens pin.
struct PrefixTransformer;

impl Transformer for PrefixTransformer {
    fn transform(&mut self, url: &mut Vec<u8>) -> TransformStatus {
        if url.contains(&b':') || url.starts_with(b"#") {
            return TransformStatus::NoChange;
        }
        let mut mapped = OLD_BASE.to_vec();
        let stripped = url.strip_prefix(b"/").unwrap_or(url);
        for byte in stripped {
            if *byte == b'"' {
                mapped.extend_from_slice(b"%22");
            } else {
                mapped.push(*byte);
            }
        }
        if mapped == *url {
            TransformStatus::NoChange
        } else {
            *url = mapped;
            TransformStatus::Success
        }
    }
}

fn transform(input: &[u8]) -> Vec<u8> {
    let mut transformer = PrefixTransformer;
    transform_urls(input, &mut transformer).expect("transform failed")
}

fn transform_streaming(pieces: &[&[u8]]) -> Vec<u8> {
    let mut transformer = PrefixTransformer;
    let mut scanner = Scanner::new();
    let mut result = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let last = index + 1 == pieces.len();
        let mut out = Vec::new();
        assert!(scanner.transform_streaming(
            piece,
            last,
            &mut transformer,
            &mut out
        ));
        result.extend_from_slice(b"portion=");
        result.extend_from_slice(&out);
        result.extend_from_slice(b", retain=");
        result.extend_from_slice(scanner.retained_for_reparse());
        result.extend_from_slice(b"|");
    }
    result
}

struct FailTransformer;

impl Transformer for FailTransformer {
    fn transform(&mut self, _url: &mut Vec<u8>) -> TransformStatus {
        TransformStatus::Failure
    }
}

#[test]
fn transform_urls_fails() {
    let mut transformer = FailTransformer;
    assert!(transform_urls(b"url(foo)", &mut transformer).is_none());
}

#[test]
fn streaming_char_by_char() {
    // Hand-written: feeds the input one byte at a time.
    let input = b"@import \\"other.css\\"; ul { list-style-image:url(a.png); }";
    let mut transformer = PrefixTransformer;
    let mut scanner = Scanner::new();
    let mut result = Vec::new();
    for (index, byte) in input.iter().enumerate() {
        let last = index + 1 == input.len();
        let mut out = Vec::new();
        assert!(scanner.transform_streaming(
            &[*byte],
            last,
            &mut transformer,
            &mut out
        ));
        result.extend_from_slice(&out);
        result.push(b'|');
    }
    assert_eq!(
        result.as_slice(),
        b"||||||||||||||||||@import \\"http://old-base.com/other.css\\"|;| ||||ul {| |l|i|s|t|-|s|t|y|l|e|-|i|m|a|g|e|:||||||||||url(http://old-base.com/a.png)|;| |}|" as &[u8]);
}

#[test]
fn unquoted_url_long_whitespace_is_linear() {
    // Hand-written: 1MB of trailing whitespace must stay linear.
    let mut input = b"a{background:url(x".to_vec();
    input.extend(std::iter::repeat_n(b' ', 1 << 20));
    let start = std::time::Instant::now();
    let out = transform(&input);
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(out.as_slice(), b"a{background:url(http://old-base.com/x");
}

#[test]
fn helper_predicates() {
    assert!(has_import(b"a @import b"));
    assert!(has_import(b"a @IMPORT b"));
    assert!(!has_import(b"a @media b"));
    assert!(has_url(b"a url(b"));
    assert!(!has_url(b"a URL(b"));
    assert!(is_stylesheet_or_alternate(b"stylesheet"));
    assert!(is_stylesheet_or_alternate(b"alternate stylesheet"));
    assert!(!is_stylesheet_or_alternate(b"alternate"));
    assert!(is_alternate_stylesheet(b"alternate stylesheet"));
    assert!(!is_alternate_stylesheet(b"stylesheet"));
    assert!(!is_alternate_stylesheet(b""));
}

'''

def translate_test(name, body):
    lines = ["#[test]", f"fn {snake(name)}() {{"]
    consts = {}
    renames = {}
    for m in re.finditer(
            r"const char (\w+)\[\] =\s*((?:\"(?:[^\"\\]|\\.)*\"\s*)+);",
            body):
        rust_name = snake(m.group(1))
        renames[m.group(1)] = rust_name
        consts[rust_name] = rust_bytes(concat_arg(m.group(2)))
    arrays = {}
    for m in re.finditer(
            r"const char\* (\w+)\[\] =\s*\{(.*?)\};", body, re.S):
        pieces = []
        for lit in re.findall(r'"(?:[^"\\]|\\.)*"', m.group(2)):
            pieces.append(rust_bytes(cxx_decode(lit)))
        arrays[m.group(1)] = pieces
    stream_cases = list(re.finditer(
        r"EXPECT_EQ\(\s*((?:\"(?:[^\"\\]|\\.)*\"\s*?)+?)\s*,"
        r"\s*TransformStreaming\((\w+)\)\s*\)", body))
    if stream_cases:
        for m in stream_cases:
            expected = rust_bytes(concat_arg(m.group(1)))
            pieces = arrays[m.group(2)]
            lines.append(f"    let out = transform_streaming(&[{', '.join(pieces)}]);")
            lines.append(f"    assert_eq!(out.as_slice(), {expected} as &[u8]);")
        lines.append("}")
        return "\n".join(lines)

    def side(source):
        source = source.strip()
        if re.fullmatch(r"\w+", source):
            assert source in renames, (name, source)
            return renames[source]
        return rust_bytes(concat_arg(source))

    # EXPECT_*(EXPECTED, Transform(INPUT)); each side is a var or a literal.
    expects = list(re.finditer(
        r"EXPECT_(?:STREQ|EQ)\(\s*(\w+|\"(?:[^\"\\]|\\.)*\")",
        body))
    assert expects, f"no cases in {name}"
    for var, value in consts.items():
        lines.append(f"    let {var} = {value};")
    # Re-scan full expectations with balanced matching.
    pos = 0
    cases = []
    while True:
        m = re.search(r"EXPECT_(?:STREQ|EQ)\(\s*", body[pos:])
        if not m:
            break
        rest = body[pos + m.end():]
        exp_src, rest = slurp_side(rest)
        rest = re.sub(r"^\s*,\s*Transform\(\s*", "", rest, count=1)
        in_src, rest = slurp_side(rest)
        rest = re.sub(r"^\s*\)\s*\)", "", rest, count=1)
        cases.append((exp_src, in_src))
        pos = len(body) - len(rest)
    for exp_src, in_src in cases:
        lines.append(f"    assert_eq!(transform({side(in_src)}).as_slice(),")
        lines.append(f"               {side(exp_src)} as &[u8]);")
    lines.append("}")
    return "\n".join(lines)


def slurp_side(text):
    """Slurps a var or literal-concat; returns (source, rest)."""
    text = text.strip()
    if text.startswith('"'):
        out = []
        while text.startswith('"'):
            m = re.match(r'"(?:[^"\\]|\\.)*"', text)
            assert m, text[:60]
            out.append(m.group(0))
            text = text[m.end():].lstrip()
        return " ".join(out), text
    m = re.match(r"(\w+)", text)
    assert m, text[:60]
    return m.group(1), text[m.end():]


def main():
    src = open(sys.argv[1]).read()
    bodies = re.findall(
        r"TEST_F\(RewriteDomainTransformerTest, (\w+)\) \{(.*?)\n\}\n",
        src, re.S)
    assert bodies, "no tests found"
    out = [PREAMBLE]
    count = 0
    for name, body in bodies:
        if name in SKIP_STREAMING_HAND:
            continue
        out.append(translate_test(name, strip_comments(body)))
        out.append("")
        count += 1
    with open(sys.argv[2], "w") as handle:
        handle.write("\n".join(out))
    print(f"translated {count} scanner tests")


main()

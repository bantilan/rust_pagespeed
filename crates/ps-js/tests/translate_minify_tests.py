#!/usr/bin/env python3
"""Mechanically translate js_minify_test.cc TEST_F bodies to Rust tests.

Usage: translate_minify_tests.py <js_minify_test.cc> <out.rs>

Handles the pure-Check* tests (string-concat or known-const arguments).
Tests needing control flow, StrCat, source-map goldens, or the collapse
API (DoNotCreateHtmlEndTag, KeywordPrecedesRegex, DoNotCrash,
CollapsingStringTest, SourceMapsSimple, SourceMapsComplex) are skipped;
they live hand-written in minify_special.rs. Fails loudly on anything
it does not recognize.

With --consts NAME... instead of <out.rs>, prints Rust const items for
the requested file-scope C++ consts (for minify_special.rs).
"""

import re
import sys


def strip_comments(body):
    """Drop // and /* */ comments outside string literals."""
    out = []
    i, n = 0, len(body)
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
        elif c == "/" and i + 1 < n and body[i + 1] == "*":
            i += 2
            while i + 1 < n and not (body[i] == "*" and body[i + 1] == "/"):
                i += 1
            i += 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def split_statements(body):
    """Split on top-level semicolons (strings and parens respected)."""
    stmts, depth, cur, instr, esc = [], 0, [], False, False
    for c in body:
        if instr:
            cur.append(c)
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                instr = False
        elif c == '"':
            instr = True
            cur.append(c)
        elif c == "(":
            depth += 1
            cur.append(c)
        elif c == ")":
            depth -= 1
            cur.append(c)
        elif c == ";" and depth == 0:
            stmts.append("".join(cur).strip())
            cur = []
        else:
            cur.append(c)
    rest = "".join(cur).strip()
    assert not rest, f"trailing text: {rest!r}"
    return [s for s in stmts if s]


def split_args(argstr):
    """Split call arguments on top-level commas (strings respected)."""
    args, depth, cur, instr, esc = [], 0, [], False, False
    for c in argstr:
        if instr:
            cur.append(c)
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                instr = False
        elif c == '"':
            instr = True
            cur.append(c)
        elif c == "(":
            depth += 1
            cur.append(c)
        elif c == ")":
            depth -= 1
            cur.append(c)
        elif c == "," and depth == 0:
            args.append("".join(cur).strip())
            cur = []
        else:
            cur.append(c)
    args.append("".join(cur).strip())
    return args


CXX_ESCAPES = {
    "n": 0x0A, "r": 0x0D, "t": 0x09, "f": 0x0C, "v": 0x0B,
    "a": 0x07, "b": 0x08, "\\": 0x5C, '"': 0x22, "?": 0x3F, "'": 0x27,
}


def cxx_decode(literal):
    """Decode one C++ string literal (with quotes) to bytes."""
    assert literal.startswith('"') and literal.endswith('"'), literal
    inner = literal[1:-1]
    out = bytearray()
    i = 0
    while i < len(inner):
        c = inner[i]
        if c != "\\":
            out.append(ord(c))
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
            # C++ \x is greedy; the suite never relies on >2 digits.
            assert len(digits) == 2, f"greedy \\x hazard: {literal!r}"
            out.append(int(digits, 16))
        elif e in "01234567":
            digits = ""
            while i < len(inner) and len(digits) < 3 and inner[i] in "01234567":
                digits += inner[i]
                i += 1
            out.append(int(digits, 8) & 0xFF)
        else:
            assert e in CXX_ESCAPES, f"bad escape \\{e} in {literal!r}"
            out.append(CXX_ESCAPES[e])
            i += 1
    return bytes(out)


def concat_arg(arg):
    """Decode a (possibly concatenated) C++ string argument to bytes."""
    lits = re.findall(r'"(?:[^"\\]|\\.)*"', arg)
    assert lits and "".join(lits).replace(" ", "").replace("\n", "") == re.sub(
        r"\s+", "", arg
    ), f"not pure string concat: {arg!r}"
    out = bytearray()
    for lit in lits:
        out += cxx_decode(lit)
    return bytes(out)


def rust_bytes(data):
    """Render bytes as a Rust b"..." literal."""
    parts = []
    for byte in data:
        if byte == 0x0A:
            parts.append("\\n")
        elif byte == 0x0D:
            parts.append("\\r")
        elif byte == 0x09:
            parts.append("\\t")
        elif byte == 0x22:
            parts.append('\\"')
        elif byte == 0x5C:
            parts.append("\\\\")
        elif 0x20 <= byte < 0x7F:
            parts.append(chr(byte))
        else:
            parts.append(f"\\x{byte:02x}")
    return 'b"' + "".join(parts) + '"'


def snake(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()



SKIP = {
    "DoNotCreateHtmlEndTag",
    "KeywordPrecedesRegex",
    "DoNotCrash",
    "CollapsingStringTest",
    "SourceMapsSimple",
    "SourceMapsComplex",
    # Byte-array input (NUL-truncated StringPiece); hand-written.
    "SignedCharDoesntSignExtend",
}

CONSTS = {
    "kBeforeCompilation": "BEFORE_COMPILATION",
    "kAfterCompilationOld": "AFTER_COMPILATION_OLD",
    "kAfterCompilationNew": "AFTER_COMPILATION_NEW",
    "kCrashTestString": "CRASH_TEST_STRING",
    "kCollapsingStringTestString": "COLLAPSING_STRING",
    "kCollapsedTestString": "COLLAPSED_STRING",
}

HEADER = '''//! Ported C++ minify suite.
//!
//! Mechanically translated from `js_minify_test.cc` (see
//! `translate_minify_tests.py` in this directory); do not hand-edit
//! expectations. Any divergence from the C++ baseline is a port bug.
//! Flow-heavy tests live in `minify_special.rs`.

use ps_js::minify::{
    legacy_minified_size, legacy_minify_js, minify_utf8,
};
use ps_js::tokenizer::TokenizerPatterns;

fn patterns() -> TokenizerPatterns {
    TokenizerPatterns::new()
}

/// Both minifiers must agree (`CheckMinification`).
fn check_both(input: &[u8], expected: &[u8]) {
    let patterns = patterns();
    let (out, ok) = minify_utf8(&patterns, input);
    assert!(ok, "new minifier failed on {input:?}");
    assert_eq!(out, expected, "new minifier on {input:?}");
    let out = legacy_minify_js(input).expect("old minifier failed");
    assert_eq!(out, expected, "old minifier on {input:?}");
    assert_eq!(legacy_minified_size(input), Some(expected.len()));
}

/// Tokenizer minifier only (`CheckNewMinification`).
fn check_new(input: &[u8], expected: &[u8]) {
    let patterns = patterns();
    let (out, ok) = minify_utf8(&patterns, input);
    assert!(ok, "new minifier failed on {input:?}");
    assert_eq!(out, expected, "new minifier on {input:?}");
}

/// Legacy minifier only (`CheckOldMinification`).
fn check_old(input: &[u8], expected: &[u8]) {
    let out = legacy_minify_js(input).expect("old minifier failed");
    assert_eq!(out, expected, "old minifier on {input:?}");
    assert_eq!(legacy_minified_size(input), Some(expected.len()));
}

/// Both minifiers must fail (`CheckError`).
fn check_error(input: &[u8]) {
    let patterns = patterns();
    assert!(!minify_utf8(&patterns, input).1, "new accepted {input:?}");
    assert!(legacy_minify_js(input).is_none(), "old accepted {input:?}");
    assert_eq!(legacy_minified_size(input), None);
}

/// Legacy minifier must fail (`CheckOldError`).
fn check_old_error(input: &[u8]) {
    assert!(legacy_minify_js(input).is_none(), "old accepted {input:?}");
    assert_eq!(legacy_minified_size(input), None);
}

/// Tokenizer minifier must fail (`CheckNewError`).
fn check_new_error(input: &[u8]) {
    let patterns = patterns();
    assert!(!minify_utf8(&patterns, input).1, "new accepted {input:?}");
}

fn check_files(before: &[u8], expected: &[u8]) {
    let patterns = patterns();
    let (out, ok) = minify_utf8(&patterns, before);
    assert!(ok, "new minifier failed on file");
    assert_eq!(out, expected);
}

'''


def extract_consts(src):
    """Decode file-scope const char[k] defs listed in CONSTS."""
    found = {}
    for cxx, _ in CONSTS.items():
        m = re.search(
            rf"(?:const char\* {cxx}|const char {cxx}\[\])\s*=\s*((?:\"(?:[^\"\\]|\\.)*\"\s*)+);",
            src,
        )
        assert m, f"const {cxx} not found"
        lits = re.findall(r'"(?:[^"\\]|\\.)*"', m.group(1))
        found[cxx] = b"".join(cxx_decode(lit) for lit in lits)
    return found


def rust_consts(consts, used):
    lines = []
    for cxx, rust in CONSTS.items():
        if rust in used:
            lines.append(f"const {rust}: &[u8] = {rust_bytes(consts[cxx])};")
    return "\n".join(lines)


def arg_to_rust(arg, consts):
    arg = arg.strip()
    if arg in CONSTS:
        return CONSTS[arg]
    return rust_bytes(concat_arg(arg))


def translate_test(name, body, consts):
    lines = ["#[test]", f"fn {snake(name)}() {{"]
    for stmt in split_statements(strip_comments(body)):
        m = re.fullmatch(r"(\w+)\((.*)\)", stmt, re.S)
        assert m, f"not a call: {stmt!r}"
        call, argstr = m.group(1), m.group(2)
        args = split_args(argstr)
        if call == "CheckMinification":
            assert len(args) == 2, stmt
            lines.append(
                f"    check_both({arg_to_rust(args[0], consts)},"
                f" {arg_to_rust(args[1], consts)});"
            )
        elif call == "CheckNewMinification":
            assert len(args) == 2, stmt
            lines.append(
                f"    check_new({arg_to_rust(args[0], consts)},"
                f" {arg_to_rust(args[1], consts)});"
            )
        elif call == "CheckOldMinification":
            assert len(args) == 2, stmt
            lines.append(
                f"    check_old({arg_to_rust(args[0], consts)},"
                f" {arg_to_rust(args[1], consts)});"
            )
        elif call == "CheckError":
            assert len(args) == 1, stmt
            lines.append(f"    check_error({arg_to_rust(args[0], consts)});")
        elif call == "CheckOldError":
            assert len(args) == 1, stmt
            lines.append(f"    check_old_error({arg_to_rust(args[0], consts)});")
        elif call == "CheckNewError":
            assert len(args) == 1, stmt
            lines.append(f"    check_new_error({arg_to_rust(args[0], consts)});")
        elif call == "CheckFileMinification":
            assert len(args) == 2, stmt
            before = concat_arg(args[0]).decode("utf-8")
            after = concat_arg(args[1]).decode("utf-8")
            lines.append(
                f"    check_files(include_bytes!(\"../testdata/third_party/{before}\"),"
                f" include_bytes!(\"../testdata/third_party/{after}\"));"
            )
        else:
            raise AssertionError(f"unknown call in {name}: {stmt!r}")
    lines.append("}")
    return "\n".join(lines)


def main():
    src = open(sys.argv[1]).read()
    consts = extract_consts(src)
    if len(sys.argv) > 2 and sys.argv[2] == "--consts":
        for name in sys.argv[3:]:
            rust = CONSTS[name]
            print(f"const {rust}: &[u8] = {rust_bytes(consts[name])};")
        return
    bodies = re.findall(r"TEST_F\(JsMinifyTest, (\w+)\) \{(.*?)\n\}\n", src, re.S)
    assert bodies, "no tests found"
    rendered = []
    count = 0
    for name, body in bodies:
        if name in SKIP:
            continue
        rendered.append(translate_test(name, body, consts))
        count += 1
    used = "\n".join(rendered)
    out = [HEADER, rust_consts(consts, used), ""]
    for text in rendered:
        out.append(text)
        out.append("")
    with open(sys.argv[2], "w") as handle:
        handle.write("\n".join(out))
    print(f"translated {count} tests ({len(SKIP)} skipped as hand-written)")


main()

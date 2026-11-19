#!/usr/bin/env python3
"""Mechanically translate js_tokenizer_test.cc TEST_F bodies to Rust tests.

Usage: translate_tokenizer_tests.py <js_tokenizer_test.cc> <out.rs>

Only depends on the small helper-call vocabulary of the C++ suite
(BeginTokenizing/ExpectToken/ExpectParseStack/ExpectEndOfInput/ExpectError/
ExpectErrorEventually/ExpectTokenizeFileSuccessfully). Fails loudly on any
statement it does not recognize, so drift can never silently slip through.
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


HEADER = '''//! Ported C++ tokenizer suite.
//!
//! Mechanically translated from `js_tokenizer_test.cc` (see
//! `translate_tokenizer_tests.py` in this directory); do not hand-edit
//! expectations. Any divergence from the C++ baseline is a port bug.

use ps_js::keywords::TokenType;
use ps_js::tokenizer::{Tokenizer, TokenizerPatterns};

struct Harness<'a> {
    tokenizer: Tokenizer<'a>,
}

impl<'a> Harness<'a> {
    fn new(patterns: &'a TokenizerPatterns, input: &'a [u8]) -> Harness<'a> {
        Harness {
            tokenizer: Tokenizer::new(patterns, input),
        }
    }

    fn token(&mut self, expected_type: TokenType, expected_token: &[u8]) {
        let (actual_type, span) = self.tokenizer.next_token();
        let actual_token = self.tokenizer.text(span);
        assert_eq!(
            (expected_type, expected_token),
            (actual_type, actual_token)
        );
        assert!(!self.tokenizer.has_error());
    }

    fn stack(&self, expected: &str) {
        assert_eq!(self.tokenizer.parse_stack_for_test(), expected);
    }

    fn end(&mut self) {
        self.token(TokenType::EndOfInput, b"");
        self.stack("");
    }

    fn error(&mut self, expected_token: &[u8]) {
        let (actual_type, span) = self.tokenizer.next_token();
        let actual_token = self.tokenizer.text(span);
        assert_eq!((TokenType::Error, expected_token), (actual_type, actual_token));
        assert!(self.tokenizer.has_error());
    }
}

fn tokenize_to_error(patterns: &TokenizerPatterns, input: &[u8]) -> TokenType {
    let mut tokenizer = Tokenizer::new(patterns, input);
    loop {
        let (kind, _) = tokenizer.next_token();
        if matches!(kind, TokenType::Error | TokenType::EndOfInput) {
            return kind;
        }
    }
}

fn tokenize_file(name: &str, contents: &[u8]) {
    let patterns = TokenizerPatterns::new();
    let mut tokenizer = Tokenizer::new(&patterns, contents);
    let mut output = Vec::with_capacity(contents.len());
    loop {
        let (kind, span) = tokenizer.next_token();
        if kind == TokenType::EndOfInput {
            break;
        }
        assert!(
            !tokenizer.has_error(),
            "error tokenizing {name} at {:?} (stack {})",
            &contents[span.0..span.1.min(span.0 + 150)],
            tokenizer.parse_stack_for_test(),
        );
        output.extend_from_slice(tokenizer.text(span));
    }
    assert_eq!(output, contents);
}

'''


def translate_test(name, body):
    lines = [f"#[test]", f"fn {snake(name)}() {{"]
    needs_patterns = ("BeginTokenizing" in body or "Eventually" in body)
    if needs_patterns:
        lines.append("    let patterns = TokenizerPatterns::new();")
    begun = False
    for stmt in split_statements(strip_comments(body)):
        m = re.fullmatch(r"(\w+)\((.*)\)", stmt, re.S)
        assert m, f"not a call: {stmt!r}"
        call, argstr = m.group(1), m.group(2)
        args = split_args(argstr)
        if call == "BeginTokenizing":
            assert len(args) == 1, stmt
            data = concat_arg(args[0])
            if not begun:
                lines.append(
                    f"    let mut t = Harness::new(&patterns, {rust_bytes(data)});"
                )
                begun = True
            else:
                lines.append(
                    f"    t = Harness::new(&patterns, {rust_bytes(data)});"
                )
        elif call == "ExpectToken":
            assert len(args) in (2, 3), stmt
            enum_ref = args[0]
            assert enum_ref.startswith("JsKeywords::k"), stmt
            variant = enum_ref[len("JsKeywords::k"):]
            data = concat_arg(args[1])
            lines.append(
                f"    t.token(TokenType::{variant}, {rust_bytes(data)});"
            )
            if len(args) == 3:
                stack = concat_arg(args[2]).decode("utf-8")
                lines.append(f'    t.stack("{stack}");')
        elif call == "ExpectParseStack":
            assert len(args) == 1, stmt
            stack = concat_arg(args[0]).decode("utf-8")
            lines.append(f'    t.stack("{stack}");')
        elif call == "ExpectEndOfInput":
            assert not args or args == [""], stmt
            lines.append("    t.end();")
        elif call == "ExpectError":
            assert len(args) == 1, stmt
            lines.append(f"    t.error({rust_bytes(concat_arg(args[0]))});")
        elif call == "ExpectErrorEventually":
            assert len(args) == 1, stmt
            data = rust_bytes(concat_arg(args[0]))
            lines.append(
                f"    assert_eq!(tokenize_to_error(&patterns, {data}), TokenType::Error);"
            )
        elif call == "ExpectNoErrorEventually":
            assert len(args) == 1, stmt
            data = rust_bytes(concat_arg(args[0]))
            lines.append(
                f"    assert_eq!(tokenize_to_error(&patterns, {data}), TokenType::EndOfInput);"
            )
        elif call == "ExpectTokenizeFileSuccessfully":
            assert len(args) == 1, stmt
            filename = concat_arg(args[0]).decode("utf-8")
            lines.append(
                f'    tokenize_file("{filename}", '
                f'include_bytes!("../testdata/third_party/{filename}"));'
            )
        else:
            raise AssertionError(f"unknown call: {stmt!r}")
    lines.append("}")
    return "\n".join(lines)


def main():
    src = open(sys.argv[1]).read()
    bodies = re.findall(r"TEST_F\(JsTokenizerTest, (\w+)\) \{(.*?)\n\}\n", src, re.S)
    assert bodies, "no tests found"
    out = [HEADER]
    for name, body in bodies:
        out.append(translate_test(name, body))
        out.append("")
    with open(sys.argv[2], "w") as handle:
        handle.write("\n".join(out))
    print(f"translated {len(bodies)} tests")


main()

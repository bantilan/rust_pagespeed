#!/usr/bin/env python3
"""Translates plain-literal ValidateExpected cases from html_parse_test.cc
into a Rust golden test file.

Usage: python3 tool/translate_parse_tests.py  (run from crates/ps-html)
"""
import re

# Cases needing per-test setup the plain harness cannot provide. Each has a
# dedicated hand-written test instead:
# - case_fold: requires set_case_fold(true) on the writer.
# - comment_inside_first_div: requires the comment-inserting filter.
# - 1: the eight Insert*Script tests, requiring the script-inserting filter.
# - delete_saving_children_*: require the deleting filter with counters.
SKIP_IDS = {
    "case_fold",
    "comment_inside_first_div",
    "1",
    "delete_saving_children_open",
    "delete_saving_children_close",
    "delete_saving_children_inner",
    "delete_saving_children_outer",
    "delete_saving_children_inner_middle",
    "delete_saving_children_inner_end",
    "delete_saving_children_inner_deep",
    "delete_saving_children_outer_distant",
}


def split_args(text):
    """Splits top-level comma-separated args honoring strings/parens."""
    args, depth, current = [], 0, ""
    instr, esc = False, False
    for c in text:
        if instr:
            current += c
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                instr = False
        else:
            if c == '"':
                instr = True
                current += c
            elif c == '(':
                depth += 1
                current += c
            elif c == ')':
                depth -= 1
                current += c
            elif c == ',' and depth == 0:
                args.append(current.strip())
                current = ""
            else:
                current += c
    args.append(current.strip())
    return args


def cxx_unescape(text):
    """Decodes a C++ string literal body to bytes."""
    out = bytearray()
    i = 0
    while i < len(text):
        c = text[i]
        if c != "\\":
            out.extend(c.encode("utf-8"))
            i += 1
            continue
        n = text[i + 1]
        simple = {"n": 10, "t": 9, "r": 13, "f": 12, "\\": 92, '"': 34, "'": 39}
        if n in simple:
            out.append(simple[n])
            i += 2
        elif n == "x":
            out.append(int(text[i + 2 : i + 4], 16))
            i += 4
        elif n in "01234567":
            digits = text[i + 1 : i + 4]
            value = ""
            for digit in digits:
                if digit not in "01234567":
                    break
                value += digit
            out.append(int(value, 8))
            i += 1 + len(value)
        else:
            raise ValueError(f"unknown escape \\{n} in {text[:60]!r}")
    return bytes(out)


def cxx_arg_to_bytes(arg):
    """Decodes one argument made of adjacent C++ string literals.

    Multi-line cases split the text across literals; only whitespace may
    appear between the closing and opening quotes, and it is not content.
    """
    out = bytearray()
    i, n = 0, len(arg)
    seen = 0
    while i < n:
        c = arg[i]
        if c == '"':
            j = i + 1
            body = []
            while j < n:
                d = arg[j]
                if d == "\\":
                    body.append(arg[j : j + 2])
                    j += 2
                    continue
                if d == '"':
                    break
                body.append(d)
                j += 1
            if j >= n:
                raise ValueError(f"unterminated literal in {arg[:60]!r}")
            out.extend(cxx_unescape("".join(body)))
            seen += 1
            i = j + 1
        elif c in " \t\r\n":
            i += 1
        else:
            raise ValueError(f"non-literal text in {arg[:60]!r}")
    if seen == 0:
        raise ValueError(f"no literals in {arg[:60]!r}")
    return bytes(out)


def rust_bytes(data: bytes) -> str:
    """Renders bytes as a Rust byte-string with escapes as needed."""
    parts = []
    for b in data:
        if b == 34:
            parts.append('\\"')
        elif b == 92:
            parts.append("\\\\")
        elif 32 <= b < 127:
            parts.append(chr(b))
        elif b == 10:
            parts.append("\\n")
        elif b == 9:
            parts.append("\\t")
        elif b == 13:
            parts.append("\\r")
        else:
            parts.append(f"\\x{b:02x}")
    return 'b"' + "".join(parts) + '"'


def main():
    src = open(
        "/home/gamingph/Projects/Rust/mod_pagespeed/pagespeed/kernel/html/html_parse_test.cc"
    ).read()
    calls = []
    for m in re.finditer(r"ValidateExpected\(", src):
        depth, i = 1, m.end()
        start = i
        instr, esc = False, False
        while depth > 0 and i < len(src):
            c = src[i]
            if instr:
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == '"':
                    instr = False
            else:
                if c == '"':
                    instr = True
                elif c == "(":
                    depth += 1
                elif c == ")":
                    depth -= 1
            i += 1
        calls.append(src[start : i - 1])

    cases = []
    skipped = 0
    for call in calls:
        args = split_args(call)
        if len(args) != 3 or any(
            not (a.startswith('"') and a.endswith('"')) for a in args
        ):
            skipped += 1
            continue
        try:
            ident = cxx_arg_to_bytes(args[0]).decode("ascii")
            html_in = cxx_arg_to_bytes(args[1])
            expected = cxx_arg_to_bytes(args[2])
        except ValueError:
            skipped += 1
            continue
        if ident in SKIP_IDS:
            skipped += 1
            continue
        cases.append((ident, html_in, expected))

    lines = [
        "//! GENERATED by tool/translate_parse_tests.py — do not edit.",
        "//! Round-trip goldens ported from html_parse_test.cc ValidateExpected cases.",
        "",
        "use crate::harness::validate_expected;",
        "",
        "#[test]",
        "fn translated_parse_goldens() {",
        "    let cases: &[(&str, &[u8], &[u8])] = &[",
    ]
    for ident, html_in, expected in cases:
        lines.append(
            f'        ("{ident}", {rust_bytes(html_in)}, {rust_bytes(expected)}),' 
        )
    lines += [
        "    ];",
        "    for (id, input, expected) in cases {",
        "        validate_expected(id, input, expected);",
        "    }",
        "}",
        "",
    ]
    open("tests/goldens/generated_parse.rs", "w").write("\n".join(lines))
    print(f"translated {len(cases)}, skipped {skipped}")


if __name__ == "__main__":
    main()

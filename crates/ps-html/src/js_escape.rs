//! JS string-literal escaping (`EscapeToJsStringLiteral`).
//!
//! Splices untrusted text into single-quoted JS literals: backslashes,
//! quotes, and line breaks escape, `'` escapes only in the unquoted form,
//! and HTML-significant sequences (`<script`, `<!--`, `-->`, `/script`)
//! break so inline scripts cannot terminate early.

/// Escapes for a JS string literal (`EscapeToJsStringLiteral`); with
/// `add_quotes` the result is wrapped in double quotes and `'` stays raw.
#[must_use]
pub fn escape_to_js_string_literal(input: &[u8], add_quotes: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() + 2);
    if add_quotes {
        out.push(b'"');
    }
    let mut index = 0;
    while index < input.len() {
        let rest = &input[index..];
        match input[index] {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\'' => {
                if add_quotes {
                    out.push(b'\'');
                } else {
                    out.extend_from_slice(b"\\'");
                }
            }
            b'<' => {
                if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case(b"<script")
                    || rest.starts_with(b"<!--")
                {
                    out.extend_from_slice(b"\\u003c");
                } else {
                    out.push(b'<');
                }
            }
            b'-' => {
                if rest.starts_with(b"-->") {
                    out.extend_from_slice(b"\\u002d");
                } else {
                    out.push(b'-');
                }
            }
            b'/' => {
                if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case(b"/script") {
                    out.push(b'\\');
                }
                out.push(b'/');
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    if add_quotes {
        out.push(b'"');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_both(name: &str, expect: &[u8], input: &[u8]) {
        assert_eq!(
            escape_to_js_string_literal(input, false),
            expect,
            "unquoted {name}"
        );
        let mut quoted = vec![b'"'];
        quoted.extend_from_slice(expect);
        quoted.push(b'"');
        // The quoted form leaves `'` raw, so quote-sensitive inputs
        // assert separately.
        if !input.contains(&b'\'') {
            assert_eq!(
                escape_to_js_string_literal(input, true),
                quoted,
                "quoted {name}"
            );
        }
    }

    #[test]
    fn js_escape_basic() {
        expect_both("normal", b"abc", b"abc");
        expect_both("quote", b"abc\\\"d", b"abc\"d");
        expect_both("backslash", b"abc\\\\d", b"abc\\d");
        expect_both("carriage_control", b"abc\\n\\rde", b"abc\n\rde");
    }

    #[test]
    fn js_avoid_close_script() {
        expect_both(
            "avoid_close_script",
            b"Foo<\\/script>Bar",
            b"Foo</script>Bar",
        );
        expect_both("not_heavily_excessive_escaping", b"/s", b"/s");
        expect_both(
            "avoid_close_script_space",
            b"Foo<\\/script  >Bar",
            b"Foo</script  >Bar",
        );
        expect_both(
            "avoid_close_script_case",
            b"Foo<\\/scrIpt>Bar",
            b"Foo</scrIpt>Bar",
        );
        // `</scripty>` needs no escape, but escaping is safe.
        expect_both(
            "close_script_conservative",
            b"Foo<\\/scripty>Bar",
            b"Foo</scripty>Bar",
        );
    }

    #[test]
    fn js_single_quotes() {
        assert_eq!(escape_to_js_string_literal(b"foo'", false), b"foo\\'");
        assert_eq!(escape_to_js_string_literal(b"foo'", true), b"\"foo'\"");
    }

    #[test]
    fn js_avoid_weird_parsing_sequence() {
        assert_eq!(
            escape_to_js_string_literal(b"a <ScrIpt", false),
            b"a \\u003cScrIpt"
        );
        assert_eq!(
            escape_to_js_string_literal(b"Foo <!-- ", false),
            b"Foo \\u003c!-- "
        );
        assert_eq!(
            escape_to_js_string_literal(b"Bar ---> ", false),
            b"Bar -\\u002d-> "
        );
    }

    #[test]
    fn js_dont_escape_way_too_much() {
        assert_eq!(escape_to_js_string_literal(b"<div", false), b"<div");
        assert_eq!(escape_to_js_string_literal(b"-----!", false), b"-----!");
    }
}

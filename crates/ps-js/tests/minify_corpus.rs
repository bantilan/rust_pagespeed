//! Ported C++ minify suite.
//!
//! Mechanically translated from `js_minify_test.cc` (see
//! `translate_minify_tests.py` in this directory); do not hand-edit
//! expectations. Any divergence from the C++ baseline is a port bug.
//! Flow-heavy tests live in `minify_special.rs`.

use ps_js::minify::{legacy_minified_size, legacy_minify_js, minify_utf8};
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

const BEFORE_COMPILATION: &[u8] = b"// is.js\n\n// (c) 2001 Douglas Crockford\n// 2001 June 3\n\n\n// is\n\n// The -is- object is used to identify the browser.  Every browser edition\n// identifies itself, but there is no standard way of doing it, and some of\n// the identification is deceptive. This is because the authors of web\n// browsers are liars. For example, Microsoft's IE browsers claim to be\n// Mozilla 4. Netscape 6 claims to be version 5.\n\nvar is = {\n    ie:      navigator.appName == 'Microsoft Internet Explorer',\n    java:    navigator.javaEnabled(),\n    ns:      navigator.appName == 'Netscape',\n    ua:      navigator.userAgent.toLowerCase(),\n    version: parseFloat(navigator.appVersion.substr(21)) ||\n             parseFloat(navigator.appVersion),\n    win:     navigator.platform == 'Win32'\n}\nis.mac = is.ua.indexOf('mac') >= 0;\nif (is.ua.indexOf('opera') >= 0) {\n    is.ie = is.ns = false;\n    is.opera = true;\n}\nif (is.ua.indexOf('gecko') >= 0) {\n    is.ie = is.ns = false;\n    is.gecko = true;\n}\n";
const AFTER_COMPILATION_OLD: &[u8] = b"var is={ie:navigator.appName=='Microsoft Internet Explorer',java:navigator.javaEnabled(),ns:navigator.appName=='Netscape',ua:navigator.userAgent.toLowerCase(),version:parseFloat(navigator.appVersion.substr(21))||parseFloat(navigator.appVersion),win:navigator.platform=='Win32'}\nis.mac=is.ua.indexOf('mac')>=0;if(is.ua.indexOf('opera')>=0){is.ie=is.ns=false;is.opera=true;}\nif(is.ua.indexOf('gecko')>=0){is.ie=is.ns=false;is.gecko=true;}";
const AFTER_COMPILATION_NEW: &[u8] = b"var is={ie:navigator.appName=='Microsoft Internet Explorer',java:navigator.javaEnabled(),ns:navigator.appName=='Netscape',ua:navigator.userAgent.toLowerCase(),version:parseFloat(navigator.appVersion.substr(21))||parseFloat(navigator.appVersion),win:navigator.platform=='Win32'}\nis.mac=is.ua.indexOf('mac')>=0;if(is.ua.indexOf('opera')>=0){is.ie=is.ns=false;is.opera=true;}if(is.ua.indexOf('gecko')>=0){is.ie=is.ns=false;is.gecko=true;}";

#[test]
fn basic() {
    check_old(BEFORE_COMPILATION, AFTER_COMPILATION_OLD);
    check_new(BEFORE_COMPILATION, AFTER_COMPILATION_NEW);
}

#[test]
fn already_minified() {
    check_both(AFTER_COMPILATION_NEW, AFTER_COMPILATION_NEW);
}

#[test]
fn json() {
    check_both(
        b"{  \"foo\": {\"bar\": 1}, \"baz\": 2 } ",
        b"{\"foo\":{\"bar\":1},\"baz\":2}",
    );
}

#[test]
fn error_unclosed_comment() {
    check_error(b"/* not valid javascript");
}

#[test]
fn error_unclosed_string() {
    check_error(b"\"not valid javascript");
}

#[test]
fn error_unclosed_regex() {
    check_error(b"/not_valid_javascript");
}

#[test]
fn error_regex_newline() {
    check_error(b"/not_valid\njavascript/;");
}

#[test]
fn deal_with_crlf() {
    check_both(b"var x = 1;\r\nvar y = 2;", b"var x=1;var y=2;");
}

#[test]
fn deal_with_tabs() {
    check_both(b"var x = 1;\n\tvar y = 2;", b"var x=1;var y=2;");
}

#[test]
fn escaped_crlf_in_string_literal() {
    check_both(b"var x = 'foo\\\r\nbar';", b"var x='foo\\\r\nbar';");
}

#[test]
fn empty_input() {
    check_both(b"", b"");
}

#[test]
fn treat_carriage_return_as_linebreak() {
    check_both(b"x = 1\ry = 2", b"x=1\ny=2");
}

#[test]
fn carriage_return_ends_line_comment() {
    check_both(b"x = 1 // foobar\ry = 2", b"x=1\ny=2");
}

#[test]
fn leave_i_e_conditional_compilation_comments() {
    check_old(b"/*@cc_on\n  /*@if (@_win32)\n    document.write('IE');\n  @else @*/\n    document.write('other');\n  /*@end\n@*/", b"/*@cc_on\n  /*@if (@_win32)\n    document.write('IE');\n  @else @*/\ndocument.write('other');/*@end\n@*/");
    check_new(b"/*@cc_on\n  /*@if (@_win32)\n    document.write('IE');\n  @else @*/\n    document.write('other');\n  /*@end\n@*/", b"/*@cc_on\n  /*@if (@_win32)\n    document.write('IE');\n  @else @*/document.write('other');/*@end\n@*/");
}

#[test]
fn do_not_join_plusses() {
    check_both(
        b"var x = 'date=' + +new Date();",
        b"var x='date='+ +new Date();",
    );
}

#[test]
fn do_not_join_plus_and_plus_plus() {
    check_both(b"var x = y + ++z;", b"var x=y+ ++z;");
}

#[test]
fn do_not_join_plus_plus_and_plus() {
    check_both(b"var x = y++ + z;", b"var x=y++ +z;");
}

#[test]
fn do_not_join_minuses() {
    check_both(
        b"var x = 'date=' - -new Date();",
        b"var x='date='- -new Date();",
    );
}

#[test]
fn do_not_join_minus_and_minus_minus() {
    check_both(b"var x = y - --z;", b"var x=y- --z;");
}

#[test]
fn do_not_join_minus_minus_and_minus() {
    check_both(b"var x = y-- - z;", b"var x=y-- -z;");
}

#[test]
fn do_join_bangs() {
    check_both(b"var x = ! ! y;", b"var x=!!y;");
}

#[test]
fn remove_surrounding_sgml_comment() {
    check_both(b"<!--\nvar x = 42;\n//-->", b"var x=42;");
}

#[test]
fn remove_surrounding_sgml_comment_without_slash_slash() {
    check_both(b"<!--\nvar x = 42;\n-->\n", b"var x=42;");
}

#[test]
fn sgml_line_comment() {
    check_both(
        b"var x = 42; <!-- comment\nvar y = 17;",
        b"var x=42;var y=17;",
    );
}

#[test]
fn remove_sgml_comment_close_on_own_line1() {
    check_both(b"var x = 42;\n    --> \n", b"var x=42;");
}

#[test]
fn remove_sgml_comment_close_on_own_line2() {
    check_both(b"-->\nvar x = 42;\n", b"var x=42;");
}

#[test]
fn do_not_remove_sgml_comment_close_in_mid_line() {
    check_both(b"var x = 42; --> \n", b"var x=42;-->");
}

#[test]
fn do_not_create_line_comment() {
    check_both(b"var x = 42 / /foo/;\n", b"var x=42/ /foo/;");
}

#[test]
fn do_not_create_sgml_line_comment1() {
    check_both(b"if (x <! --y) { x = 0; }\n", b"if(x<! --y){x=0;}");
}

#[test]
fn do_not_create_sgml_line_comment2() {
    check_both(b"if (x < !--y) { x = 0; }\n", b"if(x< !--y){x=0;}");
}

#[test]
fn do_not_join_decimal_integer_and_dot() {
    check_new(b"0192  . toString()", b"0192 .toString()");
}

#[test]
fn do_join_hex_octal_integer_and_dot() {
    check_both(
        b"0x3e2  . toString() + 0172  . toString()",
        b"0x3e2.toString()+0172.toString()",
    );
}

#[test]
fn do_join_decimal_fraction_and_dot() {
    check_both(
        b"3.5 . toString() + 3e2 . toString()",
        b"3.5.toString()+3e2.toString()",
    );
}

#[test]
fn tricky_regex_literal() {
    check_both(
        b"var x = a[0] / b /i;\n var y = a[0] + / b /i;",
        b"var x=a[0]/b/i;var y=a[0]+/ b /i;",
    );
}

#[test]
fn object_literal_regex_literal() {
    check_both(b"{foo: 123} / x /i;", b"{foo:123}/ x /i;");
    check_new(b"x={foo: 1} / x /i;", b"x={foo:1}/x/i;");
}

#[test]
fn regex_literal_with_brackets1() {
    check_both(
        b"var x = /http:\\/\\/[^/]+\\//, y = 3;",
        b"var x=/http:\\/\\/[^/]+\\//,y=3;",
    );
}

#[test]
fn regex_literal_with_brackets2() {
    check_both(b"var x = /z[\\]/ ]/, y = 3;", b"var x=/z[\\]/ ]/,y=3;");
}

#[test]
fn return_regex1() {
    check_both(b"return / x /g;", b"return/ x /g;");
}

#[test]
fn return_regex2() {
    check_both(b"return/#.+/.test(\n'#24' );", b"return/#.+/.test('#24');");
}

#[test]
fn throw_regex() {
    check_both(b"throw / x /g;", b"throw/ x /g;");
}

#[test]
fn return_throw_number() {
    check_both(b"return 1;\nthrow 2;", b"return 1;throw 2;");
}

#[test]
fn loop_regex() {
    check_new(b"while (0) /\\//.exec('');", b"while(0)/\\//.exec('');");
    check_new(b"for (x in y) / z /.exec(x);", b"for(x in y)/ z /.exec(x);");
}

#[test]
fn label_regex() {
    check_both(b"{ foo: / x /.exec(''); }", b"{foo:/ x /.exec('');}");
}

#[test]
fn semicolon_insertion_increment() {
    check_both(b"a\n++b\nc++\nd", b"a\n++b\nc++\nd");
    check_new(b"a\n++\nb\nc++\nd", b"a\n++b\nc++\nd");
}

#[test]
fn semicolon_insertion_decrement() {
    check_both(b"a\n--b\nc--\nd", b"a\n--b\nc--\nd");
    check_new(b"a\n--\nb\nc--\nd", b"a\n--b\nc--\nd");
}

#[test]
fn semicolon_insertion_addition() {
    check_both(b"i\n+\nj", b"i+j");
}

#[test]
fn semicolon_insertion_subtraction() {
    check_both(b"i\n-\nj", b"i-j");
}

#[test]
fn semicolon_insertion_logical_or() {
    check_both(b"i\n||\nj", b"i||j");
}

#[test]
fn semicolon_insertion_func_call() {
    check_both(b"a = b + c\n(d + e).print()", b"a=b+c(d+e).print()");
}

#[test]
fn semicolon_insertion_regex() {
    check_both(b"i=0\n/ [a-z] /g.exec(s)", b"i=0/[a-z]/g.exec(s)");
}

#[test]
fn semicolon_insertion_comment() {
    check_both(b"a=b\n /*hello*/ c=d\n", b"a=b\nc=d");
}

#[test]
fn semicolon_insertion_while_stmt() {
    check_both(b"while\n(true);", b"while(true);");
}

#[test]
fn semicolon_insertion_return_stmt1() {
    check_both(b"return\n(true);", b"return\n(true);");
}

#[test]
fn semicolon_insertion_return_stmt2() {
    check_both(b"return\n/*comment*/(true);", b"return\n(true);");
}

#[test]
fn semicolon_insertion_throw_stmt() {
    check_both(b"throw\n  'error';", b"throw\n'error';");
}

#[test]
fn semicolon_insertion_break_stmt() {
    check_both(b"break\nlabel;", b"break\nlabel;");
}

#[test]
fn semicolon_insertion_continue_stmt() {
    check_both(b"continue\nlabel;", b"continue\nlabel;");
}

#[test]
fn semicolon_insertion_debugger_stmt() {
    check_both(b"debugger\nfoo;", b"debugger\nfoo;");
}

#[test]
fn latin1_input() {
    check_both(
        b"str='Qu\xe9 pasa';// 'qu\xe9' means 'what'\ncents=/* 73\xa2 is $0.73 */73;",
        b"str='Qu\xe9 pasa';cents=73;",
    );
}

#[test]
fn minify_angular() {
    check_files(
        include_bytes!("../testdata/third_party/angular.original"),
        include_bytes!("../testdata/third_party/angular.minified"),
    );
}

#[test]
fn minify_j_query() {
    check_files(
        include_bytes!("../testdata/third_party/jquery.original"),
        include_bytes!("../testdata/third_party/jquery.minified"),
    );
}

#[test]
fn minify_prototype() {
    check_files(
        include_bytes!("../testdata/third_party/prototype.original"),
        include_bytes!("../testdata/third_party/prototype.minified"),
    );
}

#[test]
fn template_literal_fails_closed() {
    check_old_error(b"var t = `hello ${name},\nwelcome!`;");
    check_new_error(b"var t = `hello ${name},\nwelcome!`;");
}

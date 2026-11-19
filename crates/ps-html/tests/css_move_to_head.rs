//! Stylesheet hoisting tests ported from `css_move_to_head_filter_test.cc`.
#[path = "harness.rs"]
mod harness;

use harness::{run_with_filters, SharedSink};
use ps_html::filters::MoveCssToHeadFilter;
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

fn run_filtered(input: &[u8], to_head: bool, above_scripts: bool) -> Vec<u8> {
    run_with_filters(input, |parser| {
        parser.add_filter(Box::new(MoveCssToHeadFilter::new(to_head, above_scripts)));
    })
}

#[test]
fn moves_css_to_head() {
    let input = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "</head>\n",
        "<body>\n",
        "  Hello,\n",
        "  <link rel='stylesheet' href='a.css' type='text/css'>",
        "<link rel='stylesheet' href='b.css' type='text/css'>\n",
        "  <style type='text/css'>a {color: red }</style>\n",
        "  World!\n",
        "  <link rel='stylesheet' href='c.css' type='text/css'>\n",
        "</body>\n",
    );
    let expected = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "<link rel='stylesheet' href='a.css' type='text/css'>",
        "<link rel='stylesheet' href='b.css' type='text/css'>",
        "<style type='text/css'>a {color: red }</style>",
        "<link rel='stylesheet' href='c.css' type='text/css'>",
        "</head>\n",
        "<body>\n",
        "  Hello,\n",
        "  \n",
        "  \n",
        "  World!\n",
        "  \n",
        "</body>\n",
    );
    assert_eq!(
        run_filtered(input.as_bytes(), true, false),
        expected.as_bytes()
    );
}

#[test]
fn doesnt_move_out_of_noscript() {
    let input = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "</head>\n",
        "<body>\n",
        "  <style>a {color: red}</style>\n",
        "  <noscript>\n",
        "    <link rel='stylesheet' href='a.css'>\n",
        "  </noscript>\n",
        "  <style>p {color: blue}</style>\n",
        "</body>\n",
    );
    let expected = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "<style>a {color: red}</style>",
        "</head>\n",
        "<body>\n",
        "  \n",
        "  <noscript>\n",
        "    <link rel='stylesheet' href='a.css'>\n",
        "  </noscript>\n",
        "  <style>p {color: blue}</style>\n",
        "</body>\n",
    );
    assert_eq!(
        run_filtered(input.as_bytes(), true, false),
        expected.as_bytes()
    );
}

#[test]
fn doesnt_move_scoped_style() {
    let input = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "</head>\n",
        "<body>\n",
        "  <style>div {color: green}</style>\n",
        "  <style scoped>a {color: red}</style>\n",
        "  <style scoped='scoped'>p {color: blue}</style>\n",
        "  <link rel='stylesheet' href='a.css'>\n",
        "  <p>Blue with a <a href='scoped_style.html'>red link</a></body>\n",
    );
    let expected = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "<style>div {color: green}</style>",
        "</head>\n",
        "<body>\n",
        "  \n",
        "  <style scoped>a {color: red}</style>\n",
        "  <style scoped='scoped'>p {color: blue}</style>\n",
        "  <link rel='stylesheet' href='a.css'>\n",
        "  <p>Blue with a <a href='scoped_style.html'>red link</a></body>\n",
    );
    assert_eq!(
        run_filtered(input.as_bytes(), true, false),
        expected.as_bytes()
    );
}

#[test]
fn move_past_scoped_div() {
    // `scoped` on a div is not a barrier (only `style scoped` is).
    let input = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "</head>\n",
        "<body>\n",
        "  <style>div {color: green}</style>\n",
        "  <div scoped>Just a div, move along!</div>\n",
        "  <style>p {color: blue}</style>\n",
        "  <link rel='stylesheet' href='a.css'>\n",
        "</body>\n",
    );
    let expected = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "<style>div {color: green}</style>",
        "<style>p {color: blue}</style>",
        "<link rel='stylesheet' href='a.css'>",
        "</head>\n",
        "<body>\n",
        "  \n",
        "  <div scoped>Just a div, move along!</div>\n",
        "  \n",
        "  \n",
        "</body>\n",
    );
    assert_eq!(
        run_filtered(input.as_bytes(), true, false),
        expected.as_bytes()
    );
}

#[test]
fn doesnt_reorder_css() {
    let html = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "</head>\n",
        "<body>\n",
        "  <link rel='stylesheet' href='a.css' type='text/css'>\n",
        "  <link rel='stylesheet' href='b.css' type='text/css'>\n",
        "  <style type='text/css'>a { color: red }</style>\n",
        "  <link rel='stylesheet' href='d.css' type='text/css'>\n",
        "</body>\n",
    );
    let output = run_filtered(html.as_bytes(), true, false);
    let find = |needle: &[u8]| {
        output
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap()
    };
    // All present, still in order (last link not above the style).
    let (a, b, c, d) = (
        find(b"href='a.css'"),
        find(b"href='b.css'"),
        find(b"a { color: red }"),
        find(b"href='d.css'"),
    );
    assert!(a <= b && b <= c && c <= d);
}

#[test]
fn moves_above_first_script() {
    let input = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "  <link rel='stylesheet' type='text/css' href='a.css'>\n",
        "  <meta name='application-name' content='Foo'>\n",
        "  <script src='b.js'></script>\n",
        "  <!-- Comment -->\n",
        "  <style>.foo { color: red }</style>\n",
        "  <script src='c.js'></script>\n",
        "  <link rel='icon' href='d.png'>\n",
        "  <link rel='stylesheet' href='e.css'>\n",
        "</head>\n",
        "<body>\n",
        "  <link rel='stylesheet' type='text/css' href='f.css'>\n",
        "</body>\n",
    );
    let expected = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "  <link rel='stylesheet' type='text/css' href='a.css'>\n",
        "  <meta name='application-name' content='Foo'>\n",
        "  ",
        "<style>.foo { color: red }</style>",
        "<link rel='stylesheet' href='e.css'>",
        "<link rel='stylesheet' type='text/css' href='f.css'>",
        "<script src='b.js'></script>\n",
        "  <!-- Comment -->\n",
        "  \n",
        "  <script src='c.js'></script>\n",
        "  <link rel='icon' href='d.png'>\n",
        "  \n",
        "</head>\n",
        "<body>\n",
        "  \n",
        "</body>\n",
    );
    assert_eq!(
        run_filtered(input.as_bytes(), false, true),
        expected.as_bytes()
    );
}

#[test]
fn moves_above_script_after_head() {
    let input = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "  <link rel='stylesheet' type='text/css' href='a.css'>\n",
        "  <meta name='application-name' content='Foo'>\n",
        "  <!-- Comment -->\n",
        "  <style>.foo { color: red }</style>\n",
        "  <link rel='icon' href='d.png'>\n",
        "  <link rel='stylesheet' href='e.css'>\n",
        "</head>\n",
        "<body>\n",
        "  <script src='b.js'></script>\n",
        "  <link rel='stylesheet' type='text/css' href='f.css'>\n",
        "</body>\n",
    );
    let expected = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "  <link rel='stylesheet' type='text/css' href='a.css'>\n",
        "  <meta name='application-name' content='Foo'>\n",
        "  <!-- Comment -->\n",
        "  <style>.foo { color: red }</style>\n",
        "  <link rel='icon' href='d.png'>\n",
        "  <link rel='stylesheet' href='e.css'>\n",
        "</head>\n",
        "<body>\n",
        "  <link rel='stylesheet' type='text/css' href='f.css'>",
        "<script src='b.js'></script>\n",
        "  \n",
        "</body>\n",
    );
    assert_eq!(
        run_filtered(input.as_bytes(), false, true),
        expected.as_bytes()
    );
}

#[test]
fn moves_to_head_even_if_script_after() {
    // Both flags on: head wins as the anchor.
    let input = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "  <link rel='stylesheet' type='text/css' href='a.css'>\n",
        "  <meta name='application-name' content='Foo'>\n",
        "  <!-- Comment -->\n",
        "  <style>.foo { color: red }</style>\n",
        "  <link rel='icon' href='d.png'>\n",
        "  <link rel='stylesheet' href='e.css'>\n",
        "</head>\n",
        "<body>\n",
        "  <script src='b.js'></script>\n",
        "  <link rel='stylesheet' type='text/css' href='f.css'>\n",
        "</body>\n",
    );
    let expected = concat!(
        "<head>\n",
        "  <title>Example</title>\n",
        "  <link rel='stylesheet' type='text/css' href='a.css'>\n",
        "  <meta name='application-name' content='Foo'>\n",
        "  <!-- Comment -->\n",
        "  <style>.foo { color: red }</style>\n",
        "  <link rel='icon' href='d.png'>\n",
        "  <link rel='stylesheet' href='e.css'>\n",
        "<link rel='stylesheet' type='text/css' href='f.css'>",
        "</head>\n",
        "<body>\n",
        "  <script src='b.js'></script>\n",
        "  \n",
        "</body>\n",
    );
    assert_eq!(
        run_filtered(input.as_bytes(), true, true),
        expected.as_bytes()
    );
}

fn run_flushed(chunks: &[&[u8]], to_head: bool, above_scripts: bool) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(MoveCssToHeadFilter::new(to_head, above_scripts)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/move.html", false);
    for (index, chunk) in chunks.iter().enumerate() {
        parser.write(chunk);
        if index + 1 < chunks.len() {
            parser.flush();
        }
    }
    parser.finish();
    sink.bytes()
}

#[test]
fn move_to_head_flush_edge() {
    // `</head>` opens the second window: the link still moves up to it.
    let output = run_flushed(
        &[
            b"<html>\n  <head>\n    <title>Example</title>",
            b"</head>\n  <body>\n    <link rel='stylesheet' type='text/css' href='f.css'>",
            b"\n  </body>\n</html>\n",
        ],
        true,
        false,
    );
    assert_eq!(
        output,
        concat!(
            "<html>\n",
            "  <head>\n",
            "    <title>Example</title>",
            "<link rel='stylesheet' type='text/css' href='f.css'>",
            "</head>\n",
            "  <body>\n",
            "    \n",
            "  </body>\n",
            "</html>\n",
        )
        .as_bytes()
    );
}

#[test]
fn move_to_head_over_flush_edge() {
    // `</head>` left the window: nothing moves.
    let output = run_flushed(
        &[
            b"<html>\n  <head>\n    <title>Example</title></head>",
            b"\n  <body>\n    <link rel='stylesheet' type='text/css' href='f.css'>",
            b"\n  </body>\n</html>\n",
        ],
        true,
        false,
    );
    assert_eq!(
        output,
        concat!(
            "<html>\n",
            "  <head>\n",
            "    <title>Example</title>",
            "</head>\n",
            "  <body>\n",
            "    <link rel='stylesheet' type='text/css' href='f.css'>\n",
            "  </body>\n",
            "</html>\n",
        )
        .as_bytes()
    );
}

#[test]
fn move_above_scripts_flush_edge() {
    // `<script>` opens the second window: the link still moves above it.
    let output = run_flushed(
        &[
            b"<html>\n  <head>\n    <title>Example</title>",
            b"<script src='b.js'></script>\n  </head>\n  <body>\n    <link rel='stylesheet' type='text/css' href='f.css'>",
            b"\n  </body>\n</html>\n",
        ],
        false,
        true,
    );
    assert_eq!(
        output,
        concat!(
            "<html>\n",
            "  <head>\n",
            "    <title>Example</title>",
            "<link rel='stylesheet' type='text/css' href='f.css'>",
            "<script src='b.js'></script>\n",
            "  </head>\n",
            "  <body>\n",
            "    \n",
            "  </body>\n",
            "</html>\n",
        )
        .as_bytes()
    );
}

#[test]
fn move_above_scripts_over_flush_edge() {
    // `<script>` left the window: nothing moves.
    let output = run_flushed(
        &[
            b"<html>\n  <head>\n    <title>Example</title><script src='b.js'></script>",
            b"\n  </head>\n  <body>\n    <link rel='stylesheet' type='text/css' href='f.css'>",
            b"\n  </body>\n</html>\n",
        ],
        false,
        true,
    );
    assert_eq!(
        output,
        concat!(
            "<html>\n",
            "  <head>\n",
            "    <title>Example</title>",
            "<script src='b.js'></script>\n",
            "  </head>\n",
            "  <body>\n",
            "    <link rel='stylesheet' type='text/css' href='f.css'>\n",
            "  </body>\n",
            "</html>\n",
        )
        .as_bytes()
    );
}

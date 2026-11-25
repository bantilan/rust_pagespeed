//! Eligibility of existing width lists for an explicit layout sizes policy.
use ps_html::srcset::{supports_width_sizes, MAX_SIZES_CANDIDATES, MAX_SIZES_SRCSET_BYTES};

#[test]
fn accepts_width_lists_without_changing_author_text() {
    for input in [
        "large.jpg 800w, small.jpg 300w, middle.jpg 768w",
        "  /small.jpg\t0300w ,\n/large.jpg 800w\r\n",
        "https://example.org/a%20b.jpg?a=1&b=2 640w",
        "data:image/png;base64,AAAA 300w, other.jpg 600w",
        "data:image/svg+xml,%3Csvg%3E,%3C/svg%3E 300w",
        "café.jpg 400w",
        "one.jpg 4294967295w",
    ] {
        let before = input.as_bytes().to_vec();
        assert!(supports_width_sizes(input.as_bytes()), "{input:?}");
        assert_eq!(input.as_bytes(), before);
    }
}

#[test]
fn rejects_unsupported_or_ambiguous_lists() {
    for input in [
        "",
        " \n",
        "one.jpg",
        "one.jpg 1x",
        "one.jpg 2x, two.jpg 3x",
        "one.jpg 300w, two.jpg 2x",
        "one.jpg 300w, two.jpg",
        "one.jpg 300w, two.jpg 300w",
        "one.jpg 0300w, two.jpg 300w",
        "one.jpg 0w",
        "one.jpg -1w",
        "one.jpg +1w",
        "one.jpg 1.5w",
        "one.jpg 4294967296w",
        "one.jpg 300W",
        "one.jpg 300w 200h",
        "one.jpg 300w 1x",
        ",one.jpg 300w",
        "one.jpg 300w,",
        "one.jpg 300w,, two.jpg 600w",
        "one.jpg, 300w",
        "one.jpg 300w two.jpg 600w",
        "one.jpg (300w)",
        "one\0.jpg 300w",
        "one\u{b}.jpg 300w",
        "<img> 300w",
        "\"one.jpg\" 300w",
        "one.jpg 300w;",
        "one.jpg 300w, ,",
    ] {
        assert!(!supports_width_sizes(input.as_bytes()), "{input:?}");
    }
    assert!(!supports_width_sizes(b"\xff.jpg 300w"));
}

#[test]
fn limits_bytes_and_candidates_before_allocation() {
    let list = (1..=MAX_SIZES_CANDIDATES)
        .map(|w| format!("image{w}.jpg {w}w"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(supports_width_sizes(list.as_bytes()));
    assert!(!supports_width_sizes(
        format!("{list}, extra.jpg 1000w").as_bytes()
    ));
    let at_limit = format!("{} 1w", "x".repeat(MAX_SIZES_SRCSET_BYTES - 3));
    assert_eq!(at_limit.len(), MAX_SIZES_SRCSET_BYTES);
    assert!(supports_width_sizes(at_limit.as_bytes()));
    assert!(!supports_width_sizes(format!("x{at_limit}").as_bytes()));
    assert!(!supports_width_sizes(&vec![
        b' ';
        MAX_SIZES_SRCSET_BYTES + 1
    ]));
}

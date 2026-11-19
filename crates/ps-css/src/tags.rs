//! HTML tag enum (`htmltagenum.h`, `htmltagindex.cc`).
//!
//! Case-insensitive builtin lookup; discriminants preserved from C++
//! (they are documented as persistable).
#![forbid(unsafe_code)]

#[allow(clippy::all)]
#[path = "gen/tags_gen.rs"]
mod generated;

pub use generated::HtmlTag;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_vectors() {
        assert_eq!(HtmlTag::find_builtin(b"p"), HtmlTag::P);
        assert_eq!(HtmlTag::find_builtin(b"P"), HtmlTag::P);
        assert_eq!(HtmlTag::find_builtin(b"blockquote"), HtmlTag::Blockquote);
        assert_eq!(HtmlTag::find_builtin(b""), HtmlTag::ZeroLength);
        assert_eq!(HtmlTag::find_builtin(b"notatag"), HtmlTag::Unknown);
        assert_eq!(
            HtmlTag::find_builtin(b"!DOCTYPE html"),
            HtmlTag::BangDoctype
        );
        assert_eq!(HtmlTag::find_builtin(b"!foo"), HtmlTag::BogusComment);
        assert_eq!(HtmlTag::find_builtin(b"?foo"), HtmlTag::BogusComment);
        assert_eq!(HtmlTag::find_builtin(b"plaintext"), HtmlTag::Plaintext);
        assert_eq!(HtmlTag::Unknown as i32, 0);
    }
}

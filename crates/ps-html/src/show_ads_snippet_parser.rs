//! ShowAds snippet parsing (`ShowAdsSnippetParser::ParseStrict`).
//!
//! Strictly parses `google_name = value;` assignments (string or number
//! values, semicolon or newline separated) into a sorted attribute map.
//! Attribute names match `google_` + word characters; `google_ad_format`
//! values match `WxH[_suffix]`. Tokenizing rides the shared [`ps_js`]
//! tokenizer (patterns owned here so scripts never pay recompilation).

use std::collections::BTreeMap;

use ps_js::keywords::TokenType;
use ps_js::tokenizer::{Tokenizer, TokenizerPatterns};

use crate::ads_attribute;

/// Parsed snippet attributes, sorted by name (`AttributeMap`).
pub type AttributeMap = BTreeMap<Vec<u8>, Vec<u8>>;

/// Strict snippet parser (`ShowAdsSnippetParser`).
#[derive(Debug)]
pub struct ShowAdsSnippetParser {
    patterns: &'static TokenizerPatterns,
}

impl ShowAdsSnippetParser {
    /// Builds the parser (compiles tokenizer patterns once).
    #[must_use]
    pub fn new() -> Self {
        ShowAdsSnippetParser {
            patterns: TokenizerPatterns::shared(),
        }
    }

    /// Strictly parses a snippet (`ParseStrict`); `None` rejects.
    #[must_use]
    pub fn parse_strict(&self, content: &[u8]) -> Option<AttributeMap> {
        let snippet = strip_enclosing_comment_tag(trim_whitespace(content));
        let mut tokenizer = Tokenizer::new(&self.patterns, snippet);
        let mut parsed = AttributeMap::new();
        loop {
            let (kind, span) = next_significant(&mut tokenizer);
            let token = tokenizer.text(span);
            if kind == TokenType::Identifier {
                if !is_valid_attribute_name(token) {
                    return None;
                }
                if parsed.contains_key(token) {
                    return None;
                }
                let (kind, span) = next_significant(&mut tokenizer);
                if kind != TokenType::Operator || tokenizer.text(span) != b"=" {
                    return None;
                }
                let (kind, span) = next_significant(&mut tokenizer);
                if kind != TokenType::StringLiteral && kind != TokenType::Number {
                    return None;
                }
                let value = strip_enclosing_quotes(tokenizer.text(span)).to_vec();
                if !is_valid_attribute(token, &value) {
                    return None;
                }
                parsed.insert(token.to_vec(), value);
                let (kind, span) = next_significant(&mut tokenizer);
                if kind == TokenType::EndOfInput {
                    return Some(parsed);
                }
                if kind == TokenType::Operator && tokenizer.text(span) == b";"
                    || kind == TokenType::SemiInsert
                {
                    continue;
                }
                return None;
            } else if kind == TokenType::EndOfInput {
                return Some(parsed);
            } else if kind == TokenType::Operator && token == b";" {
                continue;
            } else {
                return None;
            }
        }
    }
}

impl Default for ShowAdsSnippetParser {
    fn default() -> Self {
        ShowAdsSnippetParser::new()
    }
}

/// Advances past whitespace, comments, and line separators.
fn next_significant(tokenizer: &mut Tokenizer<'_>) -> (TokenType, (usize, usize)) {
    loop {
        let (kind, span) = tokenizer.next_token();
        if kind != TokenType::Whitespace
            && kind != TokenType::Comment
            && kind != TokenType::LineSeparator
        {
            return (kind, span);
        }
    }
}

/// Trims HTML spaces (`TrimWhitespace`: space, tab, LF, FF, CR).
fn trim_whitespace(mut text: &[u8]) -> &[u8] {
    while let Some(first) = text.first() {
        if matches!(first, b' ' | b'\t' | b'\n' | 0x0c | b'\r') {
            text = &text[1..];
        } else {
            break;
        }
    }
    while let Some(last) = text.last() {
        if matches!(last, b' ' | b'\t' | b'\n' | 0x0c | b'\r') {
            text = &text[..text.len() - 1];
        } else {
            break;
        }
    }
    text
}

/// Removes an enclosing `<!--` ... `//-->` pair.
fn strip_enclosing_comment_tag(input: &[u8]) -> &[u8] {
    if input.starts_with(b"<!--") && input.ends_with(b"//-->") {
        &input[4..input.len() - 5]
    } else {
        input
    }
}

/// Removes matching enclosing quotes.
fn strip_enclosing_quotes(input: &[u8]) -> &[u8] {
    if input.len() >= 2
        && (input.starts_with(b"\"") && input.ends_with(b"\"")
            || input.starts_with(b"'") && input.ends_with(b"'"))
    {
        &input[1..input.len() - 1]
    } else {
        input
    }
}

/// Name rule (`google_([a-zA-Z0-9]*)(_[a-zA-Z0-9_]+)?`, full match):
/// after the `google_` prefix every byte is alphanumeric or `_`.
fn is_valid_attribute_name(name: &[u8]) -> bool {
    let Some(rest) = name.strip_prefix(b"google_") else {
        return false;
    };
    rest.iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

/// Value rule: `google_ad_format` must match
/// `([0-9]*)x([0-9]*)(_[a-zA-Z0-9_]+)?` after trimming; all other
/// attributes accept anything.
fn is_valid_attribute(name: &[u8], value: &[u8]) -> bool {
    if name != ads_attribute::GOOGLE_AD_FORMAT {
        return true;
    }
    is_valid_ad_format(trim_whitespace(value))
}

/// Hand-rolled format pattern (equivalent to the RE2 full match, without
/// the regex dependency): digits, one `x`, digits, then nothing or `_`
/// plus one or more word characters.
fn is_valid_ad_format(value: &[u8]) -> bool {
    let digits = value
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let rest = &value[digits..];
    let Some(rest) = rest.strip_prefix(b"x") else {
        return false;
    };
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    let rest = &rest[digits..];
    if rest.is_empty() {
        return true;
    }
    let Some(suffix) = rest.strip_prefix(b"_") else {
        return false;
    };
    !suffix.is_empty()
        && suffix
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(snippet: &[u8]) -> Option<AttributeMap> {
        ShowAdsSnippetParser::new().parse_strict(snippet)
    }

    fn check_standard(map: &AttributeMap) {
        assert_eq!(map.len(), 4);
        assert_eq!(
            map.get(b"google_ad_client".as_slice()).unwrap(),
            b"ca-pub-xxxxxxxxxxxxxx"
        );
        assert_eq!(map.get(b"google_ad_slot".as_slice()).unwrap(), b"xxxxxxxxx");
        assert_eq!(map.get(b"google_ad_width".as_slice()).unwrap(), b"728");
        assert_eq!(map.get(b"google_ad_height".as_slice()).unwrap(), b"90");
    }

    #[test]
    fn parse_strict_empty() {
        let map = parse(b"").expect("empty parses");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_strict_valid() {
        let map = parse(
            b"google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_width = 728;google_ad_height = 90;",
        )
        .expect("valid parses");
        check_standard(&map);
    }

    #[test]
    fn parse_strict_valid_single_quote() {
        let map = parse(
            b"google_ad_client = 'ca-pub-xxxxxxxxxxxxxx';/* ad served */google_ad_slot = 'xxxxxxxxx';google_ad_width = 728;google_ad_height = 90;",
        )
        .expect("single quotes parse");
        check_standard(&map);
    }

    #[test]
    fn parse_strict_valid_empty_lines() {
        let map = parse(
            b"\n\n\n\n\ngoogle_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";\n\n\n\ngoogle_ad_width = 728;google_ad_height = 90;",
        )
        .expect("empty lines parse");
        check_standard(&map);
    }

    #[test]
    fn parse_strict_valid_empty_statement() {
        let map = parse(
            b"\n\n\n\n\ngoogle_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";;;;;google_ad_width = 728;google_ad_height = 90;",
        )
        .expect("empty statements parse");
        check_standard(&map);
    }

    #[test]
    fn parse_strict_valid_without_semicolon() {
        let map = parse(
            b"google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\"\n/* ad served */google_ad_slot = \"xxxxxxxxx\"\ngoogle_ad_width = 728\ngoogle_ad_height = 90\n",
        )
        .expect("newlines parse");
        check_standard(&map);
    }

    #[test]
    fn parse_strict_valid_with_enclosing_comment_tag() {
        let map = parse(
            b"<!--google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_width = 728;google_ad_height = 90;//-->",
        )
        .expect("comment tags parse");
        check_standard(&map);
    }

    #[test]
    fn parse_strict_valid_with_enclosing_comment_tag_and_whitespaces() {
        let map = parse(
            b"    <!--google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_width = 728;google_ad_height = 90;//-->    ",
        )
        .expect("padded comment tags parse");
        check_standard(&map);
    }

    #[test]
    fn parse_strict_google_ad_format() {
        let map = parse(
            b"google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_format = \"728x90\";google_ad_width = 728;google_ad_height = 90;",
        )
        .expect("format parses");
        assert_eq!(map.len(), 5);
        assert_eq!(map.get(b"google_ad_format".as_slice()).unwrap(), b"728x90");
    }

    #[test]
    fn parse_weird_google_ad_format() {
        let map = parse(b"google_ad_format = \"728x90_as\";").expect("suffix parses");
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get(b"google_ad_format".as_slice()).unwrap(),
            b"728x90_as"
        );
        let map = parse(b"google_ad_format = \"180x90_0ads_al_s\";").expect("suffix parses");
        assert_eq!(
            map.get(b"google_ad_format".as_slice()).unwrap(),
            b"180x90_0ads_al_s"
        );
    }

    #[test]
    fn parse_strict_google_ad_format_with_whitespaces() {
        let map = parse(
            b"google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_format = \"  728x90  \";google_ad_width = 728;google_ad_height = 90;",
        )
        .expect("padded format parses");
        assert_eq!(map.len(), 5);
        // Validation trims, but the stored value keeps its spaces.
        assert_eq!(
            map.get(b"google_ad_format".as_slice()).unwrap(),
            b"  728x90  "
        );
    }

    #[test]
    fn parse_short_attribute() {
        let map = parse(b"google_language = \"de\"").expect("short attr parses");
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(b"google_language".as_slice()).unwrap(), b"de");
    }

    #[test]
    fn parse_strict_google_ad_format_with_unexpected_prefix() {
        assert!(parse(
            b"google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_format = \"test_722x92\";google_ad_width = 728;google_ad_height = 90;"
        )
        .is_none());
    }

    #[test]
    fn parse_strict_google_ad_format_with_unexpected_ends() {
        assert!(parse(
            b"google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_format = \"test_722x92_rimg\";google_ad_width = 728;google_ad_height = 90;"
        )
        .is_none());
    }

    #[test]
    fn parse_strict_invalid_attribute_name_not_started_with_google() {
        assert!(parse(
            b"<!--dgoogle_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_width = 728;google_ad_height = 90;//-->"
        )
        .is_none());
    }

    #[test]
    fn parse_strict_invalid_attribute_name_illegal_char() {
        assert!(parse(
            b"google_ad_invalid-name = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_width = 728;google_ad_height = 90;"
        )
        .is_none());
    }

    #[test]
    fn parse_strict_invalid_duplicate() {
        assert!(parse(
            b"<!--google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_slot = \"xxxxxxxxy\";google_ad_width = 728;google_ad_height = 90;//-->"
        )
        .is_none());
    }

    #[test]
    fn parse_strict_invalid_missing_semicolon() {
        assert!(parse(
            b"google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\" /* ad served */google_ad_slot = \"xxxxxxxxx\"\ngoogle_ad_width = 728\ngoogle_ad_height = 90\n"
        )
        .is_none());
    }

    #[test]
    fn parse_strict_invalid_modified() {
        assert!(parse(
            b"<!--google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";if (test) google_ad_client = \"ca-pub-xxxxxxxxxxxxxy\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_width = 728;google_ad_height = 90;//-->"
        )
        .is_none());
    }

    #[test]
    fn parse_strict_invalid_assignment() {
        assert!(parse(
            b"google_ad_client = \"ca-pub-xxxxxxxxxxxxxx\";/* ad served */google_ad_slot = \"xxxxxxxxx\";google_ad_width = 728;google_ad_height = google_ad_width;"
        )
        .is_none());
    }

    #[test]
    fn parse_color_array() {
        // Arrays could work in principle, but are not worth the effort.
        assert!(
            parse(b"google_color_border = [\"336699\",\"CC99CC\",\"578A24\",\"191933\"]").is_none()
        );
    }
}

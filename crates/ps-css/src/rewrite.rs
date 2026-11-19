//! Stylesheet URL rewriting (`CssImageRewriter::RewriteCss` walk).
//!
//! Visits URI values in image-bearing declarations and rewrites them
//! through a caller-supplied mapper (fetching and authorization live
//! in the driver layer).
#![forbid(unsafe_code)]

use crate::ast::{RulesetType, Stylesheet};
use crate::property::Prop;
use crate::unicode_text::UnicodeText;
use crate::value::ValueType;

/// Properties whose URI values are rewritten
/// (`CssImageRewriter::RewriteCss`).
#[must_use]
pub fn is_image_property(prop: Prop) -> bool {
    matches!(
        prop,
        Prop::Background
            | Prop::BackgroundImage
            | Prop::Content
            | Prop::Cursor
            | Prop::ListStyle
            | Prop::ListStyleImage
    )
}

/// Location of a URI value: ruleset, declaration, value indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UrlLocation {
    /// Ruleset index.
    pub ruleset: usize,
    /// Declaration index within the ruleset.
    pub declaration: usize,
    /// Value index within the declaration.
    pub value: usize,
}

/// Collects URI-value locations in image declarations, in document
/// order. Unparsed-region rulesets are skipped.
#[must_use]
pub fn collect_image_urls(stylesheet: &Stylesheet) -> Vec<UrlLocation> {
    let mut out = Vec::new();
    for (ruleset_index, ruleset) in stylesheet.rulesets().iter().enumerate() {
        if ruleset.kind() != RulesetType::Ruleset {
            continue;
        }
        for (decl_index, declaration) in ruleset.declarations().iter().enumerate() {
            if !is_image_property(declaration.prop()) {
                continue;
            }
            for (value_index, value) in declaration.values().iter().enumerate() {
                if value.lexical_type() == ValueType::Uri {
                    out.push(UrlLocation {
                        ruleset: ruleset_index,
                        declaration: decl_index,
                        value: value_index,
                    });
                }
            }
        }
    }
    out
}

/// Rewrites URI values through a mapper (`RewriteImage` slot loop).
/// The mapper sees the raw URL text and returns its replacement, or
/// `None` to leave it alone (unresolvable, unauthorized, unchanged).
/// Returns the number of values rewritten.
pub fn rewrite_image_urls(
    stylesheet: &mut Stylesheet,
    map: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
) -> usize {
    let locations = collect_image_urls(stylesheet);
    let mut rewritten = 0;
    for location in locations {
        let value = &mut stylesheet.rulesets_mut()[location.ruleset].declarations_mut()
            [location.declaration]
            .values_mut()[location.value];
        let Some(text) = value.string_value_mut() else {
            continue;
        };
        if let Some(replacement) = map(text.as_bytes()) {
            *text = UnicodeText::from_utf8(&replacement);
            rewritten += 1;
        }
    }
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn walk_and_rewrite() {
        let mut parser = Parser::new(
            b"a { background: url(old.png) red; color: red; } \
              b { list-style-image: url(l.png); }",
        );
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        let mut stylesheet = parser.parse_raw_stylesheet();
        assert_eq!(parser.errors_seen_mask(), 0);
        let locations = collect_image_urls(&stylesheet);
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].declaration, 0);
        assert_eq!(locations[1].ruleset, 1);
        let count = rewrite_image_urls(&mut stylesheet, &mut |url| {
            if url == b"old.png" {
                Some(b"new.png".to_vec())
            } else {
                None
            }
        });
        assert_eq!(count, 1);
        let mut minifier = crate::minify::Minifier::new(false);
        minifier.minify_stylesheet(&stylesheet);
        assert_eq!(
            minifier.finish().0,
            b"a{background:url(new.png) red;color:red}b{list-style-image:url(l.png)}"
        );
    }

    #[test]
    fn skips_unparsed_regions() {
        let mut parser = Parser::new(b"a { color: red; } @bad url(x);");
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        let stylesheet = parser.parse_raw_stylesheet();
        assert!(collect_image_urls(&stylesheet).is_empty());
    }
}

//! URL absolutification (`css_absolutify.{h,cc}`).
//!
//! Rewrites import links and URI values (including function-nested
//! ones like `image-set()`, plus unparseable verbatim regions)
//! through a caller-supplied transformer.
#![forbid(unsafe_code)]

use crate::ast::{Declarations, RulesetType, Stylesheet};
use crate::scanner::{transform_urls, TransformStatus, Transformer};
use crate::unicode_text::UnicodeText;
use crate::value::{FunctionParameters, Value, ValueType};

/// Absolutifies `@import` links (`AbsolutifyImports`): links that
/// resolve to a different web-valid URL are replaced.
pub fn absolutify_imports(
    stylesheet: &mut Stylesheet,
    resolve: &dyn Fn(&[u8]) -> Option<Vec<u8>>,
) -> bool {
    let mut modified = false;
    for import in stylesheet.imports_mut() {
        let link = import.link().as_bytes().to_vec();
        if let Some(resolved) = resolve(&link) {
            if resolved != link {
                import.set_link(UnicodeText::from_utf8(&resolved));
                modified = true;
            }
        }
    }
    modified
}

/// Absolutifies style URLs (`AbsolutifyUrls`): font faces always
/// handle parseable sections; rulesets per the flags.
pub fn absolutify_urls(
    stylesheet: &mut Stylesheet,
    transformer: &mut dyn Transformer,
    handle_parseable_ruleset_sections: bool,
    handle_unparseable_sections: bool,
) -> bool {
    let mut modified = false;
    for face in stylesheet.font_faces_mut() {
        if absolutify_declarations(
            face.declarations_mut(),
            transformer,
            true,
            handle_unparseable_sections,
        ) {
            modified = true;
        }
    }
    for ruleset in stylesheet.rulesets_mut() {
        if handle_unparseable_sections {
            match ruleset.kind() {
                RulesetType::Ruleset => {
                    if ruleset.selectors().is_dummy() {
                        let original = ruleset.selectors().bytes_in_original_buffer().to_vec();
                        if let Some(rewritten) = transform_urls(&original, transformer) {
                            ruleset
                                .selectors_mut()
                                .set_bytes_in_original_buffer(&rewritten);
                            modified = true;
                        }
                    }
                }
                RulesetType::UnparsedRegion => {
                    let original = ruleset
                        .unparsed_region()
                        .bytes_in_original_buffer()
                        .to_vec();
                    if let Some(rewritten) = transform_urls(&original, transformer) {
                        ruleset.unparsed_region_mut().set_bytes(&rewritten);
                        modified = true;
                    }
                }
            }
        }
        if ruleset.kind() == RulesetType::Ruleset
            && absolutify_declarations(
                ruleset.declarations_mut(),
                transformer,
                handle_parseable_ruleset_sections,
                handle_unparseable_sections,
            )
        {
            modified = true;
        }
    }
    modified
}

/// Absolutifies declaration URLs (`AbsolutifyDeclarations`).
/// NOTE: finding any URI value marks the output modified, even when
/// the transformer leaves it unchanged (upstream quirk).
pub fn absolutify_declarations(
    declarations: &mut Declarations,
    transformer: &mut dyn Transformer,
    handle_parseable_sections: bool,
    handle_unparseable_sections: bool,
) -> bool {
    let mut modified = false;
    for declaration in declarations.iter_mut() {
        if declaration.prop() == crate::property::Prop::Unparseable {
            if handle_unparseable_sections {
                let original = declaration.bytes_in_original_buffer().to_vec();
                if let Some(rewritten) = transform_urls(&original, transformer) {
                    declaration.set_bytes_in_original_buffer(&rewritten);
                    modified = true;
                }
            }
        } else if handle_parseable_sections {
            for value in declaration.values_mut().iter_mut() {
                let replacement = match value.lexical_type() {
                    ValueType::Uri => {
                        modified = true;
                        let mut url = value.string_value().as_bytes().to_vec();
                        if transformer.transform(&mut url) == TransformStatus::Success {
                            Some(Value::text(ValueType::Uri, UnicodeText::from_utf8(&url)))
                        } else {
                            None
                        }
                    }
                    ValueType::Function => {
                        transform_function_urls(value, transformer, &mut modified)
                    }
                    _ => None,
                };
                if let Some(replacement) = replacement {
                    *value = replacement;
                }
            }
        }
    }
    modified
}

/// Copies a function value with nested `url()`s transformed
/// (`TransformFunctionUrls`); `None` when nothing changed.
fn transform_function_urls(
    function: &Value,
    transformer: &mut dyn Transformer,
    saw_url: &mut bool,
) -> Option<Value> {
    let params = function.parameters_with_separators();
    let mut copy = FunctionParameters::new();
    let mut changed = false;
    for (index, param) in params.values().iter().enumerate() {
        let replacement = match param.lexical_type() {
            ValueType::Uri => {
                *saw_url = true;
                let mut url = param.string_value().as_bytes().to_vec();
                if transformer.transform(&mut url) == TransformStatus::Success {
                    Some(Value::text(ValueType::Uri, UnicodeText::from_utf8(&url)))
                } else {
                    None
                }
            }
            ValueType::Function => transform_function_urls(param, transformer, saw_url),
            _ => None,
        };
        match replacement {
            Some(value) => {
                changed = true;
                copy.add(params.separator(index), value);
            }
            None => {
                copy.add(params.separator(index), param.clone());
            }
        }
    }
    if !changed {
        return None;
    }
    Some(Value::function(function.function_name().clone(), copy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    struct Prefix {
        base: Vec<u8>,
    }

    impl Transformer for Prefix {
        fn transform(&mut self, url: &mut Vec<u8>) -> TransformStatus {
            if url.contains(&b':') {
                return TransformStatus::NoChange;
            }
            let mut mapped = self.base.clone();
            mapped.extend_from_slice(url);
            *url = mapped;
            TransformStatus::Success
        }
    }

    fn parse(contents: &[u8]) -> Stylesheet {
        let mut parser = Parser::new(contents);
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        parser.parse_raw_stylesheet()
    }

    #[test]
    fn absolutify_vectors() {
        let mut stylesheet = parse(b"@import url(b.css);a{background:url(i.png)}");
        assert!(absolutify_imports(&mut stylesheet, &|link| {
            if link.contains(&b':') {
                return None;
            }
            Some([b"http://h/".as_slice(), link].concat())
        }));
        assert_eq!(stylesheet.import(0).link().as_bytes(), b"http://h/b.css");
        let mut transformer = Prefix {
            base: b"http://h/".to_vec(),
        };
        assert!(absolutify_urls(
            &mut stylesheet,
            &mut transformer,
            true,
            true
        ));
        let mut minifier = crate::minify::Minifier::new(false);
        minifier.minify_stylesheet(&stylesheet);
        assert_eq!(
            minifier.finish().0,
            b"@import url(http://h/b.css);a{background:url(http://h/i.png)}"
        );
    }

    #[test]
    fn nested_function_urls() {
        let mut stylesheet = parse(b"a{background:image-set(url(a.png) 1x,url(b.png) 2x)}");
        let mut transformer = Prefix {
            base: b"http://h/".to_vec(),
        };
        assert!(absolutify_urls(
            &mut stylesheet,
            &mut transformer,
            true,
            false
        ));
        let mut minifier = crate::minify::Minifier::new(false);
        minifier.minify_stylesheet(&stylesheet);
        assert_eq!(
            minifier.finish().0,
            b"a{background:image-set(url(http://h/a.png) 1x,url(http://h/b.png) 2x)}"
        );
    }

    #[test]
    fn unparseable_regions_rewritten() {
        // Invalid declaration content is preserved verbatim and its
        // URLs still absolutify when unparseable handling is on.
        let mut stylesheet = parse(b"a{color<:url(x.png)>}");
        let mut transformer = Prefix {
            base: b"http://h/".to_vec(),
        };
        assert!(absolutify_urls(
            &mut stylesheet,
            &mut transformer,
            true,
            true
        ));
        let mut minifier = crate::minify::Minifier::new(false);
        minifier.minify_stylesheet(&stylesheet);
        assert_eq!(minifier.finish().0, b"a{color<:url(http://h/x.png)>}");
        // With it off, nothing changes.
        let mut stylesheet = parse(b"a{color<:url(x.png)>}");
        let mut transformer = Prefix {
            base: b"http://h/".to_vec(),
        };
        assert!(!absolutify_urls(
            &mut stylesheet,
            &mut transformer,
            true,
            false
        ));
    }
}

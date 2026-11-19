//! CSS minification (`css_minify.{h,cc}`).
//!
//! Parses in preservation mode (quirks off) and re-serializes the AST
//! minimally. Parse errors fail closed: callers serve the original.
#![forbid(unsafe_code)]

use crate::ast::{
    Declaration, Declarations, FontFace, Import, Ruleset, RulesetType, Stylesheet, UnparsedRegion,
};
use crate::escapes::{
    escape_identifier, escape_unicode_identifier, escape_unicode_string, escape_unicode_url,
};
use crate::identifier::Ident;
use crate::media::{MediaExpression, MediaQualifier, MediaQueries, MediaQuery};
use crate::parser::Parser;
use crate::property::Prop;
use crate::selector::{Combinator, Selector, Selectors, SimpleSelector, SimpleSelectors};
use crate::string_util::format_g;
use crate::unicode_text::UnicodeText;
use crate::value::{FunctionParameters, Value, ValueType, Values};

/// Length units whose zero keeps no suffix (`kLengths`, sorted).
const LENGTHS: &[&str] = &[
    "ch", "cm", "em", "ex", "in", "mm", "pc", "pt", "px", "q", "rem", "vh", "vmax", "vmin", "vw",
];

fn is_length(unit: &[u8]) -> bool {
    LENGTHS
        .binary_search_by(|known| known.as_bytes().cmp(unit))
        .is_ok()
}

/// Whether a zero value keeps its unit (`UnitsRequiredForValueZero`).
fn units_required_for_zero(unit: &[u8], in_calc: bool) -> bool {
    unit == b"%" || !is_length(unit) || in_calc
}

/// The minifier (`CssMinify`).
#[derive(Debug)]
pub struct Minifier {
    out: Vec<u8>,
    ok: bool,
    urls: Option<Vec<Vec<u8>>>,
    in_calc: bool,
}

impl Minifier {
    /// Creates a minifier; `collect_urls` enables the URL collector
    /// (`set_url_collector`).
    #[must_use]
    pub fn new(collect_urls: bool) -> Minifier {
        Minifier {
            out: Vec::new(),
            ok: true,
            urls: collect_urls.then(Vec::new),
            in_calc: false,
        }
    }

    /// Parses and minifies text (`ParseStylesheet`).
    pub fn parse_stylesheet(&mut self, input: &[u8]) -> bool {
        let mut parser = Parser::new(input);
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        let stylesheet = parser.parse_raw_stylesheet();
        self.minify_stylesheet(&stylesheet);
        self.ok && parser.errors_seen_mask() == 0
    }

    /// Minifies a parsed stylesheet (`CssMinify::Stylesheet`).
    pub fn minify_stylesheet(&mut self, stylesheet: &Stylesheet) {
        self.minify_charsets(stylesheet.charsets());
        for import in stylesheet.imports() {
            self.minify_import(import);
        }
        self.minify_font_faces_grouped(stylesheet.font_faces());
        self.minify_rulesets_grouped(stylesheet.rulesets());
    }

    /// Minifies declarations (`CssMinify::Declarations`).
    pub fn minify_declarations(&mut self, declarations: &Declarations) {
        self.join_declarations(declarations, b";");
    }

    /// Takes the output and status.
    #[must_use]
    pub fn finish(self) -> (Vec<u8>, bool) {
        (self.out, self.ok)
    }

    /// Collected URLs (`url_collector_`).
    #[must_use]
    pub fn urls(&self) -> Option<&[Vec<u8>]> {
        self.urls.as_deref()
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.ok {
            self.out.extend_from_slice(bytes);
        }
    }

    fn write_url(&mut self, url: &UnicodeText) {
        if let Some(urls) = self.urls.as_mut() {
            urls.push(url.as_bytes().to_vec());
        }
        let escaped = escape_unicode_url(url);
        self.write(&escaped);
    }

    fn minify_charsets(&mut self, charsets: &[UnicodeText]) {
        for charset in charsets {
            self.write(b"@charset \"");
            let escaped = escape_unicode_string(charset);
            self.write(&escaped);
            self.write(b"\";");
        }
    }

    fn minify_import(&mut self, import: &Import) {
        self.write(b"@import url(");
        self.write_url(import.link());
        self.write(b")");
        if !import.media_queries().is_empty() {
            self.write(b" ");
            self.join_media_queries(import.media_queries(), b",");
        }
        self.write(b";");
    }

    fn join_media_queries(&mut self, queries: &MediaQueries, sep: &[u8]) {
        for (index, query) in queries.iter().enumerate() {
            if index > 0 {
                self.write(sep);
            }
            self.minify_media_query(query);
        }
    }

    fn minify_media_query(&mut self, query: &MediaQuery) {
        match query.qualifier() {
            MediaQualifier::Only => self.write(b"only "),
            MediaQualifier::Not => self.write(b"not "),
            MediaQualifier::None => {}
        }
        let media_type = escape_unicode_identifier(query.media_type());
        self.write(&media_type);
        if !query.media_type().is_empty() && !query.expressions().is_empty() {
            self.write(b" and ");
        }
        for (index, expression) in query.expressions().iter().enumerate() {
            if index > 0 {
                self.write(b" and ");
            }
            self.minify_media_expression(expression);
        }
    }

    fn minify_media_expression(&mut self, expression: &MediaExpression) {
        self.write(b"(");
        let name = escape_unicode_identifier(expression.name());
        self.write(&name);
        if expression.has_value() {
            self.write(b":");
            // Raw bytes, unescaped (unparsed region).
            self.write(expression.value().as_bytes());
        }
        self.write(b")");
    }

    fn minify_media_start(&mut self, queries: &MediaQueries) {
        if !queries.is_empty() {
            self.write(b"@media ");
            self.join_media_queries(queries, b",");
            self.write(b"{");
        }
    }

    fn minify_media_end(&mut self, queries: &MediaQueries) {
        if !queries.is_empty() {
            self.write(b"}");
        }
    }

    fn minify_font_faces_grouped(&mut self, faces: &[FontFace]) {
        let mut index = 0;
        while index < faces.len() {
            let first = faces[index].media_queries().clone();
            self.minify_media_start(&first);
            self.minify_font_face(&faces[index]);
            index += 1;
            while index < faces.len() && media_queries_equal(&first, faces[index].media_queries()) {
                self.minify_font_face(&faces[index]);
                index += 1;
            }
            self.minify_media_end(&first);
        }
    }

    fn minify_font_face(&mut self, face: &FontFace) {
        self.write(b"@font-face{");
        self.join_declarations(face.declarations(), b";");
        self.write(b"}");
    }

    fn minify_rulesets_grouped(&mut self, rulesets: &[Ruleset]) {
        let mut index = 0;
        while index < rulesets.len() {
            let first = rulesets[index].media_queries().clone();
            self.minify_media_start(&first);
            self.minify_ruleset_ignoring_media(&rulesets[index]);
            index += 1;
            while index < rulesets.len()
                && media_queries_equal(&first, rulesets[index].media_queries())
            {
                self.minify_ruleset_ignoring_media(&rulesets[index]);
                index += 1;
            }
            self.minify_media_end(&first);
        }
    }

    fn minify_ruleset_ignoring_media(&mut self, ruleset: &Ruleset) {
        match ruleset.kind() {
            RulesetType::Ruleset => {
                if ruleset.selectors().is_dummy() {
                    self.write(ruleset.selectors().bytes_in_original_buffer());
                } else {
                    self.join_selectors(ruleset.selectors(), b",");
                }
                self.write(b"{");
                self.join_declarations(ruleset.declarations(), b";");
                self.write(b"}");
            }
            RulesetType::UnparsedRegion => {
                self.minify_unparsed_region(ruleset.unparsed_region());
            }
        }
    }

    fn join_selectors(&mut self, selectors: &Selectors, sep: &[u8]) {
        for (index, selector) in selectors.iter().enumerate() {
            if index > 0 {
                self.write(sep);
            }
            self.minify_selector(selector);
        }
    }

    fn minify_selector(&mut self, selector: &Selector) {
        let mut chains = selector.iter();
        if let Some(first) = chains.next() {
            self.minify_simple_selectors(first, true);
            for chain in chains {
                self.minify_simple_selectors(chain, false);
            }
        }
    }

    fn minify_simple_selectors(&mut self, chains: &SimpleSelectors, is_first: bool) {
        match chains.combinator() {
            Combinator::Child => self.write(b">"),
            Combinator::Sibling => self.write(b"+"),
            Combinator::None | Combinator::Descendant => {
                if !is_first {
                    self.write(b" ");
                }
            }
        }
        for simple in chains.iter() {
            self.minify_simple_selector(simple);
        }
    }

    fn minify_simple_selector(&mut self, selector: &SimpleSelector) {
        // SimpleSelector::ToString is already basically minified.
        let text = selector.to_css_string();
        self.write(&text);
    }

    fn minify_font(&mut self, values: &Values) {
        assert!(values.len() >= 5);
        if !is_normal_identifier(&values[0]) {
            self.minify_value(&values[0]);
            self.write(b" ");
        }
        if !is_normal_identifier(&values[1]) {
            self.minify_value(&values[1]);
            self.write(b" ");
        }
        if !is_normal_identifier(&values[2]) {
            self.minify_value(&values[2]);
            self.write(b" ");
        }
        self.minify_value(&values[3]);
        if !is_normal_identifier(&values[4]) {
            self.write(b"/");
            self.minify_value(&values[4]);
        }
        for (index, value) in values.iter().enumerate().skip(5) {
            self.write(if index == 5 { b" " } else { b"," });
            self.minify_value(value);
        }
    }

    fn join_declarations(&mut self, declarations: &Declarations, sep: &[u8]) {
        for (index, declaration) in declarations.iter().enumerate() {
            if index > 0 {
                self.write(sep);
            }
            self.minify_declaration(declaration);
        }
    }

    fn minify_declaration(&mut self, declaration: &Declaration) {
        if declaration.prop() == Prop::Unparseable {
            self.write(declaration.bytes_in_original_buffer());
            return;
        }
        let prop = escape_identifier(&declaration.prop_text());
        self.write(&prop);
        self.write(b":");
        match declaration.prop() {
            Prop::FontFamily => {
                self.join_values(declaration.values(), b",");
            }
            Prop::Font => {
                if declaration.values().len() == 1 {
                    self.join_values(declaration.values(), b" ");
                } else if declaration.values().len() >= 5 {
                    self.minify_font(declaration.values());
                } else {
                    // C++ logs "Unexpected number of values" and fails.
                    self.ok = false;
                }
            }
            _ => {
                if declaration.prop_text() == b"unicode-range" {
                    self.join_values(declaration.values(), b"");
                } else {
                    self.join_values(declaration.values(), b" ");
                }
            }
        }
        if declaration.is_important() {
            self.write(b"!important");
        }
    }

    fn join_values(&mut self, values: &Values, sep: &[u8]) {
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                self.write(sep);
            }
            self.minify_value(value);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn minify_value(&mut self, value: &Value) {
        match value.lexical_type() {
            ValueType::Number => {
                if value.bytes_in_original_buffer().is_empty() {
                    let text = format_g(value.float_value(), 16);
                    self.write_number_text(text.as_bytes());
                } else {
                    self.write_number_text(value.bytes_in_original_buffer());
                }
                let unit = value.dimension_unit_text();
                if !unit.is_empty()
                    && (value.float_value() != 0.0 || units_required_for_zero(&unit, self.in_calc))
                {
                    if unit == b"%" {
                        self.write(b"%");
                    } else {
                        let escaped = escape_identifier(&unit);
                        self.write(&escaped);
                    }
                }
            }
            ValueType::Uri => {
                self.write(b"url(");
                self.write_url(value.string_value());
                self.write(b")");
            }
            ValueType::Function => {
                let name = escape_unicode_identifier(value.function_name());
                if name == b"calc" {
                    self.in_calc = true;
                }
                self.write(&name);
                self.write(b"(");
                self.minify_function_parameters(value.parameters_with_separators());
                self.write(b")");
                self.in_calc = false;
            }
            ValueType::Rect => {
                self.write(b"rect(");
                self.minify_function_parameters(value.parameters_with_separators());
                self.write(b")");
            }
            ValueType::Color => {
                assert!(value.color_value().is_defined());
                self.write(value.color_value().to_css_shorthand().as_bytes());
            }
            ValueType::String => {
                if value.bytes_in_original_buffer().is_empty() {
                    self.write(b"\"");
                    let escaped = escape_unicode_string(value.string_value());
                    self.write(&escaped);
                    self.write(b"\"");
                } else {
                    self.write(value.bytes_in_original_buffer());
                }
            }
            ValueType::Ident => {
                let text = escape_unicode_identifier(&value.identifier_text());
                self.write(&text);
            }
            ValueType::Comma => {
                self.write(b",");
            }
            ValueType::Unknown => {
                self.ok = false;
            }
            ValueType::Default => {}
        }
    }

    /// Writes a number, stripping a redundant leading zero.
    fn write_number_text(&mut self, text: &[u8]) {
        if let Some(rest) = text.strip_prefix(b"0.") {
            self.write(b".");
            self.write(rest);
        } else if let Some(rest) = text.strip_prefix(b"-0.") {
            self.write(b"-.");
            self.write(rest);
        } else {
            self.write(text);
        }
    }

    fn minify_function_parameters(&mut self, params: &FunctionParameters) {
        if !params.is_empty() {
            self.minify_value(params.value(0));
        }
        for index in 1..params.len() {
            match params.separator(index) {
                crate::value::Separator::Comma => self.write(b","),
                crate::value::Separator::Space => self.write(b" "),
            }
            self.minify_value(params.value(index));
        }
    }

    fn minify_unparsed_region(&mut self, region: &UnparsedRegion) {
        self.write(region.bytes_in_original_buffer());
    }
}

fn is_normal_identifier(value: &Value) -> bool {
    value.lexical_type() == ValueType::Ident && value.identifier().ident() == Ident::Normal
}

fn media_queries_equal(left: &MediaQueries, right: &MediaQueries) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .all(|(a, b)| media_query_equal(a, b))
}

fn media_query_equal(left: &MediaQuery, right: &MediaQuery) -> bool {
    left.qualifier() == right.qualifier()
        && left.media_type() == right.media_type()
        && left.expressions().len() == right.expressions().len()
        && left
            .expressions()
            .iter()
            .zip(right.expressions().iter())
            .all(|(a, b)| {
                a.name() == b.name()
                    && a.has_value() == b.has_value()
                    && (!a.has_value() || a.value() == b.value())
            })
}

/// Minifies CSS text, failing closed (`RewriteCss` shape).
#[must_use]
pub fn minify_css(input: &[u8]) -> Option<Vec<u8>> {
    let mut minifier = Minifier::new(false);
    if minifier.parse_stylesheet(input) {
        Some(minifier.finish().0)
    } else {
        None
    }
}

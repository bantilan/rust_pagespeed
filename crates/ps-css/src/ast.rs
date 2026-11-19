//! CSS AST nodes (`parser.h` data structures).
#![forbid(unsafe_code)]

use crate::media::MediaQueries;
use crate::property::{Prop, Property};
use crate::selector::Selectors;
use crate::unicode_text::UnicodeText;
use crate::value::Values;

// Error flags (`Parser::k*Error`).
pub const NO_ERROR: u64 = 0;
pub const UTF8_ERROR: u64 = 1 << 0;
pub const DECLARATION_ERROR: u64 = 1 << 1;
pub const SELECTOR_ERROR: u64 = 1 << 2;
pub const FUNCTION_ERROR: u64 = 1 << 3;
pub const MEDIA_ERROR: u64 = 1 << 4;
pub const COUNTER_ERROR: u64 = 1 << 5;
pub const HTML_COMMENT_ERROR: u64 = 1 << 6;
pub const VALUE_ERROR: u64 = 1 << 7;
pub const RULESET_ERROR: u64 = 1 << 8;
pub const SKIPPED_TOKEN_ERROR: u64 = 1 << 9;
pub const CHARSET_ERROR: u64 = 1 << 10;
pub const BLOCK_ERROR: u64 = 1 << 11;
pub const NUMBER_ERROR: u64 = 1 << 12;
pub const IMPORT_ERROR: u64 = 1 << 13;
pub const AT_RULE_ERROR: u64 = 1 << 14;
pub const CSS_COMMENT_ERROR: u64 = 1 << 15;

/// Error number from a flag (`ErrorNumber`): lowest set bit index.
#[must_use]
pub fn error_number(flag: u64) -> i32 {
    for bit in 0..64 {
        if flag & (1 << bit) != 0 {
            return bit;
        }
    }
    debug_assert!(false, "invalid error flag");
    -1
}

/// One remembered parse error (`ErrorInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    /// Error number (`error_num`).
    pub error_num: i32,
    /// Byte offset (`byte_offset`).
    pub byte_offset: usize,
    /// Full message with context (raw bytes).
    pub message: Vec<u8>,
}

/// A declaration: property + values + importance (`Declaration`).
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    property: Property,
    values: Values,
    important: bool,
    verbatim: Vec<u8>,
}

impl Declaration {
    /// Builds a declaration, taking values.
    #[must_use]
    pub fn new(property: Property, values: Values, important: bool) -> Declaration {
        Declaration {
            property,
            values,
            important,
            verbatim: Vec::new(),
        }
    }

    /// Builds a single-value declaration, copying the value.
    #[must_use]
    pub fn single(property: Property, value: &crate::value::Value, important: bool) -> Declaration {
        Declaration {
            property,
            values: vec![value.clone()],
            important,
            verbatim: Vec::new(),
        }
    }

    /// Dummy declaration holding verbatim text (`UNPARSEABLE`).
    #[must_use]
    pub fn unparseable(verbatim: &[u8]) -> Declaration {
        Declaration {
            property: Property::from_prop(Prop::Unparseable),
            values: Vec::new(),
            important: false,
            verbatim: verbatim.to_vec(),
        }
    }

    /// Property (`property()`).
    #[must_use]
    pub fn property(&self) -> &Property {
        &self.property
    }

    /// Values (`values()`).
    #[must_use]
    pub fn values(&self) -> &Values {
        &self.values
    }

    /// Mutable values (`mutable_values()`).
    pub fn values_mut(&mut self) -> &mut Values {
        &mut self.values
    }

    /// Importance (`IsImportant()`).
    #[must_use]
    pub fn is_important(&self) -> bool {
        self.important
    }

    /// Verbatim bytes (`bytes_in_original_buffer()`).
    #[must_use]
    pub fn bytes_in_original_buffer(&self) -> &[u8] {
        &self.verbatim
    }

    /// Property enum (`prop()`).
    #[must_use]
    pub fn prop(&self) -> Prop {
        self.property.prop()
    }

    /// Property text (`prop_text()`).
    #[must_use]
    pub fn prop_text(&self) -> Vec<u8> {
        self.property.prop_text()
    }

    /// Replaces the property (`set_property`).
    pub fn set_property(&mut self, property: Property) {
        self.property = property;
    }

    /// Replaces the values (`set_values`).
    pub fn set_values(&mut self, values: Values) {
        self.values = values;
    }

    /// Sets importance (`set_important`).
    pub fn set_important(&mut self, important: bool) {
        self.important = important;
    }

    /// Replaces verbatim bytes (`set_bytes_in_original_buffer`).
    pub fn set_bytes_in_original_buffer(&mut self, bytes: &[u8]) {
        self.verbatim = bytes.to_vec();
    }

    /// Debug serialization (`Declaration::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::value::{join_css_strings, values_to_css_string};
        if self.prop() == Prop::Unparseable {
            let mut out = b"/* Unparsed declaration: */ ".to_vec();
            out.extend_from_slice(&self.verbatim);
            return out;
        }
        let mut out = self.prop_text();
        out.extend_from_slice(b": ");
        match self.prop() {
            Prop::FontFamily => {
                out.extend_from_slice(&join_css_strings(
                    self.values.iter().map(crate::value::Value::to_css_string),
                    b",",
                ));
            }
            Prop::Font => {
                if self.values.len() == 1 {
                    out.extend_from_slice(&values_to_css_string(&self.values));
                } else if self.values.len() < 5 {
                    out.extend_from_slice(b"bad");
                } else {
                    for (index, value) in self.values.iter().enumerate() {
                        let text = value.to_css_string();
                        match index {
                            0..=2 => {
                                if text != b"normal" {
                                    out.extend_from_slice(&text);
                                    out.push(b' ');
                                }
                            }
                            3 => out.extend_from_slice(&text),
                            4 => {
                                if text != b"normal" {
                                    out.push(b'/');
                                    out.extend_from_slice(&text);
                                }
                            }
                            5 => {
                                out.push(b' ');
                                out.extend_from_slice(&text);
                            }
                            _ => {
                                out.push(b',');
                                out.extend_from_slice(&text);
                            }
                        }
                    }
                }
            }
            _ => {
                out.extend_from_slice(&values_to_css_string(&self.values));
            }
        }
        if self.important {
            out.extend_from_slice(b" !important");
        }
        out
    }
}

/// Debug serialization (`Declarations::ToString`).
#[must_use]
pub fn declarations_to_css_string(declarations: &Declarations) -> Vec<u8> {
    crate::value::join_css_strings(declarations.iter().map(Declaration::to_css_string), b"; ")
}

/// A declaration list (`Declarations`).
pub type Declarations = Vec<Declaration>;

/// Verbatim unparsed bytes (`UnparsedRegion`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnparsedRegion {
    verbatim: Vec<u8>,
}

impl UnparsedRegion {
    /// Wraps verbatim bytes.
    #[must_use]
    pub fn new(verbatim: &[u8]) -> UnparsedRegion {
        UnparsedRegion {
            verbatim: verbatim.to_vec(),
        }
    }

    /// Verbatim bytes (`bytes_in_original_buffer()`).
    #[must_use]
    pub fn bytes_in_original_buffer(&self) -> &[u8] {
        &self.verbatim
    }

    /// Replaces the bytes (`set_bytes_in_original_buffer`).
    pub fn set_bytes(&mut self, bytes: &[u8]) {
        self.verbatim = bytes.to_vec();
    }

    /// Debug serialization (`UnparsedRegion::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        let mut out = b"/* Unparsed region: */ ".to_vec();
        out.extend_from_slice(&self.verbatim);
        out
    }
}

/// Ruleset flavor (`Ruleset::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesetType {
    Ruleset,
    UnparsedRegion,
}

/// A ruleset or a dummy unparsed at-rule (`Ruleset`).
#[derive(Debug, Clone, PartialEq)]
pub struct Ruleset {
    kind: RulesetType,
    media_queries: MediaQueries,
    selectors: Selectors,
    declarations: Declarations,
    unparsed_region: Option<UnparsedRegion>,
}

impl Ruleset {
    /// Empty ruleset (`Ruleset()`).
    #[must_use]
    pub fn new() -> Ruleset {
        Ruleset {
            kind: RulesetType::Ruleset,
            media_queries: Vec::new(),
            selectors: Selectors::new(),
            declarations: Vec::new(),
            unparsed_region: None,
        }
    }

    /// Dummy ruleset holding an unparsed region.
    #[must_use]
    pub fn dummy_unparsed(region: UnparsedRegion) -> Ruleset {
        Ruleset {
            kind: RulesetType::UnparsedRegion,
            media_queries: Vec::new(),
            selectors: Selectors::new(),
            declarations: Vec::new(),
            unparsed_region: Some(region),
        }
    }

    /// Flavor (`type()`).
    #[must_use]
    pub fn kind(&self) -> RulesetType {
        self.kind
    }

    /// Media queries (`media_queries()`).
    #[must_use]
    pub fn media_queries(&self) -> &MediaQueries {
        &self.media_queries
    }

    /// Media query `i` (`media_query(i)`).
    #[must_use]
    pub fn media_query(&self, index: usize) -> &crate::media::MediaQuery {
        &self.media_queries[index]
    }

    /// Mutable media queries (`mutable_media_queries()`).
    pub fn media_queries_mut(&mut self) -> &mut MediaQueries {
        &mut self.media_queries
    }

    /// Replaces media queries (`set_media_queries`).
    pub fn set_media_queries(&mut self, queries: MediaQueries) {
        self.media_queries = queries;
    }

    /// Selectors (`selectors()`).
    #[must_use]
    pub fn selectors(&self) -> &Selectors {
        debug_assert_eq!(self.kind, RulesetType::Ruleset);
        &self.selectors
    }

    /// Selector `i` (`selector(i)`).
    #[must_use]
    pub fn selector(&self, index: usize) -> &crate::selector::Selector {
        debug_assert_eq!(self.kind, RulesetType::Ruleset);
        self.selectors.at(index)
    }

    /// Declarations (`declarations()`).
    #[must_use]
    pub fn declarations(&self) -> &Declarations {
        debug_assert_eq!(self.kind, RulesetType::Ruleset);
        &self.declarations
    }

    /// Declaration `i` (`declaration(i)`).
    #[must_use]
    pub fn declaration(&self, index: usize) -> &Declaration {
        debug_assert_eq!(self.kind, RulesetType::Ruleset);
        &self.declarations[index]
    }

    /// Mutable selectors (`mutable_selectors()`).
    pub fn selectors_mut(&mut self) -> &mut Selectors {
        debug_assert_eq!(self.kind, RulesetType::Ruleset);
        &mut self.selectors
    }

    /// Mutable declarations (`mutable_declarations()`).
    pub fn declarations_mut(&mut self) -> &mut Declarations {
        debug_assert_eq!(self.kind, RulesetType::Ruleset);
        &mut self.declarations
    }

    /// Replaces selectors (`set_selectors`).
    pub fn set_selectors(&mut self, selectors: Selectors) {
        debug_assert_eq!(self.kind, RulesetType::Ruleset);
        self.selectors = selectors;
    }

    /// Replaces declarations (`set_declarations`).
    pub fn set_declarations(&mut self, declarations: Declarations) {
        debug_assert_eq!(self.kind, RulesetType::Ruleset);
        self.declarations = declarations;
    }

    /// Unparsed region (`unparsed_region()`).
    #[must_use]
    pub fn unparsed_region(&self) -> &UnparsedRegion {
        assert_eq!(self.kind, RulesetType::UnparsedRegion);
        self.unparsed_region
            .as_ref()
            .expect("unparsed ruleset without region")
    }

    /// Mutable unparsed region (`mutable_unparsed_region()`).
    pub fn unparsed_region_mut(&mut self) -> &mut UnparsedRegion {
        debug_assert_eq!(self.kind, RulesetType::UnparsedRegion);
        self.unparsed_region
            .as_mut()
            .expect("unparsed ruleset without region")
    }

    /// Debug serialization (`Ruleset::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::media::media_queries_to_css_string;
        let mut out = Vec::new();
        if !self.media_queries.is_empty() {
            out.extend_from_slice(b"@media ");
            out.extend_from_slice(&media_queries_to_css_string(&self.media_queries));
            out.extend_from_slice(b" { ");
        }
        match self.kind {
            RulesetType::Ruleset => {
                out.extend_from_slice(&self.selectors.to_css_string());
                out.extend_from_slice(b" {");
                out.extend_from_slice(&declarations_to_css_string(&self.declarations));
                out.push(b'}');
            }
            RulesetType::UnparsedRegion => {
                out = self
                    .unparsed_region
                    .as_ref()
                    .expect("region")
                    .to_css_string();
            }
        }
        if !self.media_queries.is_empty() {
            out.extend_from_slice(b" }");
        }
        out
    }
}

/// Debug serialization (`Rulesets` joined).
#[must_use]
pub fn rulesets_to_css_string(rulesets: &Rulesets) -> Vec<u8> {
    crate::value::join_css_strings(rulesets.iter().map(Ruleset::to_css_string), b"\n")
}

impl Default for Ruleset {
    fn default() -> Ruleset {
        Ruleset::new()
    }
}

/// A ruleset list (`Rulesets`).
pub type Rulesets = Vec<Ruleset>;

/// A charset list (`Charsets`).
pub type Charsets = Vec<UnicodeText>;

/// An `@import` rule (`Import`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Import {
    media_queries: MediaQueries,
    link: UnicodeText,
}

impl Import {
    /// Empty import (`Import()`).
    #[must_use]
    pub fn new() -> Import {
        Import::default()
    }

    /// Media queries (`media_queries()`).
    #[must_use]
    pub fn media_queries(&self) -> &MediaQueries {
        &self.media_queries
    }

    /// Link (`link()`).
    #[must_use]
    pub fn link(&self) -> &UnicodeText {
        &self.link
    }

    /// Replaces media queries (`set_media_queries`).
    pub fn set_media_queries(&mut self, queries: MediaQueries) {
        self.media_queries = queries;
    }

    /// Sets the link (`set_link`).
    pub fn set_link(&mut self, link: UnicodeText) {
        self.link = link;
    }

    /// Debug serialization (`Import::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::escapes::escape_unicode_url;
        use crate::media::media_queries_to_css_string;
        let mut out = b"@import url(\"".to_vec();
        out.extend_from_slice(&escape_unicode_url(&self.link));
        out.extend_from_slice(b"\") ");
        out.extend_from_slice(&media_queries_to_css_string(&self.media_queries));
        out.extend_from_slice(b";");
        out
    }
}

/// Debug serialization (`Charsets::ToString`).
#[must_use]
pub fn charsets_to_css_string(charsets: &Charsets) -> Vec<u8> {
    use crate::escapes::escape_unicode_string;
    let mut out = Vec::new();
    for charset in charsets {
        out.extend_from_slice(b"@charset \"");
        out.extend_from_slice(&escape_unicode_string(charset));
        out.extend_from_slice(b"\";");
    }
    out
}

/// An import list (`Imports`).
pub type Imports = Vec<Import>;

/// An `@font-face` rule (`FontFace`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FontFace {
    media_queries: MediaQueries,
    declarations: Declarations,
}

impl FontFace {
    /// Empty font face (`FontFace()`).
    #[must_use]
    pub fn new() -> FontFace {
        FontFace::default()
    }

    /// Media queries (`media_queries()`).
    #[must_use]
    pub fn media_queries(&self) -> &MediaQueries {
        &self.media_queries
    }

    /// Declarations (`declarations()`).
    #[must_use]
    pub fn declarations(&self) -> &Declarations {
        &self.declarations
    }

    /// Replaces media queries (`set_media_queries`).
    pub fn set_media_queries(&mut self, queries: MediaQueries) {
        self.media_queries = queries;
    }

    /// Replaces declarations (`set_declarations`).
    pub fn set_declarations(&mut self, declarations: Declarations) {
        self.declarations = declarations;
    }

    /// Mutable media queries (`mutable_media_queries()`).
    pub fn media_queries_mut(&mut self) -> &mut MediaQueries {
        &mut self.media_queries
    }

    /// Mutable declarations (`mutable_declarations()`).
    pub fn declarations_mut(&mut self) -> &mut Declarations {
        &mut self.declarations
    }

    /// Debug serialization (`FontFace::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::media::media_queries_to_css_string;
        let mut out = Vec::new();
        if !self.media_queries.is_empty() {
            out.extend_from_slice(b"@media ");
            out.extend_from_slice(&media_queries_to_css_string(&self.media_queries));
            out.extend_from_slice(b" { ");
        }
        out.extend_from_slice(b"@font-face { ");
        out.extend_from_slice(&declarations_to_css_string(&self.declarations));
        out.extend_from_slice(b" }");
        if !self.media_queries.is_empty() {
            out.extend_from_slice(b" }");
        }
        out
    }
}

/// A font-face list (`FontFaces`).
pub type FontFaces = Vec<FontFace>;

/// Stylesheet flavor (`Stylesheet::StylesheetType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StylesheetType {
    #[default]
    Author,
    User,
    System,
}

/// A stylesheet (`Stylesheet`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stylesheet {
    kind: StylesheetType,
    charsets: Charsets,
    imports: Imports,
    font_faces: FontFaces,
    rulesets: Rulesets,
}

impl Stylesheet {
    /// Empty author stylesheet (`Stylesheet()`).
    #[must_use]
    pub fn new() -> Stylesheet {
        Stylesheet::default()
    }

    /// Flavor (`type()`).
    #[must_use]
    pub fn kind(&self) -> StylesheetType {
        self.kind
    }

    /// Charsets (`charsets()`).
    #[must_use]
    pub fn charsets(&self) -> &Charsets {
        &self.charsets
    }

    /// Imports (`imports()`).
    #[must_use]
    pub fn imports(&self) -> &Imports {
        &self.imports
    }

    /// Font faces (`font_faces()`).
    #[must_use]
    pub fn font_faces(&self) -> &FontFaces {
        &self.font_faces
    }

    /// Rulesets (`rulesets()`).
    #[must_use]
    pub fn rulesets(&self) -> &Rulesets {
        &self.rulesets
    }

    /// Charset `i` (`charset(i)`).
    #[must_use]
    pub fn charset(&self, index: usize) -> &UnicodeText {
        &self.charsets[index]
    }

    /// Import `i` (`import(i)`).
    #[must_use]
    pub fn import(&self, index: usize) -> &Import {
        &self.imports[index]
    }

    /// Font face `i` (`font_face(i)`).
    #[must_use]
    pub fn font_face(&self, index: usize) -> &FontFace {
        &self.font_faces[index]
    }

    /// Ruleset `i` (`ruleset(i)`).
    #[must_use]
    pub fn ruleset(&self, index: usize) -> &Ruleset {
        &self.rulesets[index]
    }

    /// Sets the flavor (`set_type`).
    pub fn set_kind(&mut self, kind: StylesheetType) {
        self.kind = kind;
    }

    /// Mutable charsets (`mutable_charsets()`).
    pub fn charsets_mut(&mut self) -> &mut Charsets {
        &mut self.charsets
    }

    /// Mutable imports (`mutable_imports()`).
    pub fn imports_mut(&mut self) -> &mut Imports {
        &mut self.imports
    }

    /// Mutable font faces (`mutable_font_faces()`).
    pub fn font_faces_mut(&mut self) -> &mut FontFaces {
        &mut self.font_faces
    }

    /// Mutable rulesets (`mutable_rulesets()`).
    pub fn rulesets_mut(&mut self) -> &mut Rulesets {
        &mut self.rulesets
    }

    /// Debug serialization (`Stylesheet::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::value::join_css_strings;
        let kind = match self.kind {
            StylesheetType::Author => "AUTHOR",
            StylesheetType::User => "USER",
            StylesheetType::System => "SYSTEM",
        };
        let mut out = format!("/* {kind} */\n").into_bytes();
        out.extend_from_slice(&charsets_to_css_string(&self.charsets));
        out.push(b'\n');
        out.extend_from_slice(&join_css_strings(
            self.imports.iter().map(Import::to_css_string),
            b"\n",
        ));
        out.push(b'\n');
        out.extend_from_slice(&join_css_strings(
            self.font_faces.iter().map(FontFace::to_css_string),
            b"\n",
        ));
        out.push(b'\n');
        out.extend_from_slice(&join_css_strings(
            self.rulesets.iter().map(Ruleset::to_css_string),
            b"\n",
        ));
        out.push(b'\n');
        out
    }
}

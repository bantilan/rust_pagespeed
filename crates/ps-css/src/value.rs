//! CSS values (`value.h`, `value.cc`).
//!
//! [`Value`] is a tagged union of lexical units; [`FunctionParameters`]
//! pairs values with their comma/space separators.
#![forbid(unsafe_code)]

use crate::color::HtmlColor;
use crate::identifier::{Ident, Identifier};
use crate::unicode_text::UnicodeText;

/// Lexical unit types (`Value::ValueType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Number,
    Uri,
    Function,
    Rect,
    Color,
    String,
    Ident,
    Comma,
    Unknown,
    Default,
}

/// Dimension units (`Value::Unit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Em,
    Ex,
    Px,
    Cm,
    Mm,
    In,
    Pt,
    Pc,
    Deg,
    Rad,
    Grad,
    Ms,
    S,
    Hz,
    Khz,
    Percent,
    Vh,
    Vm,
    Vw,
    Other,
    NoUnit,
}

const UNIT_TEXT: &[&str] = &[
    "em", "ex", "px", "cm", "mm", "in", "pt", "pc", "deg", "rad", "grad", "ms", "s", "hz", "khz",
    "%", "vh", "vm", "vw", "OTHER", "",
];

impl Unit {
    /// Unit text (`TextFromUnit`).
    #[must_use]
    pub fn text(self) -> &'static str {
        UNIT_TEXT[self as usize]
    }

    /// Classifies unit text (`UnitFromText`): ASCII case-insensitive.
    /// NOTE: length-4 inputs compare only the first 3 bytes against
    /// "grad" (upstream quirk, ported faithfully).
    #[must_use]
    pub fn from_text(text: &[u8]) -> Unit {
        let lower = text.to_ascii_lowercase();
        match lower.len() {
            0 => Unit::NoUnit,
            1 => match lower[0] {
                b'%' => Unit::Percent,
                b's' => Unit::S,
                _ => Unit::Other,
            },
            2 => match (lower[0], lower[1]) {
                (b'v', b'h') => Unit::Vh,
                (b'v', b'm') => Unit::Vm,
                (b'v', b'w') => Unit::Vw,
                (b'e', b'm') => Unit::Em,
                (b'e', b'x') => Unit::Ex,
                (b'p', b'x') => Unit::Px,
                (b'p', b't') => Unit::Pt,
                (b'p', b'c') => Unit::Pc,
                (b'c', b'm') => Unit::Cm,
                (b'm', b'm') => Unit::Mm,
                (b'm', b's') => Unit::Ms,
                (b'i', b'n') => Unit::In,
                (b'h', b'z') => Unit::Hz,
                _ => Unit::Other,
            },
            3 => match lower.as_slice() {
                b"deg" => Unit::Deg,
                b"rad" => Unit::Rad,
                b"khz" => Unit::Khz,
                _ => Unit::Other,
            },
            4 => {
                if lower[..3] == b"gra"[..] {
                    Unit::Grad
                } else {
                    Unit::Other
                }
            }
            _ => Unit::Other,
        }
    }
}

/// Parameter separators (`FunctionParameters::Separator`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    Comma,
    Space,
}

/// Function parameter list with separators (`FunctionParameters`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FunctionParameters {
    separators: Vec<Separator>,
    values: Vec<Value>,
}

impl FunctionParameters {
    /// Empty list.
    #[must_use]
    pub fn new() -> FunctionParameters {
        FunctionParameters::default()
    }

    /// Adds a value with its preceding separator (`AddSepValue`).
    pub fn add(&mut self, separator: Separator, value: Value) {
        self.separators.push(separator);
        self.values.push(value);
        debug_assert_eq!(self.separators.len(), self.values.len());
    }

    /// Separator before value `i` (`separator(i)`).
    #[must_use]
    pub fn separator(&self, index: usize) -> Separator {
        self.separators[index]
    }

    /// All values (`values()`).
    #[must_use]
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Value `i` (`value(i)`).
    #[must_use]
    pub fn value(&self, index: usize) -> &Value {
        &self.values[index]
    }

    /// Mutable value `i`.
    #[must_use]
    pub fn value_mut(&mut self, index: usize) -> &mut Value {
        &mut self.values[index]
    }

    /// Number of values (`size()`).
    #[must_use]
    pub fn len(&self) -> usize {
        debug_assert_eq!(self.separators.len(), self.values.len());
        self.values.len()
    }

    /// Whether there are no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Deep equality (`Equals`).
    #[must_use]
    pub fn equals(&self, other: &FunctionParameters) -> bool {
        self == other
    }

    /// Debug serialization (`FunctionParameters::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.values.is_empty() {
            out.extend_from_slice(&self.values[0].to_css_string());
        }
        for index in 1..self.values.len() {
            match self.separators[index] {
                Separator::Comma => out.extend_from_slice(b", "),
                Separator::Space => out.push(b' '),
            }
            out.extend_from_slice(&self.values[index].to_css_string());
        }
        out
    }
}

/// A CSS value / lexical unit (`Value`).
#[derive(Debug, Clone)]
pub enum Value {
    /// `COMMA`, `UNKNOWN`, or `DEFAULT`.
    Token(ValueType),
    /// Number with unit; `OTHER` units keep raw text.
    Number {
        num: f64,
        unit: Unit,
        unit_text: UnicodeText,
        verbatim: Vec<u8>,
    },
    /// `URI` or `STRING` with string content.
    Text {
        kind: ValueType,
        text: UnicodeText,
        verbatim: Vec<u8>,
    },
    /// Identifier.
    Ident(Identifier),
    /// `RECT` with parameters.
    Rect(FunctionParameters),
    /// Named function with parameters.
    Function {
        name: UnicodeText,
        params: FunctionParameters,
    },
    /// Color.
    Color(HtmlColor),
}

impl Value {
    /// `COMMA`/`UNKNOWN`/`DEFAULT` token.
    #[must_use]
    pub fn token(kind: ValueType) -> Value {
        debug_assert!(matches!(
            kind,
            ValueType::Comma | ValueType::Default | ValueType::Unknown
        ));
        Value::Token(kind)
    }

    /// Number with a known unit.
    #[must_use]
    pub fn number(num: f64, unit: Unit) -> Value {
        debug_assert_ne!(unit, Unit::Other);
        Value::Number {
            num,
            unit,
            unit_text: UnicodeText::new(),
            verbatim: Vec::new(),
        }
    }

    /// Number with a textual unit (classified, keeping raw text for
    /// `OTHER`).
    #[must_use]
    pub fn number_with_unit(num: f64, unit: &UnicodeText) -> Value {
        let kind = Unit::from_text(unit.as_bytes());
        let unit_text = if kind == Unit::Other {
            unit.clone()
        } else {
            UnicodeText::new()
        };
        Value::Number {
            num,
            unit: kind,
            unit_text,
            verbatim: Vec::new(),
        }
    }

    /// `URI` or `STRING` value.
    #[must_use]
    pub fn text(kind: ValueType, text: UnicodeText) -> Value {
        debug_assert!(matches!(kind, ValueType::Uri | ValueType::String));
        Value::Text {
            kind,
            text,
            verbatim: Vec::new(),
        }
    }

    /// Identifier value.
    #[must_use]
    pub fn ident(identifier: Identifier) -> Value {
        Value::Ident(identifier)
    }

    /// `RECT` value.
    #[must_use]
    pub fn rect(params: FunctionParameters) -> Value {
        Value::Rect(params)
    }

    /// Named function value.
    #[must_use]
    pub fn function(name: UnicodeText, params: FunctionParameters) -> Value {
        Value::Function { name, params }
    }

    /// Color value.
    #[must_use]
    pub fn color(color: HtmlColor) -> Value {
        Value::Color(color)
    }

    /// Lexical unit type (`GetLexicalUnitType`).
    #[must_use]
    pub fn lexical_type(&self) -> ValueType {
        match self {
            Value::Token(kind) => *kind,
            Value::Text { kind, .. } => *kind,
            Value::Number { .. } => ValueType::Number,
            Value::Ident(_) => ValueType::Ident,
            Value::Rect(_) => ValueType::Rect,
            Value::Function { .. } => ValueType::Function,
            Value::Color(_) => ValueType::Color,
        }
    }

    /// Unit text (`GetDimensionUnitText`).
    #[must_use]
    pub fn dimension_unit_text(&self) -> Vec<u8> {
        match self {
            Value::Number {
                unit, unit_text, ..
            } => {
                if *unit == Unit::Other {
                    unit_text.as_bytes().to_vec()
                } else {
                    unit.text().as_bytes().to_vec()
                }
            }
            _ => {
                debug_assert!(false, "dimension of non-number");
                Vec::new()
            }
        }
    }

    /// Unit (`GetDimension`).
    #[must_use]
    pub fn dimension(&self) -> Unit {
        match self {
            Value::Number { unit, .. } => *unit,
            _ => {
                debug_assert!(false, "dimension of non-number");
                Unit::NoUnit
            }
        }
    }

    /// Saturating integer conversion (`GetIntegerValue`): NaN maps to
    /// 0, out-of-range values clamp (Rust's `as` cast matches the C++
    /// saturation exactly, including truncation toward zero).
    #[must_use]
    pub fn integer_value(&self) -> i32 {
        match self {
            #[allow(clippy::cast_possible_truncation)]
            Value::Number { num, .. } => *num as i32,
            _ => {
                debug_assert!(false, "integer of non-number");
                0
            }
        }
    }

    /// Float value (`GetFloatValue`).
    #[must_use]
    pub fn float_value(&self) -> f64 {
        match self {
            Value::Number { num, .. } => *num,
            _ => {
                debug_assert!(false, "float of non-number");
                0.0
            }
        }
    }

    /// Function parameters, ignoring separators (`GetParameters`).
    #[must_use]
    pub fn parameters(&self) -> &[Value] {
        match self {
            Value::Function { params, .. } | Value::Rect(params) => params.values(),
            _ => {
                debug_assert!(false, "parameters of non-function");
                &[]
            }
        }
    }

    /// Function parameters with separators
    /// (`GetParametersWithSeparators`).
    #[must_use]
    pub fn parameters_with_separators(&self) -> &FunctionParameters {
        static EMPTY: FunctionParameters = FunctionParameters {
            separators: Vec::new(),
            values: Vec::new(),
        };
        match self {
            Value::Function { params, .. } | Value::Rect(params) => params,
            _ => {
                debug_assert!(false, "parameters of non-function");
                &EMPTY
            }
        }
    }

    /// Mutable parameters.
    pub fn parameters_mut(&mut self) -> Option<&mut FunctionParameters> {
        match self {
            Value::Function { params, .. } | Value::Rect(params) => Some(params),
            _ => None,
        }
    }

    /// Function name (`GetFunctionName`).
    #[must_use]
    pub fn function_name(&self) -> &UnicodeText {
        static EMPTY: UnicodeText = UnicodeText::new_const_empty();
        match self {
            Value::Function { name, .. } => name,
            _ => {
                debug_assert!(false, "name of non-function");
                &EMPTY
            }
        }
    }

    /// String content (`GetStringValue`).
    #[must_use]
    pub fn string_value(&self) -> &UnicodeText {
        static EMPTY: UnicodeText = UnicodeText::new_const_empty();
        match self {
            Value::Text { text, .. } => text,
            _ => {
                debug_assert!(false, "string of non-string");
                &EMPTY
            }
        }
    }

    /// Mutable string content.
    pub fn string_value_mut(&mut self) -> Option<&mut UnicodeText> {
        match self {
            Value::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    /// Identifier text (`GetIdentifierText`).
    #[must_use]
    pub fn identifier_text(&self) -> UnicodeText {
        match self {
            Value::Ident(identifier) => identifier.ident_text(),
            _ => {
                debug_assert!(false, "ident text of non-ident");
                UnicodeText::new()
            }
        }
    }

    /// Identifier (`GetIdentifier`).
    #[must_use]
    pub fn identifier(&self) -> &Identifier {
        static UNKNOWN: Identifier = Identifier::const_unknown();
        match self {
            Value::Ident(identifier) => identifier,
            _ => {
                debug_assert!(false, "ident of non-ident");
                &UNKNOWN
            }
        }
    }

    /// Color value (`GetColorValue`).
    #[must_use]
    pub fn color_value(&self) -> &HtmlColor {
        static UNDEFINED: HtmlColor = HtmlColor::const_undefined();
        match self {
            Value::Color(color) => color,
            _ => {
                debug_assert!(false, "color of non-color");
                &UNDEFINED
            }
        }
    }

    /// Deep equality (`Equals`). NOTE: `COMMA` falls into the C++
    /// `default:` arm (`LOG(FATAL)`); it is unreachable because commas
    /// inside functions use separators and top-level commas split
    /// font-family lists structurally. The port compares them by type.
    #[must_use]
    pub fn equals(&self, other: &Value) -> bool {
        if self.lexical_type() != other.lexical_type() {
            return false;
        }
        match (self, other) {
            (Value::Token(_), Value::Token(_)) => true,
            (
                Value::Number { num, unit, .. },
                Value::Number {
                    num: other_num,
                    unit: other_unit,
                    ..
                },
            ) => unit == other_unit && num == other_num,
            (
                Value::Text { text, .. },
                Value::Text {
                    text: other_text, ..
                },
            ) => text == other_text,
            (Value::Ident(left), Value::Ident(right)) => {
                if left.ident() != right.ident() {
                    return false;
                }
                if left.ident() == Ident::Other {
                    return left.ident_text() == right.ident_text();
                }
                true
            }
            (Value::Color(left), Value::Color(right)) => left.equals(right),
            (
                Value::Function { name, params },
                Value::Function {
                    name: other_name,
                    params: other_params,
                },
            ) => name == other_name && params.equals(other_params),
            (Value::Rect(params), Value::Rect(other_params)) => params.equals(other_params),
            _ => false,
        }
    }
}

impl Value {
    /// Verbatim bytes (`bytes_in_original_buffer()`).
    #[must_use]
    pub fn bytes_in_original_buffer(&self) -> &[u8] {
        match self {
            Value::Number { verbatim, .. } | Value::Text { verbatim, .. } => verbatim,
            _ => &[],
        }
    }

    /// Stores verbatim bytes (`set_bytes_in_original_buffer`).
    pub fn set_bytes_in_original_buffer(&mut self, bytes: &[u8]) {
        match self {
            Value::Number { verbatim, .. } | Value::Text { verbatim, .. } => {
                *verbatim = bytes.to_vec();
            }
            _ => {}
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        self.equals(other)
    }
}

impl Value {
    /// Debug serialization (`Value::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::escapes::{
            escape_unicode_identifier, escape_unicode_string, escape_unicode_url,
        };
        use crate::string_util::format_g;
        match self.lexical_type() {
            ValueType::Number => {
                let mut out = format_g(self.float_value(), 6).into_bytes();
                out.extend_from_slice(&self.dimension_unit_text());
                out
            }
            ValueType::Uri => {
                let mut out = b"url(".to_vec();
                out.extend_from_slice(&escape_unicode_url(self.string_value()));
                out.push(b')');
                out
            }
            ValueType::Function => {
                let mut out = escape_unicode_identifier(self.function_name());
                out.push(b'(');
                out.extend_from_slice(&self.parameters_with_separators().to_css_string());
                out.push(b')');
                out
            }
            ValueType::Rect => {
                let mut out = b"rect(".to_vec();
                out.extend_from_slice(&self.parameters_with_separators().to_css_string());
                out.push(b')');
                out
            }
            ValueType::Color => {
                if self.color_value().is_defined() {
                    self.color_value().to_hex_string().into_bytes()
                } else {
                    b"bad".to_vec()
                }
            }
            ValueType::String => {
                let mut out = b"\"".to_vec();
                out.extend_from_slice(&escape_unicode_string(self.string_value()));
                out.push(b'"');
                out
            }
            ValueType::Ident => escape_unicode_identifier(&self.identifier_text()),
            ValueType::Comma => b",".to_vec(),
            ValueType::Unknown => b"UNKNOWN".to_vec(),
            ValueType::Default => Vec::new(),
        }
    }
}

/// Joins value strings (`Values::ToString`).
#[must_use]
pub fn values_to_css_string(values: &Values) -> Vec<u8> {
    join_css_strings(values.iter().map(Value::to_css_string), b" ")
}

/// Joins byte strings with a delimiter (`JoinElementStrings`).
pub(crate) fn join_css_strings(parts: impl Iterator<Item = Vec<u8>>, delimiter: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, part) in parts.enumerate() {
        if index > 0 {
            out.extend_from_slice(delimiter);
        }
        out.extend_from_slice(&part);
    }
    out
}

/// A value list (`Values`).
pub type Values = Vec<Value>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_vectors() {
        assert_eq!(Unit::from_text(b""), Unit::NoUnit);
        assert_eq!(Unit::from_text(b"%"), Unit::Percent);
        assert_eq!(Unit::from_text(b"S"), Unit::S);
        assert_eq!(Unit::from_text(b"PX"), Unit::Px);
        assert_eq!(Unit::from_text(b"Vh"), Unit::Vh);
        assert_eq!(Unit::from_text(b"Deg"), Unit::Deg);
        assert_eq!(Unit::from_text(b"grad"), Unit::Grad);
        // Length-4 compares only 3 bytes (upstream quirk).
        assert_eq!(Unit::from_text(b"gram"), Unit::Grad);
        assert_eq!(Unit::from_text(b"xx"), Unit::Other);
        assert_eq!(Unit::from_text(b"degrees"), Unit::Other);
        assert_eq!(Unit::Px.text(), "px");
        assert_eq!(Unit::NoUnit.text(), "");
        assert_eq!(Unit::Other.text(), "OTHER");
    }

    #[test]
    fn integer_saturation() {
        // Port of OutOfRangeNumericValueSaturates.
        assert_eq!(Value::number(1e11, Unit::Px).integer_value(), i32::MAX);
        assert_eq!(Value::number(-1e11, Unit::Px).integer_value(), i32::MIN);
        assert_eq!(Value::number(12.0, Unit::Px).integer_value(), 12);
        assert_eq!(Value::number(12.9, Unit::Px).integer_value(), 12);
        assert_eq!(Value::number(f64::NAN, Unit::Px).integer_value(), 0);
    }

    #[test]
    fn equality_vectors() {
        assert!(Value::number(1.0, Unit::Px).equals(&Value::number(1.0, Unit::Px)));
        assert!(!Value::number(1.0, Unit::Px).equals(&Value::number(1.0, Unit::Em)));
        assert!(Value::token(ValueType::Comma).equals(&Value::token(ValueType::Comma)));
    }
}

//! JavaScript keywords (`js_keywords.{h,gperf}`).
//!
//! Keyword types with value/regex-precedence/reserved flags.
#![forbid(unsafe_code)]

/// Keyword classification flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeywordFlag {
    /// Ordinary keyword.
    None,
    /// Literal value (`true`/`false`/`null`).
    IsValue,
    /// May precede a regex literal.
    CanPrecedeRegEx,
    /// Reserved (non-strict): error to use.
    ReservedNonStrict,
    /// Reserved (strict): treated as identifier (no strict-mode errors).
    ReservedStrict,
}

/// Token/keyword types (`JsKeywords::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenType {
    Null,
    True,
    False,
    Break,
    Case,
    Catch,
    Const,
    Default,
    Finally,
    For,
    Instanceof,
    New,
    Var,
    Continue,
    Function,
    Return,
    Void,
    Delete,
    If,
    This,
    Do,
    While,
    Else,
    In,
    Switch,
    Throw,
    Try,
    Typeof,
    With,
    Debugger,
    Class,
    Enum,
    Export,
    Extends,
    Import,
    Super,
    Implements,
    Interface,
    Let,
    Package,
    Private,
    Protected,
    Public,
    Static,
    Yield,
    NotAKeyword,
    Comment,
    Whitespace,
    LineSeparator,
    SemiInsert,
    Regex,
    StringLiteral,
    Number,
    Operator,
    Identifier,
    EndOfInput,
    Error,
}

impl TokenType {
    /// Whether this is a real keyword (`IsAKeyword`: before the sentinel).
    #[must_use]
    pub const fn is_keyword(self) -> bool {
        !matches!(
            self,
            TokenType::NotAKeyword
                | TokenType::Comment
                | TokenType::Whitespace
                | TokenType::LineSeparator
                | TokenType::SemiInsert
                | TokenType::Regex
                | TokenType::StringLiteral
                | TokenType::Number
                | TokenType::Operator
                | TokenType::Identifier
                | TokenType::EndOfInput
                | TokenType::Error
        )
    }
}

/// Keyword table (gperf order): name, type, flag.
const KEYWORDS: &[(&str, TokenType, KeywordFlag)] = &[
    ("false", TokenType::False, KeywordFlag::IsValue),
    ("null", TokenType::Null, KeywordFlag::IsValue),
    ("true", TokenType::True, KeywordFlag::IsValue),
    ("case", TokenType::Case, KeywordFlag::CanPrecedeRegEx),
    ("delete", TokenType::Delete, KeywordFlag::CanPrecedeRegEx),
    ("do", TokenType::Do, KeywordFlag::CanPrecedeRegEx),
    ("in", TokenType::In, KeywordFlag::CanPrecedeRegEx),
    (
        "instanceof",
        TokenType::Instanceof,
        KeywordFlag::CanPrecedeRegEx,
    ),
    ("new", TokenType::New, KeywordFlag::CanPrecedeRegEx),
    ("return", TokenType::Return, KeywordFlag::CanPrecedeRegEx),
    ("throw", TokenType::Throw, KeywordFlag::CanPrecedeRegEx),
    ("typeof", TokenType::Typeof, KeywordFlag::CanPrecedeRegEx),
    ("void", TokenType::Void, KeywordFlag::CanPrecedeRegEx),
    ("break", TokenType::Break, KeywordFlag::None),
    ("catch", TokenType::Catch, KeywordFlag::None),
    ("const", TokenType::Const, KeywordFlag::None),
    ("continue", TokenType::Continue, KeywordFlag::None),
    ("debugger", TokenType::Debugger, KeywordFlag::None),
    ("default", TokenType::Default, KeywordFlag::None),
    ("else", TokenType::Else, KeywordFlag::None),
    ("finally", TokenType::Finally, KeywordFlag::None),
    ("for", TokenType::For, KeywordFlag::None),
    ("function", TokenType::Function, KeywordFlag::None),
    ("if", TokenType::If, KeywordFlag::None),
    ("switch", TokenType::Switch, KeywordFlag::None),
    ("this", TokenType::This, KeywordFlag::None),
    ("try", TokenType::Try, KeywordFlag::None),
    ("while", TokenType::While, KeywordFlag::None),
    ("with", TokenType::With, KeywordFlag::None),
    ("var", TokenType::Var, KeywordFlag::None),
    ("class", TokenType::Class, KeywordFlag::ReservedNonStrict),
    ("enum", TokenType::Enum, KeywordFlag::ReservedNonStrict),
    ("export", TokenType::Export, KeywordFlag::ReservedNonStrict),
    (
        "extends",
        TokenType::Extends,
        KeywordFlag::ReservedNonStrict,
    ),
    ("import", TokenType::Import, KeywordFlag::ReservedNonStrict),
    ("super", TokenType::Super, KeywordFlag::ReservedNonStrict),
    (
        "implements",
        TokenType::Implements,
        KeywordFlag::ReservedStrict,
    ),
    (
        "interface",
        TokenType::Interface,
        KeywordFlag::ReservedStrict,
    ),
    ("let", TokenType::Let, KeywordFlag::ReservedStrict),
    ("package", TokenType::Package, KeywordFlag::ReservedStrict),
    ("private", TokenType::Private, KeywordFlag::ReservedStrict),
    (
        "protected",
        TokenType::Protected,
        KeywordFlag::ReservedStrict,
    ),
    ("public", TokenType::Public, KeywordFlag::ReservedStrict),
    ("static", TokenType::Static, KeywordFlag::ReservedStrict),
    ("yield", TokenType::Yield, KeywordFlag::ReservedStrict),
];

/// Looks up a word (`JsKeywords::Lookup`).
#[must_use]
pub fn lookup(word: &str) -> (TokenType, KeywordFlag) {
    // Linear scan over 45 entries; the gperf hash is an optimization.
    for (name, keyword, flag) in KEYWORDS {
        if *name == word {
            return (*keyword, *flag);
        }
    }
    (TokenType::NotAKeyword, KeywordFlag::None)
}

/// Whether a keyword may precede a regex literal
/// (`CanKeywordPrecedeRegEx`).
#[must_use]
pub fn can_keyword_precede_regex(name: &str) -> bool {
    let (keyword, flag) = lookup(name);
    keyword.is_keyword() && flag == KeywordFlag::CanPrecedeRegEx
}

/// Number of keywords (`num_keywords`).
#[must_use]
pub const fn num_keywords() -> usize {
    KEYWORDS.len()
}

/// Iterates all keyword names (`JsKeywords::Iterator`).
pub fn keyword_names() -> impl Iterator<Item = &'static str> {
    KEYWORDS.iter().map(|(name, _, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_shape() {
        assert_eq!(num_keywords(), 45);
        assert_eq!(
            lookup("return"),
            (TokenType::Return, KeywordFlag::CanPrecedeRegEx)
        );
        assert_eq!(lookup("while"), (TokenType::While, KeywordFlag::None));
        assert_eq!(lookup("bogus"), (TokenType::NotAKeyword, KeywordFlag::None));
        assert!(can_keyword_precede_regex("throw"));
        assert!(!can_keyword_precede_regex("while"));
        assert!(!can_keyword_precede_regex("bogus"));
        assert!(TokenType::If.is_keyword());
        assert!(!TokenType::Identifier.is_keyword());
    }
}

//! Ported C++ tokenizer suite.
//!
//! Mechanically translated from `js_tokenizer_test.cc` (see
//! `translate_tokenizer_tests.py` in this directory); do not hand-edit
//! expectations. Any divergence from the C++ baseline is a port bug.

use ps_js::keywords::TokenType;
use ps_js::tokenizer::{Tokenizer, TokenizerPatterns};

struct Harness<'a> {
    tokenizer: Tokenizer<'a>,
}

impl<'a> Harness<'a> {
    fn new(patterns: &'a TokenizerPatterns, input: &'a [u8]) -> Harness<'a> {
        Harness {
            tokenizer: Tokenizer::new(patterns, input),
        }
    }

    fn token(&mut self, expected_type: TokenType, expected_token: &[u8]) {
        let (actual_type, span) = self.tokenizer.next_token();
        let actual_token = self.tokenizer.text(span);
        assert_eq!((expected_type, expected_token), (actual_type, actual_token));
        assert!(!self.tokenizer.has_error());
    }

    fn stack(&self, expected: &str) {
        assert_eq!(self.tokenizer.parse_stack_for_test(), expected);
    }

    fn end(&mut self) {
        self.token(TokenType::EndOfInput, b"");
        self.stack("");
    }

    fn error(&mut self, expected_token: &[u8]) {
        let (actual_type, span) = self.tokenizer.next_token();
        let actual_token = self.tokenizer.text(span);
        assert_eq!(
            (TokenType::Error, expected_token),
            (actual_type, actual_token)
        );
        assert!(self.tokenizer.has_error());
    }
}

fn tokenize_to_error(patterns: &TokenizerPatterns, input: &[u8]) -> TokenType {
    let mut tokenizer = Tokenizer::new(patterns, input);
    loop {
        let (kind, _) = tokenizer.next_token();
        if matches!(kind, TokenType::Error | TokenType::EndOfInput) {
            return kind;
        }
    }
}

fn tokenize_file(name: &str, contents: &[u8]) {
    let patterns = TokenizerPatterns::new();
    let mut tokenizer = Tokenizer::new(&patterns, contents);
    let mut output = Vec::with_capacity(contents.len());
    loop {
        let (kind, span) = tokenizer.next_token();
        if kind == TokenType::EndOfInput {
            break;
        }
        assert!(
            !tokenizer.has_error(),
            "error tokenizing {name} at {:?} (stack {})",
            &contents[span.0..span.1.min(span.0 + 150)],
            tokenizer.parse_stack_for_test(),
        );
        output.extend_from_slice(tokenizer.text(span));
    }
    assert_eq!(output, contents);
}

#[test]
fn empty_input() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"");
    t.end();
}

#[test]
fn blocks() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"if (foo){\n}else if(bar){\n}else baz;");
    t.stack("Start");
    t.token(TokenType::If, b"if");
    t.stack("Start BkKwd");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"(");
    t.stack("Start BkKwd (");
    t.token(TokenType::Identifier, b"foo");
    t.stack("Start BkKwd ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start BkHdr {");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Operator, b"}");
    t.stack("Start");
    t.token(TokenType::Else, b"else");
    t.stack("Start BkHdr");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::If, b"if");
    t.stack("Start BkHdr BkKwd");
    t.token(TokenType::Operator, b"(");
    t.stack("Start BkHdr BkKwd (");
    t.token(TokenType::Identifier, b"bar");
    t.stack("Start BkHdr BkKwd ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start BkHdr {");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Operator, b"}");
    t.stack("Start");
    t.token(TokenType::Else, b"else");
    t.stack("Start BkHdr");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"baz");
    t.stack("Start BkHdr Expr");
    t.token(TokenType::Operator, b";");
    t.stack("Start");
    t.end();
}

#[test]
fn functions() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"function foo(){return 1}\nbar=function(){return 2};\n(function(){window=5})();",
    );
    t.stack("Start");
    t.token(TokenType::Function, b"function");
    t.stack("Start BkKwd");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"foo");
    t.stack("Start BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start BkHdr {");
    t.token(TokenType::Return, b"return");
    t.stack("Start BkHdr { RetTh");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Number, b"1");
    t.stack("Start BkHdr { RetTh Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"bar");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Function, b"function");
    t.stack("Start Expr Oper BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Operator, b")");
    t.stack("Start Expr Oper BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start Expr Oper BkHdr {");
    t.token(TokenType::Return, b"return");
    t.stack("Start Expr Oper BkHdr { RetTh");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Number, b"2");
    t.stack("Start Expr Oper BkHdr { RetTh Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b";");
    t.stack("Start");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Operator, b"(");
    t.stack("Start (");
    t.token(TokenType::Function, b"function");
    t.stack("Start ( BkKwd");
    t.token(TokenType::Operator, b"(");
    t.stack("Start ( BkKwd (");
    t.token(TokenType::Operator, b")");
    t.stack("Start ( BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start ( BkHdr {");
    t.token(TokenType::Identifier, b"window");
    t.stack("Start ( BkHdr { Expr");
    t.token(TokenType::Operator, b"=");
    t.stack("Start ( BkHdr { Expr Oper");
    t.token(TokenType::Number, b"5");
    t.stack("Start ( BkHdr { Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b"(");
    t.stack("Start Expr (");
    t.token(TokenType::Operator, b")");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b";");
    t.stack("Start");
    t.end();
}

#[test]
fn for_loop() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"loop:for(var x=0;x<5;++x){\n  break loop;\n}");
    t.token(TokenType::Identifier, b"loop");
    t.token(TokenType::Operator, b":");
    t.token(TokenType::For, b"for");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Var, b"var");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Number, b"0");
    t.stack("Start BkKwd ( Other Expr");
    t.token(TokenType::Operator, b";");
    t.stack("Start BkKwd (");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"<");
    t.token(TokenType::Number, b"5");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::Operator, b"++");
    t.token(TokenType::Identifier, b"x");
    t.stack("Start BkKwd ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start BkHdr {");
    t.token(TokenType::LineSeparator, b"\n  ");
    t.token(TokenType::Break, b"break");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"loop");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Operator, b"}");
    t.end();
}

#[test]
fn keywords() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"(function(){\n var\n   x=typeof null\n const y=void false\n delete x\n if(this instanceof String){\n  debugger\n  for(z in this)\n   continue\n  do break;while(true)\n  switch(y){\n   case 0:\n   default:\n    try{\n     with(this){\n      throw new Object()\n     }\n    }catch(e){\n     return\n    }finally{}\n  }\n }else return\n})");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Function, b"function");
    t.stack("Start ( BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::LineSeparator, b"\n ");
    t.stack("Start ( BkHdr {");
    t.token(TokenType::Var, b"var");
    t.stack("Start ( BkHdr { Other");
    t.token(TokenType::LineSeparator, b"\n   ");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Typeof, b"typeof");
    t.stack("Start ( BkHdr { Other Expr Oper");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Null, b"null");
    t.stack("Start ( BkHdr { Other Expr");
    t.token(TokenType::SemiInsert, b"\n ");
    t.stack("Start ( BkHdr {");
    t.token(TokenType::Const, b"const");
    t.stack("Start ( BkHdr { Other");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Void, b"void");
    t.stack("Start ( BkHdr { Other Expr Oper");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::False, b"false");
    t.stack("Start ( BkHdr { Other Expr");
    t.token(TokenType::SemiInsert, b"\n ");
    t.stack("Start ( BkHdr {");
    t.token(TokenType::Delete, b"delete");
    t.stack("Start ( BkHdr { Oper");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::SemiInsert, b"\n ");
    t.stack("Start ( BkHdr {");
    t.token(TokenType::If, b"if");
    t.stack("Start ( BkHdr { BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::This, b"this");
    t.stack("Start ( BkHdr { BkKwd ( Expr");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Instanceof, b"instanceof");
    t.stack("Start ( BkHdr { BkKwd ( Expr Oper");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"String");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::LineSeparator, b"\n  ");
    t.stack("Start ( BkHdr { BkHdr {");
    t.token(TokenType::Debugger, b"debugger");
    t.stack("Start ( BkHdr { BkHdr { Jump");
    t.token(TokenType::SemiInsert, b"\n  ");
    t.stack("Start ( BkHdr { BkHdr {");
    t.token(TokenType::For, b"for");
    t.stack("Start ( BkHdr { BkHdr { BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Identifier, b"z");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::In, b"in");
    t.stack("Start ( BkHdr { BkHdr { BkKwd ( Expr Oper");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::This, b"this");
    t.stack("Start ( BkHdr { BkHdr { BkKwd ( Expr");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::LineSeparator, b"\n   ");
    t.stack("Start ( BkHdr { BkHdr { BkHdr");
    t.token(TokenType::Continue, b"continue");
    t.stack("Start ( BkHdr { BkHdr { BkHdr Jump");
    t.token(TokenType::SemiInsert, b"\n  ");
    t.stack("Start ( BkHdr { BkHdr {");
    t.token(TokenType::Do, b"do");
    t.stack("Start ( BkHdr { BkHdr { BkHdr");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Break, b"break");
    t.stack("Start ( BkHdr { BkHdr { BkHdr Jump");
    t.token(TokenType::Operator, b";");
    t.stack("Start ( BkHdr { BkHdr {");
    t.token(TokenType::While, b"while");
    t.stack("Start ( BkHdr { BkHdr { BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::True, b"true");
    t.token(TokenType::Operator, b")");
    t.stack("Start ( BkHdr { BkHdr { BkHdr");
    t.token(TokenType::LineSeparator, b"\n  ");
    t.token(TokenType::Switch, b"switch");
    t.stack("Start ( BkHdr { BkHdr { BkHdr BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b")");
    t.stack("Start ( BkHdr { BkHdr { BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start ( BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::LineSeparator, b"\n   ");
    t.token(TokenType::Case, b"case");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { Oper");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Number, b"0");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start ( BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::LineSeparator, b"\n   ");
    t.token(TokenType::Default, b"default");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { Other");
    t.token(TokenType::Operator, b":");
    t.stack("Start ( BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::LineSeparator, b"\n    ");
    t.token(TokenType::Try, b"try");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::LineSeparator, b"\n     ");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::With, b"with");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr { BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::This, b"this");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr { BkKwd ( Expr");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::LineSeparator, b"\n      ");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::Throw, b"throw");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr { BkHdr { RetTh");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::New, b"new");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr { BkHdr { RetTh Oper");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"Object");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr { BkHdr { RetTh Expr");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::LineSeparator, b"\n     ");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr { BkHdr { RetTh Expr");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::LineSeparator, b"\n    ");
    t.token(TokenType::Operator, b"}");
    t.stack("Start ( BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::Catch, b"catch");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkKwd");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Identifier, b"e");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::Operator, b"{");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::LineSeparator, b"\n     ");
    t.token(TokenType::Return, b"return");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr { RetTh");
    t.token(TokenType::LineSeparator, b"\n    ");
    t.token(TokenType::Operator, b"}");
    t.stack("Start ( BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::Finally, b"finally");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start ( BkHdr { BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::LineSeparator, b"\n  ");
    t.stack("Start ( BkHdr { BkHdr { BkHdr {");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::LineSeparator, b"\n ");
    t.token(TokenType::Operator, b"}");
    t.stack("Start ( BkHdr {");
    t.token(TokenType::Else, b"else");
    t.stack("Start ( BkHdr { BkHdr");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Return, b"return");
    t.stack("Start ( BkHdr { BkHdr RetTh");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Operator, b"}");
    t.stack("Start ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start Expr");
    t.end();
    t.stack("");
}

#[test]
fn strict_mode_reserved_words() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"var implements,interface,let,package\n   ,private,protected,public,static,yield;",
    );
    t.token(TokenType::Var, b"var");
    t.stack("Start Other");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"implements");
    t.stack("Start Other Expr");
    t.token(TokenType::Operator, b",");
    t.stack("Start Other");
    t.token(TokenType::Identifier, b"interface");
    t.token(TokenType::Operator, b",");
    t.token(TokenType::Identifier, b"let");
    t.token(TokenType::Operator, b",");
    t.token(TokenType::Identifier, b"package");
    t.token(TokenType::LineSeparator, b"\n   ");
    t.token(TokenType::Operator, b",");
    t.token(TokenType::Identifier, b"private");
    t.token(TokenType::Operator, b",");
    t.token(TokenType::Identifier, b"protected");
    t.token(TokenType::Operator, b",");
    t.token(TokenType::Identifier, b"public");
    t.token(TokenType::Operator, b",");
    t.token(TokenType::Identifier, b"static");
    t.token(TokenType::Operator, b",");
    t.token(TokenType::Identifier, b"yield");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn reserved_words_as_field_names1() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"document.true=true;");
    t.token(TokenType::Identifier, b"document");
    t.token(TokenType::Operator, b".");
    t.token(TokenType::Identifier, b"true");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::True, b"true");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn reserved_words_as_field_names2() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"x={if:false,class:3};");
    t.token(TokenType::Identifier, b"x");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b"=");
    t.stack("Start Expr Oper");
    t.token(TokenType::Operator, b"{");
    t.stack("Start Expr Oper {");
    t.token(TokenType::Identifier, b"if");
    t.stack("Start Expr Oper { Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start Expr Oper { Oper");
    t.token(TokenType::False, b"false");
    t.stack("Start Expr Oper { Expr");
    t.token(TokenType::Operator, b",");
    t.stack("Start Expr Oper {");
    t.token(TokenType::Identifier, b"class");
    t.stack("Start Expr Oper { Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start Expr Oper { Oper");
    t.token(TokenType::Number, b"3");
    t.stack("Start Expr Oper { Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b";");
    t.stack("Start");
    t.end();
}

#[test]
fn reserved_words_as_label_names() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"{if:false,class:3};");
    t.token(TokenType::Operator, b"{");
    t.stack("Start {");
    t.token(TokenType::If, b"if");
    t.stack("Start { BkKwd");
    t.error(b":false,class:3};");
}

#[test]
fn comments() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"x=1; // hello\ny=/* world */2;\nz<!--sgml\nfoo-->bar;\n --> sgml\n",
    );
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Number, b"1");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Comment, b"// hello");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Comment, b"/* world */");
    t.token(TokenType::Number, b"2");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"z");
    t.token(TokenType::Comment, b"<!--sgml");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Operator, b"--");
    t.token(TokenType::Operator, b">");
    t.token(TokenType::Identifier, b"bar");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n ");
    t.token(TokenType::Comment, b"--> sgml");
    t.token(TokenType::LineSeparator, b"\n");
    t.end();
}

#[test]
fn line_comments() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"1//foo\r+2//bar\xe2\x80\xa8+3//baz\xe2\x80\xa94//quux\xe2\x81\x9f+5//hello, world!\n6",
    );
    t.token(TokenType::Number, b"1");
    t.token(TokenType::Comment, b"//foo");
    t.token(TokenType::LineSeparator, b"\r");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Number, b"2");
    t.token(TokenType::Comment, b"//bar");
    t.token(TokenType::LineSeparator, b"\xe2\x80\xa8");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Number, b"3");
    t.token(TokenType::Comment, b"//baz");
    t.token(TokenType::SemiInsert, b"\xe2\x80\xa9");
    t.token(TokenType::Number, b"4");
    t.token(TokenType::Comment, b"//quux\xe2\x81\x9f+5//hello, world!");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Number, b"6");
    t.end();
}

#[test]
fn sgml_comments() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"4+<!--foo\n3+\n/*foo*/ --> foo\nx --> foo\n");
    t.token(TokenType::Number, b"4");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Comment, b"<!--foo");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Number, b"3");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Comment, b"/*foo*/");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Comment, b"--> foo");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"--");
    t.token(TokenType::Operator, b">");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::LineSeparator, b"\n");
    t.end();
}

#[test]
fn whitespace() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"\xef\xbb\xbfa\x0c=\t1\x0b+ 2\r5\xc2\xa0\xe2\x81\x9f+\xe1\x9a\x807\xe2\x80\xa9;\xe2\x80\xa8");
    t.token(TokenType::Whitespace, b"\xef\xbb\xbf");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Whitespace, b"\x0c");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Whitespace, b"\t");
    t.token(TokenType::Number, b"1");
    t.token(TokenType::Whitespace, b"\x0b");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Number, b"2");
    t.token(TokenType::SemiInsert, b"\r");
    t.token(TokenType::Number, b"5");
    t.token(TokenType::Whitespace, b"\xc2\xa0\xe2\x81\x9f");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Whitespace, b"\xe1\x9a\x80");
    t.token(TokenType::Number, b"7");
    t.token(TokenType::LineSeparator, b"\xe2\x80\xa9");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\xe2\x80\xa8");
    t.end();
}

#[test]
fn numeric_literals() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"var  foo =\n  .85 - 0191.e+3+0171.toString();\n1..property",
    );
    t.token(TokenType::Var, b"var");
    t.token(TokenType::Whitespace, b"  ");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::LineSeparator, b"\n  ");
    t.token(TokenType::Number, b".85");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"-");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Number, b"0191.e+3");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Number, b"0171");
    t.token(TokenType::Operator, b".");
    t.token(TokenType::Identifier, b"toString");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Number, b"1.");
    t.token(TokenType::Operator, b".");
    t.token(TokenType::Identifier, b"property");
    t.end();
    t.end();
}

#[test]
fn regex_literals() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"foo=/quux/;\nbar=/quux/ig;\nbaz=a/quux/ig;");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Regex, b"/quux/");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"bar");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Regex, b"/quux/ig");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"baz");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Identifier, b"quux");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Identifier, b"ig");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn regex_vs_slash() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"if(a*(b+c)/d<e)/d<e/.exec('\\'');\nelse/x/.exec(\"\");",
    );
    t.stack("Start");
    t.token(TokenType::If, b"if");
    t.stack("Start BkKwd");
    t.token(TokenType::Operator, b"(");
    t.stack("Start BkKwd (");
    t.token(TokenType::Identifier, b"a");
    t.stack("Start BkKwd ( Expr");
    t.token(TokenType::Operator, b"*");
    t.stack("Start BkKwd ( Expr Oper");
    t.token(TokenType::Operator, b"(");
    t.stack("Start BkKwd ( Expr Oper (");
    t.token(TokenType::Identifier, b"b");
    t.stack("Start BkKwd ( Expr Oper ( Expr");
    t.token(TokenType::Operator, b"+");
    t.stack("Start BkKwd ( Expr Oper ( Expr Oper");
    t.token(TokenType::Identifier, b"c");
    t.stack("Start BkKwd ( Expr Oper ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkKwd ( Expr");
    t.token(TokenType::Operator, b"/");
    t.stack("Start BkKwd ( Expr Oper");
    t.token(TokenType::Identifier, b"d");
    t.stack("Start BkKwd ( Expr");
    t.token(TokenType::Operator, b"<");
    t.stack("Start BkKwd ( Expr Oper");
    t.token(TokenType::Identifier, b"e");
    t.stack("Start BkKwd ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkHdr");
    t.token(TokenType::Regex, b"/d<e/");
    t.stack("Start BkHdr Expr");
    t.token(TokenType::Operator, b".");
    t.stack("Start BkHdr Expr .");
    t.token(TokenType::Identifier, b"exec");
    t.stack("Start BkHdr Expr");
    t.token(TokenType::Operator, b"(");
    t.stack("Start BkHdr Expr (");
    t.token(TokenType::StringLiteral, b"'\\''");
    t.stack("Start BkHdr Expr ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkHdr Expr");
    t.token(TokenType::Operator, b";");
    t.stack("Start");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Else, b"else");
    t.stack("Start BkHdr");
    t.token(TokenType::Regex, b"/x/");
    t.stack("Start BkHdr Expr");
    t.token(TokenType::Operator, b".");
    t.stack("Start BkHdr Expr .");
    t.token(TokenType::Identifier, b"exec");
    t.stack("Start BkHdr Expr");
    t.token(TokenType::Operator, b"(");
    t.stack("Start BkHdr Expr (");
    t.token(TokenType::StringLiteral, b"\"\"");
    t.stack("Start BkHdr Expr ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkHdr Expr");
    t.token(TokenType::Operator, b";");
    t.stack("Start");
    t.end();
    t.stack("");
}

#[test]
fn operators1() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"foo /= bar+++baz;\na=b==c&&d===e;\na>>>=b>=c?d>>_:$;",
    );
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"/=");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"bar");
    t.token(TokenType::Operator, b"++");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Identifier, b"baz");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::Operator, b"==");
    t.token(TokenType::Identifier, b"c");
    t.token(TokenType::Operator, b"&&");
    t.token(TokenType::Identifier, b"d");
    t.token(TokenType::Operator, b"===");
    t.token(TokenType::Identifier, b"e");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b">>>=");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::Operator, b">=");
    t.token(TokenType::Identifier, b"c");
    t.token(TokenType::Operator, b"?");
    t.token(TokenType::Identifier, b"d");
    t.token(TokenType::Operator, b">>");
    t.token(TokenType::Identifier, b"_");
    t.token(TokenType::Operator, b":");
    t.token(TokenType::Identifier, b"$");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn operators2() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a+++b\na+ ++b\na+ +b\na---b\na-++b\n!!b\n");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"++");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"++");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"--");
    t.token(TokenType::Operator, b"-");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"-");
    t.token(TokenType::Operator, b"++");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Operator, b"!");
    t.token(TokenType::Operator, b"!");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::LineSeparator, b"\n");
    t.end();
}

#[test]
fn colons1() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"switch(x){default:{}/x/i}\nfoobar:{}/x/i;\na?b:{}/x/i",
    );
    t.token(TokenType::Switch, b"switch");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Default, b"default");
    t.stack("Start BkHdr { Other");
    t.token(TokenType::Operator, b":");
    t.stack("Start BkHdr {");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::Regex, b"/x/i");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"foobar");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::Regex, b"/x/i");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"?");
    t.token(TokenType::Identifier, b"b");
    t.stack("Start Expr ? Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start Expr Oper");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Identifier, b"i");
    t.end();
}

#[test]
fn colons2() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"{foo:{}/x/i}\ny={foo:{}/x/i}\nz=function(){foo:{}/x/i}",
    );
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Operator, b":");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::Regex, b"/x/i");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Operator, b":");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Identifier, b"i");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"z");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Function, b"function");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Operator, b":");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::Regex, b"/x/i");
    t.token(TokenType::Operator, b"}");
    t.end();
}

#[test]
fn object_literals_array() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"[{a:42},{a:32}]");
    t.token(TokenType::Operator, b"[");
    t.stack("Start [");
    t.token(TokenType::Operator, b"{");
    t.stack("Start [ {");
    t.token(TokenType::Identifier, b"a");
    t.stack("Start [ { Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start [ { Oper");
    t.token(TokenType::Number, b"42");
    t.stack("Start [ { Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start [ Expr");
    t.token(TokenType::Operator, b",");
    t.stack("Start [");
    t.token(TokenType::Operator, b"{");
    t.stack("Start [ {");
    t.token(TokenType::Identifier, b"a");
    t.stack("Start [ { Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start [ { Oper");
    t.token(TokenType::Number, b"32");
    t.stack("Start [ { Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start [ Expr");
    t.token(TokenType::Operator, b"]");
    t.stack("Start Expr");
    t.end();
}

#[test]
fn trailing_commas() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"x={a:1,}\ny=[,,]");
    t.token(TokenType::Identifier, b"x");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b"=");
    t.stack("Start Expr Oper");
    t.token(TokenType::Operator, b"{");
    t.stack("Start Expr Oper {");
    t.token(TokenType::Identifier, b"a");
    t.stack("Start Expr Oper { Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start Expr Oper { Oper");
    t.token(TokenType::Number, b"1");
    t.stack("Start Expr Oper { Expr");
    t.token(TokenType::Operator, b",");
    t.stack("Start Expr Oper {");
    t.token(TokenType::Operator, b"}");
    t.stack("Start Expr");
    t.token(TokenType::SemiInsert, b"\n");
    t.stack("Start");
    t.token(TokenType::Identifier, b"y");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b"=");
    t.stack("Start Expr Oper");
    t.token(TokenType::Operator, b"[");
    t.stack("Start Expr Oper [");
    t.token(TokenType::Operator, b",");
    t.stack("Start Expr Oper [");
    t.token(TokenType::Operator, b",");
    t.stack("Start Expr Oper [");
    t.token(TokenType::Operator, b"]");
    t.stack("Start Expr");
    t.end();
}

#[test]
fn comma_operator() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"x=(y++,y*5)");
    t.token(TokenType::Identifier, b"x");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b"=");
    t.stack("Start Expr Oper");
    t.token(TokenType::Operator, b"(");
    t.stack("Start Expr Oper (");
    t.token(TokenType::Identifier, b"y");
    t.stack("Start Expr Oper ( Expr");
    t.token(TokenType::Operator, b"++");
    t.stack("Start Expr Oper ( Expr");
    t.token(TokenType::Operator, b",");
    t.stack("Start Expr Oper ( Expr Oper");
    t.token(TokenType::Identifier, b"y");
    t.stack("Start Expr Oper ( Expr");
    t.token(TokenType::Operator, b"*");
    t.stack("Start Expr Oper ( Expr Oper");
    t.token(TokenType::Number, b"5");
    t.stack("Start Expr Oper ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start Expr");
    t.end();
}

#[test]
fn object_literal_regex_literal() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"{foo:1} / x/i;\nx={foo:1} / x/i;");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Operator, b":");
    t.token(TokenType::Number, b"1");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Regex, b"/ x/i");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Operator, b"{");
    t.token(TokenType::Identifier, b"foo");
    t.token(TokenType::Operator, b":");
    t.token(TokenType::Number, b"1");
    t.token(TokenType::Operator, b"}");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Identifier, b"i");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn empty_block_regex_literal() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"if(true){{}/foo/}");
    t.token(TokenType::If, b"if");
    t.stack("Start BkKwd");
    t.token(TokenType::Operator, b"(");
    t.stack("Start BkKwd (");
    t.token(TokenType::True, b"true");
    t.stack("Start BkKwd ( Expr");
    t.token(TokenType::Operator, b")");
    t.stack("Start BkHdr");
    t.token(TokenType::Operator, b"{");
    t.stack("Start BkHdr {");
    t.token(TokenType::Operator, b"{");
    t.stack("Start BkHdr { {");
    t.token(TokenType::Operator, b"}");
    t.stack("Start BkHdr {");
    t.token(TokenType::Regex, b"/foo/");
    t.stack("Start BkHdr { Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start");
    t.end();
}

#[test]
fn tricky_regex_literal() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"var x=a[0] / b /i;\nvar y=a[0]+/ b /i;");
    t.token(TokenType::Var, b"var");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"[");
    t.token(TokenType::Number, b"0");
    t.token(TokenType::Operator, b"]");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Identifier, b"i");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Var, b"var");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"[");
    t.token(TokenType::Number, b"0");
    t.token(TokenType::Operator, b"]");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Regex, b"/ b /i");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn regex_literals_with_brackets() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"/http:\\/\\/[^/]+\\// / /z[\\]/ ]/");
    t.token(TokenType::Regex, b"/http:\\/\\/[^/]+\\//");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"/");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Regex, b"/z[\\]/ ]/");
    t.end();
}

#[test]
fn return_regex() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"return / x /g;\nreturn/#.+/.test(\n'#24' );");
    t.token(TokenType::Return, b"return");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Regex, b"/ x /g");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Return, b"return");
    t.token(TokenType::Regex, b"/#.+/");
    t.token(TokenType::Operator, b".");
    t.token(TokenType::Identifier, b"test");
    t.token(TokenType::Operator, b"(");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::StringLiteral, b"'#24'");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b")");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn return_regex2() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"return / x /g;");
    t.token(TokenType::Return, b"return");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Regex, b"/ x /g");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn throw_regex() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"throw / x /g;");
    t.token(TokenType::Throw, b"throw");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Regex, b"/ x /g");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn unicode_regex_flags() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"/\xe2\x98\x83/\xe0\xb2\xa0_\xe0\xb2\xa0\xe2\x80\xa9;",
    );
    t.token(TokenType::Regex, b"/\xe2\x98\x83/\xe0\xb2\xa0_\xe0\xb2\xa0");
    t.token(TokenType::LineSeparator, b"\xe2\x80\xa9");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn semicolon_insertion1() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"3\n// foo\n--> foo\n-5\n// bar\n6");
    t.token(TokenType::Number, b"3");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Comment, b"// foo");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Comment, b"--> foo");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Operator, b"-");
    t.token(TokenType::Number, b"5");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Comment, b"// bar");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Number, b"6");
    t.end();
}

#[test]
fn semicolon_insertion2() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"w\n++\nx\ny++\nz");
    t.token(TokenType::Identifier, b"w");
    t.stack("Start Expr");
    t.token(TokenType::SemiInsert, b"\n");
    t.stack("Start");
    t.token(TokenType::Operator, b"++");
    t.stack("Start Oper");
    t.token(TokenType::LineSeparator, b"\n");
    t.stack("Start Oper");
    t.token(TokenType::Identifier, b"x");
    t.stack("Start Expr");
    t.token(TokenType::SemiInsert, b"\n");
    t.stack("Start");
    t.token(TokenType::Identifier, b"y");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b"++");
    t.stack("Start Expr");
    t.token(TokenType::SemiInsert, b"\n");
    t.stack("Start");
    t.token(TokenType::Identifier, b"z");
    t.stack("Start Expr");
    t.end();
}

#[test]
fn semicolon_insertion3() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"x\nin\xe1\x9a\x80y;\nx\nin\\u0063\ny;\nx\ninstanceof\ny;\nx\ninstanceof\xe0\xb2\xa0_\ny;",
    );
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::In, b"in");
    t.token(TokenType::Whitespace, b"\xe1\x9a\x80");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"in\\u0063");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Instanceof, b"instanceof");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"instanceof\xe0\xb2\xa0_");
    t.token(TokenType::SemiInsert, b"\n");
    t.token(TokenType::Identifier, b"y");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn semicolon_insertion4() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"x={}\n{debugger}");
    t.token(TokenType::Identifier, b"x");
    t.stack("Start Expr");
    t.token(TokenType::Operator, b"=");
    t.stack("Start Expr Oper");
    t.token(TokenType::Operator, b"{");
    t.stack("Start Expr Oper {");
    t.token(TokenType::Operator, b"}");
    t.stack("Start Expr");
    t.token(TokenType::SemiInsert, b"\n");
    t.stack("Start");
    t.token(TokenType::Operator, b"{");
    t.stack("Start {");
    t.token(TokenType::Debugger, b"debugger");
    t.stack("Start { Jump");
    t.token(TokenType::Operator, b"}");
    t.stack("Start");
    t.end();
}

#[test]
fn unclosed_block_comment() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"debugger; /* foo");
    t.token(TokenType::Debugger, b"debugger");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::Whitespace, b" ");
    t.error(b"/* foo");
}

#[test]
fn unclosed_regex_literal() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"var bar=/quux;");
    t.stack("Start");
    t.token(TokenType::Var, b"var");
    t.stack("Start Other");
    t.token(TokenType::Whitespace, b" ");
    t.stack("Start Other");
    t.token(TokenType::Identifier, b"bar");
    t.stack("Start Other Expr");
    t.token(TokenType::Operator, b"=");
    t.stack("Start Other Expr Oper");
    t.error(b"/quux;");
    t.stack("Start Other Expr Oper");
}

#[test]
fn linebreak_in_regex() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"x=/foo\nquux/;");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"=");
    t.error(b"/foo\nquux/;");
    t = Harness::new(&patterns, b"x=/foo\xe2\x98\x83quux/+/foo\xe2\x80\xa9quux/;");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Regex, b"/foo\xe2\x98\x83quux/");
    t.token(TokenType::Operator, b"+");
    t.error(b"/foo\xe2\x80\xa9quux/;");
    t = Harness::new(&patterns, b"x=/foo\\\nquux/;");
    t.token(TokenType::Identifier, b"x");
    t.token(TokenType::Operator, b"=");
    t.error(b"/foo\\\nquux/;");
}

#[test]
fn linebreak_in_string_literal() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"'foo\\\nquux'+\"foo\\\r\nquux\"+\"foo\\\n\rquux\"+'foo\\\n\nquux';",
    );
    t.token(TokenType::StringLiteral, b"'foo\\\nquux'");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::StringLiteral, b"\"foo\\\r\nquux\"");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::StringLiteral, b"\"foo\\\n\rquux\"");
    t.token(TokenType::Operator, b"+");
    t.error(b"'foo\\\n\nquux';");
    t = Harness::new(
        &patterns,
        b"'foo\xe2\x98\x83quux'+'foo\\\xe2\x80\xa8quux'+'foo\xe2\x80\xa8quux';",
    );
    t.token(TokenType::StringLiteral, b"'foo\xe2\x98\x83quux'");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::StringLiteral, b"'foo\\\xe2\x80\xa8quux'");
    t.token(TokenType::Operator, b"+");
    t.error(b"'foo\xe2\x80\xa8quux';");
}

#[test]
fn escaped_quotes_in_string_literal1() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'foo\\\\\\'bar';");
    t.token(TokenType::StringLiteral, b"'foo\\\\\\'bar'");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn escaped_quotes_in_string_literal2() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"\"baz\"+'foo\\\\\\'ba\"r';");
    t.token(TokenType::StringLiteral, b"\"baz\"");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::StringLiteral, b"'foo\\\\\\'ba\"r'");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn escaped_quotes_in_string_literal3() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'b\\\\az'+'foo\\'bar';");
    t.token(TokenType::StringLiteral, b"'b\\\\az'");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::StringLiteral, b"'foo\\'bar'");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn escaped_quotes_in_string_literal4() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'b\\'az'+'foobar';");
    t.token(TokenType::StringLiteral, b"'b\\'az'");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::StringLiteral, b"'foobar'");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn escaped_quotes_in_string_literal5() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"\"f\xffoo\\\\\\\"bar\";");
    t.token(TokenType::StringLiteral, b"\"f\xffoo\\\\\\\"bar\"");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn unclosed_string_literal() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"bar='quux;");
    t.token(TokenType::Identifier, b"bar");
    t.token(TokenType::Operator, b"=");
    t.error(b"'quux;");
}

#[test]
fn unclosed_string_literal_ending_in_escaped_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"bar='quux\\'");
    t.token(TokenType::Identifier, b"bar");
    t.token(TokenType::Operator, b"=");
    t.error(b"'quux\\'");
}

#[test]
fn unclosed_double_quoted_string_ending_in_escaped_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"bar=\"quux\\\"");
    t.token(TokenType::Identifier, b"bar");
    t.token(TokenType::Operator, b"=");
    t.error(b"\"quux\\\"");
}

#[test]
fn unclosed_string_literal_three_backslashes_before_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'quux\\\\\\'");
    t.error(b"'quux\\\\\\'");
}

#[test]
fn unclosed_string_escaped_quote_then_more_input() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a='b\\';c");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"=");
    t.error(b"'b\\';c");
}

#[test]
fn unclosed_string_escaped_quote_after_line_continuation() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'a\\\n\\'");
    t.error(b"'a\\\n\\'");
}

#[test]
fn closed_string_even_backslashes_before_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'a\\\\'");
    t.token(TokenType::StringLiteral, b"'a\\\\'");
    t.end();
    t = Harness::new(&patterns, b"\"a\\\\\\\\\"");
    t.token(TokenType::StringLiteral, b"\"a\\\\\\\\\"");
    t.end();
}

#[test]
fn closed_string_escaped_quote_inside() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'a\\'b' + \"c\\\"d\"");
    t.token(TokenType::StringLiteral, b"'a\\'b'");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::StringLiteral, b"\"c\\\"d\"");
    t.end();
}

#[test]
fn closed_string_line_continuation_before_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'a\\\n'");
    t.token(TokenType::StringLiteral, b"'a\\\n'");
    t.end();
    t = Harness::new(&patterns, b"'a\\\r\n'");
    t.token(TokenType::StringLiteral, b"'a\\\r\n'");
    t.end();
}

#[test]
fn string_boundary_cases() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"''");
    t.token(TokenType::StringLiteral, b"''");
    t.end();
    t = Harness::new(&patterns, b"'\\\\'");
    t.token(TokenType::StringLiteral, b"'\\\\'");
    t.end();
    t = Harness::new(&patterns, b"'\\''");
    t.token(TokenType::StringLiteral, b"'\\''");
    t.end();
    t = Harness::new(&patterns, b"'\\'");
    t.error(b"'\\'");
}

#[test]
fn escaped_quote_then_second_string_on_same_line() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'a\\' + 'b'");
    t.token(TokenType::StringLiteral, b"'a\\' + '");
    t.token(TokenType::Identifier, b"b");
    t.error(b"'");
    t = Harness::new(&patterns, b"\"a\\\" , \"b\"");
    t.token(TokenType::StringLiteral, b"\"a\\\" , \"");
    t.token(TokenType::Identifier, b"b");
    t.error(b"\"");
}

#[test]
fn invalid_utf8_around_closing_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'\xff\\\\'");
    t.token(TokenType::StringLiteral, b"'\xff\\\\'");
    t.end();
    t = Harness::new(&patterns, b"'\\\\\xff'");
    t.token(TokenType::StringLiteral, b"'\\\\\xff'");
    t.end();
    t = Harness::new(&patterns, b"'\xc3\\'");
    t.error(b"'\xc3\\'");
}

#[test]
fn shift_jis_trail_byte_backslash_before_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"var s='\x83\\';");
    t.token(TokenType::Var, b"var");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"s");
    t.token(TokenType::Operator, b"=");
    t.error(b"'\x83\\';");
}

#[test]
fn closed_string_letters_and_escapes_before_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'A'");
    t.token(TokenType::StringLiteral, b"'A'");
    t.end();
    t = Harness::new(&patterns, b"'\\x41'");
    t.token(TokenType::StringLiteral, b"'\\x41'");
    t.end();
    assert_eq!(tokenize_to_error(&patterns, b"'\\x41\\'"), TokenType::Error);
}

#[test]
fn closed_string_linebreak_continuation_pairs_before_quote() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"'a\\\n\r'");
    t.token(TokenType::StringLiteral, b"'a\\\n\r'");
    t.end();
    t = Harness::new(&patterns, b"'a\\\r'");
    t.token(TokenType::StringLiteral, b"'a\\\r'");
    t.end();
}

#[test]
fn unmatched_close_paren() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"bar='quux');");
    t.token(TokenType::Identifier, b"bar");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::StringLiteral, b"'quux'");
    t.error(b");");
}

#[test]
fn bogus_input_character() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"var #foo;");
    t.token(TokenType::Var, b"var");
    t.token(TokenType::Whitespace, b" ");
    t.error(b"#foo;");
}

#[test]
fn backslashes_in_identifier() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a\\u03c0b");
    t.token(TokenType::Identifier, b"a\\u03c0b");
    t.end();
}

#[test]
fn backslashes_in_string() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"\"a\\\"b\"");
    t.token(TokenType::StringLiteral, b"\"a\\\"b\"");
    t.end();
}

#[test]
fn combine_pluses() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a+++b");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"++");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Identifier, b"b");
    t.end();
}

#[test]
fn combine_pluses2() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a+ ++b");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"++");
    t.token(TokenType::Identifier, b"b");
    t.end();
}

#[test]
fn combine_pluses_space() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a+ +b");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Identifier, b"b");
    t.end();
}

#[test]
fn combine_minuses() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a---b");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"--");
    t.token(TokenType::Operator, b"-");
    t.token(TokenType::Identifier, b"b");
    t.end();
}

#[test]
fn combine_mixed() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a--+b");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"--");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Identifier, b"b");
    t.end();
}

#[test]
fn combine_mixed2() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"a-++b");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b"-");
    t.token(TokenType::Operator, b"++");
    t.token(TokenType::Identifier, b"b");
    t.end();
}

#[test]
fn combine_bangs() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"!!b");
    t.token(TokenType::Operator, b"!");
    t.token(TokenType::Operator, b"!");
    t.token(TokenType::Identifier, b"b");
    t.end();
}

#[test]
fn numbers_and_dots_and_identifiers_and_keywords() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"return a.b+5.3");
    t.token(TokenType::Return, b"return");
    t.token(TokenType::Whitespace, b" ");
    t.token(TokenType::Identifier, b"a");
    t.token(TokenType::Operator, b".");
    t.token(TokenType::Identifier, b"b");
    t.token(TokenType::Operator, b"+");
    t.token(TokenType::Number, b"5.3");
    t.end();
}

#[test]
fn number_property() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"1..property");
    t.token(TokenType::Number, b"1.");
    t.token(TokenType::Operator, b".");
    t.token(TokenType::Identifier, b"property");
    t.end();
}

#[test]
fn line_comment_at_end_of_input() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"hello//world");
    t.token(TokenType::Identifier, b"hello");
    t.token(TokenType::Comment, b"//world");
    t.end();
}

#[test]
fn latin1_block_comment() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"/* qu\xe9 pasa */\n");
    t.token(TokenType::Comment, b"/* qu\xe9 pasa */");
    t.token(TokenType::LineSeparator, b"\n");
    t.end();
}

#[test]
fn latin1_line_comment() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"// qu\xe9 pasa\n");
    t.token(TokenType::Comment, b"// qu\xe9 pasa");
    t.token(TokenType::LineSeparator, b"\n");
    t.end();
}

#[test]
fn latin1_string_literal() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"\"qu\xe9 pasa\"\n");
    t.token(TokenType::StringLiteral, b"\"qu\xe9 pasa\"");
    t.token(TokenType::LineSeparator, b"\n");
    t.end();
    t = Harness::new(&patterns, b"'\xaa\\'\xbb\\\r\n\xcc'\n");
    t.token(TokenType::StringLiteral, b"'\xaa\\'\xbb\\\r\n\xcc'");
    t.token(TokenType::LineSeparator, b"\n");
    t.end();
}

#[test]
fn latin1_input() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(
        &patterns,
        b"str='Qu\xe9 pasa';// 'qu\xe9' means 'what'\ncents=/* 73\xa2 is $0.73 */73;",
    );
    t.token(TokenType::Identifier, b"str");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::StringLiteral, b"'Qu\xe9 pasa'");
    t.token(TokenType::Operator, b";");
    t.token(TokenType::Comment, b"// 'qu\xe9' means 'what'");
    t.token(TokenType::LineSeparator, b"\n");
    t.token(TokenType::Identifier, b"cents");
    t.token(TokenType::Operator, b"=");
    t.token(TokenType::Comment, b"/* 73\xa2 is $0.73 */");
    t.token(TokenType::Number, b"73");
    t.token(TokenType::Operator, b";");
    t.end();
}

#[test]
fn json_heuristic() {
    let patterns = TokenizerPatterns::new();
    let mut t = Harness::new(&patterns, b"{\"foo\":{\"bar\":1},\"baz\":2}");
    t.token(TokenType::Operator, b"{");
    t.stack("Start {");
    t.token(TokenType::StringLiteral, b"\"foo\"");
    t.stack("Start { Expr");
    t.token(TokenType::Operator, b":");
    t.stack("Start Oper { Oper");
    t.token(TokenType::Operator, b"{");
    t.stack("Start Oper { Oper {");
    t.token(TokenType::StringLiteral, b"\"bar\"");
    t.token(TokenType::Operator, b":");
    t.stack("Start Oper { Oper { Oper");
    t.token(TokenType::Number, b"1");
    t.stack("Start Oper { Oper { Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start Oper { Expr");
    t.token(TokenType::Operator, b",");
    t.stack("Start Oper {");
    t.token(TokenType::StringLiteral, b"\"baz\"");
    t.token(TokenType::Operator, b":");
    t.stack("Start Oper { Oper");
    t.token(TokenType::Number, b"2");
    t.stack("Start Oper { Expr");
    t.token(TokenType::Operator, b"}");
    t.stack("Start Expr");
    t.end();
}

#[test]
fn tokenize_angular() {
    tokenize_file(
        "angular.original",
        include_bytes!("../testdata/third_party/angular.original"),
    );
}

#[test]
fn tokenize_j_query() {
    tokenize_file(
        "jquery.original",
        include_bytes!("../testdata/third_party/jquery.original"),
    );
}

#[test]
fn tokenize_prototype() {
    tokenize_file(
        "prototype.original",
        include_bytes!("../testdata/third_party/prototype.original"),
    );
}

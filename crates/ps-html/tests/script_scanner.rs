//! Script scanner tests ported from `script_tag_scanner_test.cc`.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;

use harness::{run_with_filters, wrap_body};
use ps_html::element::NodeId;
use ps_html::filter::HtmlFilter;
use ps_html::lexer::Lexer;
use ps_html::parser::Dom;
use ps_html::script_scanner::{
    ScriptClassification, ScriptTagScanner, EXECUTE_ASYNC, EXECUTE_DEFER, EXECUTE_FOR_EVENT,
    EXECUTE_SYNC,
};

#[derive(Debug, Clone)]
struct ScriptInfo {
    url: Vec<u8>,
    classification: ScriptClassification,
    flags: u32,
}

/// Collects script info on start tags (`ScriptCollector`).
#[derive(Debug, Clone, Default)]
struct Collector {
    scripts: Rc<RefCell<Vec<ScriptInfo>>>,
}

impl HtmlFilter for Collector {
    fn name(&self) -> &'static str {
        "ScriptCollector"
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let (classification, src) = ScriptTagScanner::parse_script_element(dom, id);
        if classification != ScriptClassification::NonScript {
            let url = match src {
                Some(index) => {
                    let attribute = &mut dom.element_attributes_mut(id)[index];
                    attribute.decoded_value_or_null().unwrap_or(b"").to_vec()
                }
                None => Vec::new(),
            };
            let flags = ScriptTagScanner::execution_mode(dom, id);
            self.scripts.borrow_mut().push(ScriptInfo {
                url,
                classification,
                flags,
            });
        }
    }
}

/// Parses a body-wrapped fragment, asserting a clean round trip
/// (`ValidateNoChanges`), and returns the collected scripts.
fn collect(fragment: &[u8]) -> Vec<ScriptInfo> {
    let wrapped = wrap_body(fragment);
    let collector = Collector::default();
    let probe = collector.clone();
    let output = run_with_filters(&wrapped, |parser| {
        parser.add_filter(Box::new(probe));
    });
    assert_eq!(output, wrapped);
    let scripts = collector.scripts.borrow().to_vec();
    scripts
}

fn script_with_type(attr: &str) -> Vec<u8> {
    format!("<script type=\"{attr}\"></script>").into_bytes()
}

fn script_with_lang(attr: &str) -> Vec<u8> {
    format!("<script language=\"{attr}\"></script>").into_bytes()
}

#[test]
fn not_found_script_tag() {
    assert!(collect(b"<noscript>").is_empty());
}

#[test]
fn find_script_tag() {
    let scripts = collect(b"<script src=\"myscript.js\"></script>");
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].url, b"myscript.js");
    assert_eq!(scripts[0].classification, ScriptClassification::JavaScript);
}

#[test]
fn type_no_value_is_js() {
    // Valueless `type` falls back to the (absent) language: JS.
    let scripts = collect(b"<script type></script>");
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].classification, ScriptClassification::JavaScript);
}

#[test]
fn type_empty_is_js() {
    let scripts = collect(b"<script type=\"\"></script>");
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].classification, ScriptClassification::JavaScript);
}

#[test]
fn type_no_value_falls_back_to_language() {
    let scripts = collect(b"<script type language=tcl></script>");
    assert_eq!(scripts.len(), 1);
    assert_eq!(
        scripts[0].classification,
        ScriptClassification::UnknownScript
    );
}

#[test]
fn language_no_value_is_js() {
    let scripts = collect(b"<script language></script>");
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].classification, ScriptClassification::JavaScript);
}

#[test]
fn language_empty_is_js() {
    let scripts = collect(b"<script language=\"\"></script>");
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].classification, ScriptClassification::JavaScript);
}

#[test]
fn undecodable_language_is_unknown() {
    let scripts = collect("<script language=muñecos></script>".as_bytes());
    assert_eq!(scripts.len(), 1);
    assert_eq!(
        scripts[0].classification,
        ScriptClassification::UnknownScript
    );
}

#[test]
fn undecodable_type_is_unknown() {
    let scripts = collect("<script type=muñecos></script>".as_bytes());
    assert_eq!(scripts.len(), 1);
    assert_eq!(
        scripts[0].classification,
        ScriptClassification::UnknownScript
    );
}

#[test]
fn type_beats_language() {
    let scripts = collect(b"<script type=\"text/ecmascript\" language=tcl></script>");
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].classification, ScriptClassification::JavaScript);
}

#[test]
fn script_type_matrix() {
    // First 16 mimetypes are JS; the rest (plus whitespace-only) are not.
    let types = [
        "application/ecmascript",
        "application/javascript",
        "application/x-ecmascript",
        "application/x-javascript",
        "text/ecmascript",
        "text/javascript",
        "text/javascript1.0",
        "text/javascript1.1",
        "text/javascript1.2",
        "text/javascript1.3",
        "text/javascript1.4",
        "text/javascript1.5",
        "text/jscript",
        "text/livescript",
        "text/x-ecmascript",
        "text/x-javascript",
        "text/tcl",
        "text/ecmascript4",
        "text/javascript2.0",
        "                  ",
    ];
    let mut html = Vec::new();
    for mime in &types {
        html.extend_from_slice(&script_with_type(mime));
    }
    let scripts = collect(&html);
    assert_eq!(scripts.len(), 20);
    for (index, info) in scripts.iter().enumerate() {
        let expected = if index <= 15 {
            ScriptClassification::JavaScript
        } else {
            ScriptClassification::UnknownScript
        };
        assert_eq!(info.classification, expected, "type {}", types[index]);
    }
}

#[test]
fn modern_script_types_unknown() {
    let types = [
        "module",
        "importmap",
        "speculationrules",
        "application/json",
        "application/ld+json",
    ];
    let mut html = Vec::new();
    for mime in &types {
        html.extend_from_slice(&script_with_type(mime));
    }
    let scripts = collect(&html);
    assert_eq!(scripts.len(), 5);
    for info in &scripts {
        assert_eq!(info.classification, ScriptClassification::UnknownScript);
    }
}

#[test]
fn script_types_normalize_case_and_whitespace() {
    let types = [
        "  application/ecmascRipt",
        "      applicAtion/javascript  ",
        "application/x-ecmaScript  ",
        "   applicAtion/x-javascript",
        "text/Ecmascript",
        "     text/jaVasCript    ",
        " TEXt/javascript1.0\t",
        "  text/javascript1.1",
        " teXt/javascripT1.2",
        "\ttExt/javascRipt1.3 ",
        "  text/javascRipT1.4  ",
        "  Text/javAscript1.5 ",
        "   Text/jscrIpt",
        "   text/lIvescript",
        "teXt/x-ecmasCript ",
        "tExt/x-jaVascript ",
        "Text/Tcl ",
        " text/Ecmascript4",
        "tExt/javascript2.0",
        "text/javasc ript",
    ];
    let mut html = Vec::new();
    for mime in &types {
        html.extend_from_slice(&script_with_type(mime));
    }
    let scripts = collect(&html);
    assert_eq!(scripts.len(), 20);
    for (index, info) in scripts.iter().enumerate() {
        let expected = if index <= 15 {
            ScriptClassification::JavaScript
        } else {
            ScriptClassification::UnknownScript
        };
        assert_eq!(
            info.classification,
            expected,
            "type {mime}",
            mime = types[index]
        );
    }
}

#[test]
fn language_matrix() {
    let langs = [
        "ecmascript",
        "javascript",
        "javascript1.0",
        "javascript1.1",
        "javascript1.2",
        "javascript1.3",
        "javascript1.4",
        "javascript1.5",
        "jscript",
        "livescript",
        "x-ecmascript",
        "x-javascript",
        "tcl",
        "ecmascript4",
        "javascript2.0",
    ];
    let mut html = Vec::new();
    for lang in &langs {
        html.extend_from_slice(&script_with_lang(lang));
    }
    let scripts = collect(&html);
    assert_eq!(scripts.len(), 15);
    for (index, info) in scripts.iter().enumerate() {
        let expected = if index <= 11 {
            ScriptClassification::JavaScript
        } else {
            ScriptClassification::UnknownScript
        };
        assert_eq!(info.classification, expected, "lang {}", langs[index]);
    }
}

#[test]
fn language_normalizes_case() {
    let langs = [
        "ecmasCript",
        "javAscript",
        "javascript1.0",
        "javascRipt1.1",
        "javascripT1.2",
        "javaScrIpt1.3",
        "jaVasCript1.4",
        "javaScriPt1.5",
        "jscRiPt",
        "livEscript",
        "x-ecmaScript",
        "x-jaVascript",
        "tCl",
        "ecmasCript4",
        "jaVascript2.0",
    ];
    let mut html = Vec::new();
    for lang in &langs {
        html.extend_from_slice(&script_with_lang(lang));
    }
    let scripts = collect(&html);
    assert_eq!(scripts.len(), 15);
    for (index, info) in scripts.iter().enumerate() {
        let expected = if index <= 11 {
            ScriptClassification::JavaScript
        } else {
            ScriptClassification::UnknownScript
        };
        assert_eq!(info.classification, expected, "lang {}", langs[index]);
    }
}

#[test]
fn language_keeps_whitespace_significant() {
    // Unlike `type`, `language` whitespace is significant: all unknown.
    let langs = [
        " ecmascript",
        "javascript\t",
        "  javascript1.0  ",
        " javascript1.1",
        "javascript1.2 ",
        "  javascript1.3",
        "javascript1.4 ",
        "  javascript1.5",
        "jscript ",
        "livescript  ",
        "  x-ecmascript",
        "x-javascript\t",
        "  tcl  ",
        "ecmascript4  ",
        "  javascript2.0",
    ];
    let mut html = Vec::new();
    for lang in &langs {
        html.extend_from_slice(&script_with_lang(lang));
    }
    let scripts = collect(&html);
    assert_eq!(scripts.len(), 15);
    for (index, info) in scripts.iter().enumerate() {
        assert_eq!(
            info.classification,
            ScriptClassification::UnknownScript,
            "lang {}",
            langs[index]
        );
    }
}

fn assert_flags(specs: &[(&str, u32)]) {
    let mut html = Vec::new();
    for (attrs, _) in specs {
        html.extend_from_slice(format!("<script {attrs}></script>").as_bytes());
    }
    let scripts = collect(&html);
    assert_eq!(scripts.len(), specs.len());
    for (info, (attrs, expected)) in scripts.iter().zip(specs.iter()) {
        assert_eq!(info.flags, *expected, "attrs {attrs}");
    }
}

#[test]
fn for_event_flags() {
    assert_flags(&[
        ("for event", EXECUTE_FOR_EVENT),
        ("for=\"\" event=\"\"", EXECUTE_FOR_EVENT),
        ("for", EXECUTE_SYNC),
        ("event", EXECUTE_SYNC),
        ("for=\"a\" event=\"b\"", EXECUTE_FOR_EVENT),
        ("for=\"window\" event=\"b\"", EXECUTE_FOR_EVENT),
        (
            "for=\"window\" event=\"b\" async",
            EXECUTE_FOR_EVENT | EXECUTE_ASYNC,
        ),
        ("for=\"window\" event=\"onload\"", EXECUTE_SYNC),
        ("for=\"window\" event=onload async", EXECUTE_ASYNC),
        ("for=\"window\" event=\"onload()\"", EXECUTE_SYNC),
        ("for=\"wiNdow \" event=\" onLoad  \"", EXECUTE_SYNC),
        ("for=\" windOw\" event=\"OnloAd() \"", EXECUTE_SYNC),
    ]);
}

#[test]
fn async_defer_flags() {
    assert_flags(&[
        ("language=tcl async", EXECUTE_ASYNC),
        ("async=\"irrelevant\"", EXECUTE_ASYNC),
        ("defer", EXECUTE_DEFER),
        ("defer async", EXECUTE_DEFER | EXECUTE_ASYNC),
        ("language=tcl async src=a", EXECUTE_ASYNC),
        ("async=\"irrelevant\" src=a", EXECUTE_ASYNC),
        ("defer src=a", EXECUTE_DEFER),
        ("defer async src=a", EXECUTE_DEFER | EXECUTE_ASYNC),
    ]);
}

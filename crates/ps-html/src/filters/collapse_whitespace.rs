//! Whitespace collapsing (`CollapseWhitespaceFilter`).
//!
//! Shrinks every run of HTML whitespace in character data to a single
//! character (a newline wins over preceding spaces), except inside
//! `code`/`pre`/`script`/`style`/`textarea` and in pass-through spans.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Collapses whitespace runs outside sensitive elements.
#[derive(Debug, Default)]
pub struct CollapseWhitespaceFilter {
    keyword_stack: Vec<Keyword>,
}

fn is_sensitive(keyword: Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Code | Keyword::Pre | Keyword::Script | Keyword::Style | Keyword::Textarea
    )
}

/// Collapses one character run in place (the C++ read/write-pointer loop).
fn collapse_contents(contents: &mut Vec<u8>) {
    let mut write = 0;
    let mut in_whitespace = false;
    for read in 0..contents.len() {
        let byte = contents[read];
        match byte {
            b' ' | b'\t' | b'\r' | 0x0c => {
                if !in_whitespace {
                    contents[write] = byte;
                    write += 1;
                    in_whitespace = true;
                }
            }
            b'\n' => {
                // A newline overwrites a preceding whitespace run.
                if in_whitespace {
                    write -= 1;
                }
                contents[write] = byte;
                write += 1;
                in_whitespace = true;
            }
            _ => {
                contents[write] = byte;
                write += 1;
                in_whitespace = false;
            }
        }
    }
    contents.truncate(write);
}

impl HtmlFilter for CollapseWhitespaceFilter {
    fn name(&self) -> &'static str {
        "CollapseWhitespace"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.keyword_stack.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let keyword = dom.element_keyword(id);
        if is_sensitive(keyword) {
            self.keyword_stack.push(keyword);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let keyword = dom.element_keyword(id);
        if self.keyword_stack.last() == Some(&keyword) {
            self.keyword_stack.pop();
        } else {
            debug_assert!(!is_sensitive(keyword));
        }
    }

    fn characters(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        // Text passed through unparsed can hold any markup, pre included.
        if self.keyword_stack.is_empty() && !lexer.passing_through() {
            collapse_contents(dom.leaf_contents_mut(id));
        }
    }
}

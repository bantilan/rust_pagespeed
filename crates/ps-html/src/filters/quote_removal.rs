//! Attribute quote removal (`HtmlAttributeQuoteRemoval`).
//!
//! Drops quotes around attribute values made only of identifier-safe
//! bytes (`[A-Za-z0-9-._:]`), keeping quotes on empty values and 8-bit
//! values, and skipping XHTML documents entirely.

use crate::element::{NodeId, QuoteStyle};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::parser::Dom;

/// Removes unneeded attribute quotes, counting them.
#[derive(Debug, Default)]
pub struct QuoteRemovalFilter {
    total_quotes_removed: u32,
}

fn byte_needs_no_quotes(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b':')
}

/// True when the value must stay quoted (`NeedsQuotes`): any unsafe
/// byte, or an empty value (browsers disagree on those).
fn needs_quotes(value: Option<&[u8]>) -> bool {
    match value {
        Some(bytes) => bytes.is_empty() || bytes.iter().any(|byte| !byte_needs_no_quotes(*byte)),
        // Valueless attributes never reach here (already `NoQuote`), but
        // keeping them quoted is the conservative answer anyway.
        None => true,
    }
}

impl QuoteRemovalFilter {
    /// Total attributes unquoted so far (`total_quotes_removed`).
    #[must_use]
    pub fn total_quotes_removed(&self) -> u32 {
        self.total_quotes_removed
    }
}

impl HtmlFilter for QuoteRemovalFilter {
    fn name(&self) -> &'static str {
        "HtmlAttributeQuoteRemoval"
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        // XHTML doctypes require quotes.
        if lexer.doctype().is_xhtml() {
            return;
        }
        let mut rewritten = 0;
        for attribute in dom.element_attributes_mut(id).iter_mut() {
            if attribute.quote_style() != QuoteStyle::NoQuote
                && !needs_quotes(attribute.escaped_value())
            {
                attribute.set_quote_style(QuoteStyle::NoQuote);
                rewritten += 1;
            }
        }
        self.total_quotes_removed += rewritten;
    }
}

//! JavaScript rewrite decisions (`javascript_code_block.{h,cc}`,
//! `javascript_filter.cc` size/stats subset).
//!
//! Minifies code blocks (experimental tokenizer by default, legacy
//! fallback available), identifies known libraries in rewritten code,
//! tracks shrink statistics, and preserves the original on minify
//! failure. HTML-side orchestration (combine/inline/outline filters,
//! analytics ordering) consumes these primitives in Step 4.
#![forbid(unsafe_code)]

use crate::library::LibraryIdentification;
use crate::minify::{legacy_minify_js, minify_utf8_with_source_map};
use crate::sourcemap::Mapping;
use crate::tokenizer::TokenizerPatterns;

/// JavaScript stat names (`JavascriptRewriteConfig::InitStats`).
pub mod stats {
    /// Blocks minified.
    pub const BLOCKS_MINIFIED: &str = "javascript_blocks_minified";
    /// Libraries identified.
    pub const LIBRARIES_IDENTIFIED: &str = "javascript_libraries_identified";
    /// Minification failures.
    pub const MINIFICATION_FAILURES: &str = "javascript_minification_failures";
    /// Total bytes saved.
    pub const TOTAL_BYTES_SAVED: &str = "javascript_total_bytes_saved";
    /// Total original bytes.
    pub const TOTAL_ORIGINAL_BYTES: &str = "javascript_total_original_bytes";
    /// Minify uses.
    pub const MINIFY_USES: &str = "javascript_minify_uses";
    /// Reducing minifications.
    pub const REDUCING_MINIFICATIONS: &str = "javascript_reducing_minifications";
    /// Minification disabled.
    pub const MINIFICATION_DISABLED: &str = "javascript_minification_disabled";
    /// Did-not-shrink outcomes.
    pub const DID_NOT_SHRINK: &str = "javascript_did_not_shrink";
    /// Failed writes.
    pub const FAILED_TO_WRITE: &str = "javascript_failed_to_write";
}

/// Introspection debug comment
/// (`JavascriptCodeBlock::kIntrospectionComment`).
pub const INTROSPECTION_COMMENT: &str =
    "This script contains introspective JavaScript and is unsafe to replace.";

/// Whether renaming a script URL is unsafe
/// (`JavascriptCodeBlock::UnsafeToRename`): scripts reflecting over
/// `<script>` elements break when minified and relocated.
#[must_use]
pub fn unsafe_to_rename(script: &[u8]) -> bool {
    let text = String::from_utf8_lossy(script);
    text.contains("document.getElementsByTagName('script')")
        || text.contains("document.getElementsByTagName(\"script\")")
        || text.contains("$('script')")
        || text.contains("$(\"script\")")
}

/// Whether a script is likely strict-mode (`IsLikelyStrictMode`):
/// a `'use strict'` string in the prologue (strings, semicolons,
/// and trivia only before it).
#[must_use]
pub fn is_likely_strict_mode(patterns: &TokenizerPatterns, input: &[u8]) -> bool {
    use crate::keywords::TokenType;
    use crate::tokenizer::Tokenizer;
    let mut tokenizer = Tokenizer::new(patterns, input);
    loop {
        let (kind, (start, end)) = tokenizer.next_token();
        match kind {
            TokenType::Comment
            | TokenType::Whitespace
            | TokenType::LineSeparator
            | TokenType::SemiInsert => {}
            TokenType::Operator => {
                if input.get(start..end) != Some(b";".as_slice()) {
                    return false;
                }
            }
            TokenType::StringLiteral => {
                let text = input.get(start..end).unwrap_or(b"");
                if text == b"'use strict'" || text == b"\"use strict\"" {
                    return true;
                }
            }
            _ => return false,
        }
    }
}

/// Code-block rewrite configuration (the `JavascriptRewriteConfig`
/// subset `JavascriptCodeBlock` consults).
#[derive(Debug, Clone, Copy)]
pub struct CodeBlockConfig {
    /// Minify output (`minify`).
    pub minify: bool,
    /// Use the tokenizer minifier (`use_experimental_minifier`).
    pub use_experimental_minifier: bool,
}

impl Default for CodeBlockConfig {
    fn default() -> Self {
        CodeBlockConfig {
            minify: true,
            use_experimental_minifier: true,
        }
    }
}

/// Outcome counters for one rewrite (mirrors the stat updates).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RewriteOutcome {
    /// Blocks minified (every successful minify, shrinking or not).
    pub blocks_minified: u64,
    /// Reducing uses (minified output actually smaller).
    pub reducing_uses: u64,
    /// Libraries identified.
    pub libraries_identified: u64,
    /// Minification failures.
    pub minification_failures: u64,
    /// Original bytes (only when shrinking, like C++).
    pub original_bytes: u64,
    /// Bytes saved (only when shrinking).
    pub bytes_saved: u64,
}

/// One JavaScript code block (`JavascriptCodeBlock`).
#[derive(Debug)]
pub struct CodeBlock<'a> {
    patterns: &'a TokenizerPatterns,
    config: CodeBlockConfig,
    library_identification: Option<&'a mut LibraryIdentification>,
    original: Vec<u8>,
    rewritten: Vec<u8>,
    source_mappings: Vec<Mapping>,
    rewritten_flag: bool,
    successful: bool,
    source_map_url: Option<String>,
    outcome: RewriteOutcome,
}

impl<'a> CodeBlock<'a> {
    /// Creates a block (`JavascriptCodeBlock`, message-id plumbing omitted).
    pub fn new(
        patterns: &'a TokenizerPatterns,
        config: CodeBlockConfig,
        library_identification: Option<&'a mut LibraryIdentification>,
        original_code: &[u8],
    ) -> CodeBlock<'a> {
        CodeBlock {
            patterns,
            config,
            library_identification,
            original: original_code.to_vec(),
            rewritten: Vec::new(),
            source_mappings: Vec::new(),
            rewritten_flag: false,
            successful: false,
            source_map_url: None,
            outcome: RewriteOutcome::default(),
        }
    }

    /// Rewrites once (`Rewrite`; idempotent).
    pub fn rewrite(&mut self) -> bool {
        if self.rewritten_flag {
            return self.successful;
        }
        self.rewritten_flag = true;
        self.successful = false;
        // Bail unless minifying or identifying (C++ early return).
        if !self.config.minify && self.library_identification.is_none() {
            return self.successful;
        }
        let (minified, ok) = if self.config.use_experimental_minifier {
            minify_utf8_with_source_map(
                self.patterns,
                &self.original,
                Some(&mut self.source_mappings),
            )
        } else {
            match legacy_minify_js(&self.original) {
                Some(code) => (code, true),
                None => (Vec::new(), false),
            }
        };
        if ok {
            self.rewritten = minified;
            self.outcome.blocks_minified += 1;
            if self.config.minify && self.rewritten.len() < self.original.len() {
                // Minification will actually be used: record byte stats.
                self.successful = true;
                self.outcome.reducing_uses += 1;
                self.outcome.original_bytes += self.original.len() as u64;
                self.outcome.bytes_saved += (self.original.len() - self.rewritten.len()) as u64;
            }
            // else: minified but not smaller (did-not-shrink path).
        } else {
            // Minification failed: preserve trimmed original for library
            // identification only (not a successful rewrite).
            self.rewritten = trim_whitespace(&self.original);
            self.outcome.minification_failures += 1;
        }
        self.successful
    }

    /// Recognizes the rewritten code as a known library
    /// (`LibraryUrl()`-equivalent; requires a prior `rewrite()`).
    pub fn library_url(&mut self) -> Option<String> {
        if !self.rewritten_flag {
            return None;
        }
        let found = self
            .library_identification
            .as_ref()
            .and_then(|libraries| libraries.find(&self.rewritten));
        if found.is_some() {
            self.outcome.libraries_identified += 1;
        }
        found
    }

    /// Rewritten code (`rewritten_code`).
    #[must_use]
    pub fn rewritten_code(&self) -> &[u8] {
        &self.rewritten
    }

    /// Original code (`original_code`).
    #[must_use]
    pub fn original_code(&self) -> &[u8] {
        &self.original
    }

    /// Source mappings recorded during minification.
    #[must_use]
    pub fn source_mappings(&self) -> &[Mapping] {
        &self.source_mappings
    }

    /// Outcome counters.
    #[must_use]
    pub fn outcome(&self) -> RewriteOutcome {
        self.outcome
    }

    /// Appends a sourceMappingURL comment (`AppendSourceMapUrl`).
    pub fn append_source_map_url(&mut self, url: &str) {
        self.source_map_url = Some(url.to_string());
        self.rewritten.extend_from_slice(b"\n//# sourceMappingURL=");
        self.rewritten.extend_from_slice(url.as_bytes());
        self.rewritten.push(b'\n');
    }

    /// Takes the rewritten string, resetting rewrite state
    /// (`SwapRewrittenString`; requires a successful rewrite).
    pub fn take_rewritten(&mut self) -> Option<Vec<u8>> {
        if !self.rewritten_flag || !self.successful {
            return None;
        }
        let mut out = Vec::new();
        std::mem::swap(&mut out, &mut self.rewritten);
        self.rewritten_flag = false;
        self.successful = false;
        Some(out)
    }
}

/// Trims ASCII whitespace from both ends (`TrimWhitespace`).
fn trim_whitespace(text: &[u8]) -> Vec<u8> {
    let mut start = 0;
    let mut end = text.len();
    while start < end && text[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && text[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    text[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns() -> TokenizerPatterns {
        TokenizerPatterns::new()
    }

    #[test]
    fn rewrite_minify_and_stats() {
        let patterns = patterns();
        let mut block = CodeBlock::new(
            &patterns,
            CodeBlockConfig::default(),
            None,
            b"var   x   =   1;  // comment\n",
        );
        assert!(block.rewrite());
        assert_eq!(block.rewritten_code(), b"var x=1;");
        assert_eq!(block.outcome().blocks_minified, 1);
        assert!(block.outcome().bytes_saved > 0);
        // Idempotent: second rewrite returns the cached outcome.
        assert!(block.rewrite());
        assert_eq!(block.outcome().blocks_minified, 1);
        assert!(block.take_rewritten().is_some());
        assert!(block.take_rewritten().is_none()); // state reset
    }

    #[test]
    fn failure_fallback_and_library() {
        let patterns = patterns();
        let mut libraries = LibraryIdentification::new();
        // Unterminated string: minify fails, trimmed original preserved.
        let mut block = CodeBlock::new(
            &patterns,
            CodeBlockConfig::default(),
            Some(&mut libraries),
            b"var x = 'oops;",
        );
        assert!(!block.rewrite());
        assert_eq!(block.rewritten_code(), b"var x = 'oops;");
        assert_eq!(block.outcome().minification_failures, 1);
        // Minified-but-not-smaller: no success, but blocks_minified
        // still counts (reducing_uses does not).
        let mut block = CodeBlock::new(&patterns, CodeBlockConfig::default(), None, b"a;");
        assert!(!block.rewrite()); // "a;" cannot shrink
        assert_eq!(block.outcome().blocks_minified, 1);
        assert_eq!(block.outcome().reducing_uses, 0);
        // Library recognition runs separately after rewriting, keyed
        // on the MINIFIED code (as registered from config).
        let (minified, ok) = crate::minify::minify_utf8(&patterns, b"var lib = 1;");
        assert!(ok);
        let mut libraries = LibraryIdentification::new();
        let hasher = ps_cache::hash::Md5Hasher::with_size(crate::library::NUM_HASH_CHARS);
        libraries.register(
            minified.len() as i64,
            &ps_cache::hash::Hasher::hash(&hasher, &minified),
            "http://cdn.example.com/lib.js",
        );
        let mut block = CodeBlock::new(
            &patterns,
            CodeBlockConfig::default(),
            Some(&mut libraries),
            b"var lib = 1;",
        );
        block.rewrite();
        assert_eq!(
            block.library_url().as_deref(),
            Some("http://cdn.example.com/lib.js")
        );
        assert_eq!(block.outcome().libraries_identified, 1);
    }

    #[test]
    fn introspection_and_source_map_url() {
        assert!(unsafe_to_rename(
            b"var s = document.getElementsByTagName('script');"
        ));
        assert!(unsafe_to_rename(b"$('script').each(f);"));
        assert!(!unsafe_to_rename(b"var s = document.title;"));
        let patterns = patterns();
        let mut block = CodeBlock::new(&patterns, CodeBlockConfig::default(), None, b"var x = 1;");
        assert!(block.rewrite());
        block.append_source_map_url("a.js.map");
        assert!(block
            .rewritten_code()
            .ends_with(b"\n//# sourceMappingURL=a.js.map\n"));
    }
}

//! CSS import-flattening tree (`CssHierarchy`).
//!
//! One node per stylesheet in an `@import` tree: parsing, media
//! subsetting, charset checks, and the two roll-ups (flattened text
//! for nested levels, merged stylesheet for the root). Parents own
//! children outright; ancestor URLs (not pointers) drive recursion
//! detection, and statistics flow through an explicit server handle.
//!
//! NOTE: log messages wait for the message handler.

use ps_css::ast::Stylesheet;
use ps_css::minify::Minifier;
use ps_css::parser::Parser;
use ps_http::google_url::GoogleUrl;

use super::css::{
    FLATTEN_COMPLEX_QUERIES, FLATTEN_INVALID_URL, FLATTEN_LIMIT_EXCEEDED, FLATTEN_MINIFY_FAILED,
    FLATTEN_RECURSION,
};
use crate::server_context::ServerContext;

/// Failure-reason prefix (`kFailureReasonPrefix`).
const FAILURE_REASON_PREFIX: &str = "Flattening failed: ";

/// One stylesheet in a flattening tree (`CssHierarchy`).
pub struct CssHierarchy {
    url: String,
    css_base_url: GoogleUrl,
    css_trim_url: GoogleUrl,
    ancestor_urls: Vec<String>,
    children: Vec<CssHierarchy>,
    input_contents: Vec<u8>,
    minified_contents: Vec<u8>,
    stylesheet: Option<Stylesheet>,
    charset: Vec<u8>,
    charset_source: String,
    media: Vec<Vec<u8>>,
    input_contents_resolved: bool,
    flattening_succeeded: bool,
    flattening_failure_reason: String,
    unparseable_detected: bool,
    flattened_result_limit: i64,
}

impl CssHierarchy {
    /// Builds an empty (flattenable-to-nothing) node.
    #[must_use]
    pub fn new() -> Self {
        CssHierarchy {
            url: String::new(),
            css_base_url: GoogleUrl::new(""),
            css_trim_url: GoogleUrl::new(""),
            ancestor_urls: Vec::new(),
            children: Vec::new(),
            input_contents: Vec::new(),
            minified_contents: Vec::new(),
            stylesheet: None,
            charset: Vec::new(),
            charset_source: "from unknown".to_string(),
            media: Vec::new(),
            input_contents_resolved: false,
            flattening_succeeded: true,
            flattening_failure_reason: String::new(),
            unparseable_detected: false,
            flattened_result_limit: 0,
        }
    }

    /// Initializes the root (`InitializeRoot`).
    pub fn initialize_root(
        &mut self,
        css_base_url: GoogleUrl,
        css_trim_url: GoogleUrl,
        input_contents: Vec<u8>,
        has_unparseables: bool,
        flattened_result_limit: i64,
        stylesheet: Stylesheet,
    ) {
        self.css_base_url = css_base_url;
        self.css_trim_url = css_trim_url;
        self.input_contents = input_contents;
        self.stylesheet = Some(stylesheet);
        self.unparseable_detected = has_unparseables;
        self.flattened_result_limit = flattened_result_limit;
    }

    /// Whether the node has an import to expand (`NeedsRewriting`).
    #[must_use]
    pub fn needs_rewriting(&self) -> bool {
        self.flattening_succeeded && !self.url.is_empty()
    }

    /// Node URL for humans (`url_for_humans`).
    #[must_use]
    pub fn url_for_humans(&self) -> String {
        if self.url.is_empty() {
            "inline".to_string()
        } else if self.url.starts_with("data:") {
            "data URL".to_string()
        } else {
            self.url.clone()
        }
    }

    /// Node URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Base for relative URLs in the input CSS.
    #[must_use]
    pub fn css_base_url(&self) -> &GoogleUrl {
        &self.css_base_url
    }

    /// Base the output URL trims against.
    #[must_use]
    pub fn css_trim_url(&self) -> &GoogleUrl {
        &self.css_trim_url
    }

    /// Base the contents resolve against (`css_resolution_base`).
    #[must_use]
    pub fn css_resolution_base(&self) -> &GoogleUrl {
        if self.input_contents_resolved {
            &self.css_trim_url
        } else {
            &self.css_base_url
        }
    }

    /// Borrows the parsed stylesheet.
    #[must_use]
    pub fn stylesheet(&self) -> Option<&Stylesheet> {
        self.stylesheet.as_ref()
    }

    /// Mutably borrows the parsed stylesheet.
    pub fn stylesheet_mut(&mut self) -> Option<&mut Stylesheet> {
        self.stylesheet.as_mut()
    }

    /// Takes the parsed stylesheet back out.
    #[must_use]
    pub fn into_stylesheet(self) -> Option<Stylesheet> {
        self.stylesheet
    }

    /// Input text.
    #[must_use]
    pub fn input_contents(&self) -> &[u8] {
        &self.input_contents
    }

    /// Replaces the input text (`set_input_contents`).
    pub fn set_input_contents(&mut self, contents: Vec<u8>) {
        self.input_contents = contents;
    }

    /// Flattened text (`minified_contents`).
    #[must_use]
    pub fn minified_contents(&self) -> &[u8] {
        &self.minified_contents
    }

    /// Replaces the flattened text (`set_minified_contents`).
    pub fn set_minified_contents(&mut self, contents: Vec<u8>) {
        self.minified_contents = contents;
    }

    /// Node charset.
    #[must_use]
    pub fn charset(&self) -> &[u8] {
        &self.charset
    }

    /// Replaces the node charset.
    pub fn set_charset(&mut self, charset: Vec<u8>) {
        self.charset = charset;
    }

    /// Charset provenance.
    #[must_use]
    pub fn charset_source(&self) -> &str {
        &self.charset_source
    }

    /// Applicable media (empty means all).
    #[must_use]
    pub fn media(&self) -> &[Vec<u8>] {
        &self.media
    }

    /// Replaces the applicable media.
    pub fn set_media(&mut self, media: Vec<Vec<u8>>) {
        self.media = media;
    }

    /// Child nodes.
    #[must_use]
    pub fn children(&self) -> &[CssHierarchy] {
        &self.children
    }

    /// Mutably borrows the child nodes.
    pub fn children_mut(&mut self) -> &mut [CssHierarchy] {
        &mut self.children
    }

    /// Marks contents resolved (`set_input_contents_resolved`).
    pub fn set_input_contents_resolved(&mut self, resolved: bool) {
        self.input_contents_resolved = resolved;
    }

    /// Whether flattening still holds.
    #[must_use]
    pub fn flattening_succeeded(&self) -> bool {
        self.flattening_succeeded
    }

    /// Sets the flattening verdict (`set_flattening_succeeded`).
    pub fn set_flattening_succeeded(&mut self, ok: bool) {
        self.flattening_succeeded = ok;
    }

    /// Failure reason for debug output.
    #[must_use]
    pub fn flattening_failure_reason(&self) -> &str {
        &self.flattening_failure_reason
    }

    /// Adds a failure reason (`AddFlatteningFailureReason`):
    /// de-duplicated case-insensitively, joined with ` AND `, prefixed
    /// once the node has failed.
    pub fn add_flattening_failure_reason(&mut self, reason: &str) {
        if reason.is_empty() {
            return;
        }
        let trimmed = reason.strip_prefix(FAILURE_REASON_PREFIX).unwrap_or(reason);
        if self
            .flattening_failure_reason
            .to_ascii_lowercase()
            .contains(&trimmed.to_ascii_lowercase())
        {
            return;
        }
        if self.flattening_succeeded {
            if !self.flattening_failure_reason.is_empty() {
                self.flattening_failure_reason.push_str(" AND ");
            }
        } else if self.flattening_failure_reason.is_empty() {
            self.flattening_failure_reason = FAILURE_REASON_PREFIX.to_string();
        } else {
            if !self
                .flattening_failure_reason
                .contains(FAILURE_REASON_PREFIX)
            {
                self.flattening_failure_reason =
                    format!("{FAILURE_REASON_PREFIX}{}", self.flattening_failure_reason);
            }
            self.flattening_failure_reason.push_str(" AND ");
        }
        self.flattening_failure_reason.push_str(trimmed);
    }

    /// Whether anything unparseable was seen.
    #[must_use]
    pub fn unparseable_detected(&self) -> bool {
        self.unparseable_detected
    }

    /// Result-size bound (0 means none).
    #[must_use]
    pub fn flattened_result_limit(&self) -> i64 {
        self.flattened_result_limit
    }

    /// Whether an ancestor already handles this URL (`IsRecursive`).
    fn is_recursive(&self) -> bool {
        self.ancestor_urls.iter().any(|url| *url == self.url)
    }

    /// Intersects containing and import media (`DetermineImportMedia`);
    /// false when nothing applies (child skipped, not an error).
    fn determine_import_media(
        &mut self,
        containing_media: &[Vec<u8>],
        import_media: &[Vec<u8>],
    ) -> bool {
        self.media = import_media.to_vec();
        ps_css::util::clear_vector_if_contains_media_all(&mut self.media);
        if self.media.is_empty() {
            self.media = containing_media.to_vec();
        } else {
            self.media.sort();
            ps_css::util::eliminate_elements_not_in(&mut self.media, containing_media);
            if self.media.is_empty() {
                return false;
            }
        }
        true
    }

    /// Intersects ruleset and node media in place
    /// (`DetermineRulesetMedia`); false drops the ruleset.
    fn determine_ruleset_media(&self, ruleset_media: &mut Vec<Vec<u8>>) -> bool {
        ps_css::util::clear_vector_if_contains_media_all(ruleset_media);
        ruleset_media.sort();
        if !self.media.is_empty() {
            ps_css::util::eliminate_elements_not_in(ruleset_media, &self.media);
            if ruleset_media.is_empty() {
                return false;
            }
        }
        true
    }

    /// Determines the node charset and checks it against the parent's
    /// (`CheckCharsetOk`): headers, then `@charset`, then inheritance.
    pub fn check_charset_ok(
        &mut self,
        header_charset: Option<&str>,
        parent_charset: &[u8],
        parent_source: &str,
        parent_for_humans: &str,
    ) -> Result<(), String> {
        if self.charset.is_empty() {
            if let Some(charset) = header_charset {
                self.charset = charset.as_bytes().to_vec();
                self.charset_source = "from headers".to_string();
            }
        }
        if self.charset.is_empty() {
            if let Some(sheet) = &self.stylesheet {
                if !sheet.charsets().is_empty() {
                    self.charset = sheet.charset(0).as_bytes().to_vec();
                    self.charset_source = "from an @charset".to_string();
                }
            }
        }
        if self.charset.is_empty() {
            self.charset = parent_charset.to_vec();
            self.charset_source = "from the enclosing CSS".to_string();
            return Ok(());
        }
        if !self.charset.eq_ignore_ascii_case(parent_charset) {
            return Err(format!(
                "The charset of {} ({} {}) is different from that of its parent ({}): {} {}",
                self.url_for_humans(),
                String::from_utf8_lossy(&self.charset),
                self.charset_source(),
                parent_for_humans,
                String::from_utf8_lossy(parent_charset),
                parent_source,
            ));
        }
        Ok(())
    }

    /// Parses the input contents and subsets ruleset media (`Parse`).
    pub fn parse(&mut self, server: &ServerContext) -> bool {
        if self.stylesheet.is_some() {
            return true;
        }
        let mut parser = Parser::new(&self.input_contents);
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        let mut stylesheet = parser.parse_raw_stylesheet();
        if parser.errors_seen_mask() != ps_css::ast::NO_ERROR {
            return false;
        }
        if parser.unparseable_sections_seen_mask() != ps_css::ast::NO_ERROR {
            self.unparseable_detected = true;
        }
        let mut index = 0;
        while index < stylesheet.rulesets().len() {
            let queries = stylesheet.rulesets()[index].media_queries().clone();
            let Some(mut ruleset_media) = ps_css::util::media_queries_to_strings(&queries) else {
                server.variable(FLATTEN_COMPLEX_QUERIES).add(1);
                self.set_flattening_succeeded(false);
                self.add_flattening_failure_reason(&format!(
                    "A media query is too complex in {}",
                    self.url_for_humans()
                ));
                break;
            };
            if self.determine_ruleset_media(&mut ruleset_media) {
                let converted = ps_css::util::strings_to_media_queries(&ruleset_media);
                *stylesheet.rulesets_mut()[index].media_queries_mut() = converted;
                index += 1;
            } else {
                stylesheet.rulesets_mut().remove(index);
            }
        }
        self.stylesheet = Some(stylesheet);
        true
    }

    /// Expands `@import`s into children (`ExpandChildren`); true when
    /// any child needs rewriting.
    pub fn expand_children(&mut self, server: &ServerContext) -> bool {
        let mut result = false;
        let imports = match &self.stylesheet {
            Some(sheet) => sheet.imports().len(),
            None => return false,
        };
        self.children.clear();
        for _ in 0..imports {
            self.children.push(CssHierarchy::new());
        }
        for index in 0..imports {
            let (link, media_queries) = {
                let sheet = self.stylesheet.as_ref().expect("parsed sheet");
                let import = &sheet.imports()[index];
                (
                    import.link().as_bytes().to_vec(),
                    import.media_queries().clone(),
                )
            };
            let import_url =
                GoogleUrl::resolve(self.css_resolution_base(), &String::from_utf8_lossy(&link));
            if !import_url.is_web_valid() {
                server.variable(FLATTEN_INVALID_URL).add(1);
                let parent = self.url_for_humans();
                let child = &mut self.children[index];
                child.set_flattening_succeeded(false);
                child.add_flattening_failure_reason(&format!(
                    "Invalid import URL {} in {parent}",
                    String::from_utf8_lossy(&link),
                ));
                continue;
            }
            let Some(media_types) = ps_css::util::media_queries_to_strings(&media_queries) else {
                server.variable(FLATTEN_COMPLEX_QUERIES).add(1);
                let child = &mut self.children[index];
                child.set_flattening_succeeded(false);
                child.add_flattening_failure_reason(&format!(
                    "Complex media queries in the @import of {}",
                    child.url_for_humans()
                ));
                continue;
            };
            let media = self.media.clone();
            let child = &mut self.children[index];
            if !child.determine_import_media(&media, &media_types) {
                continue;
            }
            // Snapshot parent state for the nested init (borrowed
            // child rules out a direct call).
            let (trim, limit, mut ancestors) = (
                self.css_trim_url.clone(),
                self.flattened_result_limit,
                self.ancestor_urls.clone(),
            );
            if self.url.is_empty() {
                if !self.css_base_url.spec().is_empty() {
                    ancestors.push(self.css_base_url.spec().to_string());
                }
            } else {
                ancestors.push(self.url.clone());
            }
            child.css_base_url = import_url;
            child.url = child.css_base_url.spec().to_string();
            child.ancestor_urls = ancestors;
            child.css_trim_url = trim;
            child.flattened_result_limit = limit;
            if child.is_recursive() {
                server.variable(FLATTEN_RECURSION).add(1);
                child.set_flattening_succeeded(false);
                let human = child.url_for_humans();
                child.add_flattening_failure_reason(&format!("Recursive @import of {human}"));
            } else {
                result = true;
            }
        }
        result
    }

    /// Rolls up flattened text (`RollUpContents`).
    pub fn roll_up_contents(&mut self, server: &ServerContext) {
        if !self.minified_contents.is_empty() {
            return;
        }
        if self.stylesheet.is_none() {
            if self.input_contents.is_empty() {
                return;
            } else if !self.parse(server) {
                self.minified_contents = self.input_contents.clone();
                return;
            }
        }
        for index in 0..self.children.len() {
            if !self.flattening_succeeded {
                break;
            }
            let (ok, reason) = {
                let child = &self.children[index];
                (
                    child.flattening_succeeded,
                    child.flattening_failure_reason.clone(),
                )
            };
            self.flattening_succeeded &= ok;
            self.add_flattening_failure_reason(&reason);
            self.children[index].flattening_failure_reason.clear();
        }
        for index in 0..self.children.len() {
            if self.unparseable_detected {
                break;
            }
            self.unparseable_detected = self.children[index].unparseable_detected;
        }
        for index in 0..self.children.len() {
            if !self.flattening_succeeded {
                break;
            }
            self.children[index].roll_up_contents(server);
            let (ok, reason) = {
                let child = &self.children[index];
                (
                    child.flattening_succeeded,
                    child.flattening_failure_reason.clone(),
                )
            };
            self.flattening_succeeded &= ok;
            self.add_flattening_failure_reason(&reason);
            self.children[index].flattening_failure_reason.clear();
        }
        if !self.flattening_succeeded {
            self.children.clear();
            let sheet = self.stylesheet.as_ref().expect("parsed sheet");
            let mut minifier = Minifier::new(false);
            minifier.minify_stylesheet(sheet);
            let (out, ok) = minifier.finish();
            if ok {
                self.minified_contents = out;
            } else {
                self.minified_contents = self.input_contents.clone();
            }
            return;
        }
        for index in 0..self.children.len() {
            let contents = self.children[index].minified_contents.clone();
            self.minified_contents.extend_from_slice(&contents);
        }
        // `@charset`/`@import` go away on success, but save them until
        // the minify and limit checks pass.
        let (saved_charsets, saved_imports) = {
            let sheet = self.stylesheet.as_mut().expect("parsed sheet");
            (
                std::mem::take(sheet.charsets_mut()),
                std::mem::take(sheet.imports_mut()),
            )
        };
        let mut minifier = Minifier::new(false);
        minifier.minify_stylesheet(self.stylesheet.as_ref().expect("parsed sheet"));
        let (out, minified_ok) = minifier.finish();
        if minified_ok {
            self.minified_contents.extend_from_slice(&out);
        }
        if !minified_ok {
            server.variable(FLATTEN_MINIFY_FAILED).add(1);
            self.flattening_succeeded = false;
            let human = self.url_for_humans();
            self.add_flattening_failure_reason(&format!("Minification failed for {human}"));
        } else if self.flattened_result_limit > 0 {
            let size = self.minified_contents.len() as i64;
            if size >= self.flattened_result_limit {
                server.variable(FLATTEN_LIMIT_EXCEEDED).add(1);
                self.flattening_succeeded = false;
                self.add_flattening_failure_reason(&format!(
                    "Flattening limit ({}) exceeded ({size})",
                    self.flattened_result_limit
                ));
            }
        }
        if !self.flattening_succeeded {
            self.children.clear();
            let sheet = self.stylesheet.as_mut().expect("parsed sheet");
            *sheet.charsets_mut() = saved_charsets;
            *sheet.imports_mut() = saved_imports;
            self.minified_contents.clear();
            let mut minifier = Minifier::new(false);
            minifier.minify_stylesheet(self.stylesheet.as_ref().expect("parsed sheet"));
            let (out, ok) = minifier.finish();
            if minified_ok && ok {
                self.minified_contents = out;
            } else {
                self.minified_contents = self.input_contents.clone();
            }
        }
    }

    /// Rolls up the merged stylesheet (`RollUpStylesheets`).
    pub fn roll_up_stylesheets(&mut self, server: &ServerContext) -> bool {
        if self.stylesheet.is_none() {
            if self.input_contents.is_empty() {
                return true;
            } else if !self.parse(server) {
                return false;
            } else if {
                let sheet = self.stylesheet.as_ref().expect("parsed sheet");
                !sheet.charsets().is_empty() || !sheet.imports().is_empty()
            } {
                self.flattening_succeeded = false;
            }
        }
        for index in 0..self.children.len() {
            if !self.flattening_succeeded {
                break;
            }
            let (ok, reason) = {
                let child = &self.children[index];
                (
                    child.flattening_succeeded,
                    child.flattening_failure_reason.clone(),
                )
            };
            self.flattening_succeeded &= ok;
            self.add_flattening_failure_reason(&reason);
            self.children[index].flattening_failure_reason.clear();
        }
        for index in 0..self.children.len() {
            if self.unparseable_detected {
                break;
            }
            self.unparseable_detected = self.children[index].unparseable_detected;
        }
        for index in 0..self.children.len() {
            if !self.flattening_succeeded {
                break;
            }
            let ok = self.children[index].roll_up_stylesheets(server)
                && self.children[index].flattening_succeeded;
            if !ok {
                self.flattening_succeeded = false;
            }
            let reason = self.children[index].flattening_failure_reason.clone();
            self.add_flattening_failure_reason(&reason);
            self.children[index].flattening_failure_reason.clear();
        }
        if self.flattening_succeeded {
            let mut merged_rulesets = Vec::new();
            let mut merged_fonts = Vec::new();
            for index in (0..self.children.len()).rev() {
                let child = &mut self.children[index];
                if let Some(sheet) = child.stylesheet.as_mut() {
                    merged_rulesets.splice(..0, std::mem::take(sheet.rulesets_mut()));
                    merged_fonts.splice(..0, std::mem::take(sheet.font_faces_mut()));
                }
            }
            let sheet = self.stylesheet.as_mut().expect("parsed sheet");
            sheet.charsets_mut().clear();
            sheet.imports_mut().clear();
            sheet.rulesets_mut().splice(..0, merged_rulesets);
            sheet.font_faces_mut().splice(..0, merged_fonts);
        }
        self.children.clear();
        true
    }
}

impl Default for CssHierarchy {
    fn default() -> Self {
        Self::new()
    }
}

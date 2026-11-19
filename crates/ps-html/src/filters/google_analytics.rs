//! Synchronous Google Analytics to async (`GoogleAnalyticsFilter`).
//!
//! Finds synchronous `ga.js` loads (script `src` or `document.write`)
//! plus a `_gat._getTracker`/`_createTracker` init with no unhandled
//! calls, then rewrites: later loads vanish, the init call retargets a
//! shim, and the first load becomes the async glue script. Detection is
//! document-global across scripts; any tag, comment, CDATA, IE directive,
//! second characters node, or mid-script flush resets the match.
//!
//! TODO(step4): publish the page-load/rewrite counters as the
//! `google_analytics_page_load_count` / `google_analytics_rewritten_count`
//! statistics variables once the driver owns a registry.

use crate::element::{LeafKind, NodeId};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Tracker shim call (`kGaSnippetGetTracker`).
pub const GA_SNIPPET_GET_TRACKER: &[u8] = b"_modpagespeed_getRewriteTracker(";

/// Async glue prologue (`kGaSnippetPrefix`).
pub const GA_SNIPPET_PREFIX: &[u8] = b"var _gaq = _gaq || [];\n(function(){\n  function functionName(fn) {\n    var name = /\\W*function\\s+([\\w\\$]+)\\(/.exec(fn);\n    if (!name)\n      return 'No name';\n    return name[1];\n  }\n  var nameSpace = '_gat';\n  var existingGat = window[nameSpace];\n  if (existingGat && typeof existingGat['_getTracker'] == 'function') {\n    return;\n  }\n\n  var gaqAccounts = [];\n  function setAccount(acct, prefix) {\n    if (gaqAccounts[prefix] != acct) {\n      gaqAccounts[prefix] = acct;\n      _gaq.push([prefix + '_setAccount', acct]);\n    }\n  }\n\n  window['_modpagespeed_getRewriteTracker'] = function(tracker_acct,\n                                                       tracker_name) {\n    var prefix = tracker_name ? tracker_name + '.' : '';\n\n    function deferTrackerFunc(fn) {\n      return function() {\n        setAccount(tracker_acct, prefix);\n        var pushArgs = [fn];\n        [].push.apply(pushArgs, arguments);\n        _gaq.push(pushArgs);\n      };\n    }\n    var pageTrackerMethodNames = [\n";

/// Async glue epilogue (`kGaSnippetSuffix`).
pub const GA_SNIPPET_SUFFIX: &[u8] = b"    ];\n    var pageTracker = {\n      _initData: function() {},\n    };\n    for (var i=pageTrackerMethodNames.length; i--;) {\n      var n = pageTrackerMethodNames[i];\n      pageTracker[n] = deferTrackerFunc(prefix + n);\n    }\n    return pageTracker;\n  };\n\n  var ga = document.createElement('script');\n  ga.type = 'text/javascript'; ga.async = true;\n  ga.src = ('https:' == document.location.protocol ? 'https://ssl' :\n            'http://www') + '.google-analytics.com/ga.js';\n  var s = document.getElementsByTagName('script')[0];\n  s.parentNode.insertBefore(ga, s);\n})();\n";

/// Recognized `ga.js` load suffix.
const GA_JS_URL_SUFFIX: &[u8] = b"google-analytics.com/ga.js";
/// `document.write` load opener.
const GA_JS_DOCUMENT_WRITE_START: &[u8] = b"document.write(";
/// `document.write` load closer.
const GA_JS_DOCUMENT_WRITE_END: &[u8] = b"%3C/script%3E\"));";
/// Tracker init calls.
const GA_JS_GET_TRACKER: &[u8] = b"_gat._getTracker(";
/// Tracker create calls (accepted as init too).
const GA_JS_CREATE_TRACKER: &[u8] = b"_gat._createTracker(";
/// Lookbehind window for the `document.write` opener.
const MAX_WRITE_DISTANCE: usize = 80;

/// Default forwarded methods (parsed out of `ga.js` docs).
const DEFAULT_GLUE_METHODS: &[&str] = &[
    "_trackPageview",
    "_trackEvent",
    "_trackTrans",
    "_addIgnoredOrganic",
    "_addIgnoredRef",
    "_addItem",
    "_addOrganic",
    "_addTrans",
    "_clearIgnoredOrganic",
    "_clearIgnoredRef",
    "_clearOrganic",
    "_clearXKey",
    "_clearXValue",
    "_cookiePathCopy",
    "_deleteCustomVar",
    "_link",
    "_linkByPost",
    "_sendXEvent",
    "_setAllowAnchor",
    "_setAllowHash",
    "_setAllowLinker",
    "_setAutoTrackOutbound",
    "_setCampCIdKey",
    "_setCampContentKey",
    "_setCampIdKey",
    "_setCampMediumKey",
    "_setCampNOKey",
    "_setCampNameKey",
    "_setCampSourceKey",
    "_setCampTermKey",
    "_setCampaignCookieTimeout",
    "_setCampaignTrack",
    "_setClientInfo",
    "_setCookiePath",
    "_setCookiePersistence",
    "_setCookieTimeout",
    "_setCustomVar",
    "_setDetectFlash",
    "_setDetectTitle",
    "_setDomainName",
    "_setHrefExamineLimit",
    "_setLocalGifPath",
    "_setLocalRemoteServerMode",
    "_setLocalServerMode",
    "_setMaxCustomVariables",
    "_setNamespace",
    "_setReferrerOverride",
    "_setRemoteServerMode",
    "_setSampleRate",
    "_setSessionCookieTimeout",
    "_setSessionTimeout",
    "_setTrackOutboundSubdomains",
    "_setTrans",
    "_setTransactionDelim",
    "_setVar",
    "_setVisitorCookieTimeout",
    "_setXKey",
    "_setXValue",
];

/// Default rewrite-vetoing methods (getters + async-only calls).
const DEFAULT_UNHANDLED_METHODS: &[&str] = &[
    "_anonymizeIp",
    "_createEventTracker",
    "_createXObj",
    "_require",
    "_visitCode",
    "_get",
    "_getAccount",
    "_getClientInfo",
    "_getDetectFlash",
    "_getDetectTitle",
    "_getLinkerUrl",
    "_getLocalGifPath",
    "_getName",
    "_getServiceMode",
    "_getTrackerByName",
    "_getVersion",
    "_getVisitorCustomVar",
    "_getXKey",
    "_getXValue",
    "_setAccount",
];

/// One pending script edit (`ScriptEditor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorType {
    /// Whole `<script src>` load (deleted or prepended).
    ScriptSrcLoad,
    /// Inline `document.write` load span.
    DocWriteLoad,
    /// Init call span.
    Init,
}

/// One pending script edit.
#[derive(Debug, Clone, Copy)]
struct ScriptEditor {
    script: NodeId,
    characters: Option<NodeId>,
    pos: Option<usize>,
    len: usize,
    editor_type: EditorType,
}

impl ScriptEditor {
    /// Splices a replacement (`NewContents`): whole contents when the
    /// span is absent, else the `[pos, pos+len)` window.
    fn new_contents(&self, dom: &Dom, replacement: &[u8]) -> Vec<u8> {
        let Some(chars) = self.characters else {
            return replacement.to_vec();
        };
        let Some(pos) = self.pos else {
            return replacement.to_vec();
        };
        let old = dom.leaf_contents(chars);
        let mut out = Vec::with_capacity(old.len() + replacement.len());
        out.extend_from_slice(&old[..pos]);
        out.extend_from_slice(replacement);
        out.extend_from_slice(&old[pos + self.len..]);
        out
    }
}

/// Rewrites sync GA loads as async (`GoogleAnalyticsFilter`).
#[derive(Debug)]
pub struct GoogleAnalyticsFilter {
    glue_methods: Vec<String>,
    unhandled_methods: Vec<String>,
    script_element: Option<NodeId>,
    script_characters: Option<NodeId>,
    is_load_found: bool,
    is_init_found: bool,
    editors: Vec<ScriptEditor>,
    page_load_count: i64,
    rewritten_count: i64,
}

impl GoogleAnalyticsFilter {
    /// Builds the filter with default method lists.
    #[must_use]
    pub fn new() -> Self {
        GoogleAnalyticsFilter {
            glue_methods: DEFAULT_GLUE_METHODS
                .iter()
                .map(|method| method.to_string())
                .collect(),
            unhandled_methods: DEFAULT_UNHANDLED_METHODS
                .iter()
                .map(|method| method.to_string())
                .collect(),
            script_element: None,
            script_characters: None,
            is_load_found: false,
            is_init_found: false,
            editors: Vec::new(),
            page_load_count: 0,
            rewritten_count: 0,
        }
    }

    /// Builds the filter with custom method lists (tests).
    #[must_use]
    pub fn with_methods(glue: Vec<String>, unhandled: Vec<String>) -> Self {
        let mut filter = GoogleAnalyticsFilter::new();
        filter.glue_methods = glue;
        filter.unhandled_methods = unhandled;
        filter
    }

    /// Documents seen (`google_analytics_page_load_count`).
    #[must_use]
    pub fn page_load_count(&self) -> i64 {
        self.page_load_count
    }

    /// Documents rewritten (`google_analytics_rewritten_count`).
    #[must_use]
    pub fn rewritten_count(&self) -> i64 {
        self.rewritten_count
    }

    /// Drops all match state (`ResetFilter`).
    fn reset_filter(&mut self) {
        self.script_element = None;
        self.script_characters = None;
        self.is_init_found = false;
        self.is_load_found = false;
        self.editors.clear();
    }

    /// Matches a `document.write` load (`MatchSyncLoad`): the `ga.js`
    /// suffix with its opener at most 80 bytes back and the closer after.
    fn match_sync_load(contents: &[u8]) -> Option<(usize, usize)> {
        let url_pos = find_subslice(contents, GA_JS_URL_SUFFIX)?;
        let window_start = url_pos.saturating_sub(MAX_WRITE_DISTANCE);
        let window = &contents[window_start..url_pos];
        let write_pos = find_subslice(window, GA_JS_DOCUMENT_WRITE_START)? + window_start;
        let end_search = url_pos + GA_JS_URL_SUFFIX.len();
        let end_pos = find_subslice(&contents[end_search..], GA_JS_DOCUMENT_WRITE_END)?
            + end_search
            + GA_JS_DOCUMENT_WRITE_END.len();
        Some((write_pos, end_pos - write_pos))
    }

    /// Matches a tracker init call (`MatchSyncInit`).
    fn match_sync_init(contents: &[u8], start: usize) -> Option<(usize, usize)> {
        let start = start.min(contents.len());
        if let Some(pos) = find_subslice_from(contents, GA_JS_GET_TRACKER, start) {
            return Some((pos, GA_JS_GET_TRACKER.len()));
        }
        find_subslice_from(contents, GA_JS_CREATE_TRACKER, start)
            .map(|pos| (pos, GA_JS_CREATE_TRACKER.len()))
    }

    /// Whether any unhandled call hides in the contents
    /// (`MatchUnhandledCalls`): each `._` occurrence prefix-checked
    /// against the veto list, then only spaces may precede `(`.
    fn match_unhandled_calls(&self, contents: &[u8]) -> bool {
        let mut rest = contents;
        while let Some(candidate) = find_subslice(rest, b"._") {
            rest = &rest[candidate + 1..];
            for method in &self.unhandled_methods {
                if !rest.starts_with(method.as_bytes()) {
                    continue;
                }
                for byte in &rest[method.len()..] {
                    if *byte == b'(' {
                        return true;
                    } else if !is_html_space(*byte) {
                        break;
                    }
                }
            }
        }
        false
    }

    /// Scans one closed script for loads and inits (`FindRewritableScripts`).
    fn find_rewritable_scripts(&mut self, dom: &mut Dom) {
        let Some(script) = self.script_element else {
            return;
        };
        if !dom.is_rewritable(script) {
            return;
        }
        let src = match dom.find_attribute(script, Keyword::Src) {
            Some(index) => dom.decoded_attribute(script, index).0.unwrap_or_default(),
            None => Vec::new(),
        };
        if !src.is_empty() {
            if src.ends_with(GA_JS_URL_SUFFIX) {
                self.is_load_found = true;
                self.editors.push(ScriptEditor {
                    script,
                    characters: self.script_characters,
                    pos: None,
                    len: 0,
                    editor_type: EditorType::ScriptSrcLoad,
                });
            }
            return;
        }
        let Some(chars) = self.script_characters else {
            return;
        };
        let contents = dom.leaf_contents(chars);
        if contents.is_empty() {
            return;
        }
        let mut start = 0;
        if let Some((pos, len)) = Self::match_sync_load(&contents) {
            self.is_load_found = true;
            self.editors.push(ScriptEditor {
                script,
                characters: Some(chars),
                pos: Some(pos),
                len,
                editor_type: EditorType::DocWriteLoad,
            });
            start = pos + len;
        }
        if self.is_load_found {
            if let Some((pos, len)) = Self::match_sync_init(&contents, start) {
                self.is_init_found = true;
                self.editors.push(ScriptEditor {
                    script,
                    characters: Some(chars),
                    pos: Some(pos),
                    len,
                    editor_type: EditorType::Init,
                });
            }
        }
        if self.is_init_found && self.match_unhandled_calls(&contents) {
            self.reset_filter();
        }
    }

    /// Builds the async glue script (`GetSyncToAsyncScript`).
    fn sync_to_async_script(&self) -> Vec<u8> {
        let mut out = GA_SNIPPET_PREFIX.to_vec();
        let last = self.glue_methods.len() - 1;
        for (index, method) in self.glue_methods.iter().enumerate() {
            out.extend_from_slice(b"        '");
            out.extend_from_slice(method.as_bytes());
            if index == last {
                out.extend_from_slice(b"'\n");
            } else {
                out.extend_from_slice(b"',\n");
            }
        }
        out.extend_from_slice(GA_SNIPPET_SUFFIX);
        out
    }

    /// Applies the edits (`RewriteAsAsync`): later loads vanish, the init
    /// retargets the shim, and the first load becomes the glue script.
    fn rewrite_as_async(&mut self, dom: &mut Dom) -> bool {
        if !self.is_init_found || !self.is_load_found {
            return false;
        }
        let Some(first) = self.editors.first().copied() else {
            return false;
        };
        if !dom.is_rewritable(first.script) {
            return false;
        }
        debug_assert!(matches!(
            first.editor_type,
            EditorType::ScriptSrcLoad | EditorType::DocWriteLoad
        ));
        if !matches!(
            first.editor_type,
            EditorType::ScriptSrcLoad | EditorType::DocWriteLoad
        ) {
            return false;
        }
        for index in (1..self.editors.len()).rev() {
            let editor = self.editors[index];
            match editor.editor_type {
                EditorType::ScriptSrcLoad => {
                    dom.delete_node(editor.script);
                }
                EditorType::DocWriteLoad => {
                    let replacement = editor.new_contents(dom, b"");
                    self.replace_characters(dom, &editor, &replacement);
                }
                EditorType::Init => {
                    let replacement = editor.new_contents(dom, GA_SNIPPET_GET_TRACKER);
                    self.replace_characters(dom, &editor, &replacement);
                }
            }
        }
        match first.editor_type {
            EditorType::ScriptSrcLoad => {
                let glue = self.sync_to_async_script();
                let leaf = dom.new_leaf(Some(first.script), LeafKind::Characters, glue);
                dom.prepend_child(first.script, leaf);
                dom.delete_attribute(first.script, Keyword::Src);
            }
            _ => {
                let glue = self.sync_to_async_script();
                let replacement = first.new_contents(dom, &glue);
                self.replace_characters(dom, &first, &replacement);
            }
        }
        true
    }

    /// Swaps a characters node for fresh contents (`ReplaceNode`).
    fn replace_characters(&self, dom: &mut Dom, editor: &ScriptEditor, contents: &[u8]) {
        if let Some(chars) = editor.characters {
            let leaf = dom.new_leaf(Some(editor.script), LeafKind::Characters, contents.to_vec());
            dom.insert_node_before_node(chars, leaf);
            dom.delete_node(chars);
        }
    }
}

impl Default for GoogleAnalyticsFilter {
    fn default() -> Self {
        GoogleAnalyticsFilter::new()
    }
}

/// First occurrence of a needle (`StringPiece::find`).
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// First occurrence at or after an offset.
fn find_subslice_from(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    find_subslice(&haystack[start.min(haystack.len())..], needle)
        .map(|pos| pos + start.min(haystack.len()))
}

/// HTML whitespace (`IsHtmlSpace`).
fn is_html_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
}

impl HtmlFilter for GoogleAnalyticsFilter {
    fn name(&self) -> &'static str {
        "GoogleAnalytics"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.reset_filter();
        self.page_load_count += 1;
    }

    fn end_document(&mut self, dom: &mut Dom, _lexer: &Lexer) {
        if self.is_load_found && self.is_init_found && self.rewrite_as_async(dom) {
            self.rewritten_count += 1;
        }
        self.reset_filter();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        // No tags allowed inside a script element.
        if self.script_element.is_some() {
            self.reset_filter();
        }
        if dom.element_keyword(id) == Keyword::Script {
            self.script_element = Some(id);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.script_element.is_none() {
            return;
        }
        if Some(id) != self.script_element {
            self.reset_filter();
            return;
        }
        self.find_rewritable_scripts(dom);
        self.script_element = None;
        self.script_characters = None;
    }

    fn characters(&mut self, _dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.script_element.is_none() {
            return;
        }
        if self.script_characters.is_some() {
            self.reset_filter();
        } else {
            self.script_characters = Some(id);
        }
    }

    fn comment(&mut self, _dom: &mut Dom, _lexer: &Lexer, _id: NodeId) {
        if self.script_element.is_some() {
            self.reset_filter();
        }
    }

    fn cdata(&mut self, _dom: &mut Dom, _lexer: &Lexer, _id: NodeId) {
        if self.script_element.is_some() {
            self.reset_filter();
        }
    }

    fn ie_directive(&mut self, _dom: &mut Dom, _lexer: &Lexer, _id: NodeId) {
        if self.script_element.is_some() {
            self.reset_filter();
        }
    }

    fn flush(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        if self.script_element.is_some() {
            self.reset_filter();
        }
    }
}

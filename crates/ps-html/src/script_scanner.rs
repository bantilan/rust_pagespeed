//! Script element classification (`ScriptTagScanner`).
//!
//! Shared rewriter helper: decides whether a `<script>` element is
//! JavaScript (from `type`/`language` attributes), which execution-mode
//! flags it carries, and whether it has an integrity attribute.

use crate::element::NodeId;
use crate::name::Keyword;
use crate::parser::Dom;

/// Script classification (`ScriptClassification`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptClassification {
    NonScript,
    UnknownScript,
    JavaScript,
}

/// Synchronous execution (no flags, `kExecuteSync`).
pub const EXECUTE_SYNC: u32 = 0;
/// Deferred execution (`kExecuteDefer`).
pub const EXECUTE_DEFER: u32 = 1;
/// Async execution (`kExecuteAsync`).
pub const EXECUTE_ASYNC: u32 = 2;
/// IE `for`/`event` execution (`kExecuteForEvent`).
pub const EXECUTE_FOR_EVENT: u32 = 4;

/// JavaScript mimetypes, byte-sorted (`javascript_mimetypes`).
const JAVASCRIPT_MIMETYPES: &[&[u8]] = &[
    b"application/ecmascript",
    b"application/javascript",
    b"application/x-ecmascript",
    b"application/x-javascript",
    b"text/ecmascript",
    b"text/javascript",
    b"text/javascript1.0",
    b"text/javascript1.1",
    b"text/javascript1.2",
    b"text/javascript1.3",
    b"text/javascript1.4",
    b"text/javascript1.5",
    b"text/jscript",
    b"text/livescript",
    b"text/x-ecmascript",
    b"text/x-javascript",
];

fn is_js_mime(normalized: &[u8]) -> bool {
    JAVASCRIPT_MIMETYPES.binary_search(&normalized).is_ok()
}

/// Trims ASCII whitespace and lowercases (`Normalized`).
fn normalized(value: &[u8]) -> Vec<u8> {
    value
        .trim_ascii()
        .iter()
        .map(|byte| byte.to_ascii_lowercase())
        .collect()
}

/// Script element classifier (stateless; the C++ constructor only
/// debug-checks the mimetype table order).
#[derive(Debug, Default)]
pub struct ScriptTagScanner;

impl ScriptTagScanner {
    /// Classifies an element, returning the `src` attribute index for
    /// scripts (`ParseScriptElement`).
    pub fn parse_script_element(
        dom: &mut Dom,
        id: NodeId,
    ) -> (ScriptClassification, Option<usize>) {
        if dom.element_keyword(id) != Keyword::Script {
            return (ScriptClassification::NonScript, None);
        }
        let src = dom.find_attribute(id, Keyword::Src);

        // `type` decides first; a valueless `type` falls back to `language`.
        let mut check_lang_attr = false;
        let mut lang = ScriptClassification::JavaScript;
        match dom.find_attribute(id, Keyword::Type) {
            None => check_lang_attr = true,
            Some(index) => {
                let (decoded, error) = decoded_attr(dom, id, index);
                if error {
                    lang = ScriptClassification::UnknownScript;
                } else if let Some(value) = decoded {
                    if !(value.is_empty() || is_js_mime(&normalized(&value))) {
                        lang = ScriptClassification::UnknownScript;
                    }
                } else {
                    check_lang_attr = true;
                }
            }
        }

        // Without `type`, the deprecated `language` decides: empty or
        // missing means JS; matching is case-insensitive but whitespace
        // is significant.
        if check_lang_attr {
            match dom.find_attribute(id, Keyword::Language) {
                None => lang = ScriptClassification::JavaScript,
                Some(index) => {
                    let (decoded, error) = decoded_attr(dom, id, index);
                    if error {
                        lang = ScriptClassification::UnknownScript;
                    } else if let Some(value) = decoded {
                        let lowered: Vec<u8> =
                            value.iter().map(|byte| byte.to_ascii_lowercase()).collect();
                        let mut mime = b"text/".to_vec();
                        mime.extend_from_slice(&lowered);
                        if !(lowered.is_empty() || is_js_mime(&mime)) {
                            lang = ScriptClassification::UnknownScript;
                        }
                    } else {
                        lang = ScriptClassification::JavaScript;
                    }
                }
            }
        }

        (lang, src)
    }

    /// Execution-mode flags (`ExecutionMode`).
    pub fn execution_mode(dom: &mut Dom, id: NodeId) -> u32 {
        let mut flags = EXECUTE_SYNC;
        if dom.find_attribute(id, Keyword::Async).is_some() {
            flags |= EXECUTE_ASYNC;
        }
        if dom.find_attribute(id, Keyword::Defer).is_some() {
            flags |= EXECUTE_DEFER;
        }
        // IE `for`/`event` scripts run at hard-to-describe times unless
        // they name the standard window-onload pair.
        if let (Some(for_index), Some(event_index)) = (
            dom.find_attribute(id, Keyword::For),
            dom.find_attribute(id, Keyword::Event),
        ) {
            let (for_value, _) = decoded_attr(dom, id, for_index);
            let (event_value, _) = decoded_attr(dom, id, event_index);
            if normalized(for_value.as_deref().unwrap_or(b"")) != b"window" {
                flags |= EXECUTE_FOR_EVENT;
            }
            let event = normalized(event_value.as_deref().unwrap_or(b""));
            if event != b"onload" && event != b"onload()" {
                flags |= EXECUTE_FOR_EVENT;
            }
        }
        flags
    }

    /// True when any attribute is named `integrity` (case-insensitive).
    pub fn has_integrity_attribute(dom: &Dom, id: NodeId) -> bool {
        dom.element_attributes(id)
            .iter()
            .any(|attr| attr.name_str().eq_ignore_ascii_case(b"integrity"))
    }
}

/// Decoded value plus decoding-error flag for one attribute.
fn decoded_attr(dom: &mut Dom, id: NodeId, index: usize) -> (Option<Vec<u8>>, bool) {
    let attribute = &mut dom.element_attributes_mut(id)[index];
    let decoded = attribute.decoded_value_or_null().map(<[u8]>::to_vec);
    (decoded, attribute.decoding_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mimetypes_sorted() {
        // Mirrors the constructor DCHECK on table order.
        let mut sorted = JAVASCRIPT_MIMETYPES.to_vec();
        sorted.sort();
        assert_eq!(sorted, JAVASCRIPT_MIMETYPES);
    }
}

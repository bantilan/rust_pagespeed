//! Noscript fallback (`SupportNoscriptFilter`).
//!
//! Prepends a noscript redirect (back to `?PageSpeed=noscript`) to the
//! first `<body>` whenever a script-requiring filter is active. The
//! escaped document URL and the enablement verdict both come from the
//! driver (URL layer and UA capabilities, respectively).

use ps_config::filter::{Filter, FilterState, REQUIRES_SCRIPT_EXECUTION};

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Noscript redirect template (`kNoScriptRedirectFormatter`, URL twice).
const NOSCRIPT_REDIRECT_FORMAT: (&[u8], &[u8], &[u8]) = (
    b"<noscript><meta HTTP-EQUIV=\"refresh\" content=\"0;url='",
    b"'\" /><style><!--table,div,span,font,p{display:none} --></style>\
      <div style=\"display:block\">Please click <a href=\"",
    b"\">here</a> if you are not redirected within a few seconds.</div></noscript>",
);

/// Whether any enabled script-requiring filter needs the fallback
/// (`IsAnyFilterRequiringScriptExecutionEnabled`).
#[must_use]
pub fn should_insert_noscript(
    filters: &FilterState,
    supports_js_defer: bool,
    supports_image_inlining: bool,
) -> bool {
    for filter in REQUIRES_SCRIPT_EXECUTION {
        if !filters.enabled(*filter) {
            continue;
        }
        let active = match filter {
            Filter::DeferIframe | Filter::DeferJavascript => supports_js_defer,
            Filter::DedupInlinedImages
            | Filter::DelayImages
            | Filter::LazyloadImages
            | Filter::LocalStorageCache => supports_image_inlining,
            Filter::Mobilize => false,
            _ => true,
        };
        if active {
            return true;
        }
    }
    false
}

/// Prepends the noscript redirect to the first body.
#[derive(Debug)]
pub struct SupportNoscriptFilter {
    url: Vec<u8>,
    should_insert: bool,
}

impl SupportNoscriptFilter {
    /// Builds the filter (escaped document URL + enablement verdict).
    #[must_use]
    pub fn new(escaped_url: &[u8], should_insert: bool) -> Self {
        SupportNoscriptFilter {
            url: escaped_url.to_vec(),
            should_insert,
        }
    }
}

impl HtmlFilter for SupportNoscriptFilter {
    fn name(&self) -> &'static str {
        "SupportNoscript"
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.should_insert && dom.element_keyword(id) == Keyword::Body {
            let (open, middle, close) = NOSCRIPT_REDIRECT_FORMAT;
            let mut markup = Vec::with_capacity(open.len() + middle.len() + close.len());
            markup.extend_from_slice(open);
            markup.extend_from_slice(&self.url);
            markup.extend_from_slice(middle);
            markup.extend_from_slice(&self.url);
            markup.extend_from_slice(close);
            let leaf = dom.new_leaf(Some(id), crate::element::LeafKind::Characters, markup);
            dom.prepend_child(id, leaf);
            self.should_insert = false;
        }
    }
}

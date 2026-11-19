//! Attribute elision (`ElideAttributesFilter`).
//!
//! Drops values from boolean attributes (`<option selected="selected">` →
//! `<option selected>`, except in XHTML) and removes attributes carrying
//! their default value (`<form method="get">` → `<form>`).

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Boolean (tag, attribute) pairs (`kBooleanAttrs`).
const BOOLEAN_ATTRS: &[(Keyword, Keyword)] = &[
    (Keyword::Area, Keyword::Nohref),
    (Keyword::Audio, Keyword::Autoplay),
    (Keyword::Audio, Keyword::Controls),
    (Keyword::Audio, Keyword::Loop),
    (Keyword::Audio, Keyword::Muted),
    (Keyword::Button, Keyword::Autofocus),
    (Keyword::Button, Keyword::Disabled),
    (Keyword::Command, Keyword::Checked),
    (Keyword::Command, Keyword::Disabled),
    (Keyword::Details, Keyword::Open),
    (Keyword::Form, Keyword::Novalidate),
    (Keyword::Frame, Keyword::Noresize),
    (Keyword::Keygen, Keyword::Autofocus),
    (Keyword::Keygen, Keyword::Disabled),
    (Keyword::Iframe, Keyword::Seamless),
    (Keyword::Img, Keyword::Ismap),
    (Keyword::Input, Keyword::Autofocus),
    (Keyword::Input, Keyword::Checked),
    (Keyword::Input, Keyword::Defaultchecked),
    (Keyword::Input, Keyword::Disabled),
    (Keyword::Input, Keyword::Formnovalidate),
    (Keyword::Input, Keyword::Indeterminate),
    (Keyword::Input, Keyword::Multiple),
    (Keyword::Input, Keyword::Readonly),
    (Keyword::Input, Keyword::Required),
    (Keyword::Object, Keyword::Declare),
    (Keyword::Ol, Keyword::Reversed),
    (Keyword::Optgroup, Keyword::Disabled),
    (Keyword::Option, Keyword::Defaultselected),
    (Keyword::Option, Keyword::Disabled),
    (Keyword::Option, Keyword::Selected),
    (Keyword::Script, Keyword::Async),
    (Keyword::Script, Keyword::Defer),
    (Keyword::Select, Keyword::Autofocus),
    (Keyword::Select, Keyword::Disabled),
    (Keyword::Select, Keyword::Multiple),
    (Keyword::Select, Keyword::Required),
    (Keyword::Style, Keyword::Scoped),
    (Keyword::Textarea, Keyword::Autofocus),
    (Keyword::Textarea, Keyword::Disabled),
    (Keyword::Textarea, Keyword::Readonly),
    (Keyword::Textarea, Keyword::Required),
    (Keyword::Video, Keyword::Autoplay),
    (Keyword::Video, Keyword::Controls),
    (Keyword::Video, Keyword::Loop),
    (Keyword::Video, Keyword::Muted),
];

/// Default-valued (tag, attribute, value, requires-HTML5) rows
/// (`kDefaultList`). `button/type=submit` stays out (IE ignores it).
const DEFAULT_ATTRS: &[(Keyword, Keyword, &[u8], bool)] = &[
    (Keyword::A, Keyword::Shape, b"rect", false),
    (Keyword::Area, Keyword::Shape, b"rect", false),
    (Keyword::Command, Keyword::Type, b"command", true),
    (Keyword::Form, Keyword::Autocomplete, b"on", true),
    (
        Keyword::Form,
        Keyword::Enctype,
        b"application/x-www-form-urlencoded",
        false,
    ),
    (Keyword::Form, Keyword::Method, b"get", false),
    (Keyword::Frame, Keyword::Frameborder, b"1", false),
    (Keyword::Frame, Keyword::Scrolling, b"auto", false),
    (Keyword::Iframe, Keyword::Frameborder, b"1", false),
    (Keyword::Iframe, Keyword::Scrolling, b"auto", false),
    (Keyword::Keygen, Keyword::Keytype, b"rsa", true),
    (Keyword::Menu, Keyword::Type, b"list", true),
    (Keyword::Param, Keyword::Valuetype, b"data", false),
    (Keyword::Script, Keyword::Language, b"javascript", true),
    (Keyword::Script, Keyword::Type, b"text/javascript", true),
    (Keyword::Source, Keyword::Media, b"all", true),
    (Keyword::Style, Keyword::Type, b"text/css", true),
    (Keyword::Style, Keyword::Media, b"all", true),
    (Keyword::Textarea, Keyword::Wrap, b"soft", true),
    (Keyword::Col, Keyword::Span, b"1", false),
    (Keyword::Colgroup, Keyword::Span, b"1", false),
    (Keyword::Td, Keyword::Colspan, b"1", false),
    (Keyword::Td, Keyword::Rowspan, b"1", false),
    (Keyword::Th, Keyword::Colspan, b"1", false),
    (Keyword::Th, Keyword::Rowspan, b"1", false),
];

/// Elides boolean and default-valued attributes.
#[derive(Debug, Default)]
pub struct ElideAttributesFilter;

impl HtmlFilter for ElideAttributesFilter {
    fn name(&self) -> &'static str {
        "ElideAttributes"
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        let doctype = lexer.doctype();
        let keyword = dom.element_keyword(id);

        if !doctype.is_xhtml() {
            // Boolean attributes lose their value (but keep the name).
            for attribute in dom.element_attributes_mut(id).iter_mut() {
                if attribute.escaped_value().is_some()
                    && BOOLEAN_ATTRS.contains(&(keyword, attribute.keyword()))
                {
                    attribute.set_escaped_value(None);
                }
            }
        }

        // Default-valued attributes are removed wholesale.
        let mut index = 0;
        loop {
            if index >= dom.element_attributes(id).len() {
                break;
            }
            let remove = {
                let attribute = &mut dom.element_attributes_mut(id)[index];
                let attr_keyword = attribute.keyword();
                match attribute.decoded_value_or_null() {
                    Some(decoded) => DEFAULT_ATTRS.iter().any(|(tag, attr, value, requires_v5)| {
                        *tag == keyword
                            && *attr == attr_keyword
                            && (!requires_v5 || doctype.is_version_5())
                            && decoded.eq_ignore_ascii_case(value)
                    }),
                    None => false,
                }
            };
            if remove {
                dom.element_attributes_mut(id).remove(index);
            } else {
                index += 1;
            }
        }
    }
}

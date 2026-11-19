//! Srcset image candidates (`SrcSetSlot`, collections).
//!
//! Parses `srcset` attributes per the WHATWG algorithm, binds one
//! slot per candidate URL, and writes the attribute back once after
//! all candidates render (`Commit`). Collection sharing across
//! filters waits for multi-filter chains (each scan builds its own).

use std::cell::RefCell;
use std::rc::Rc;

use ps_http::google_url::{GoogleUrl, UrlRelativity};

use super::element::NodeId;
use super::parser::Dom;
use super::resource::ResourceHandle;
use super::resource_slot::{relativize_or_passthrough, SlotBase};

/// Candidate cap (`SrcSetSlotCollection::kMaxSlots`).
pub const MAX_SLOTS: usize = 256;

/// One parsed candidate (`ImageCandidate`).
#[derive(Debug, Clone, Default)]
pub struct SrcSetCandidate {
    /// Candidate URL text.
    pub url: String,
    /// Density/width descriptor (`1x`, `100w`, ...).
    pub descriptor: String,
}

/// Whether a byte is HTML whitespace.
fn is_html_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\x0C' | b'\r')
}

/// Parses candidates (`ParseSrcSet`, WHATWG embedded-content rules).
#[must_use]
pub fn parse_srcset(input: &[u8]) -> Vec<SrcSetCandidate> {
    let mut out = Vec::new();
    let mut rest = input;
    loop {
        // Strip leading whitespace, commas.
        while let Some((&first, tail)) = rest.split_first() {
            if is_html_space(first) || first == b',' {
                rest = tail;
            } else {
                break;
            }
        }
        if rest.is_empty() {
            break;
        }
        // The URL ends at whitespace.
        let url_end = rest.iter().position(|byte| is_html_space(*byte));
        let (url, after_url) = match url_end {
            Some(end) => (&rest[..end], &rest[end..]),
            None => (rest, &[][..]),
        };
        rest = after_url;
        // Trailing commas mean no descriptor.
        let mut url = url;
        let mut expect_descriptor = true;
        while let Some((&last, head)) = url.split_last() {
            if last == b',' {
                url = head;
                expect_descriptor = false;
            } else {
                break;
            }
        }
        let mut descriptor: &[u8] = &[];
        if expect_descriptor {
            let mut inside_paren = false;
            let mut pos = 0;
            while pos < rest.len() {
                let byte = rest[pos];
                if byte == b'(' {
                    inside_paren = true;
                } else if byte == b')' && inside_paren {
                    inside_paren = false;
                } else if byte == b',' && !inside_paren {
                    break;
                }
                pos += 1;
            }
            descriptor = &rest[..pos];
            rest = &rest[pos..];
            let start = descriptor.iter().position(|byte| !is_html_space(*byte));
            let end = descriptor.iter().rposition(|byte| !is_html_space(*byte));
            descriptor = match (start, end) {
                (Some(start), Some(end)) => &descriptor[start..=end],
                _ => &[],
            };
        }
        out.push(SrcSetCandidate {
            url: String::from_utf8_lossy(url).into_owned(),
            descriptor: String::from_utf8_lossy(descriptor).into_owned(),
        });
    }
    out
}

/// Serializes candidates (`Serialize`).
#[must_use]
pub fn serialize_srcset(candidates: &[SrcSetCandidate]) -> String {
    let mut result = String::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if index != 0 {
            result.push_str(", ");
        }
        result.push_str(&candidate.url);
        if !candidate.descriptor.is_empty() {
            result.push(' ');
            result.push_str(&candidate.descriptor);
        }
    }
    result
}

/// Shared candidate table (`SrcSetSlotCollection`, single-filter).
#[derive(Debug)]
pub struct SrcSetCollection {
    element: NodeId,
    attribute: usize,
    candidates: Vec<SrcSetCandidate>,
    dirty: bool,
}

impl SrcSetCollection {
    /// Parses the attribute into a table.
    #[must_use]
    pub fn new(element: NodeId, attribute: usize, value: &[u8]) -> Self {
        SrcSetCollection {
            element,
            attribute,
            candidates: parse_srcset(value),
            dirty: false,
        }
    }

    /// Borrows the candidates.
    #[must_use]
    pub fn candidates(&self) -> &[SrcSetCandidate] {
        &self.candidates
    }

    /// Records a rendered candidate URL (`set_url`).
    pub fn set_url(&mut self, index: usize, url: String) {
        if let Some(candidate) = self.candidates.get_mut(index) {
            candidate.url = url;
            self.dirty = true;
        }
    }

    /// Writes the attribute back when candidates rendered (`Commit`).
    pub fn commit(&mut self, dom: &mut Dom) {
        if self.dirty {
            let value = serialize_srcset(&self.candidates);
            dom.element_attributes_mut(self.element)[self.attribute].set_value(value.as_bytes());
            self.dirty = false;
        }
    }
}

/// One candidate's slot (`SrcSetSlot`).
#[derive(Debug)]
pub struct SrcSetSlot {
    base: SlotBase,
    collection: Rc<RefCell<SrcSetCollection>>,
    index: usize,
    element: NodeId,
    relativity: UrlRelativity,
    driver_id: String,
    begin_line: i32,
    end_line: i32,
}

impl SrcSetSlot {
    /// Builds the slot, snapshotting the candidate's relativity and
    /// the element's line numbers.
    #[must_use]
    pub fn new(
        resource: ResourceHandle,
        collection: Rc<RefCell<SrcSetCollection>>,
        index: usize,
        dom: &mut Dom,
        element: NodeId,
        driver_id: &str,
    ) -> Self {
        let relativity = GoogleUrl::find_relativity(&collection.borrow().candidates()[index].url);
        SrcSetSlot {
            base: SlotBase::new(Some(resource)),
            collection,
            index,
            element,
            relativity,
            driver_id: driver_id.to_string(),
            begin_line: dom.element_begin_line(element),
            end_line: dom.element_end_line(element),
        }
    }

    /// Borrows the image element.
    #[must_use]
    pub fn element(&self) -> NodeId {
        self.element
    }

    /// Borrows the shared state.
    #[must_use]
    pub fn base(&self) -> &SlotBase {
        &self.base
    }

    /// Mutably borrows the shared state.
    pub fn base_mut(&mut self) -> &mut SlotBase {
        &mut self.base
    }

    /// Records the rendered URL; the attribute writes back at commit
    /// (`Render`).
    pub fn render(&mut self, base_url: &GoogleUrl, options: &ps_config::options::RewriteOptions) {
        if self.base.disable_rendering() || self.base.preserve_urls() {
            return;
        }
        let url = self
            .base
            .resource()
            .expect("srcset slot rendered without a resource")
            .url()
            .to_string();
        let rendered = relativize_or_passthrough(options, &url, self.relativity, base_url);
        self.collection.borrow_mut().set_url(self.index, rendered);
    }

    /// Describes the slot for logs (`LocationString`).
    #[must_use]
    pub fn location_string(&self) -> String {
        let mut location = if self.begin_line == self.end_line {
            format!("{}:{}", self.driver_id, self.begin_line)
        } else {
            format!("{}:{}-{}", self.driver_id, self.begin_line, self.end_line)
        };
        location.push_str(" srcset entry for ");
        location.push_str(&self.collection.borrow().candidates()[self.index].descriptor);
        location
    }
}

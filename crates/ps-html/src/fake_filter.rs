//! Test rewrite servants (`FakeFilter`).
//!
//! Appends `:id` to input bytes and counts rewrites. Drives the
//! in-place serving tests; never used on the HTML path.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ps_http::content_type::ContentType;

use super::image_url_encoder::{ImageResourceContext, ImageUrlEncoder};
use super::rewrite_filter::{InPlaceOutput, RewriteFilter};
use super::server_context::ResourceDriver;

/// Test servant (`FakeFilter`).
pub struct FakeFilter {
    id: &'static str,
    enabled: Cell<bool>,
    exceed_deadline: Cell<bool>,
    num_rewrites: Cell<u64>,
    num_encode_user_agent: Cell<u64>,
    output_content_type: RefCell<Option<ContentType>>,
}

impl FakeFilter {
    /// Builds a servant with an id (`FakeFilter(id, ...)`).
    #[must_use]
    pub fn new(id: &'static str) -> Self {
        FakeFilter {
            id,
            enabled: Cell::new(true),
            exceed_deadline: Cell::new(false),
            num_rewrites: Cell::new(0),
            num_encode_user_agent: Cell::new(0),
            output_content_type: RefCell::new(None),
        }
    }

    /// Rewrite count (`num_rewrites`).
    #[must_use]
    pub fn num_rewrites(&self) -> u64 {
        self.num_rewrites.get()
    }

    /// UA-encoding count (`num_encode_user_agent`).
    #[must_use]
    pub fn num_encode_user_agent(&self) -> u64 {
        self.num_encode_user_agent.get()
    }

    /// Resets counters (`ClearStats`).
    pub fn clear_stats(&self) {
        self.num_rewrites.set(0);
        self.num_encode_user_agent.set(0);
    }

    /// Enables/disables rewriting (`set_enabled`).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.set(enabled);
    }

    /// Arms deadline overruns (`set_exceed_deadline`; sync
    /// execution has no deadlines, so this only records the
    /// request for the async-only timeout tests).
    pub fn set_exceed_deadline(&self, exceed: bool) {
        self.exceed_deadline.set(exceed);
    }

    /// Reads the deadline flag.
    #[must_use]
    pub fn exceed_deadline(&self) -> bool {
        self.exceed_deadline.get()
    }

    /// Overrides the output content type
    /// (`set_output_content_type`).
    pub fn set_output_content_type(&self, content_type: &ContentType) {
        *self.output_content_type.borrow_mut() = Some(content_type.clone());
    }
}

impl RewriteFilter for FakeFilter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn encode_user_agent_into_resource_context(
        &self,
        driver: &Rc<ResourceDriver>,
        context: &mut ImageResourceContext,
    ) {
        ImageUrlEncoder::set_webp_and_mobile_user_agent(driver, context);
        self.num_encode_user_agent
            .set(self.num_encode_user_agent.get() + 1);
    }

    fn rewrite_in_place(
        &self,
        _driver: &Rc<ResourceDriver>,
        input: &[u8],
        content_type: &ContentType,
    ) -> Option<InPlaceOutput> {
        if !self.enabled.get() {
            return None;
        }
        self.num_rewrites.set(self.num_rewrites.get() + 1);
        let mut rewritten = input.to_vec();
        rewritten.push(b':');
        rewritten.extend_from_slice(self.id.as_bytes());
        let output_type = self
            .output_content_type
            .borrow()
            .clone()
            .unwrap_or_else(|| content_type.clone());
        Some(InPlaceOutput {
            bytes: rewritten,
            content_type: output_type,
            optimized_image_type: None,
        })
    }
}

/// Test image servant (`FakeImageFilter`): tags the optimized
/// image type for Vary computation.
pub struct FakeImageFilter {
    inner: FakeFilter,
    optimized_image_type: Cell<ps_image::format::ImageType>,
}

impl FakeImageFilter {
    /// Builds the image servant (default type WebP).
    #[must_use]
    pub fn new() -> Self {
        FakeImageFilter {
            inner: FakeFilter::new("ic"),
            optimized_image_type: Cell::new(ps_image::format::ImageType::Webp),
        }
    }

    /// Sets the reported optimized type
    /// (`set_optimized_image_type`).
    pub fn set_optimized_image_type(&self, image_type: ps_image::format::ImageType) {
        self.optimized_image_type.set(image_type);
    }

    /// Rewrite count.
    #[must_use]
    pub fn num_rewrites(&self) -> u64 {
        self.inner.num_rewrites()
    }

    /// UA-encoding count.
    #[must_use]
    pub fn num_encode_user_agent(&self) -> u64 {
        self.inner.num_encode_user_agent()
    }

    /// Resets counters.
    pub fn clear_stats(&self) {
        self.inner.clear_stats();
    }

    /// Enables/disables rewriting.
    pub fn set_enabled(&self, enabled: bool) {
        self.inner.set_enabled(enabled);
    }

    /// Arms deadline overruns (async-only; recorded).
    pub fn set_exceed_deadline(&self, exceed: bool) {
        self.inner.set_exceed_deadline(exceed);
    }

    /// Overrides the output content type.
    pub fn set_output_content_type(&self, content_type: &ContentType) {
        self.inner.set_output_content_type(content_type);
    }
}

impl RewriteFilter for FakeImageFilter {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn encode_user_agent_into_resource_context(
        &self,
        driver: &Rc<ResourceDriver>,
        context: &mut ImageResourceContext,
    ) {
        self.inner
            .encode_user_agent_into_resource_context(driver, context);
    }

    fn rewrite_in_place(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &[u8],
        content_type: &ContentType,
    ) -> Option<InPlaceOutput> {
        let mut output = self.inner.rewrite_in_place(driver, input, content_type)?;
        output.optimized_image_type = Some(self.optimized_image_type.get());
        Some(output)
    }
}

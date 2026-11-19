//! Output filter chain (the six filter slots).
//!
//! Ports the filter registration half of `ngx_pagespeed.cc`:
//! the four init functions, their order, and the six filter
//! slots they install. Each init grabs the current filter top
//! into its namespace's next-pointer and installs itself, so
//! execution runs reverse to installation:
//!
//! ```text
//! init order:      in_place → fix_headers → base_fetch → html_rewrite
//! execution order: html_rewrite(header+body) → base_fetch(body)
//!                  → fix_headers(header) → in_place(header+body)
//! ```
//!
//! Filter BODIES are later slices (2–4); every slot here is a
//! documented pass-through stub calling its namespace's next
//! pointer, exactly the decline path each real filter takes
//! when pagespeed is off for the request.

use ngx::ffi::{
    ngx_chain_t, ngx_http_output_body_filter_pt, ngx_http_output_header_filter_pt,
    ngx_http_request_t, ngx_int_t,
};

/// Filter slots in wire execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterSlot {
    /// `ps_html_rewrite_header_filter` (Slice 3).
    HtmlRewriteHeader,
    /// `ps_html_rewrite_body_filter` (Slice 3).
    HtmlRewriteBody,
    /// `ps_base_fetch_filter` (Slice 4).
    BaseFetchBody,
    /// `ps_html_rewrite_fix_headers_filter` (Slice 3).
    FixHeadersHeader,
    /// `ps_in_place_check_header_filter` (in-place serving; Slice 5).
    InPlaceHeader,
    /// `ps_in_place_body_filter` (Slice 5).
    InPlaceBody,
}

/// Init-function order (`ps_init` comments: order is important).
pub const INIT_ORDER: [&str; 4] = ["in_place", "fix_headers", "base_fetch", "html_rewrite"];

/// Wire execution order (reverse of installation).
pub const EXECUTION_ORDER: [FilterSlot; 6] = [
    FilterSlot::HtmlRewriteHeader,
    FilterSlot::HtmlRewriteBody,
    FilterSlot::BaseFetchBody,
    FilterSlot::FixHeadersHeader,
    FilterSlot::InPlaceHeader,
    FilterSlot::InPlaceBody,
];

/// In-place serving filters (`namespace in_place`).
pub mod in_place {
    use super::*;

    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;
    static mut NEXT_BODY: ngx_http_output_body_filter_pt = None;

    /// Header filter stub (Slice 5 installs the IPRO logic).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; the chain must be
    /// initialized (postconfiguration ran).
    pub unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        // SAFETY: postconfiguration installed the next pointer
        // before any request could arrive; the expect documents
        // the invariant instead of segfaulting like C++.
        let next = unsafe { NEXT_HEADER }.expect("in-place header chain not initialized");
        // SAFETY: `r` is a live request per the caller contract.
        unsafe { next(r) }
    }

    /// Body filter stub (Slice 5 installs the IPRO logic).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; `in` is the body chain
    /// (possibly null at flush boundaries); the chain must be
    /// initialized.
    pub unsafe extern "C" fn body_filter(
        r: *mut ngx_http_request_t,
        chain: *mut ngx_chain_t,
    ) -> ngx_int_t {
        // SAFETY: as in `header_filter`.
        let next = unsafe { NEXT_BODY }.expect("in-place body chain not initialized");
        // SAFETY: `r`/`chain` per the caller contract.
        unsafe { next(r, chain) }
    }

    /// Installs the in-place filters (`ps_in_place_filter_init`).
    ///
    /// # Safety
    ///
    /// Call once from postconfiguration on the configuration
    /// thread: grabs both filter tops.
    pub unsafe fn init() {
        use ngx::ffi::{ngx_http_top_body_filter, ngx_http_top_header_filter};
        // SAFETY: postconfiguration context; top pointers are
        // live nginx globals.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
            NEXT_BODY = ngx_http_top_body_filter;
            ngx_http_top_body_filter = Some(body_filter);
        }
    }
}

/// Header-fixup filter (`namespace fix_headers`).
pub mod fix_headers {
    use super::*;

    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;

    /// Header filter stub (Slice 3 installs the token strip and
    /// caching-header fixups).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; the chain must be
    /// initialized.
    pub unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        // SAFETY: as in `in_place::header_filter`.
        let next = unsafe { NEXT_HEADER }.expect("fix-headers chain not initialized");
        // SAFETY: `r` is a live request per the caller contract.
        unsafe { next(r) }
    }

    /// Installs the fixup filter
    /// (`ps_html_rewrite_fix_headers_filter_init`; header only).
    ///
    /// # Safety
    ///
    /// Call once from postconfiguration.
    pub unsafe fn init() {
        use ngx::ffi::ngx_http_top_header_filter;
        // SAFETY: postconfiguration context.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
        }
    }
}

/// Base-fetch filter (`namespace ps_base_fetch`).
pub mod base_fetch {
    use super::*;

    static mut NEXT_BODY: ngx_http_output_body_filter_pt = None;

    /// Body filter stub (Slice 4 installs the fetch bridge).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; `in` is the body chain;
    /// the chain must be initialized.
    pub unsafe extern "C" fn body_filter(
        r: *mut ngx_http_request_t,
        chain: *mut ngx_chain_t,
    ) -> ngx_int_t {
        // SAFETY: as in `in_place::header_filter`.
        let next = unsafe { NEXT_BODY }.expect("base-fetch chain not initialized");
        // SAFETY: `r`/`chain` per the caller contract.
        unsafe { next(r, chain) }
    }

    /// Installs the filter (`ps_base_fetch_filter_init`).
    ///
    /// Note the C++ saves the header top into its namespace's
    /// next pointer but installs no header filter; the save is
    /// dead, so the port keeps no header slot for this namespace.
    ///
    /// # Safety
    ///
    /// Call once from postconfiguration.
    pub unsafe fn init() {
        use ngx::ffi::ngx_http_top_body_filter;
        // SAFETY: postconfiguration context.
        unsafe {
            NEXT_BODY = ngx_http_top_body_filter;
            ngx_http_top_body_filter = Some(body_filter);
        }
    }
}

/// HTML-rewrite filters (`namespace html_rewrite`).
pub mod html_rewrite {
    use super::*;

    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;
    static mut NEXT_BODY: ngx_http_output_body_filter_pt = None;

    /// Header filter stub (Slice 3 installs the dispatch).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; the chain must be
    /// initialized.
    pub unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        // SAFETY: as in `in_place::header_filter`.
        let next = unsafe { NEXT_HEADER }.expect("html-rewrite header chain not initialized");
        // SAFETY: `r` is a live request per the caller contract.
        unsafe { next(r) }
    }

    /// Body filter stub (Slice 3 installs the dispatch).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; `in` is the body chain;
    /// the chain must be initialized.
    pub unsafe extern "C" fn body_filter(
        r: *mut ngx_http_request_t,
        chain: *mut ngx_chain_t,
    ) -> ngx_int_t {
        // SAFETY: as in `in_place::header_filter`.
        let next = unsafe { NEXT_BODY }.expect("html-rewrite body chain not initialized");
        // SAFETY: `r`/`chain` per the caller contract.
        unsafe { next(r, chain) }
    }

    /// Installs the rewrite filters (`ps_html_rewrite_filter_init`).
    ///
    /// # Safety
    ///
    /// Call once from postconfiguration.
    pub unsafe fn init() {
        use ngx::ffi::{ngx_http_top_body_filter, ngx_http_top_header_filter};
        // SAFETY: postconfiguration context.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
            NEXT_BODY = ngx_http_top_body_filter;
            ngx_http_top_body_filter = Some(body_filter);
        }
    }
}

/// Etag filter (`ps_etag_header_filter`, own module).
pub mod etag {
    use super::*;

    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;

    /// Header filter stub (Slice 3 installs the etag logic).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; the chain must be
    /// initialized.
    pub unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        // SAFETY: as in `in_place::header_filter`.
        let next = unsafe { NEXT_HEADER }.expect("etag chain not initialized");
        // SAFETY: `r` is a live request per the caller contract.
        unsafe { next(r) }
    }

    /// Installs the etag filter (`ps_etag_filter_init`).
    ///
    /// # Safety
    ///
    /// Call once from the etag module's postconfiguration.
    pub unsafe fn init() {
        use ngx::ffi::ngx_http_top_header_filter;
        // SAFETY: postconfiguration context.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_slots_in_load_bearing_order() {
        assert_eq!(6, EXECUTION_ORDER.len());
        assert_eq!(
            [
                FilterSlot::HtmlRewriteHeader,
                FilterSlot::HtmlRewriteBody,
                FilterSlot::BaseFetchBody,
                FilterSlot::FixHeadersHeader,
                FilterSlot::InPlaceHeader,
                FilterSlot::InPlaceBody,
            ],
            EXECUTION_ORDER
        );
        assert_eq!(["in_place", "fix_headers", "base_fetch", "html_rewrite"], INIT_ORDER);
    }
}

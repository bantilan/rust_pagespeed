//! Body accounting for the native fetcher (`NgxFetch`):
//! how many newly read bytes belong to the response, and
//! whether a response whose connection closed is complete.

/// One accounting step (`NgxFetchBodyStep`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchBodyStep {
    /// How many of the newly read bytes are part of the body.
    pub accept: usize,
    /// True when the body is now complete.
    pub complete: bool,
    /// True when more bytes arrived than the response announced.
    pub overflow: bool,
}

/// Accounts for `available` newly read body bytes, given the
/// bytes already received (`NgxFetchAcceptBody`). When the
/// content length is unknown, every byte is accepted and the
/// body completes only when the connection closes.
#[must_use]
pub fn accept_body(
    content_length_known: bool,
    content_length: i64,
    bytes_received: i64,
    available: usize,
) -> FetchBodyStep {
    let mut step = FetchBodyStep {
        accept: available,
        complete: false,
        overflow: false,
    };
    if !content_length_known {
        return step;
    }
    // Bytes past the announced length are never part of the response.
    let remaining = (content_length - bytes_received).max(0);
    if (remaining as u64) < available as u64 {
        step.accept = remaining as usize;
        step.overflow = true;
    }
    step.complete = bytes_received.wrapping_add(step.accept as i64) == content_length;
    step
}

/// Whether a response to `status_code` (to a request that may
/// have been a HEAD) carries no body (`NgxFetchResponseHasNoBody`):
/// a HEAD response announces the length of a body it does not
/// send, so it is complete with its headers, like a 204 or 304.
#[must_use]
pub fn response_has_no_body(status_code: i32, head_request: bool) -> bool {
    head_request || status_code == 304 || status_code == 204
}

/// Whether a response is complete when the origin closes the
/// connection (`NgxFetchCompleteOnClose`). With an announced
/// content length the body must be exactly that long; without
/// one, a response whose headers parsed is complete at close.
#[must_use]
pub fn complete_on_close(
    headers_complete: bool,
    content_length_known: bool,
    content_length: i64,
    bytes_received: i64,
) -> bool {
    // An announced length is a promise: a body that closes short of it (or
    // ran past it) is not a complete response.
    if content_length_known {
        return headers_complete && content_length == bytes_received;
    }
    headers_complete
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_body_at_close_is_not_complete() {
        // Headers parsed, 100 bytes announced, 50 received, then the origin
        // closes the connection: the response is incomplete.
        let step = accept_body(true, 100, 0, 50);
        assert_eq!(50, step.accept);
        assert!(!step.complete);
        assert!(!step.overflow);
        assert!(!complete_on_close(true, true, 100, 50));
    }

    #[test]
    fn exact_body_is_complete() {
        let step = accept_body(true, 100, 60, 40);
        assert_eq!(40, step.accept);
        assert!(step.complete);
        assert!(!step.overflow);
        assert!(complete_on_close(true, true, 100, 100));
    }

    #[test]
    fn long_body_is_capped_and_flagged() {
        // 100 bytes announced, 90 already received, 30 more read: only 10
        // belong to the response.
        let step = accept_body(true, 100, 90, 30);
        assert_eq!(10, step.accept);
        assert!(step.complete);
        assert!(step.overflow);
    }

    #[test]
    fn long_body_at_close_is_not_complete() {
        assert!(!complete_on_close(true, true, 100, 150));
    }

    #[test]
    fn zero_length_body_rejects_any_bytes() {
        let step = accept_body(true, 0, 0, 5);
        assert_eq!(0, step.accept);
        assert!(step.complete);
        assert!(step.overflow);
    }

    #[test]
    fn unknown_length_accepts_everything_until_close() {
        // No content-length: a close-delimited body (the fetcher sends
        // HTTP/1.0 requests, so it does not negotiate chunked framing).
        // Every byte is accepted and completion is decided at close by
        // the headers alone.
        let step = accept_body(false, -1, 1000, 4096);
        assert_eq!(4096, step.accept);
        assert!(!step.complete);
        assert!(!step.overflow);
        assert!(complete_on_close(true, false, -1, 5096));
        assert!(complete_on_close(true, false, -1, 0));
    }

    #[test]
    fn incomplete_headers_at_close_are_not_complete() {
        assert!(!complete_on_close(false, false, -1, 0));
    }

    #[test]
    fn responses_without_body_are_done_at_headers() {
        assert!(response_has_no_body(200, true));
        assert!(response_has_no_body(304, false));
        assert!(response_has_no_body(204, false));
        assert!(!response_has_no_body(200, false));
        assert!(!response_has_no_body(404, false));
    }
}

//! Response parsing (`response_headers_parser.{h,cc}`,
//! `http_response_parser.{h,cc}`).
//!
//! `HeadersParser` consumes status line + header lines into
//! [`ResponseHeaders`](crate::response::ResponseHeaders), enforcing the
//! 64KB per-line cap and the blank-line terminator. `ResponseParser`
//! splits headers from body and writes body bytes VERBATIM to the
//! writer, exactly like C++ (transfer decoding happens in the fetchers,
//! not here).
#![forbid(unsafe_code)]

use crate::response::ResponseHeaders;

/// Maximum header line length (`kMaxLineSize` = 64KB).
pub const MAX_LINE_SIZE: usize = 64 * 1024;

/// Headers parser states (`ParseState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeadersState {
    /// Reading the status line.
    #[default]
    StatusLine,
    /// Reading header lines.
    Headers,
    /// Blank line seen; complete.
    Done,
}

/// Headers-parse machine state, shared by `HeadersParser` and
/// `ResponseParser` (which cannot both borrow the response through a
/// nested parser object).
#[derive(Debug, Default)]
struct HeadersMachine {
    state: HeadersState,
    buffer: String,
    overlong: bool,
}

impl HeadersMachine {
    /// Feeds bytes, returning bytes consumed.
    fn feed(&mut self, response: &mut ResponseHeaders, bytes: &[u8]) -> usize {
        if self.overlong {
            return 0;
        }
        let mut consumed = 0;
        while consumed < bytes.len() {
            let byte = bytes[consumed];
            consumed += 1;
            if byte == b'\r' {
                continue; // CR ignored (both CRLF and bare-CR tolerate).
            }
            if byte != b'\n' {
                if self.buffer.len() >= MAX_LINE_SIZE {
                    self.overlong = true;
                    return 0;
                }
                self.buffer.push(byte as char);
                continue;
            }
            if !Self::handle_line(response, &mut self.state, &mut self.buffer) {
                return consumed;
            }
            if self.state == HeadersState::Done {
                break;
            }
        }
        consumed
    }

    /// Handles one line; false asks the caller to stop feeding.
    fn handle_line(
        response: &mut ResponseHeaders,
        state: &mut HeadersState,
        buffer: &mut String,
    ) -> bool {
        let line = std::mem::take(buffer);
        if line.is_empty() {
            // Blank line: complete only when past the status line.
            if *state == HeadersState::StatusLine {
                return false; // EOF before status: stop, incomplete.
            }
            *state = HeadersState::Done;
            return false;
        }
        if *state == HeadersState::StatusLine {
            // Status line required; garbage keeps us in StatusLine.
            if line.starts_with("HTTP/") {
                response.parse_first_line(&line);
                *state = HeadersState::Headers;
            }
            return true;
        }
        // Header line: split on the FIRST colon, trim the value.
        match line.find(':') {
            Some(pos) => {
                let value = line[pos + 1..].trim_start().to_string();
                let name = line[..pos].to_string();
                response.headers_mut().add(&name, &value);
                true
            }
            None => false, // garbage line: stop, incomplete.
        }
    }
}

/// Incremental headers parser (`ResponseHeadersParser`).
#[derive(Debug)]
pub struct HeadersParser<'a> {
    response: &'a mut ResponseHeaders,
    machine: HeadersMachine,
}

impl<'a> HeadersParser<'a> {
    /// Borrows the response to fill (`ResponseHeadersParser(headers)`).
    pub fn new(response: &'a mut ResponseHeaders) -> HeadersParser<'a> {
        HeadersParser {
            response,
            machine: HeadersMachine::default(),
        }
    }

    /// Current parse state.
    #[must_use]
    pub fn state(&self) -> HeadersState {
        self.machine.state
    }

    /// Whether parsing completed (`headers_complete`).
    #[must_use]
    pub fn complete(&self) -> bool {
        self.machine.state == HeadersState::Done
    }

    /// Whether an overlong line failed the parse. C++ keeps returning 0
    /// without progress in that state; this port latches the failure so
    /// callers can observe and abandon it instead of spinning.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.machine.overlong
    }

    /// Feeds bytes, returning bytes consumed (`ParseChunk`, CRLF mode).
    ///
    /// Stops at completion; late bytes are left unconsumed for the body.
    /// Overlong lines fail the whole parse (0 from then on).
    pub fn parse_chunk(&mut self, bytes: &[u8]) -> usize {
        self.machine.feed(self.response, bytes)
    }
}

/// Streaming response parser (`HttpResponseParser`).
///
/// Splits headers from body and writes body bytes VERBATIM to the
/// writer. There is deliberately no transfer decoding, length tracking,
/// or EOF handling here: C++ leaves all of that to the fetchers.
#[derive(Debug)]
pub struct ResponseParser<'a> {
    response: &'a mut ResponseHeaders,
    writer: &'a mut Vec<u8>,
    reading_headers: bool,
    ok: bool,
    machine: HeadersMachine,
}

impl<'a> ResponseParser<'a> {
    /// Borrows the response to fill and the body writer.
    pub fn new(response: &'a mut ResponseHeaders, writer: &'a mut Vec<u8>) -> ResponseParser<'a> {
        ResponseParser {
            response,
            writer,
            reading_headers: true,
            ok: true,
            machine: HeadersMachine::default(),
        }
    }

    /// Whether all writes succeeded (`ok()`).
    #[must_use]
    pub fn ok(&self) -> bool {
        self.ok && !self.machine.overlong
    }

    /// Whether headers completed (`headers_complete()`).
    #[must_use]
    pub fn headers_complete(&self) -> bool {
        !self.reading_headers
    }

    /// Feeds a chunk (`ParseChunk`); body bytes stream verbatim.
    pub fn parse_chunk(&mut self, bytes: &[u8]) -> bool {
        if !self.ok() {
            return false;
        }
        if self.reading_headers {
            let consumed = self.machine.feed(self.response, bytes);
            if self.machine.state == HeadersState::Done {
                self.writer.extend_from_slice(&bytes[consumed..]);
                self.reading_headers = false;
            }
        } else {
            self.writer.extend_from_slice(bytes);
        }
        self.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::HttpOptions;

    fn parsed_headers(raw: &str) -> (ResponseHeaders, usize) {
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        let consumed = {
            let mut parser = HeadersParser::new(&mut response);
            parser.parse_chunk(raw.as_bytes())
        };
        (response, consumed)
    }

    #[test]
    fn headers_goldens() {
        let (response, consumed) = parsed_headers(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 3\r\n\r\nabc",
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(response.reason_phrase(), "OK");
        assert_eq!((response.major_version(), response.minor_version()), (1, 1));
        assert_eq!(
            response.headers().lookup1("Content-Type"),
            Some("text/html".to_string())
        );
        // Body bytes left unconsumed.
        assert_eq!(
            &b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 3\r\n\r\nabc"
                [consumed..],
            b"abc"
        );
        // Bare LF tolerated; missing reason defaults; first colon splits.
        let (response, _) = parsed_headers("HTTP/1.0 404\nX-A: b:c\n\n");
        assert_eq!(response.status_code(), 404);
        assert_eq!(response.reason_phrase(), "Not Found");
        assert_eq!(response.headers().lookup1("X-A"), Some("b:c".to_string()));
        // Leading garbage lines before the status line are skipped.
        let (response, _) = parsed_headers("junk\nHTTP/1.1 200 OK\n\n");
        assert_eq!(response.status_code(), 200);
        // Colon-less line stops the parse, incomplete.
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        let mut parser = HeadersParser::new(&mut response);
        parser.parse_chunk(b"HTTP/1.1 200 OK\nNoColonHere\n");
        assert!(!parser.complete());
    }

    #[test]
    fn overlong_line_fails() {
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        let mut parser = HeadersParser::new(&mut response);
        let line = format!("HTTP/1.1 200 {}\n", "x".repeat(MAX_LINE_SIZE + 1));
        assert_eq!(parser.parse_chunk(line.as_bytes()), 0);
    }

    #[test]
    fn response_split_and_verbatim_body() {
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        let mut body = Vec::new();
        let mut parser = ResponseParser::new(&mut response, &mut body);
        // Headers split mid-chunk; the tail streams to the body writer.
        assert!(parser.parse_chunk(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nab"));
        assert!(parser.headers_complete());
        assert!(parser.parse_chunk(b"c"));
        assert!(parser.ok());
        drop(parser);
        assert_eq!(body, b"abc");
        assert_eq!(response.status_code(), 200);
        // Chunked-looking bytes pass through untouched (no decoding here).
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        let mut body = Vec::new();
        let mut parser = ResponseParser::new(&mut response, &mut body);
        assert!(parser.parse_chunk(b"HTTP/1.1 200 OK\r\n\r\n4\r\nWiki\r\n0\r\n\r\n"));
        assert_eq!(body, b"4\r\nWiki\r\n0\r\n\r\n");
    }
}

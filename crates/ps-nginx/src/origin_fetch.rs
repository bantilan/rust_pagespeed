//! Blocking origin fetcher (sync HTTP/1.1 client over `TcpStream`).
//!
//! C++ fetches origins asynchronously (serf or the native nginx
//! fetcher); the sync bridge cannot park for either, so this
//! fetcher performs a plain blocking HTTP/1.1 transaction with
//! connect/read/write timeouts: `GET`/`HEAD` in origin-form, an
//! explicit `Host` request header honored verbatim (the lawyer
//! pins it for mapped inputs; proxy serving forwards the
//! client's), `Connection: close`, and framing by chunked,
//! `Content-Length`, or close-delimited bodies. Any fully
//! received HTTP response completes the fetch (`Done(true)` at
//! any status, so error pages serve and classify like C++'s);
//! only transport, parse, timeout, and cap failures fail it.
//!
//! Deliberate limits: `http` only (`https` fails, like the file
//! fetcher — no TLS); no request body (other methods fail, and
//! the proxy branch only admits `GET`/`HEAD` anyway); no
//! interim (`1xx`) responses; bodies cap at 32 MiB and heads at
//! 1 MiB; hop-by-hop request headers are never forwarded and
//! framing headers are renormalized to the delivered identity
//! bytes. `ChainedFetcher` routes same-server URLs to the file
//! fetcher first and everything else here (C++ parity: C++
//! fetches all inputs over HTTP, including loopback).

use ps_http::fetch::{AsyncFetch, AsyncFetcher};
use ps_http::google_url::GoogleUrl;
use ps_http::names::header;
use ps_http::parser::HeadersParser;
use ps_http::request::{Method, RequestHeaders};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Default fetch timeout in ms (`blocking_fetch_timeout_ms`
/// default, `FetcherTimeOutMs`): whole-transaction deadline.
pub const DEFAULT_TIMEOUT_MS: i64 = 5000;
/// Default body cap (32 MiB): the sync bridge buffers whole
/// responses, so unbounded origins must fail closed.
pub const DEFAULT_MAX_BODY_BYTES: usize = 32 << 20;
/// Head cap (1 MiB): status line plus headers past this fail.
pub const MAX_HEAD_BYTES: usize = 1 << 20;
/// Read chunk size.
const READ_CHUNK_BYTES: usize = 8192;

/// Blocking HTTP/1.1 origin fetcher.
#[derive(Debug)]
pub struct OriginFetcher {
    /// Whole-transaction deadline in ms (<= 0 selects the default).
    timeout_ms: i64,
    /// Body cap in bytes.
    max_body_bytes: usize,
}

impl Default for OriginFetcher {
    fn default() -> Self {
        OriginFetcher {
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

impl OriginFetcher {
    /// Builds a fetcher (`timeout_ms` <= 0 selects the default).
    #[must_use]
    pub fn new(timeout_ms: i64, max_body_bytes: usize) -> Self {
        OriginFetcher {
            timeout_ms,
            max_body_bytes,
        }
    }

    /// Effective timeout in ms (non-positive selects the default).
    fn effective_timeout_ms(&self) -> i64 {
        if self.timeout_ms <= 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            self.timeout_ms
        }
    }

    /// Fetches with an explicit whole-transaction timeout
    /// (`fetch_with_timeout` honors this; <= 0 selects the default).
    fn fetch_inner(&self, url: &str, fetch: &mut dyn AsyncFetch, timeout_ms: i64) {
        let timeout_ms = if timeout_ms <= 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            timeout_ms
        };
        let gurl = GoogleUrl::new(url);
        if !gurl.is_web_valid() || gurl.scheme() != "http" {
            fetch.handle_done(false);
            return;
        }
        let method = fetch.request_headers().method();
        if !matches!(method, Method::Get | Method::Head) {
            fetch.handle_done(false);
            return;
        }
        let host = gurl.host();
        if host.is_empty() {
            fetch.handle_done(false);
            return;
        }
        let port = gurl.effective_int_port();
        if port <= 0 || port > u16::MAX as i32 {
            fetch.handle_done(false);
            return;
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let port = port as u16;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
        let request = request_bytes(&gurl, fetch.request_headers());
        let Some(mut stream) = connect(&host, port, deadline) else {
            fetch.handle_done(false);
            return;
        };
        if write_all_deadline(&mut stream, &request, deadline).is_err() {
            fetch.handle_done(false);
            return;
        }
        // The borrow checker cannot hold the parser (which borrows
        // the response) across the body reads below, so headers
        // parse in this block and framing decisions flow out.
        enum Framing {
            ContentLength(u64),
            Chunked,
            CloseDelimited,
        }
        let framing = {
            let mut parser = HeadersParser::new(fetch.response_headers_mut());
            let mut head_bytes: usize = 0;
            let mut pending: Vec<u8> = Vec::new();
            let mut chunk = vec![0u8; READ_CHUNK_BYTES];
            let mut headers_done = false;
            while !parser.complete() && !parser.failed() {
                if Instant::now() >= deadline {
                    break;
                }
                set_deadline(&stream, deadline);
                let read = match stream.read(&mut chunk) {
                    Ok(0) => break, // EOF mid-head: truncated.
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                head_bytes += read;
                if head_bytes > MAX_HEAD_BYTES {
                    break;
                }
                let consumed = parser.parse_chunk(&chunk[..read]);
                if parser.complete() {
                    pending.extend_from_slice(&chunk[consumed..read]);
                    headers_done = true;
                }
            }
            if !headers_done {
                fetch.handle_done(false);
                return;
            }
            let status = fetch.response_headers().status_code();
            if !(200..600).contains(&status) {
                // No status line (0), an interim response (1xx,
                // unsupported), or garbage: fail closed.
                fetch.handle_done(false);
                return;
            }
            if method == Method::Head || status == 204 || status == 304 {
                finish_empty(fetch);
                return;
            }
            let response = fetch.response_headers();
            let encodings: Vec<String> = response
                .headers()
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(header::TRANSFER_ENCODING))
                .map(|(_, value)| value.to_string())
                .collect();
            let framing = if !encodings.is_empty() {
                if encodings.len() != 1 || !encodings[0].trim().eq_ignore_ascii_case("chunked") {
                    // Only a lone `chunked` decodes; anything else
                    // (gzip codings, multiples) fails closed.
                    fetch.handle_done(false);
                    return;
                }
                Framing::Chunked
            } else {
                let lengths: Vec<String> = response
                    .headers()
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case(header::CONTENT_LENGTH))
                    .map(|(_, value)| value.to_string())
                    .collect();
                if lengths.is_empty() {
                    Framing::CloseDelimited
                } else {
                    let mut parsed = Vec::with_capacity(lengths.len());
                    for length in &lengths {
                        let Ok(length) = length.trim().parse::<u64>() else {
                            fetch.handle_done(false);
                            return;
                        };
                        parsed.push(length);
                    }
                    if parsed.iter().any(|length| *length != parsed[0]) {
                        // Differing lengths smell of smuggling.
                        fetch.handle_done(false);
                        return;
                    }
                    Framing::ContentLength(parsed[0])
                }
            };
            (framing, pending)
        };
        let (framing, mut pending) = framing;
        let body = match framing {
            Framing::ContentLength(length) => {
                if length > self.max_body_bytes as u64 {
                    fetch.handle_done(false);
                    return;
                }
                #[allow(clippy::cast_possible_truncation)]
                let length = length as usize;
                let mut body = std::mem::take(&mut pending);
                body.truncate(length);
                let mut chunk = vec![0u8; READ_CHUNK_BYTES];
                while body.len() < length {
                    if Instant::now() >= deadline {
                        fetch.handle_done(false);
                        return;
                    }
                    set_deadline(&stream, deadline);
                    match stream.read(&mut chunk) {
                        Ok(0) => {
                            fetch.handle_done(false);
                            return;
                        } // Truncated.
                        Ok(read) => {
                            let want = (length - body.len()).min(read);
                            body.extend_from_slice(&chunk[..want]);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            fetch.handle_done(false);
                            return;
                        }
                    }
                }
                body
            }
            Framing::Chunked => {
                let mut decoder = ChunkDecoder::new(std::mem::take(&mut pending));
                // Process the seed before reading: small bodies
                // arrive whole with the head.
                if !decoder.feed(&[]) {
                    fetch.handle_done(false);
                    return;
                }
                let mut chunk = vec![0u8; READ_CHUNK_BYTES];
                loop {
                    if decoder.body_len() > self.max_body_bytes {
                        fetch.handle_done(false);
                        return;
                    }
                    if decoder.complete() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        fetch.handle_done(false);
                        return;
                    }
                    set_deadline(&stream, deadline);
                    match stream.read(&mut chunk) {
                        Ok(0) => {
                            // Closed mid-chunks: only a completed
                            // decoder (terminal chunk seen) may end
                            // here; trailers may truncate (ignored).
                            if decoder.feed_eof() {
                                break;
                            }
                            fetch.handle_done(false);
                            return;
                        }
                        Ok(read) => {
                            if !decoder.feed(&chunk[..read]) {
                                fetch.handle_done(false);
                                return;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            fetch.handle_done(false);
                            return;
                        }
                    }
                }
                if decoder.body_len() > self.max_body_bytes {
                    fetch.handle_done(false);
                    return;
                }
                decoder.into_body()
            }
            Framing::CloseDelimited => {
                let mut body = std::mem::take(&mut pending);
                let mut chunk = vec![0u8; READ_CHUNK_BYTES];
                loop {
                    if body.len() > self.max_body_bytes {
                        fetch.handle_done(false);
                        return;
                    }
                    if Instant::now() >= deadline {
                        fetch.handle_done(false);
                        return;
                    }
                    set_deadline(&stream, deadline);
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(read) => body.extend_from_slice(&chunk[..read]),
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            fetch.handle_done(false);
                            return;
                        }
                    }
                }
                if body.len() > self.max_body_bytes {
                    fetch.handle_done(false);
                    return;
                }
                body
            }
        };
        // Framing described the wire; the fetch delivers identity
        // bytes, so the length is restated exactly and the coding
        // removed (serving a stored `chunked` alongside identity
        // bytes would corrupt clients).
        {
            let headers = fetch.response_headers_mut();
            headers.headers_mut().remove_all(header::TRANSFER_ENCODING);
            headers.headers_mut().remove_all(header::CONTENT_LENGTH);
            headers
                .headers_mut()
                .add(header::CONTENT_LENGTH, &body.len().to_string());
            headers.set_original_content_length(body.len() as i64);
            headers.compute_caching();
        }
        fetch.headers_complete();
        let ok = fetch.handle_write(&body);
        fetch.handle_done(ok);
    }
}

impl AsyncFetcher for OriginFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        self.fetch_inner(url, fetch, self.effective_timeout_ms());
    }

    fn fetch_with_timeout(&self, url: &str, fetch: &mut dyn AsyncFetch, timeout_ms: i64) {
        self.fetch_inner(url, fetch, timeout_ms);
    }

    fn supports_https(&self) -> bool {
        false
    }

    fn timeout_ms(&self) -> i64 {
        self.effective_timeout_ms()
    }
}

/// File-first origin chain (port-authored: C++ fetches every
/// input over HTTP, including loopback; the file leg keeps the
/// mapped-root fast path while the origin leg covers proxy
/// origins, unmapped roots, and foreign hosts).
#[derive(Debug)]
pub struct ChainedFetcher {
    /// Same-server file leg (absent without a mapped root).
    file: Option<crate::file_fetch::FileFetcher>,
    /// Network leg for everything the file leg cannot map.
    origin: OriginFetcher,
}

impl ChainedFetcher {
    /// Chains the file leg over the origin leg.
    #[must_use]
    pub fn new(file: Option<crate::file_fetch::FileFetcher>, origin: OriginFetcher) -> Self {
        ChainedFetcher { file, origin }
    }
}

impl AsyncFetcher for ChainedFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        if let Some(file) = self.file.as_ref() {
            if file.can_fetch(url) {
                file.fetch(url, fetch);
                return;
            }
        }
        self.origin.fetch(url, fetch);
    }

    fn fetch_with_timeout(&self, url: &str, fetch: &mut dyn AsyncFetch, timeout_ms: i64) {
        if let Some(file) = self.file.as_ref() {
            if file.can_fetch(url) {
                file.fetch(url, fetch);
                return;
            }
        }
        self.origin.fetch_with_timeout(url, fetch, timeout_ms);
    }

    fn supports_https(&self) -> bool {
        // Advertise the local-file capability so resource construction does
        // not reject same-site HTTPS before the jailed file leg can run.
        // Foreign HTTPS still fails closed in the HTTP-only network leg.
        self.file.as_ref().is_some_and(AsyncFetcher::supports_https) || self.origin.supports_https()
    }

    fn timeout_ms(&self) -> i64 {
        self.origin.timeout_ms()
    }
}

/// Completes a response that carries no body (`HEAD`, 204, 304):
/// headers stand, the length restates empty, the coding is
/// dropped, and the fetch succeeds (a 304 revalidates upstream).
fn finish_empty(fetch: &mut dyn AsyncFetch) {
    let headers = fetch.response_headers_mut();
    headers.headers_mut().remove_all(header::TRANSFER_ENCODING);
    headers.headers_mut().remove_all(header::CONTENT_LENGTH);
    headers.headers_mut().add(header::CONTENT_LENGTH, "0");
    headers.set_original_content_length(0);
    headers.compute_caching();
    fetch.headers_complete();
    fetch.handle_done(true);
}

/// Serializes the origin request: method plus origin-form target
/// at HTTP/1.1, `Connection: close` (no keepalive in a blocking
/// client), the explicit `Host` when pinned else the URL's, then
/// the forwardable request headers in order.
fn request_bytes(url: &GoogleUrl, headers: &RequestHeaders) -> Vec<u8> {
    let mut head = headers.clone();
    crate::native_fetch::fix_user_agent(&mut head);
    let mut out = Vec::new();
    out.extend_from_slice(head.method().as_str().as_bytes());
    out.push(b' ');
    let path = url.path_sans_query();
    if path.is_empty() {
        out.push(b'/');
    } else {
        out.extend_from_slice(path.as_bytes());
    }
    if url.has_query() {
        out.push(b'?');
        out.extend_from_slice(url.query().as_bytes());
    }
    out.extend_from_slice(b" HTTP/1.1\r\n");
    let host = head
        .headers()
        .lookup1(header::HOST)
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| url.host_and_port());
    out.extend_from_slice(b"Host: ");
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(b"\r\nConnection: close\r\n");
    for (name, value) in head.headers().iter() {
        if !forwardable(name) {
            continue;
        }
        // Values never carry raw newlines (nginx rejects them at
        // the edge); skip any that do rather than smuggle.
        if value.bytes().any(|byte| byte == b'\r' || byte == b'\n') {
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// Whether a request header crosses to the origin: everything
/// but `Host` (emitted above) and the hop-by-hop set (`TE`,
/// trailers, codings, connection tokens, and any content length
/// — no body is ever sent — plus `Expect`, which would stall a
/// bodyless request on origins awaiting `100-continue`).
fn forwardable(name: &str) -> bool {
    !name.eq_ignore_ascii_case(header::HOST)
        && !name.eq_ignore_ascii_case(header::CONNECTION)
        && !name.eq_ignore_ascii_case(header::KEEP_ALIVE)
        && !name.eq_ignore_ascii_case(header::PROXY_AUTHENTICATE)
        && !name.eq_ignore_ascii_case(header::PROXY_AUTHORIZATION)
        && !name.eq_ignore_ascii_case(header::TE)
        && !name.eq_ignore_ascii_case("Trailer")
        && !name.eq_ignore_ascii_case(header::TRAILERS)
        && !name.eq_ignore_ascii_case(header::TRANSFER_ENCODING)
        && !name.eq_ignore_ascii_case(header::UPGRADE)
        && !name.eq_ignore_ascii_case(header::CONTENT_LENGTH)
        && !name.eq_ignore_ascii_case(header::EXPECT)
}

/// Connects to the first reachable resolved address before the
/// deadline (each attempt waits at most the remaining time).
fn connect(host: &str, port: u16, deadline: Instant) -> Option<TcpStream> {
    let addrs: Vec<_> = (host, port).to_socket_addrs().ok()?.collect();
    for addr in addrs {
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        if let Ok(stream) = TcpStream::connect_timeout(&addr, deadline - now) {
            return Some(stream);
        }
    }
    None
}

/// Bounds the next socket operation by the remaining deadline.
fn set_deadline(stream: &TcpStream, deadline: Instant) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let _ = stream.set_read_timeout(Some(remaining));
    let _ = stream.set_write_timeout(Some(remaining));
}

/// Writes the whole buffer before the deadline.
fn write_all_deadline(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: Instant,
) -> std::io::Result<()> {
    set_deadline(stream, deadline);
    stream.write_all(bytes)
}

/// Incremental chunked decoder (extensions ignored, trailers
/// skipped): feeds wire bytes, yields identity bytes.
#[derive(Debug, Default)]
struct ChunkDecoder {
    /// Unprocessed wire bytes.
    pending: Vec<u8>,
    /// Decoded body.
    body: Vec<u8>,
    /// Bytes still owed by the open chunk (`None` between chunks).
    remaining: Option<usize>,
    /// Terminal chunk seen (trailers draining).
    terminal: bool,
    /// Decoder finished (terminal chunk plus blank line, or EOF).
    done: bool,
}

impl ChunkDecoder {
    /// Seeds the decoder with body-start bytes.
    fn new(pending: Vec<u8>) -> Self {
        ChunkDecoder {
            pending,
            body: Vec::new(),
            remaining: None,
            terminal: false,
            done: false,
        }
    }

    /// Decoded length so far.
    fn body_len(&self) -> usize {
        self.body.len()
    }

    /// Whether decoding finished.
    fn complete(&self) -> bool {
        self.done
    }

    /// Consumes the decoded body.
    fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// Handles EOF: trailers may truncate, but the terminal
    /// chunk must have been seen.
    fn feed_eof(&mut self) -> bool {
        if self.terminal {
            self.done = true;
            return true;
        }
        false
    }

    /// Feeds wire bytes; false fails the decode (bad hex, a short
    /// chunk, or a missing CRLF).
    fn feed(&mut self, bytes: &[u8]) -> bool {
        self.pending.extend_from_slice(bytes);
        loop {
            if self.done {
                return true;
            }
            if self.terminal {
                // Trailer lines drain to the blank line.
                let Some(end) = find_crlf(&self.pending) else {
                    return true;
                };
                if end == 0 {
                    self.pending.drain(..2);
                    self.done = true;
                    return true;
                }
                self.pending.drain(..end + 2);
                continue;
            }
            if let Some(remaining) = self.remaining {
                if self.pending.len() < remaining + 2 {
                    return true;
                }
                self.body.extend_from_slice(&self.pending[..remaining]);
                if self.pending[remaining] != b'\r' || self.pending[remaining + 1] != b'\n' {
                    return false;
                }
                self.pending.drain(..remaining + 2);
                self.remaining = None;
                continue;
            }
            let Some(end) = find_crlf(&self.pending) else {
                return true;
            };
            let line = self.pending[..end].to_vec();
            self.pending.drain(..end + 2);
            let size_text = line.split(|byte| *byte == b';').next().unwrap_or(&[]);
            let Ok(size_text) = std::str::from_utf8(size_text) else {
                return false;
            };
            let Ok(size) = usize::from_str_radix(size_text.trim(), 16) else {
                return false;
            };
            if size == 0 {
                self.terminal = true;
                continue;
            }
            self.remaining = Some(size);
        }
    }
}

/// Finds the first CRLF, returning its offset.
fn find_crlf(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|window| window == b"\r\n")
}

/// Stub origin shared by fetcher tests (test-only).
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::mpsc;
    use std::time::Duration;

    /// Stub origin: serves scripted exchanges, capturing requests.
    pub struct Stub {
        addr: SocketAddr,
        requests: mpsc::Receiver<Vec<u8>>,
        _thread: std::thread::JoinHandle<()>,
    }

    impl Stub {
        /// Serves `response` to the first connection, then closes.
        pub fn once(response: &'static [u8]) -> Self {
            Self::script(vec![response])
        }

        /// Serves each scripted response to successive connections.
        pub fn script(responses: Vec<&'static [u8]>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (tx, rx) = mpsc::channel();
            let thread = std::thread::spawn(move || {
                for response in responses {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // Read to the blank line (requests carry no body).
                    while let Ok(read) = stream.read(&mut chunk) {
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..read]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let _ = tx.send(request);
                    let _ = stream.write_all(response);
                    // `Connection: close` semantics: shut down so
                    // close-delimited reads terminate.
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            });
            Stub {
                addr,
                requests: rx,
                _thread: thread,
            }
        }

        /// Origin URL for a path.
        pub fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.addr, path)
        }

        /// Bound socket address.
        pub fn socket_addr(&self) -> SocketAddr {
            self.addr
        }

        /// Next captured request as text.
        pub fn request_text(&self) -> String {
            let request = self
                .requests
                .recv_timeout(Duration::from_secs(10))
                .expect("origin saw the request");
            String::from_utf8(request).expect("request is text")
        }

        /// Captured-request channel (absence probes).
        pub fn requests(&self) -> &mpsc::Receiver<Vec<u8>> {
            &self.requests
        }
    }

    /// `GET` request headers.
    pub fn get_headers() -> ps_http::request::RequestHeaders {
        let mut headers = ps_http::request::RequestHeaders::new();
        headers.set_method(ps_http::request::Method::Get);
        headers
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{get_headers, Stub};
    use super::*;
    use ps_http::fetch::BufferingFetch;
    use ps_http::request::RequestContext;
    use ps_http::response::HttpOptions;
    use std::time::Duration;

    fn fetch_from(fetcher: &OriginFetcher, url: &str, headers: RequestHeaders) -> BufferingFetch {
        let mut fetch =
            BufferingFetch::new(url, RequestContext::new(HttpOptions::default_for_tests()));
        *fetch.request_headers_mut() = headers;
        fetcher.fetch(url, &mut fetch);
        fetch
    }

    #[test]
    fn plain_response_serves_with_renormalized_length() {
        let stub = Stub::once(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: 4\r\n\r\nhi\n\n",
        );
        let fetch = fetch_from(
            &OriginFetcher::default(),
            &stub.url("/a.css"),
            get_headers(),
        );
        assert!(fetch.done());
        assert!(fetch.success());
        assert_eq!(200, fetch.response_headers().status_code());
        assert_eq!(b"hi\n\n", fetch.body());
        assert_eq!(
            Some("4".to_string()),
            fetch
                .response_headers()
                .headers()
                .lookup1(header::CONTENT_LENGTH)
        );
        let request = stub.request_text();
        assert!(request.starts_with("GET /a.css HTTP/1.1\r\n"), "{request}");
        assert!(request.contains("Connection: close\r\n"), "{request}");
        assert!(
            request.contains(&format!("Host: {}\r\n", stub.socket_addr())),
            "{request}"
        );
    }

    #[test]
    fn empty_path_requests_root_with_query() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let url = format!("http://{}?x=1", stub.socket_addr());
        let fetch = fetch_from(&OriginFetcher::default(), &url, get_headers());
        assert!(fetch.success());
        let request = stub.request_text();
        assert!(request.starts_with("GET /?x=1 HTTP/1.1\r\n"), "{request}");
    }

    #[test]
    fn explicit_host_header_wins_over_url() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut headers = get_headers();
        headers.headers_mut().add(header::HOST, "proxy.test");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), headers);
        assert!(fetch.success());
        let request = stub.request_text();
        assert!(request.contains("Host: proxy.test\r\n"), "{request}");
        assert_eq!(1, request.match_indices("Host:").count(), "{request}");
    }

    #[test]
    fn client_headers_forward_but_hop_by_hop_stays() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut headers = get_headers();
        headers.headers_mut().add("Cookie", "a=b");
        headers.headers_mut().add(header::CONNECTION, "keep-alive");
        headers
            .headers_mut()
            .add(header::TRANSFER_ENCODING, "chunked");
        headers.headers_mut().add(header::CONTENT_LENGTH, "99");
        headers.headers_mut().add(header::EXPECT, "100-continue");
        headers.headers_mut().add(header::TE, "trailers");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), headers);
        assert!(fetch.success());
        let request = stub.request_text();
        assert!(request.contains("Cookie: a=b\r\n"), "{request}");
        assert!(!request.contains("keep-alive"), "{request}");
        assert!(!request.contains("Transfer-Encoding: chunked"), "{request}");
        assert!(!request.contains("Content-Length: 99"), "{request}");
        assert!(!request.contains("Expect:"), "{request}");
        assert!(request.contains("User-Agent:"), "{request}");
    }

    #[test]
    fn user_agent_gains_native_suffix() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut headers = get_headers();
        headers.headers_mut().add(header::USER_AGENT, "probe");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), headers);
        assert!(fetch.success());
        let request = stub.request_text();
        assert!(
            request.contains("User-Agent: probe (mod_pagespeed/"),
            "{request}"
        );
    }

    #[test]
    fn chunked_response_decodes_with_trailers_skipped() {
        let stub = Stub::once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
              4\r\nWiki\r\n5;ext=x\r\npedia\r\n0\r\nX-Trailer: t\r\n\r\n",
        );
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(fetch.success());
        assert_eq!(b"Wikipedia", fetch.body());
        // Framing headers renormalize to the identity bytes.
        assert_eq!(
            None,
            fetch
                .response_headers()
                .headers()
                .lookup1(header::TRANSFER_ENCODING)
        );
        assert_eq!(
            Some("9".to_string()),
            fetch
                .response_headers()
                .headers()
                .lookup1(header::CONTENT_LENGTH)
        );
    }

    #[test]
    fn chunked_eof_after_terminal_chunk_still_succeeds() {
        // Some origins close instead of sending the trailer blank line.
        let stub =
            Stub::once(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.success());
        assert_eq!(b"hi", fetch.body());
    }

    #[test]
    fn bad_chunk_hex_fails() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn truncated_chunked_fails() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nshort");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn close_delimited_reads_to_eof() {
        let stub = Stub::once(b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nno-length");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.success());
        assert_eq!(b"no-length", fetch.body());
        assert_eq!(
            Some("9".to_string()),
            fetch
                .response_headers()
                .headers()
                .lookup1(header::CONTENT_LENGTH)
        );
    }

    #[test]
    fn head_response_has_no_body() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
        let mut headers = RequestHeaders::new();
        headers.set_method(Method::Head);
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), headers);
        assert!(fetch.success());
        assert!(fetch.body().is_empty());
        let request = stub.request_text();
        assert!(request.starts_with("HEAD /x HTTP/1.1\r\n"), "{request}");
    }

    #[test]
    fn not_modified_succeeds_empty() {
        let stub = Stub::once(b"HTTP/1.1 304 Not Modified\r\nETag: \"v\"\r\n\r\n");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.success());
        assert_eq!(304, fetch.response_headers().status_code());
        assert!(fetch.body().is_empty());
    }

    #[test]
    fn origin_errors_complete_as_errors() {
        let stub = Stub::script(vec![
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot-here\n",
            b"HTTP/1.1 500 Broken\r\nContent-Length: 3\r\n\r\nbad",
        ]);
        let fetcher = OriginFetcher::default();
        let first = fetch_from(&fetcher, &stub.url("/a"), get_headers());
        assert!(first.success());
        assert_eq!(404, first.response_headers().status_code());
        assert_eq!(b"not-here\n", first.body());
        let second = fetch_from(&fetcher, &stub.url("/b"), get_headers());
        assert!(second.success());
        assert_eq!(500, second.response_headers().status_code());
    }

    #[test]
    fn differing_content_lengths_fail() {
        let stub =
            Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\nhi\n\n");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn truncated_content_length_fails() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\nshort");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn oversize_body_fails_fast() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\nshort");
        let fetcher = OriginFetcher::new(5000, 4);
        let fetch = fetch_from(&fetcher, &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn oversize_close_delimited_fails() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\n\r\n0123456789");
        let fetcher = OriginFetcher::new(5000, 4);
        let fetch = fetch_from(&fetcher, &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn garbage_status_line_fails() {
        let stub = Stub::once(b"NOTHTTP\r\n\r\n");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn interim_response_fails() {
        let stub = Stub::once(b"HTTP/1.1 103 Early Hints\r\nLink: </x>\r\n\r\n");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn unsupported_transfer_coding_fails() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nbody");
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), get_headers());
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn unroutable_host_fails() {
        // Reserved TEST-NET-1: unroutable, so the connect fails fast.
        let fetcher = OriginFetcher::new(1000, 1024);
        let mut fetch = BufferingFetch::new(
            "http://192.0.2.1/x",
            RequestContext::new(HttpOptions::default_for_tests()),
        );
        *fetch.request_headers_mut() = get_headers();
        fetcher.fetch("http://192.0.2.1/x", &mut fetch);
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn https_and_bad_urls_fail_without_connecting() {
        let fetcher = OriginFetcher::default();
        for url in [
            "https://127.0.0.1/x",
            "ftp://127.0.0.1/x",
            "http://",
            "not a url",
            "",
        ] {
            let mut fetch =
                BufferingFetch::new(url, RequestContext::new(HttpOptions::default_for_tests()));
            *fetch.request_headers_mut() = get_headers();
            fetcher.fetch(url, &mut fetch);
            assert!(fetch.done(), "{url}");
            assert!(!fetch.success(), "{url}");
        }
        assert!(!fetcher.supports_https());
    }

    #[test]
    fn post_method_fails() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let mut headers = RequestHeaders::new();
        headers.set_method(Method::Post);
        let fetch = fetch_from(&OriginFetcher::default(), &stub.url("/x"), headers);
        assert!(fetch.done());
        assert!(!fetch.success());
        assert!(stub
            .requests()
            .recv_timeout(Duration::from_millis(200))
            .is_err());
    }

    #[test]
    fn decoder_handles_split_feeds() {
        let mut decoder = ChunkDecoder::new(Vec::new());
        assert!(decoder.feed(b"4\r\nWi"));
        assert!(!decoder.complete());
        assert!(decoder.feed(b"ki\r\n0\r\n\r\n"));
        assert!(decoder.complete());
        assert_eq!(b"Wikipedia"[..4], decoder.into_body()[..]);
    }

    #[test]
    fn timeout_override_applies() {
        let fetcher = OriginFetcher::new(5000, 1024);
        assert_eq!(5000, fetcher.timeout_ms());
        assert_eq!(
            DEFAULT_TIMEOUT_MS,
            OriginFetcher::new(-1, 1024).timeout_ms()
        );
        assert_eq!(DEFAULT_TIMEOUT_MS, OriginFetcher::default().timeout_ms());
    }

    #[test]
    fn chain_routes_file_first_then_origin() {
        let dir = std::env::temp_dir().join(format!("pschain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("local.css"), b"local").unwrap();
        let file = crate::file_fetch::FileFetcher::new(
            dir.to_str().unwrap(),
            &["proxy.test".to_string()],
            1024 * 1024,
        )
        .expect("scratch root");
        assert!(file.can_fetch("http://proxy.test/local.css"));
        assert!(!file.can_fetch("http://origin.test/remote.css"));
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nremote");
        let chain = ChainedFetcher::new(Some(file), OriginFetcher::default());
        assert!(chain.supports_https());
        // Mapped name serves from disk.
        let mut local = BufferingFetch::new(
            "http://proxy.test/local.css",
            RequestContext::new(HttpOptions::default_for_tests()),
        );
        *local.request_headers_mut() = get_headers();
        chain.fetch("http://proxy.test/local.css", &mut local);
        assert!(local.success());
        assert_eq!(b"local", local.body());
        for (url, expected) in [
            ("https://proxy.test/local.css", Some(b"local".as_slice())),
            ("https://proxy.test/missing.css", None),
            ("https://foreign.test/local.css", None),
        ] {
            let mut result =
                BufferingFetch::new(url, RequestContext::new(HttpOptions::default_for_tests()));
            *result.request_headers_mut() = get_headers();
            chain.fetch_with_timeout(url, &mut result, 100);
            assert_eq!(result.success(), expected.is_some(), "{url}");
            if let Some(bytes) = expected {
                assert_eq!(result.body(), bytes);
            }
        }
        // Foreign name falls through to the origin.
        let mut remote = BufferingFetch::new(
            &stub.url("/remote.css"),
            RequestContext::new(HttpOptions::default_for_tests()),
        );
        *remote.request_headers_mut() = get_headers();
        chain.fetch(&stub.url("/remote.css"), &mut remote);
        assert!(remote.success());
        assert_eq!(b"remote", remote.body());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn chain_without_file_goes_straight_to_origin() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let chain = ChainedFetcher::new(None, OriginFetcher::default());
        let mut fetch = BufferingFetch::new(
            &stub.url("/x"),
            RequestContext::new(HttpOptions::default_for_tests()),
        );
        *fetch.request_headers_mut() = get_headers();
        chain.fetch_with_timeout(&stub.url("/x"), &mut fetch, 5000);
        assert!(fetch.success());
        assert_eq!(b"ok", fetch.body());
    }
}

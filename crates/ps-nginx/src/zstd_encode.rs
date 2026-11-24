//! Complete-body zstd encoding for the `serve_response` paths
//! (`compress_zstd` on resources, proxy, and in-place serves).
//!
//! The fetch path (HTML) encodes through [`crate::fetch`]'s
//! streaming frame; these paths hold the whole body already, so
//! one shot suffices — with a bonus the streaming path cannot
//! take: bodies that would grow under encoding stay identity.

use ps_http::request::RequestHeaders;

/// Encodes a complete response body when `compress_zstd` applies:
/// filter enabled, body-carrying status on a non-HEAD request,
/// client negotiates zstd, eligible type without an encoding yet,
/// and the encoded form is actually smaller. Returns the encoded
/// bytes (`None` serves identity), mutating `headers` in place:
/// sets Content-Encoding, ensures Vary names Accept-Encoding, and
/// drops Accept-Ranges (encoded bytes are not range-addressable).
/// Content-Length pairs need no touch: `serve_response` recomputes
/// the length from the served body.
#[must_use]
pub fn encode_response_body(
    headers: &mut Vec<(String, String)>,
    body: &[u8],
    status_code: u16,
    request: &RequestHeaders,
    enabled: bool,
) -> Option<Vec<u8>> {
    if !enabled || body.is_empty() {
        return None;
    }
    if crate::fetch_body::response_has_no_body(status_code as i32, request.method().is_head()) {
        return None;
    }
    if !request.accepts_zstd() {
        return None;
    }
    let find = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    if !ps_http::zstd::response_eligible_for_zstd(find("Content-Type"), find("Content-Encoding")) {
        return None;
    }
    let encoded = ps_http::zstd::encode_zstd(body).ok()?;
    if encoded.len() >= body.len() {
        return None;
    }
    headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case("Content-Encoding")
            && !name.eq_ignore_ascii_case("Accept-Ranges")
    });
    headers.push((
        "Content-Encoding".to_string(),
        ps_http::zstd::ZSTD_CONTENT_ENCODING.to_string(),
    ));
    let vary_covered = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("Vary") && ps_http::zstd::vary_names_accept_encoding(value)
    });
    if !vary_covered {
        headers.push(("Vary".to_string(), "Accept-Encoding".to_string()));
    }
    Some(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zstd_request() -> RequestHeaders {
        let mut request = RequestHeaders::new();
        request
            .headers_mut()
            .add("Accept-Encoding", "gzip, deflate, br, zstd");
        request
    }

    fn pair_headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    fn encoded_value(headers: &[(String, String)], name: &str) -> Option<String> {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn encodes_eligible_css() {
        let body = b".a{color:red}".repeat(200);
        let mut headers = pair_headers(&[("Content-Type", "text/css")]);
        let out =
            encode_response_body(&mut headers, &body, 200, &zstd_request(), true).expect("encodes");
        assert!(out.len() < body.len());
        assert_eq!(
            encoded_value(&headers, "Content-Encoding"),
            Some("zstd".to_string())
        );
        assert_eq!(
            encoded_value(&headers, "Vary"),
            Some("Accept-Encoding".to_string())
        );
        let decoded = zstd::bulk::decompress(&out, body.len()).expect("valid frame");
        assert_eq!(decoded, body);
    }

    #[test]
    fn declines_cleanly() {
        let body = b".a{color:red}".repeat(200);
        // Filter off.
        let mut headers = pair_headers(&[("Content-Type", "text/css")]);
        assert!(encode_response_body(&mut headers, &body, 200, &zstd_request(), false).is_none());
        // Client does not negotiate.
        let mut plain = RequestHeaders::new();
        plain.headers_mut().add("Accept-Encoding", "gzip");
        let mut headers = pair_headers(&[("Content-Type", "text/css")]);
        assert!(encode_response_body(&mut headers, &body, 200, &plain, true).is_none());
        // No body statuses.
        for status in [204, 304] {
            let mut headers = pair_headers(&[("Content-Type", "text/css")]);
            assert!(
                encode_response_body(&mut headers, &body, status, &zstd_request(), true).is_none(),
                "status {status}"
            );
        }
        // HEAD keeps identity.
        let mut head = zstd_request();
        head.set_method(ps_http::request::Method::Head);
        let mut headers = pair_headers(&[("Content-Type", "text/css")]);
        assert!(encode_response_body(&mut headers, &body, 200, &head, true).is_none());
        // Already encoded: never stack.
        let mut headers =
            pair_headers(&[("Content-Type", "text/css"), ("Content-Encoding", "gzip")]);
        assert!(encode_response_body(&mut headers, &body, 200, &zstd_request(), true).is_none());
        // Incompressible type.
        let mut headers = pair_headers(&[("Content-Type", "image/jpeg")]);
        assert!(encode_response_body(&mut headers, &body, 200, &zstd_request(), true).is_none());
        // Empty body.
        let mut headers = pair_headers(&[("Content-Type", "text/css")]);
        assert!(encode_response_body(&mut headers, b"", 200, &zstd_request(), true).is_none());
        // Declined means untouched headers.
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn never_bloats_tiny_bodies() {
        let body = b"hi";
        let mut headers = pair_headers(&[("Content-Type", "text/plain")]);
        assert!(encode_response_body(&mut headers, body, 200, &zstd_request(), true).is_none());
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn drops_accept_ranges_and_keeps_vary_coverage() {
        let body = b".a{color:red}".repeat(200);
        let mut headers = pair_headers(&[
            ("Content-Type", "text/css"),
            ("Accept-Ranges", "bytes"),
            ("Vary", "Cookie"),
        ]);
        encode_response_body(&mut headers, &body, 200, &zstd_request(), true).expect("encodes");
        assert!(encoded_value(&headers, "Accept-Ranges").is_none());
        let varies: Vec<String> = headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("Vary"))
            .map(|(_, v)| v.clone())
            .collect();
        assert_eq!(varies, vec!["Cookie", "Accept-Encoding"]);
    }
}

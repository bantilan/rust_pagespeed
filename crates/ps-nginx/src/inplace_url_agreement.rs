//! Decides whether in-place optimization may touch a
//! request: true when the server and the module read it alike
//! (`NgxInPlaceUrlAgrees`).

use ps_http::google_url::GoogleUrl;

/// True when the server and the module read the request
/// alike (`NgxInPlaceUrlAgrees`). `server_uri` is the
/// server's normalized request path (percent-decoded, dot
/// segments resolved, slash runs merged when the server
/// merges them, no query); `server_host` is the host the
/// server matched the request to. `module_path` is the path
/// of the URL the module parsed, in its canonical,
/// percent-encoded form and without query; `module_host` is
/// that URL's host. `merge_slashes` is the server's setting
/// for the request: when set, runs of slashes in the
/// module's decoded path are merged as the server merged
/// them. The module's path is decoded (and merged) and
/// compared byte for byte with the server's; the hosts are
/// compared without regard to case.
#[must_use]
pub fn url_agrees(
    server_uri: &str,
    server_host: &str,
    module_path: &str,
    module_host: &str,
    merge_slashes: bool,
) -> bool {
    if !server_host.eq_ignore_ascii_case(module_host) {
        return false;
    }
    // The server's path is percent-decoded, the module's canonical path
    // keeps its escapes. Decoding the escapes of the module's path puts
    // the two in the same form; the one further step the server takes
    // that the module does not is merging runs of slashes, applied here
    // only when the server applied it. Nothing else -- no separator or
    // dot-segment handling -- so every other normalization on which the
    // two parsers differ shows up as a difference.
    let mut path = GoogleUrl::unescape_ignore_plus(module_path);
    if merge_slashes {
        let mut merged = Vec::with_capacity(path.len());
        for byte in path {
            if byte != b'/' || merged.last() != Some(&b'/') {
                merged.push(byte);
            }
        }
        path = merged;
    }
    path == server_uri.as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "www.example.com";

    // The server's reading is the decoded, normalized path; the module's is
    // the canonical, escaped path. An ordinary path reads the same both ways.
    #[test]
    fn plain_path_is_optimized_in_place() {
        assert!(url_agrees(
            "/styles/site.css",
            HOST,
            "/styles/site.css",
            HOST,
            true
        ));
        assert!(url_agrees(
            "/images/Puzzle.jpg",
            HOST,
            "/images/Puzzle.jpg",
            HOST,
            true
        ));
        assert!(url_agrees("/", HOST, "/", HOST, true));
        assert!(url_agrees("/a+b/c.d.js", HOST, "/a+b/c.d.js", HOST, true));
    }

    // The server decodes a percent-encoded space or a UTF-8 character; the
    // module keeps the escapes. Both still name the same file.
    #[test]
    fn percent_encoded_path_is_optimized_in_place() {
        assert!(url_agrees(
            "/images/a b.png",
            HOST,
            "/images/a%20b.png",
            HOST,
            true
        ));
        assert!(url_agrees(
            "/caf\u{e9}/menu.css",
            HOST,
            "/caf%C3%A9/menu.css",
            HOST,
            true
        ));
        assert!(url_agrees(
            "/\u{65e5}\u{672c}.js",
            HOST,
            "/%E6%97%A5%E6%9C%AC.js",
            HOST,
            true
        ));
        // Reserved characters the module keeps escaped and the server decodes.
        assert!(url_agrees("/100%.css", HOST, "/100%25.css", HOST, true));
        assert!(url_agrees("/q?.css", HOST, "/q%3F.css", HOST, true));
        assert!(url_agrees("/a\"b.css", HOST, "/a%22b.css", HOST, true));
    }

    // A query string takes no part in the decision: both readings are paths
    // without their query, and the same path with any query is the same path.
    #[test]
    fn path_with_query_string_is_optimized_in_place() {
        assert!(url_agrees(
            "/styles/site.css",
            HOST,
            "/styles/site.css",
            HOST,
            true
        ));
        assert!(url_agrees(
            "/images/a b.png",
            HOST,
            "/images/a%20b.png",
            HOST,
            true
        ));
    }

    // A separator the module reads into the path and the server does not, or
    // a dot segment the server resolves and the module does not: different
    // paths, left to the server.
    #[test]
    fn path_the_server_reads_differently_is_left_to_it() {
        assert!(!url_agrees("/a\\b.css", HOST, "/a/b.css", HOST, true));
        assert!(!url_agrees("/b.css", HOST, "/x/b.css", HOST, true));
        assert!(!url_agrees("/b.css", HOST, "/a%2F..%2Fb.css", HOST, true));
        assert!(!url_agrees("/a/b.css", HOST, "/a/B.css", HOST, true));
        assert!(!url_agrees("", HOST, "/a.css", HOST, true));
    }

    // The server merges runs of slashes (merge_slashes, on by default) and
    // the module keeps them; with the server's setting known, the module's
    // decoded path is merged the same way, and the two readings agree. Off,
    // they do not. A slash run ahead of a dot segment reads differently
    // either way: the server merges before it resolves the segment, the
    // module resolves it first.
    #[test]
    fn slash_runs_are_merged_as_the_server_merges_them() {
        assert!(url_agrees("/a/b.css", HOST, "/a//b.css", HOST, true));
        assert!(url_agrees("/a/b.css", HOST, "/a%2F%2Fb.css", HOST, true));
        assert!(url_agrees("/a/b/", HOST, "//a///b//", HOST, true));
        assert!(!url_agrees("/a/b.css", HOST, "/a//b.css", HOST, false));
        assert!(!url_agrees("/b.css", HOST, "/a/b.css", HOST, true));
        assert!(!url_agrees("/b.css", HOST, "/a/b.css", HOST, false));
    }

    // An escaped slash is a separator to the server and an ordinary escape
    // to the module; decoded, both name the same bytes, and the module's key
    // keeps the spelling apart from the slash spelling.
    #[test]
    fn escaped_slash_reads_alike_once_decoded() {
        assert!(url_agrees("/a/b.css", HOST, "/a%2Fb.css", HOST, true));
    }

    // The server matches its server blocks on the host bytes as sent; the
    // module decodes and normalizes the host. A host that reads differently
    // is left to the server, in whatever block it chose.
    #[test]
    fn host_the_server_reads_differently_is_left_to_it() {
        assert!(!url_agrees(
            "/a.css",
            "st%61tic.example.com",
            "/a.css",
            "static.example.com",
            true
        ));
        assert!(!url_agrees(
            "/a.css",
            "0x7f000001",
            "/a.css",
            "127.0.0.1",
            true
        ));
        assert!(!url_agrees(
            "/a.css",
            "a.example.com",
            "/a.css",
            "b.example.com",
            true
        ));
        assert!(!url_agrees("/a.css", "", "/a.css", HOST, true));
    }

    #[test]
    fn host_case_does_not_matter() {
        assert!(url_agrees(
            "/a.css",
            "WWW.Example.com",
            "/a.css",
            HOST,
            true
        ));
    }

    #[test]
    fn literal_address_hosts_agree() {
        assert!(url_agrees(
            "/a.css",
            "127.0.0.1",
            "/a.css",
            "127.0.0.1",
            true
        ));
        assert!(url_agrees("/a.css", "[::1]", "/a.css", "[::1]", true));
    }
}

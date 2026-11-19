//! URL query parameters (`QueryParams`).
//!
//! Parses `x=0&y=1&z=2` into ordered names with optional values:
//! a missing `=` yields no value, an empty one yields `""`.

use crate::google_url::GoogleUrl;

/// Parsed query parameters (`QueryParams`).
#[derive(Debug, Default, Clone)]
pub struct QueryParams {
    entries: Vec<(String, Option<String>)>,
}

impl QueryParams {
    /// Creates empty parameters.
    #[must_use]
    pub fn new() -> Self {
        QueryParams::default()
    }

    /// Parses the query part of a URL (`ParseFromUrl`).
    pub fn parse_from_url(&mut self, url: &GoogleUrl) {
        self.parse_query_string(url.query());
    }

    /// Parses a raw query string through URL sanitizing
    /// (`ParseFromUntrustedString`).
    pub fn parse_from_untrusted_string(&mut self, query: &str) {
        let probe = GoogleUrl::new(&format!("http://example.com/?{query}"));
        if probe.is_web_valid() {
            self.parse_query_string(probe.query());
        } else {
            self.parse_query_string(query);
        }
    }

    /// Parses `&`-separated pairs.
    fn parse_query_string(&mut self, query: &str) {
        self.entries.clear();
        if query.is_empty() {
            return;
        }
        for pair in query.split('&') {
            match pair.find('=') {
                None => self.entries.push((pair.to_string(), None)),
                Some(pos) => self.entries.push((
                    pair[..pos].to_string(),
                    Some(pair[pos + 1..].to_string()),
                )),
            }
        }
    }

    /// Entry count (`size`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether empty (`empty`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Name at an index (`name`).
    #[must_use]
    pub fn name(&self, index: usize) -> &str {
        &self.entries[index].0
    }

    /// Escaped value at an index, if the pair had `=`
    /// (`EscapedValue`).
    #[must_use]
    pub fn escaped_value(&self, index: usize) -> Option<&str> {
        self.entries[index].1.as_deref()
    }

    /// Unescaped value at an index (`UnescapedValue`): `None`
    /// when the pair had no `=` or decoding fails.
    #[must_use]
    pub fn unescaped_value(&self, index: usize) -> Option<String> {
        self.entries[index]
            .1
            .as_deref()
            .and_then(unescape_query_value)
    }

    /// Whether a name occurs (`Has`).
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|(key, _)| key == name)
    }

    /// Single escaped value, if the name occurs exactly once
    /// (`Lookup1Escaped`).
    #[must_use]
    pub fn lookup1_escaped(&self, name: &str) -> Option<&str> {
        let mut found = None;
        for (key, value) in &self.entries {
            if key == name {
                if found.is_some() {
                    return None;
                }
                found = value.as_deref();
            }
        }
        found
    }

    /// Single unescaped value (`Lookup1Unescaped`): `None` when
    /// absent, repeated, valueless, or undecodable.
    #[must_use]
    pub fn lookup1_unescaped(&self, name: &str) -> Option<String> {
        let mut found = None;
        for (key, value) in &self.entries {
            if key == name {
                if found.is_some() {
                    return None;
                }
                found = Some(value.as_deref()?);
            }
        }
        found.and_then(unescape_query_value)
    }

    /// All escaped values for a name (`LookupEscaped`).
    #[must_use]
    pub fn lookup_escaped(&self, name: &str) -> Vec<Option<String>> {
        self.entries
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .collect()
    }
}

/// Decodes `%XX` and `+` (`UnescapeQueryParamValue`-ish):
/// `None` on malformed escapes.
fn unescape_query_value(value: &str) -> Option<String> {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let digit = |byte: u8| {
                    (byte as char).to_digit(16).map(|digit| digit as u8)
                };
                let high = digit(bytes[index + 1])?;
                let low = digit(bytes[index + 2])?;
                out.push(high * 16 + low);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pairs_and_bare_names() {
        let mut params = QueryParams::new();
        params.parse_from_untrusted_string("a=0&b&c=");
        assert_eq!(params.len(), 3);
        assert_eq!(params.unescaped_value(0).as_deref(), Some("0"));
        assert_eq!(params.unescaped_value(1), None);
        assert_eq!(params.unescaped_value(2).as_deref(), Some(""));
        assert!(params.has("b"));
        assert!(!params.has("z"));
    }

    #[test]
    fn lookup1_rejects_repeats_and_bad_escapes() {
        let mut params = QueryParams::new();
        params.parse_from_untrusted_string("a=1&a=2&b=%zz");
        assert_eq!(params.lookup1_unescaped("a"), None);
        assert_eq!(params.lookup1_unescaped("b"), None);
        assert_eq!(params.lookup1_unescaped("missing"), None);
    }

    #[test]
    fn unescapes_plus_and_hex() {
        assert_eq!(
            unescape_query_value("Hello%2c+World%21").as_deref(),
            Some("Hello, World!")
        );
        assert_eq!(unescape_query_value("a%2"), None);
    }
}

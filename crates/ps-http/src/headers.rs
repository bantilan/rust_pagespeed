//! Ordered HTTP header list (`pagespeed/kernel/http/headers.{h,cc}`).
//!
//! Headers keep insertion order with original name spellings; lookups are
//! ASCII case-insensitive. Only `Accept`, `Cache-Control`,
//! `Content-Encoding`, `Connection`, `Accept-Encoding`, and `Vary` values
//! are comma-split on lookup (notably *not* `Cookie`, whose dates contain
//! commas). Cookie maps land with request-cookie utilities.
#![forbid(unsafe_code)]

/// Headers split into comma-separated values on lookup
/// (`IsCommaSeparatedField`).
fn is_comma_separated(name: &str) -> bool {
    [
        "accept",
        "cache-control",
        "content-encoding",
        "connection",
        "accept-encoding",
        "vary",
    ]
    .iter()
    .any(|field| field.eq_ignore_ascii_case(name))
}

/// Splits one header value the way lookups do.
fn split_lookup_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Ordered header list (`Headers`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    /// Creates empty headers.
    #[must_use]
    pub fn new() -> Headers {
        Headers::default()
    }

    /// Clears all attributes (`Clear`, minus proto version fields which
    /// live on the typed wrappers).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of attributes (`NumAttributes`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no attributes are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Name of attribute `i` (`Name`).
    #[must_use]
    pub fn name(&self, index: usize) -> &str {
        &self.entries[index].0
    }

    /// Value of attribute `i` (`Value`).
    #[must_use]
    pub fn value(&self, index: usize) -> &str {
        &self.entries[index].1
    }

    /// Replaces the value of attribute `i` (`SetValue`).
    pub fn set_value(&mut self, index: usize, value: &str) {
        self.entries[index].1 = value.to_string();
    }

    /// Iterates `(name, value)` pairs in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// Looks up values (`Lookup`): comma-split per field, multi-instance
    /// concatenated; `None` when absent.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for (entry_name, value) in &self.entries {
            if entry_name.eq_ignore_ascii_case(name) {
                if is_comma_separated(name) {
                    let mut split = split_lookup_values(value);
                    if split.is_empty() {
                        // C++ pushes the raw value when the split is empty.
                        split.push(value.clone());
                    }
                    out.extend(split);
                } else {
                    out.push(value.clone());
                }
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Joins all values with `", "` (`LookupJoined`; `""` when absent).
    #[must_use]
    pub fn lookup_joined(&self, name: &str) -> String {
        self.lookup(name)
            .map(|values| values.join(", "))
            .unwrap_or_default()
    }

    /// Single value, or `None` unless exactly one exists (`Lookup1`).
    #[must_use]
    pub fn lookup1(&self, name: &str) -> Option<String> {
        match self.lookup(name) {
            Some(values) if values.len() == 1 => Some(values[0].clone()),
            _ => None,
        }
    }

    /// First value, if any (C++ `Headers::Lookup1` semantics: the
    /// first match wins even when several exist).
    #[must_use]
    pub fn lookup_first(&self, name: &str) -> Option<String> {
        self.lookup(name)
            .and_then(|mut values| values.drain(..).next())
    }

    /// Whether any attribute has the name (`Has`).
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|(entry, _)| entry.eq_ignore_ascii_case(name))
    }

    /// Whether a value is among the looked-up values, byte-exact
    /// (`HasValue`).
    #[must_use]
    pub fn has_value(&self, name: &str, value: &str) -> bool {
        self.lookup(name)
            .is_some_and(|values| values.iter().any(|item| item == value))
    }

    /// Case-insensitive value match (`HasValueCaseInsensitive`).
    #[must_use]
    pub fn has_value_case_insensitive(&self, name: &str, value: &str) -> bool {
        self.lookup(name)
            .is_some_and(|values| values.iter().any(|item| item.eq_ignore_ascii_case(value)))
    }

    /// Whether Cache-Control carries a directive (`HasCacheControlDirective`,
    /// via `ElementHasDirective` semantics).
    #[must_use]
    pub fn has_cache_control_directive(&self, directive: &str) -> bool {
        self.lookup(crate::names::header::CACHE_CONTROL)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| element_has_directive(value, directive))
            })
    }

    /// Adds a header, even if the name exists (`Add`).
    pub fn add(&mut self, name: &str, value: &str) {
        self.entries.push((name.to_string(), value.to_string()));
    }

    /// Removes name/value pairs (`Remove`): values match
    /// case-insensitively; comma fields rejoin all survivors into one
    /// line, other fields keep surviving lines. All instances removed.
    pub fn remove(&mut self, name: &str, value: &str) -> bool {
        // Gather split values across every matching line first.
        let mut split: Vec<String> = Vec::new();
        for (entry_name, entry_value) in &self.entries {
            if !entry_name.eq_ignore_ascii_case(name) {
                continue;
            }
            if is_comma_separated(entry_name) {
                split.extend(split_lookup_values(entry_value));
            } else {
                split.push(entry_value.clone());
            }
        }
        let matched = split.iter().any(|item| item.eq_ignore_ascii_case(value));
        if !matched {
            return false;
        }
        let survivors: Vec<&String> = split
            .iter()
            .filter(|item| !item.is_empty() && !item.eq_ignore_ascii_case(value))
            .collect();
        self.remove_all(name);
        if is_comma_separated(name) {
            if !survivors.is_empty() {
                let joined = survivors
                    .iter()
                    .map(|item| item.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.add(name, &joined);
            }
        } else {
            for item in survivors {
                self.add(name, item);
            }
        }
        true
    }

    /// Removes all headers with the name (`RemoveAll`).
    pub fn remove_all(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|(entry, _)| !entry.eq_ignore_ascii_case(name));
        before != self.entries.len()
    }

    /// Removes headers named in a case-insensitively sorted list
    /// (`RemoveAllFromSortedArray`).
    pub fn remove_all_from_sorted(&mut self, names: &[&str]) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(entry, _)| {
            names
                .binary_search_by(|probe| {
                    probe.to_ascii_lowercase().cmp(&entry.to_ascii_lowercase())
                })
                .is_err()
        });
        before != self.entries.len()
    }

    /// Removes headers with the name prefix (`RemoveAllWithPrefix`,
    /// case-insensitive like the map-based original).
    pub fn remove_all_with_prefix(&mut self, prefix: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(entry, _)| {
            entry.len() < prefix.len() || !entry[..prefix.len()].eq_ignore_ascii_case(prefix)
        });
        before != self.entries.len()
    }

    /// Removes everything not present in `keep` (`RemoveIfNotIn`).
    pub fn remove_if_not_in(&mut self, keep: &Headers) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(name, value)| {
            keep.lookup(name)
                .is_some_and(|values| values.iter().any(|item| item == value))
        });
        before != self.entries.len()
    }

    /// Replaces all instances with one (`Replace`; order may change).
    pub fn replace(&mut self, name: &str, value: &str) {
        self.remove_all(name);
        self.add(name, value);
    }

    /// Merges headers from `other` (`UpdateFrom`): names present in
    /// `other` are replaced wholesale (order-scrambling).
    pub fn update_from(&mut self, other: &Headers) {
        let mut names: Vec<&str> = other
            .entries
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        names.sort_by_key(|a| a.to_ascii_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        self.remove_all_from_sorted(&names);
        for (name, value) in &other.entries {
            self.add(name, value);
        }
    }

    /// Serializes as an HTTP block (`WriteAsHttp`).
    #[must_use]
    pub fn write_as_http(&self) -> String {
        let mut out = String::new();
        for (name, value) in &self.entries {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out
    }
}

/// Whether a comma element carries a directive (`ElementHasDirective`):
/// `;`-separated pieces match by exact name or `name=`/`name `/`name\t`
/// prefix, case-insensitively.
#[must_use]
pub fn element_has_directive(element: &str, name: &str) -> bool {
    for piece in element.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        if piece.eq_ignore_ascii_case(name) {
            return true;
        }
        if piece.len() > name.len()
            && piece[..name.len()].eq_ignore_ascii_case(name)
            && matches!(piece.as_bytes()[name.len()], b'=' | b' ' | b'\t')
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_splitting() {
        let mut headers = Headers::new();
        headers.add("Cache-Control", "max-age=60, public");
        headers.add("Cache-Control", "no-transform");
        assert_eq!(
            headers.lookup("cache-control"),
            Some(
                vec!["max-age=60", "public", "no-transform"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            )
        );
        // Cookies are NOT comma-split (dates contain commas).
        headers.add("Cookie", "a=1, b=2");
        assert_eq!(headers.lookup("cookie"), Some(vec!["a=1, b=2".to_string()]));
        // Vary splits; Date does not.
        headers.add("Vary", "Accept-Encoding, Cookie");
        assert_eq!(headers.lookup("vary").unwrap().len(), 2);
        headers.add("Date", "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(headers.lookup("date").unwrap().len(), 1);
        assert_eq!(headers.lookup("missing"), None);
        assert_eq!(
            headers.lookup_joined("Cache-Control"),
            "max-age=60, public, no-transform"
        );
        assert_eq!(headers.lookup1("Cookie"), Some("a=1, b=2".to_string()));
        assert_eq!(headers.lookup1("Cache-Control"), None); // several values
    }

    #[test]
    fn predicates() {
        let mut headers = Headers::new();
        headers.add("Cache-Control", "Public, max-age=60");
        assert!(headers.has("cache-control"));
        assert!(!headers.has("Cookie"));
        assert!(!headers.has_value("Cache-Control", "public")); // exact
        assert!(headers.has_value_case_insensitive("Cache-Control", "public"));
        assert!(headers.has_cache_control_directive("max-age"));
        assert!(headers.has_cache_control_directive("PUBLIC"));
        assert!(!headers.has_cache_control_directive("no-store"));
        assert!(element_has_directive("max-age=60", "max-age"));
        assert!(element_has_directive("max-age =60", "max-age"));
        assert!(!element_has_directive("s-maxage=60", "max-age"));
    }

    #[test]
    fn mutations() {
        let mut headers = Headers::new();
        headers.add("Cache-Control", "max-age=60, public");
        headers.add("X-Test", "a");
        headers.add("X-Test", "b");
        assert!(headers.remove("Cache-Control", "public"));
        assert_eq!(headers.lookup_joined("Cache-Control"), "max-age=60");
        assert!(!headers.remove("Cache-Control", "public"));
        assert!(headers.remove("X-Test", "a"));
        assert_eq!(headers.lookup1("X-Test"), Some("b".to_string()));
        headers.replace("X-Test", "c");
        assert_eq!(headers.lookup1("X-Test"), Some("c".to_string()));
        assert!(headers.remove_all("cache-control"));
        assert!(!headers.has("Cache-Control"));
        // Prefix and sorted removals.
        headers.add("X-Keep", "1");
        headers.add("X-Drop-A", "1");
        headers.add("X-Drop-B", "1");
        assert!(headers.remove_all_with_prefix("x-drop"));
        assert!(headers.has("X-Keep"));
        assert!(!headers.has("X-Drop-A"));
        assert!(headers.remove_all_from_sorted(&["x-keep", "x-test"]));
        assert!(headers.is_empty());
    }

    #[test]
    fn remove_case_and_rejoin() {
        let mut headers = Headers::new();
        headers.add("Cache-Control", "max-age=60, PUBLIC");
        // Values match case-insensitively; survivors rejoin into one line.
        assert!(headers.remove("cache-control", "public"));
        assert_eq!(headers.lookup_joined("Cache-Control"), "max-age=60");
        headers.add("Cache-Control", "no-transform");
        assert!(headers.remove("Cache-Control", "MAX-AGE=60"));
        // Cross-line survivors collapse into a single comma line.
        assert_eq!(headers.lookup_joined("Cache-Control"), "no-transform");
        assert!(!headers.remove("Cache-Control", "absent"));
        // Empty values are dropped on rebuild but do not count as matches.
        let mut headers = Headers::new();
        headers.add("X-Empty", "");
        assert!(!headers.remove("X-Empty", "x"));
        assert!(headers.has("X-Empty"));
    }

    #[test]
    fn update_from_and_http_shape() {
        let mut base = Headers::new();
        base.add("A", "1");
        base.add("B", "1");
        let mut fresh = Headers::new();
        fresh.add("b", "2");
        fresh.add("C", "3");
        base.update_from(&fresh);
        assert_eq!(base.lookup1("A"), Some("1".to_string()));
        assert_eq!(base.lookup1("B"), Some("2".to_string()));
        assert_eq!(base.lookup1("C"), Some("3".to_string()));
        assert_eq!(base.write_as_http(), "A: 1\r\nb: 2\r\nC: 3\r\n\r\n");
    }
}

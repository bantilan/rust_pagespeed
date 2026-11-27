//! Positive-only cross-page stylesheet policy. Entries identify validated bytes,
//! expire no later than the input response, and never authorize deferral.
use crate::server_context::ResourceDriver;
use ps_cache::{cache::KeyState, hash::Hasher};
use std::collections::BTreeMap;

const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 16 * 1024;
const MAX_AGE_MS: i64 = 3_600_000;
const VERSION: &str = "critical-css-blocking/1";

fn key(driver: &ResourceDriver) -> String {
    let identity = format!(
        "{}\n{}",
        driver.base_url().origin(),
        driver.options_signature()
    );
    format!(
        "critical-css-blocking-v1:{}",
        driver
            .server_context()
            .contents_hasher()
            .hash(identity.as_bytes())
    )
}
fn version(driver: &ResourceDriver, url: &str, bytes: &[u8]) -> String {
    let hasher = driver.server_context().contents_hasher();
    format!("{}:{}", hasher.hash(url.as_bytes()), hasher.hash(bytes))
}
fn decode(bytes: &[u8], now: i64) -> BTreeMap<String, i64> {
    let parse = || -> Option<BTreeMap<String, i64>> {
        if bytes.len() > MAX_BYTES {
            return None;
        }
        let mut lines = std::str::from_utf8(bytes).ok()?.lines();
        if lines.next()? != VERSION {
            return None;
        }
        let mut entries = BTreeMap::new();
        for (i, line) in lines.enumerate() {
            if i >= MAX_ENTRIES {
                return None;
            }
            let (id, expires) = line.split_once('\t')?;
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-:".contains(&b))
            {
                return None;
            }
            let expires: i64 = expires.parse().ok()?;
            if entries.insert(id.to_string(), expires).is_some() {
                return None;
            }
        }
        entries.retain(|_, expires| *expires > now);
        Some(entries)
    };
    parse().unwrap_or_default()
}
fn encode(entries: &BTreeMap<String, i64>) -> Vec<u8> {
    let mut out = format!("{VERSION}\n");
    for (id, expires) in entries {
        out.push_str(&format!("{id}\t{expires}\n"));
    }
    out.into_bytes()
}
struct Parsed {
    key: String,
    bytes: Vec<u8>,
    parsed_ms: i64,
    next_expiry: i64,
    entries: BTreeMap<String, i64>,
    identity: Vec<u8>,
}
thread_local! {
    // Bounded worker-local parsing cache; every lookup still reads shared bytes.
    // Exact byte comparison catches cross-worker changes without polling delays.
    static PARSED: std::cell::RefCell<std::collections::VecDeque<Parsed>> = const {
        std::cell::RefCell::new(std::collections::VecDeque::new())
    };
}
fn with_entries<R>(
    driver: &ResourceDriver,
    use_entries: impl FnOnce(&BTreeMap<String, i64>, &[u8]) -> R,
) -> R {
    let server = driver.server_context();
    let key = key(driver);
    let lookup = server.metadata_cache().get(&key);
    let bytes = if lookup.state == KeyState::Available {
        lookup.value.unwrap_or_default()
    } else {
        Vec::new()
    };
    let now = server.now_ms();
    PARSED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache.iter().position(|old| {
            old.key == key && old.bytes == bytes && old.parsed_ms <= now && now < old.next_expiry
        }) {
            let old = cache.remove(index).unwrap();
            cache.push_back(old);
        } else {
            cache.retain(|old| old.key != key);
            let entries = decode(&bytes, now);
            let next_expiry = entries.values().copied().min().unwrap_or(i64::MAX);
            let identity = server
                .contents_hasher()
                .hash(&encode(&entries))
                .into_bytes();
            if cache.len() >= 8 {
                cache.pop_front();
            }
            cache.push_back(Parsed {
                key,
                bytes: if bytes.len() <= MAX_BYTES {
                    bytes
                } else {
                    Vec::new()
                },
                parsed_ms: now,
                next_expiry,
                entries,
                identity,
            });
        }
        let parsed = cache.back().unwrap();
        use_entries(&parsed.entries, &parsed.identity)
    })
}
#[cfg(test)]
fn entries(driver: &ResourceDriver) -> BTreeMap<String, i64> {
    with_entries(driver, |entries, _| entries.clone())
}
/// Active decision digest changes on shared updates, expiry, purge or eviction.
/// A bounded worker parsing cache never skips the shared metadata read.
pub fn snapshot(driver: &ResourceDriver) -> Vec<u8> {
    if !driver.options().critical_css_keep_blocking() {
        return Vec::new();
    }
    with_entries(driver, |_, identity| identity.to_vec())
}
/// Requires the same URL AND current validated response contents.
pub fn known(driver: &ResourceDriver, url: &str, bytes: &[u8]) -> bool {
    if !driver.options().critical_css_keep_blocking() {
        return false;
    }
    let version = version(driver, url, bytes);
    with_entries(driver, |entries, _| entries.contains_key(&version))
}
/// Record a positive decision from trusted selector evidence. Returns false
/// for expired inputs or contention. No retry, synchronous logging or fetch.
pub fn remember(driver: &ResourceDriver, url: &str, bytes: &[u8], expires: i64) -> bool {
    if !driver.options().critical_css_keep_blocking() {
        return false;
    }
    let server = driver.server_context();
    let now = server.now_ms();
    let expires = expires.min(now.saturating_add(MAX_AGE_MS));
    if expires <= now {
        return false;
    }
    let id = version(driver, url, bytes);
    let mut recorded = false;
    server.metadata_cache().update(&key(driver), &mut |lookup| {
        let mut entries = lookup
            .value
            .as_deref()
            .map(|b| decode(b, now))
            .unwrap_or_default();
        if entries.get(&id).is_some_and(|old| *old >= expires) {
            recorded = true;
            return None;
        }
        if entries.len() >= MAX_ENTRIES && !entries.contains_key(&id) {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, expires)| *expires)
                .map(|(id, _)| id.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(id.clone(), expires);
        recorded = true;
        Some(encode(&entries))
    });
    recorded
}

#[cfg(test)]
mod tests {
    use super::*;
    fn driver(
        server: std::rc::Rc<crate::server_context::ServerContext>,
        url: &str,
    ) -> ResourceDriver {
        ResourceDriver::new(
            server,
            std::rc::Rc::new(std::cell::RefCell::new(
                ps_config::options::RewriteOptions::new(),
            )),
            url,
        )
    }
    #[test]
    fn concurrent_positive_updates_preserve_every_decision() {
        use std::{rc::Rc, sync::Arc};
        let metadata = Arc::new(ps_cache::cache::MemoryCache::new());
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let metadata = metadata.clone();
                std::thread::spawn(move || {
                    let server = Rc::new(crate::server_context::ServerContext::new_with_metadata(
                        Arc::new(ps_cache::cache::MemoryCache::new()),
                        metadata,
                        Arc::new(ps_http::mock::MockUrlFetcher::new()),
                        1000,
                    ));
                    let d = driver(server, "https://site.test/page");
                    for i in 0..8 {
                        assert!(remember(
                            &d,
                            &format!("https://site.test/{worker}-{i}.css"),
                            b"div{}",
                            6000
                        ));
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let server = Rc::new(crate::server_context::ServerContext::new_with_metadata(
            Arc::new(ps_cache::cache::MemoryCache::new()),
            metadata,
            Arc::new(ps_http::mock::MockUrlFetcher::new()),
            1000,
        ));
        assert_eq!(
            entries(&driver(server, "https://site.test/other")).len(),
            64
        );
    }

    #[test]
    fn decisions_share_pages_but_require_content_url_freshness_origin_and_options() {
        use std::{rc::Rc, sync::Arc};
        let metadata = Arc::new(ps_cache::cache::MemoryCache::new());
        let server = Rc::new(crate::server_context::ServerContext::new_with_metadata(
            Arc::new(ps_cache::cache::MemoryCache::new()),
            metadata.clone(),
            Arc::new(ps_http::mock::MockUrlFetcher::new()),
            1000,
        ));
        let a = driver(server.clone(), "https://site.test/a");
        let b = driver(server.clone(), "https://site.test/b");
        let url = "https://site.test/style.css";
        let empty = snapshot(&b);
        assert!(remember(&a, url, b"div{color:red}", 3000));
        assert!(known(&b, url, b"div{color:red}"));
        assert_ne!(empty, snapshot(&b));
        assert!(!known(&b, url, b"div{color:blue}"));
        assert!(!known(&b, "https://site.test/other.css", b"div{color:red}"));
        let other = driver(server.clone(), "https://other.test/a");
        assert!(!known(&other, url, b"div{color:red}"));
        let mut opts = ps_config::options::RewriteOptions::new();
        opts.set_option("CssInlineMaxBytes", "123", ps_config::scope::Scope::Server)
            .unwrap();
        let different = ResourceDriver::new(
            server.clone(),
            Rc::new(std::cell::RefCell::new(opts)),
            "https://site.test/b",
        );
        assert!(!known(&different, url, b"div{color:red}"));
        server.timer().borrow_mut().advance_ms(2000);
        assert!(!known(&b, url, b"div{color:red}"));
        assert_eq!(empty, snapshot(&b));
        assert!(!remember(&a, url, b"div{color:red}", 3000));
        for i in 0..MAX_ENTRIES + 1 {
            assert!(remember(
                &a,
                &format!("https://site.test/{i}.css"),
                b"a{}",
                6000
            ));
        }
        assert_eq!(entries(&a).len(), MAX_ENTRIES);
        assert!(snapshot(&a).len() < MAX_BYTES);
        ps_cache::cache::Cache::delete(metadata.as_ref(), &key(&a));
        assert!(entries(&a).is_empty());
    }

    #[test]
    fn bounded_codec_discards_expired_corrupt_and_oversized_records() {
        let entries = [("a:b".into(), 200)].into();
        assert_eq!(decode(&encode(&entries), 100), entries);
        assert!(decode(&encode(&entries), 200).is_empty());
        for bad in [
            b"garbage".as_slice(),
            b"critical-css-blocking/1\na:b\t200\na:b\t300\n",
            b"critical-css-blocking/1\na:b\tnan\n",
        ] {
            assert!(decode(bad, 0).is_empty());
        }
        let many: BTreeMap<_, _> = (0..129).map(|i| (format!("{i}:b"), 200)).collect();
        assert!(decode(&encode(&many), 0).is_empty());
    }
}

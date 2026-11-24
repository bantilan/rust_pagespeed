//! Shared metadata validation and recording for asynchronous combiners.
use crate::cacheable_resource::InputInfo;
use crate::metadata::{CachedResult, InputValidity, OutputPartitions};
use crate::resource::{InputResource, Resource};
use crate::server_context::ResourceDriver;
use std::cell::RefCell;

/// Retry delay for genuinely pending (held-fetch) inputs: the table
/// expires almost immediately so a stuck input cannot freeze
/// fallback HTML for longer than a flush window.
const PENDING_RETRY_MS: i64 = 1000;

/// Floor for failed-input memory: every combination miss marks the
/// render detached (vetoing HTML publication), so unbounded
/// re-attempts would disable HTML caching on any page with a
/// permanently uncombinable candidate (404s, uncacheable origins).
/// Failed candidates re-attempt at most once a minute no matter
/// how the input-errors TTL is configured.
const MIN_FAILURE_MEMORY_MS: i64 = 60_000;

pub(crate) struct Recorder {
    tables: RefCell<OutputPartitions>,
    /// How long failed (non-pending) candidates stay remembered.
    errors_ttl_ms: i64,
}
impl Recorder {
    /// Records with failed-input memory (floored, see
    /// [`MIN_FAILURE_MEMORY_MS`]).
    pub fn new(errors_ttl_ms: i64) -> Self {
        Recorder {
            tables: RefCell::new(OutputPartitions::default()),
            errors_ttl_ms: errors_ttl_ms.max(MIN_FAILURE_MEMORY_MS),
        }
    }
    /// Records a concluded failure (unmappable URL, failed fetch)
    /// on the input-errors TTL.
    pub fn missing_failed(&self, url: &str, now_ms: i64) {
        self.missing(url, now_ms, self.errors_ttl_ms);
    }
    pub fn missing(&self, url: &str, now_ms: i64, ttl_ms: i64) {
        let mut tables = self.tables.borrow_mut();
        // A failed candidate must not discard successful neighboring partitions.
        // Known failures keep their original URLs, just as
        // ordinary negative rewrite partitions do; the TTL bounds
        // re-attempts. Pending work cannot freeze fallback HTML and
        // is marked separately below.
        tables
            .debug_message
            .push("async-combination-partial".into());
        tables.other_dependency.push(InputInfo {
            url: Some(url.to_string()),
            date_ms: Some(now_ms),
            expiration_time_ms: Some(now_ms.saturating_add(ttl_ms.max(0))),
            ..Default::default()
        });
    }
    pub fn dependency(&self, input: &InputResource, now_ms: i64) {
        if !input.loaded() || !input.is_valid_and_cacheable() {
            // Held fetches retry almost immediately (their marker
            // keeps the render detached); concluded failures rest on
            // the input-errors TTL instead of re-detaching every
            // render, which would veto HTML publication forever.
            if input.load_pending() {
                self.missing(input.url(), now_ms, PENDING_RETRY_MS);
                self.tables
                    .borrow_mut()
                    .debug_message
                    .push("async-combination-pending".into());
            } else {
                self.missing(input.url(), now_ms, self.errors_ttl_ms);
            }
            return;
        }
        let mut info = InputInfo::default();
        input.fill_in_partition_input_info(true, &mut info);
        self.tables.borrow_mut().other_dependency.push(info);
    }
    pub fn record(&self, indices: impl Iterator<Item = u32>, url: String) {
        self.tables.borrow_mut().partition.push(CachedResult {
            optimizable: true,
            url,
            input: indices
                .map(|index| InputInfo {
                    index: Some(index as i32),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        });
    }
    pub fn finish(self) -> Option<OutputPartitions> {
        Some(self.tables.into_inner())
    }
}

pub(crate) fn lookup(driver: &ResourceDriver, key: &str, count: usize) -> Option<OutputPartitions> {
    let bytes = driver.server_context().metadata_cache().get(key).value?;
    let tables = crate::metadata::decode_partitions(&bytes)?;
    // A record must depend on every candidate, including uncombined ones.
    if tables.other_dependency.len() != count {
        return None;
    }
    let options = driver.options();
    if !tables.other_dependency.iter().all(|input| {
        let valid = input
            .url
            .as_deref()
            .is_none_or(|url| options.is_url_cache_valid(url, input.date_ms.unwrap_or(0)));
        matches!(
            crate::metadata::is_input_valid(
                input,
                driver.server_context().now_ms(),
                false,
                0,
                valid
            ),
            InputValidity::Valid
        )
    }) {
        return None;
    }
    let mut seen = vec![false; count];
    for partition in &tables.partition {
        if !partition.optimizable
            || partition.input.len() < 2
            || !ps_http::google_url::GoogleUrl::new(&partition.url).is_web_valid()
        {
            return None;
        }
        for input in &partition.input {
            let index = usize::try_from(input.index?).ok()?;
            if index >= count || seen[index] {
                return None;
            }
            seen[index] = true;
        }
    }
    if tables
        .debug_message
        .iter()
        .any(|message| message == "async-combination-pending")
    {
        driver.note_detached_rewrite();
    }
    Some(tables)
}

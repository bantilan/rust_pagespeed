//! Append-only statistics log: writer, reader, recovery, JSON dumps.
//!
//! Ports `pagespeed/kernel/util/statistics_logger.cc`
//! (`StatisticsLogger`, `StatisticsLogfileReader`):
//!
//! - Block format: `timestamp: <ms>` line, `statistics_instance: <id>`
//!   line, then `name: <int>` lines (alphabetical, as C++ iterates a
//!   `std::map`).
//! - Reader: yields blocks whose timestamp is within `[start, end]` and at
//!   least `granularity_ms` past the previously yielded stamp.
//! - Trimming deletes the whole file past the size cap (no rotation).
//! - `previous_dashboard_counters` recovers pre-restart deltas with the
//!   C++ per-instance reset semantics (16 MiB scan cap).
//! - `dump_json` renders `{timestamps, variables}` with `"0"` placeholders
//!   and `"{}"` for a missing log.
#![forbid(unsafe_code)]

use crate::names::DASHBOARD_COUNTERS;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Refuse to scan logs larger than this for recovery (16 MiB, as in C++).
pub const MAX_RECOVERY_SCAN_BYTES: u64 = 16 * 1024 * 1024;

/// Lines longer than this are skipped while parsing var data (as in C++).
pub const MAX_VAR_LINE_BYTES: usize = 1024;

/// One parsed log block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogBlock {
    /// Block timestamp in ms.
    pub timestamp_ms: i64,
    /// Statistics instance id (`0` when missing/unparseable, as in C++).
    pub instance_id: i64,
    /// `name -> raw value text` for well-formed `name: value` lines.
    pub vars: HashMap<String, String>,
}

/// Appends one block to the log (`DumpConsoleVarsToWriter`).
///
/// `vars` are written in byte order (C++ iterates `std::map`).
pub fn append_block(
    path: &Path,
    timestamp_ms: i64,
    instance_id: i64,
    vars: &HashMap<String, i64>,
) -> std::io::Result<()> {
    let mut names: Vec<&String> = vars.keys().collect();
    names.sort();
    let mut text = format!("timestamp: {timestamp_ms}\n");
    text.push_str(&format!("statistics_instance: {instance_id}\n"));
    for name in names {
        text.push_str(&format!("{name}: {}\n", vars[name]));
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(text.as_bytes())?;
    file.flush()
}

/// Deletes the log when it exceeds the cap (`TrimLogfileIfNeeded`).
///
/// Returns true when the file was removed.
pub fn trim_log_if_needed(path: &Path, max_bytes: u64) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > max_bytes => std::fs::remove_file(path).is_ok(),
        _ => false,
    }
}

/// Parses one block body (lines after the `timestamp:` line).
fn parse_block_body(body: &str) -> (i64, HashMap<String, String>) {
    let mut vars = HashMap::new();
    for line in body.split('\n') {
        if line.len() > MAX_VAR_LINE_BYTES {
            continue;
        }
        let Some((name, value)) = line.split_once(": ") else {
            continue;
        };
        vars.insert(name.to_string(), value.to_string());
    }
    let instance = vars
        .get("statistics_instance")
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(0);
    (instance, vars)
}

/// Reads blocks in `[start_ms, end_ms]` honoring granularity
/// (`StatisticsLogfileReader::ReadNextDataBlock`).
///
/// A block is yielded when its timestamp is within range and at least
/// `granularity_ms` past the previously yielded stamp (starting from 0).
/// Trailing bytes after the last newline that do not form a complete
/// `timestamp:` line yield nothing.
pub fn read_blocks(path: &Path, start_ms: i64, end_ms: i64, granularity_ms: i64) -> Vec<LogBlock> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    // Split into raw blocks at each "timestamp: " occurrence, as C++ scans
    // for the next occurrence to delimit the current block.
    let mut out = Vec::new();
    let mut rest = text.as_str();
    // Leading bytes before the first marker are ignored (C++ requires the
    // buffer to start with the marker; a conforming log always does).
    let Some(first) = rest.find("timestamp: ") else {
        return out;
    };
    rest = &rest[first..];
    let mut old_timestamp: i64 = 0;
    loop {
        let line_end = rest.find('\n');
        let Some(newline) = line_end else {
            break; // Partial record without a newline yields nothing.
        };
        let stamp_text = rest["timestamp: ".len()..newline].trim();
        // C++ `StringToInt64` failure leaves the previous value; a garbage
        // stamp therefore reuses the old one and is skipped by range.
        let timestamp = stamp_text.parse::<i64>().unwrap_or(old_timestamp);
        let after_line = &rest[newline + 1..];
        let block_end = after_line.find("timestamp: ").unwrap_or(after_line.len());
        let body = &after_line[..block_end];
        if timestamp >= start_ms
            && timestamp <= end_ms
            && timestamp >= old_timestamp + granularity_ms
        {
            let (instance, vars) = parse_block_body(body);
            out.push(LogBlock {
                timestamp_ms: timestamp,
                instance_id: instance,
                vars,
            });
            old_timestamp = timestamp;
        }
        if block_end == after_line.len() {
            break;
        }
        rest = &after_line[block_end..];
    }
    out
}

/// Recovers pre-restart dashboard deltas (`PreviousDashboardCounters`).
///
/// Scans blocks older than `instance_start_ms`, skipping blocks from
/// `current_instance_id`. Within one instance id, per-counter deltas
/// accumulate; a decreasing sample counts as a reset (delta = value).
/// Corrupt overflows are ignored. Returns `None` when the log is missing
/// or exceeds [`MAX_RECOVERY_SCAN_BYTES`].
pub fn previous_dashboard_counters(
    path: &Path,
    instance_start_ms: i64,
    current_instance_id: i64,
) -> Option<HashMap<String, i64>> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_RECOVERY_SCAN_BYTES {
        return None;
    }
    let blocks = read_blocks(path, 0, instance_start_ms - 1, 0);
    let mut previous: HashMap<&str, i64> = HashMap::new();
    let mut totals: HashMap<&str, i64> = HashMap::new();
    let mut last_instance: i64 = 0;
    for block in &blocks {
        if block.timestamp_ms >= instance_start_ms {
            break;
        }
        if block.instance_id == current_instance_id {
            continue;
        }
        if block.instance_id != last_instance {
            previous.clear();
        }
        last_instance = block.instance_id;
        for name in DASHBOARD_COUNTERS {
            let Some(text) = block.vars.get(*name) else {
                continue;
            };
            let Ok(value) = text.parse::<i64>() else {
                continue;
            };
            if value < 0 {
                continue;
            }
            let before = previous.get(name).copied().unwrap_or(0);
            let delta = if value >= before {
                value - before
            } else {
                value
            };
            let total = totals.entry(name).or_insert(0);
            if delta <= i64::MAX - *total {
                *total += delta;
            }
            previous.insert(name, value);
        }
    }
    let mut out = HashMap::new();
    for (name, total) in totals {
        out.insert(name.to_string(), total);
    }
    Some(out)
}

/// Escapes a var name for JSON (`HtmlKeywords::Escape` then
/// `EscapeToJsStringLiteral` with quotes; identity for plain names).
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Renders logged samples as JSON (`StatisticsLogger::DumpJSON`).
///
/// With `for_graphs`, the fixed graphs var list is dumped; otherwise exactly
/// `var_titles` (sorted, as C++ iterates a `std::set`). Missing samples
/// become `"0"` placeholders. A missing log renders `"{}"`.
pub fn dump_json(
    path: &Path,
    for_graphs: bool,
    var_titles: &[&str],
    start_ms: i64,
    end_ms: i64,
    granularity_ms: i64,
) -> String {
    if std::fs::metadata(path).is_err() {
        return "{}".to_string();
    }
    let titles: Vec<&str> = if for_graphs {
        crate::names::GRAPHS_VARS.to_vec()
    } else {
        let mut titles = var_titles.to_vec();
        titles.sort_unstable();
        titles.dedup();
        titles
    };
    let blocks = read_blocks(path, start_ms, end_ms, granularity_ms);
    let mut out = String::from("{\"timestamps\": [");
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&block.timestamp_ms.to_string());
    }
    out.push_str("],\"variables\": {");
    for (i, title) in titles.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_string(title));
        out.push_str(": [");
        for (j, block) in blocks.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            match block.vars.get(*title) {
                Some(value) => out.push_str(value),
                None => out.push('0'),
            }
        }
        out.push(']');
    }
    out.push_str("}}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("ps_stats_test_{tag}_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn sample_vars(pairs: &[(&str, i64)]) -> HashMap<String, i64> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), *value))
            .collect()
    }

    #[test]
    fn block_round_trip_and_order() {
        let path = temp_path("roundtrip");
        append_block(&path, 1000, 7, &sample_vars(&[("b_var", 2), ("a_var", 1)])).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // Exact block bytes: timestamp, instance, vars alphabetical.
        assert_eq!(
            text,
            "timestamp: 1000\nstatistics_instance: 7\na_var: 1\nb_var: 2\n"
        );
        let blocks = read_blocks(&path, 0, 2000, 0);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].timestamp_ms, 1000);
        assert_eq!(blocks[0].instance_id, 7);
        assert_eq!(blocks[0].vars.get("a_var").unwrap(), "1");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reader_range_and_granularity() {
        let path = temp_path("range");
        for stamp in [1000, 1500, 3000] {
            append_block(&path, stamp, 7, &sample_vars(&[("v", stamp)])).unwrap();
        }
        // Range filter.
        let blocks = read_blocks(&path, 1000, 2000, 0);
        assert_eq!(blocks.len(), 2);
        // Granularity: second block within 1000ms of the first is skipped.
        let blocks = read_blocks(&path, 0, 5000, 1000);
        let stamps: Vec<i64> = blocks.iter().map(|block| block.timestamp_ms).collect();
        assert_eq!(stamps, vec![1000, 3000]);
        // Partial trailing record yields nothing.
        std::fs::write(&path, "timestamp: 9999").unwrap();
        assert!(read_blocks(&path, 0, i64::MAX, 0).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn trim_deletes_whole_file() {
        let path = temp_path("trim");
        append_block(&path, 1, 1, &sample_vars(&[("v", 1)])).unwrap();
        assert!(!trim_log_if_needed(&path, 1024));
        assert!(path.exists());
        assert!(trim_log_if_needed(&path, 1));
        assert!(!path.exists());
        assert!(!trim_log_if_needed(&path, 1)); // missing: no-op
    }

    #[test]
    fn previous_counters_reset_semantics() {
        let path = temp_path("prev");
        // Instance 1: counter rises 10 -> 25.
        append_block(&path, 1000, 1, &sample_vars(&[("image_rewrites", 10)])).unwrap();
        append_block(&path, 2000, 1, &sample_vars(&[("image_rewrites", 25)])).unwrap();
        // Instance 2 (restart): counter restarts at 5, rises to 8.
        append_block(&path, 3000, 2, &sample_vars(&[("image_rewrites", 5)])).unwrap();
        append_block(&path, 4000, 2, &sample_vars(&[("image_rewrites", 8)])).unwrap();
        // Current instance 3 started at 5000: recover 25 + 8 = 33.
        let counters = previous_dashboard_counters(&path, 5000, 3).unwrap();
        assert_eq!(counters.get("image_rewrites"), Some(&33));
        // Blocks from the current instance are skipped.
        append_block(&path, 4500, 3, &sample_vars(&[("image_rewrites", 100)])).unwrap();
        let counters = previous_dashboard_counters(&path, 5000, 3).unwrap();
        assert_eq!(counters.get("image_rewrites"), Some(&33));
        // Only dashboard counters are recovered.
        assert!(!counters.contains_key("v"));
        // Missing log -> None.
        assert!(previous_dashboard_counters(&temp_path("missing"), 5000, 3).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dump_json_shape_placeholders_and_missing() {
        let path = temp_path("json");
        append_block(&path, 1000, 1, &sample_vars(&[("a_var", 1), ("b_var", 2)])).unwrap();
        append_block(&path, 2000, 1, &sample_vars(&[("a_var", 3)])).unwrap();
        let json = dump_json(&path, false, &["b_var", "a_var"], 0, 5000, 0);
        // Titles sorted (C++ iterates std::set); entries joined with ",";
        // values with ", "; missing sample becomes 0.
        assert_eq!(
            json,
            "{\"timestamps\": [1000, 2000],\"variables\": \
             {\"a_var\": [1, 3],\"b_var\": [2, 0]}}"
        );
        assert_eq!(
            dump_json(&temp_path("missing"), false, &["a_var"], 0, 1, 0),
            "{}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn malformed_lines_skipped() {
        let path = temp_path("malformed");
        std::fs::write(
            &path,
            "timestamp: 1000\nstatistics_instance: 1\nno-separator-here\nv: 5\n",
        )
        .unwrap();
        let blocks = read_blocks(&path, 0, 2000, 0);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].vars.get("v").unwrap(), "5");
        assert!(!blocks[0].vars.contains_key("no-separator-here"));
        let _ = std::fs::remove_file(&path);
    }
}

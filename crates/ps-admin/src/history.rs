//! History archive reads (`HistoryArchive` read side).
//!
//! Browses retained `PSH1` segments newest-first with cursor
//! pagination, kind matching, and date filtering. Writing and
//! background snapshotting stay with the server layer.

/// Archive page (`HistoryArchive::Page`).
#[derive(Debug, Default)]
pub struct ArchivePage {
    /// Matching log lines, oldest first, with the leading-blank
    /// prefix the live parser expects.
    pub log: String,
    /// Cursor for older records, if any.
    pub next: String,
    /// Read failure, if any.
    pub error: String,
}

/// Parsed segment/checkpoint header (`HistoryArchive::State`
/// header fields).
#[derive(Debug, Default)]
struct ArchiveHeader {
    sequence: i64,
    generation: String,
    begin: i64,
    end: i64,
    first_time: i64,
}

/// Segment filename (`Name`).
fn segment_name(sequence: i64) -> String {
    format!("segment-{sequence:020}.psh")
}

/// Parses a `PSH1` header line (`State::Parse`, header subset).
fn parse_header(data: &str, segment: bool, file_size: i64) -> Option<ArchiveHeader> {
    let line_end = data.find('\n')?;
    if line_end > 1024 {
        return None;
    }
    let line = &data[..line_end];
    let mut parts = line.split_whitespace();
    let magic = parts.next()?;
    let sequence: i64 = parts.next()?.parse().ok()?;
    let generation = parts.next()?.to_string();
    let begin: i64 = parts.next()?.parse().ok()?;
    let end: i64 = parts.next()?.parse().ok()?;
    let time: i64 = parts.next()?.parse().ok()?;
    let lost: i64 = parts.next()?.parse().ok()?;
    let gaps: i64 = parts.next()?.parse().ok()?;
    let records: i64 = parts.next()?.parse().ok()?;
    let failures: i64 = parts.next()?.parse().ok()?;
    let first_time: i64 = parts.next()?.parse().ok()?;
    let last_time: i64 = parts.next()?.parse().ok()?;
    let mut categories = [0i64; 5];
    for slot in &mut categories {
        *slot = parts.next()?.parse().ok()?;
        if *slot < 0 {
            return None;
        }
    }
    // Trailing tokens are ignored, like the `>>` chain in C++.
    if magic != "PSH1"
        || sequence < 0
        || begin < 0
        || end < begin
        || time < 0
        || generation.is_empty()
        || generation.len() > 20
        || !generation.bytes().all(|byte| byte.is_ascii_digit())
        || lost < 0
        || gaps < 0
        || records < 0
        || failures < 0
        || first_time < 0
        || last_time < 0
        || (segment && (sequence == 0 || generation == "0"))
    {
        return None;
    }
    if segment && (end - begin != file_size - line_end as i64 - 1 || !data.ends_with('\n')) {
        return None;
    }
    Some(ArchiveHeader {
        sequence,
        generation,
        begin,
        end,
        first_time,
    })
}

/// Message timestamp (`MessageTime`): `I[date]` prefix or fallback.
fn message_time(line: &str, fallback: i64) -> i64 {
    if line.len() > 2 && line.as_bytes()[1] == b'[' {
        if let Some(end) = line.find(']') {
            if let Some(time) = ps_http::time::parse_http_date(&line[2..end]) {
                return time;
            }
        }
    }
    fallback
}

/// Whether a message belongs to a history view (`Matches`).
#[must_use]
pub fn matches(kind: &str, message: &str) -> bool {
    match kind {
        "messages" => true,
        "images" => message.contains("PS_IMAGE_DETAIL\t"),
        "html_cache" => message.contains("PS_HTML_DETAIL\t"),
        "javascript" => {
            message.contains("Javascript minification failed.")
                || message.contains("Canonical script ")
                || (message.contains("Script ")
                    && (message.contains(" didn't shrink.")
                        || message.contains(" is unsafe to replace.")))
                || message.contains("javascript_filter.cc:")
                || message.contains("javascript_code_block.cc:")
        }
        "css" => message.contains("CSS ") || message.contains("css_filter.cc:"),
        _ => false,
    }
}

/// Filters a log to a date range (`FilterLog`).
#[must_use]
pub fn filter_log(log: &str, from_ms: i64, until_ms: i64) -> String {
    if from_ms == 0 && until_ms == 253402300800000 {
        return log.to_string();
    }
    let mut filtered = String::from("\n");
    let mut timestamp = -1;
    let mut begin = 0;
    while begin < log.len() {
        let Some(end) = log[begin..].find('\n').map(|pos| begin + pos) else {
            break;
        };
        let line = &log[begin..end];
        timestamp = message_time(line, timestamp);
        if timestamp >= from_ms && timestamp < until_ms {
            filtered.push_str(line);
            filtered.push('\n');
        }
        begin = end + 1;
    }
    filtered
}

/// Maximum bytes read while browsing (bounded like C++).
const MAX_BROWSE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum segments scanned per page.
const MAX_SCANNED_SEGMENTS: usize = 256;
/// Maximum sequence probes per page.
const MAX_PROBED_SEQUENCES: usize = 65536;
/// Maximum selected records per page.
const MAX_SELECTED_RECORDS: usize = 100;
/// Maximum selected bytes per page.
const MAX_SELECTED_BYTES: usize = 256 * 1024;

/// Reads a small file, mapping errors like C++ (`Read`).
fn read_file(path: &std::path::Path, cap: usize) -> Result<String, std::io::ErrorKind> {
    let bytes = std::fs::read(path).map_err(|error| error.kind())?;
    if bytes.len() > cap {
        return Err(std::io::ErrorKind::InvalidData);
    }
    String::from_utf8(bytes).map_err(|_| std::io::ErrorKind::InvalidData)
}

/// Browses retained segments newest-first (`Browse`). The live
/// snapshot overlap check takes no live snapshot here (reads
/// are archive-only); callers merge live records separately.
#[must_use]
pub fn browse(
    cache_path: &str,
    kind: &str,
    from_ms: i64,
    until_ms: i64,
    cursor: &str,
) -> ArchivePage {
    browse_with_live(cache_path, kind, from_ms, until_ms, cursor, None)
}

/// Browse with an optional live-snapshot overlap window
/// (`begin`, `end`, `generation`): archived records already
/// covered live are skipped.
#[must_use]
pub fn browse_with_live(
    cache_path: &str,
    kind: &str,
    from_ms: i64,
    until_ms: i64,
    cursor: &str,
    live: Option<(i64, i64, &str)>,
) -> ArchivePage {
    let mut page = ArchivePage::default();
    let history = std::path::Path::new(cache_path).join("history");
    let checkpoint = match read_file(&history.join("checkpoint"), 4096) {
        Ok(text) => text,
        Err(kind) => {
            page.error = if kind == std::io::ErrorKind::NotFound {
                "No snapshots have been created yet".to_string()
            } else {
                "Archive unavailable".to_string()
            };
            return page;
        }
    };
    let latest = match parse_header(&checkpoint, false, checkpoint.len() as i64) {
        Some(header) => header,
        None => {
            page.error = "Invalid archive checkpoint".to_string();
            return page;
        }
    };
    let (mut before_sequence, mut before_offset) = (0i64, 0i64);
    if !cursor.is_empty() {
        let mut parts = cursor.split(':');
        let (Some(first), Some(second), None) = (parts.next(), parts.next(), parts.next()) else {
            page.error = "Invalid page cursor".to_string();
            return page;
        };
        before_sequence = first.parse().unwrap_or(0);
        before_offset = second.parse().unwrap_or(-1);
        if before_sequence <= 0 || before_offset < 0 {
            page.error = "Invalid page cursor".to_string();
            return page;
        }
    }
    let mut newest_sequence = latest.sequence;
    if before_sequence != 0 {
        newest_sequence = newest_sequence.min(before_sequence);
    }
    // Prefix preserves the existing parser's first-potentially-partial-line rule.
    page.log = "\n".to_string();
    let mut selected: Vec<String> = Vec::new();
    let mut selected_bytes = 0usize;
    let mut scanned = 0;
    let mut probed = 0;
    let mut bytes_read = 0usize;
    let mut sequence = newest_sequence;
    while sequence > 0 && scanned < MAX_SCANNED_SEGMENTS && probed < MAX_PROBED_SEQUENCES {
        probed += 1;
        let name = segment_name(sequence);
        let data = match read_file(&history.join(&name), MAX_BROWSE_BYTES + 4096) {
            Ok(data) => data,
            Err(kind) => {
                if kind == std::io::ErrorKind::NotFound {
                    // Retention or a sequence gap.
                    sequence -= 1;
                    continue;
                }
                page.error = "Archive read failed".to_string();
                return page;
            }
        };
        bytes_read += data.len();
        let state = match parse_header(&data, true, data.len() as i64) {
            Some(state) if segment_name(state.sequence) == name => state,
            _ => {
                page.error = "Invalid archive segment".to_string();
                return page;
            }
        };
        let header_len = data.find('\n').map(|pos| pos + 1).unwrap_or(data.len());
        let mut end = data.len();
        let mut time_boundary = data.len();
        let mut record_time = state.first_time;
        if before_sequence == state.sequence {
            if before_offset < state.begin || before_offset > state.end {
                page.error = "Invalid page cursor".to_string();
                return page;
            }
            end = header_len + (before_offset - state.begin) as usize;
            if end > header_len && !data[..end].ends_with('\n') {
                page.error = "Invalid page cursor".to_string();
                return page;
            }
        }
        while end > header_len {
            let previous = if end > header_len + 1 {
                data[..end - 1].rfind('\n')
            } else {
                None
            };
            let begin = match previous {
                Some(pos) if pos >= header_len => pos + 1,
                _ => header_len,
            };
            let line = &data[begin..end - 1];
            // Reverse pagination inherits the original timestamp of multiline
            // continuations, never the time of the archival snapshot.
            if begin < time_boundary {
                record_time = state.first_time;
                time_boundary = header_len;
                let mut search = begin;
                loop {
                    let stop = data[search..]
                        .find('\n')
                        .map(|pos| search + pos)
                        .unwrap_or(data.len());
                    let found = message_time(&data[search..stop], -1);
                    if found >= 0 {
                        record_time = found;
                        time_boundary = search;
                        break;
                    }
                    if search == header_len {
                        break;
                    }
                    let back = if search > header_len + 1 {
                        data[..search - 1].rfind('\n')
                    } else {
                        None
                    };
                    search = match back {
                        Some(pos) if pos >= header_len => pos + 1,
                        _ => header_len,
                    };
                }
            }
            let covered_live = match live {
                Some((live_begin, live_end, live_generation)) => {
                    state.generation == live_generation
                        && state.begin + begin as i64 - header_len as i64 >= live_begin
                        && state.begin + end as i64 - header_len as i64 <= live_end
                }
                None => false,
            };
            if (record_time > 0 || (from_ms == 0 && until_ms == 253402300800000))
                && record_time >= from_ms
                && record_time < until_ms
                && matches(kind, line)
                && !covered_live
            {
                if selected.len() >= MAX_SELECTED_RECORDS
                    || (!selected.is_empty() && selected_bytes + line.len() > MAX_SELECTED_BYTES)
                {
                    page.next = format!(
                        "{}:{}",
                        state.sequence,
                        state.begin + end as i64 - header_len as i64
                    );
                    return finish_page(page, selected);
                }
                selected_bytes += line.len();
                selected.push(format!("{line}\n"));
            }
            end = begin;
        }
        scanned += 1;
        if scanned >= MAX_SCANNED_SEGMENTS {
            page.next = format!("{}:{}", state.sequence, state.begin);
            break;
        }
        if bytes_read >= MAX_BROWSE_BYTES && sequence > 1 {
            page.next = format!("{}:{}", state.sequence, state.begin);
            break;
        }
        sequence -= 1;
    }
    if page.next.is_empty() && sequence > 0 && probed >= MAX_PROBED_SEQUENCES {
        page.error = "Archive sequence gap exceeds the bounded browse limit".to_string();
    }
    finish_page(page, selected)
}

/// Reverses selected records into the page log.
fn finish_page(mut page: ArchivePage, selected: Vec<String>) -> ArchivePage {
    for line in selected.iter().rev() {
        page.log.push_str(line);
    }
    page
}

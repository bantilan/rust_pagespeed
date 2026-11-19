//! Statistics-log queries for graphs/console JSON (`GraphQuery`).

use std::collections::BTreeSet;

use ps_http::query_params::QueryParams;

/// Minimum graph granularity, ms (`kMinGraphGranularityMs`).
pub const MIN_GRAPH_GRANULARITY_MS: i64 = 1000;
/// Maximum graph granularity, ms (`kMaxGraphGranularityMs`, one day).
pub const MAX_GRAPH_GRANULARITY_MS: i64 = 86_400_000;
/// Maximum named variables (`kMaxGraphVarTitles`).
pub const MAX_GRAPH_VAR_TITLES: usize = 64;

/// Bounded log query (`GraphQuery`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphQuery {
    /// Start time, ms.
    pub start_time: i64,
    /// End time, ms.
    pub end_time: i64,
    /// Bucket width, ms.
    pub granularity_ms: i64,
    /// True: graph variables; false: `var_titles`.
    pub dump_for_graphs: bool,
    /// Named variables, bounded.
    pub var_titles: BTreeSet<String>,
}

/// Parses and bounds a log query (`ParseGraphQuery`).
#[must_use]
pub fn parse_graph_query(params: &QueryParams, now_ms: i64) -> GraphQuery {
    let mut query = GraphQuery {
        start_time: 0,
        end_time: now_ms,
        granularity_ms: 3000,
        dump_for_graphs: true,
        var_titles: BTreeSet::new(),
    };
    for index in 0..params.len() {
        let Some(value) = params.unescaped_value(index) else {
            continue;
        };
        match params.name(index) {
            // Not a number reads as 0.
            "start_time" => {
                query.start_time = value.parse::<i64>().unwrap_or(0);
            }
            "end_time" => {
                query.end_time = value.parse::<i64>().unwrap_or(0);
            }
            "var_titles" => {
                // Fetch specified data if users populate var_titles.
                query.dump_for_graphs = false;
                for name in value.split(',') {
                    if query.var_titles.len() >= MAX_GRAPH_VAR_TITLES {
                        break;
                    }
                    if !name.is_empty() {
                        query.var_titles.insert(name.to_string());
                    }
                }
            }
            "granularity" => {
                query.granularity_ms = value.parse::<i64>().unwrap_or(0);
            }
            _ => {}
        }
    }
    query.granularity_ms = query
        .granularity_ms
        .clamp(MIN_GRAPH_GRANULARITY_MS, MAX_GRAPH_GRANULARITY_MS);
    let latest_ms = now_ms + 86_400_000;
    query.start_time = query.start_time.clamp(0, latest_ms);
    query.end_time = query.end_time.clamp(0, latest_ms);
    query
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only projection of agh's local REST-attempt records for fleet surveys.
//! Quota counter deltas belong to the shared identity, never to one machine.

use aterm_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Per-hour bytes; agh retains 24 hours (at most 192 MiB plus small markers).
pub const HOUR_MAX: usize = 8 * 1024 * 1024;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Caller {
    identity: String,
    caller: String,
    endpoint: String,
    chain: String,
    resource: String,
}

#[derive(Default)]
struct Counts {
    attempts: u64,
    counted: u64,
    free: u64,
    unknown: u64,
}

/// Endpoint classes contain vocabulary only; repository names, refs, filenames,
/// queries and opaque IDs never enter this projection.
#[must_use]
pub fn endpoint_class(path: &str) -> String {
    let parts: Vec<_> = path
        .split('?')
        .next()
        .unwrap_or("")
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    let mut out = String::new();
    for (i, part) in parts.iter().take(8).enumerate() {
        out.push('/');
        out.push_str(
            if (parts.first() == Some(&"repos") && matches!(i, 1 | 2))
                || !matches!(
                    *part,
                    "repos"
                        | "users"
                        | "user"
                        | "orgs"
                        | "releases"
                        | "latest"
                        | "tags"
                        | "assets"
                        | "git"
                        | "ref"
                        | "refs"
                        | "matching-refs"
                        | "trees"
                        | "blobs"
                        | "commits"
                        | "contents"
                        | "branches"
                        | "compare"
                        | "rate_limit"
                        | "search"
                        | "code"
                        | "issues"
                        | "pulls"
                        | "actions"
                        | "runs"
                        | "jobs"
                        | "keys"
                        | "installation"
                        | "repositories"
                        | "graphql"
                )
            {
                ":arg"
            } else {
                part
            },
        );
    }
    out
}

/// Aggregate attempts without exposing raw paths or process arguments. Counted is
/// a conservative estimate: authenticated 304 and /rate_limit are quota-free;
/// transport failures are unknown, not silently charged or dropped.
#[must_use]
pub fn summarize(rows: &[Value], hour: u64) -> Vec<Value> {
    let mut groups: BTreeMap<Caller, Counts> = BTreeMap::new();
    for row in rows {
        let text = |key: &str| row[key].as_str().unwrap_or("unknown").to_owned();
        let endpoint = row["endpoint"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| endpoint_class(row["path"].as_str().unwrap_or("")));
        let chain = row["chain"]
            .as_array()
            .map(|chain| {
                chain
                    .iter()
                    .filter_map(|p| p["role"].as_str())
                    .collect::<Vec<_>>()
                    .join(" <- ")
            })
            .unwrap_or_else(|| "unrecorded".into());
        let resource = row["rate"]["resource"]
            .as_str()
            .unwrap_or_else(|| {
                if endpoint == "/search/code" {
                    "code_search"
                } else if endpoint.starts_with("/search/") {
                    "search"
                } else if endpoint == "/graphql" {
                    "graphql"
                } else {
                    "core"
                }
            })
            .to_owned();
        let group = groups
            .entry(Caller {
                identity: text("as"),
                caller: text("caller"),
                endpoint: endpoint.clone(),
                chain,
                resource,
            })
            .or_default();
        group.attempts += 1;
        match row["status"].as_u64() {
            Some(304) if row["as"].as_str() != Some("anonymous") => group.free += 1,
            Some(_) if endpoint == "/rate_limit" => group.free += 1,
            Some(_) => group.counted += 1,
            None => group.unknown += 1,
        }
    }
    let mut rows: Vec<_> = groups
        .into_iter()
        .map(
            |(Caller { identity, caller, endpoint, chain, resource }, Counts { attempts, counted, free, unknown })| {
                json!({
                    "hour":hour, "as":identity, "caller":caller, "endpoint":endpoint, "chain":chain, "resource":resource,
                    "attempts":attempts, "counted":counted, "free":free, "unknown":unknown,
                })
            },
        )
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row["counted"].as_u64().unwrap_or(0)));
    rows
}

/// Bounded, read-only wire rows for the last completed and current UTC hour.
/// Missing, old and incomplete records stay explicit; they never mean zero use.
#[must_use]
pub fn survey(state: &Path, now: u64) -> Vec<String> {
    #[cfg(unix)]
    {
        use aterm_uds::private_file::read_private;
        let dir = state.join("agh/rate/calls");
        let hour = now / 3600;
        [hour.saturating_sub(1), hour]
            .into_iter()
            .flat_map(|hour| {
                let read = || -> Option<Vec<Value>> {
                    let bytes = read_private(&dir, &format!("{hour}.jsonl"), HOUR_MAX).ok()?;
                    if bytes.last() != Some(&b'\n') {
                        return None;
                    }
                    bytes
                        .split(|b| *b == b'\n')
                        .filter(|line| !line.is_empty())
                        .map(aterm_json::from_slice)
                        .collect::<Result<_, _>>()
                        .ok()
                };
                let Some(rows) = read() else {
                    return vec![format!("github_rate hour={hour} status=unrecorded")];
                };
                let incomplete = dir
                    .join(format!("{hour}.incomplete"))
                    .try_exists()
                    .unwrap_or(true);
                let summary = summarize(&rows, hour);
                let mut lines = vec![format!(
                    "github_rate hour={hour} status={} attempts={} groups={} cut={}",
                    if incomplete { "incomplete" } else { "recorded" },
                    rows.len(),
                    summary.len(),
                    summary.len().saturating_sub(128)
                )];
                lines.extend(summary.iter().take(128).map(|row| {
                    format!(
                        "github_rate hour={hour} data={}",
                        aterm_codec::pct::encode(&row.to_string())
                    )
                }));
                lines
            })
            .collect()
    }
    #[cfg(not(unix))]
    {
        let _ = (state, now);
        vec!["github_rate status=unavailable".into()]
    }
}

#[cfg(test)]
#[path = "github_usage_tests.rs"]
mod tests;

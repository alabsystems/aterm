// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Receipt tracking for completed network update checks and in-flight channel head builds.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::{Source, paths::Staging};

pub(crate) fn path(staging: &Staging) -> PathBuf {
    staging.root.join("check.toml")
}

pub(crate) fn source_key(source: &Source) -> String {
    format!("{}/{}", source.owner, source.repo)
}

#[derive(Serialize)]
struct Receipt {
    schema: u32,
    updated_at: String,
    current_build: u64,
    source: String,
    outcome: &'static str,
    /// The channel head this check found with its app build in flight, and how many
    /// consecutive completed checks of this source found it so. Not keyed on the build:
    /// after an update the old build's sessions and the new build's window both check,
    /// and a per-build count would keep both on the first rung.
    #[serde(skip_serializing_if = "Option::is_none")]
    head_in_flight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head_in_flight_checks: Option<u32>,
}

/// Stamp a completed check. Returns how many consecutive completed checks, this one
/// included, found `head_in_flight` in flight (`0` when `None` or when the write failed:
/// a count nothing records is no count).
pub(crate) fn record(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    deferred: bool,
    head_in_flight: Option<&str>,
) -> u32 {
    record_at(
        staging,
        current_build,
        source,
        deferred,
        head_in_flight,
        crate::unix_now_secs(),
    )
}

fn record_at(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    deferred: bool,
    head_in_flight: Option<&str>,
    now: u64,
) -> u32 {
    let checks = head_in_flight.map(|head| {
        crate::read_ledger_text(&path(staging))
            .and_then(|text| text.parse::<aterm_toml::Value>().ok())
            .filter(|prior| {
                same_source(prior, source)
                    && prior
                        .get("head_in_flight")
                        .and_then(aterm_toml::Value::as_str)
                        == Some(head)
            })
            .map_or(0, |prior| head_in_flight_checks(&prior))
            .saturating_add(1)
    });
    let record = Receipt {
        schema: 1,
        updated_at: aterm_types::rfc3339::format_rfc3339(now),
        current_build,
        source: source_key(source),
        outcome: if deferred { "deferred" } else { "completed" },
        head_in_flight: head_in_flight.map(str::to_owned),
        head_in_flight_checks: checks,
    };
    let Ok(text) = aterm_toml::to_string(&record) else {
        return 0;
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = staging.root.join(format!(
        "check.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    if crate::install::write_private_file(&tmp, text.as_bytes()).is_ok()
        && std::fs::rename(&tmp, path(staging)).is_ok()
    {
        return checks.unwrap_or(0);
    }
    let _ = std::fs::remove_file(tmp);
    0
}

/// The `head_in_flight_checks` a parsed receipt records, `0` when it records none.
pub(crate) fn head_in_flight_checks(receipt: &aterm_toml::Value) -> u32 {
    receipt
        .get("head_in_flight_checks")
        .and_then(aterm_toml::Value::as_integer)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

pub(crate) fn matches(text: &aterm_toml::Value, current_build: u64, source: &Source) -> bool {
    same_source(text, source)
        && text
            .get("current_build")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|build| u64::try_from(build).ok())
            == Some(current_build)
}

fn same_source(text: &aterm_toml::Value, source: &Source) -> bool {
    text.get("schema").and_then(aterm_toml::Value::as_integer) == Some(1)
        && text
            .get("source")
            .and_then(aterm_toml::Value::as_str)
            .is_some_and(|recorded| recorded.eq_ignore_ascii_case(&source_key(source)))
}

/// For display only; authorization uses `matches` and the receipt.
pub(crate) fn completed_at(staging: &Staging) -> Option<String> {
    let text = crate::read_ledger_text(&path(staging))?;
    let value: aterm_toml::Value = text.parse().ok()?;
    value
        .get("updated_at")
        .and_then(aterm_toml::Value::as_str)
        .filter(|stamp| !stamp.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "check_receipt_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Request ids: `r-<utc yyyymmdd>-<hhmm>-<head>-<n>` ([`aterm_types::control_verbs::request_id`]),
//! sortable by time and naming the requester's Mac without a lookup. `<n>` counts the requests of
//! that minute and head this store holds; the store picks it under the journal's lock, so no
//! counter file is kept.

use aterm_types::control_verbs::REQUEST_ID_HEAD_MAX;

/// The head an id names for a requester on node `node`: its first
/// [`REQUEST_ID_HEAD_MAX`] ASCII letters and digits, lowercased, or `local` where it has none (a
/// Mac with no node id, which is in no fleet).
#[must_use]
pub fn head_of(node: &str) -> String {
    let head: String = node
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(REQUEST_ID_HEAD_MAX)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if head.is_empty() {
        "local".to_owned()
    } else {
        head
    }
}

/// The ids of the UTC minute holding `at_ms` (Unix milliseconds) for `head` begin with this:
/// `r-<yyyymmdd>-<hhmm>-<head>-`.
#[must_use]
pub fn prefix(at_ms: u64, head: &str) -> String {
    let secs = at_ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let (y, m, d) = aterm_stamp::civil::civil_from_days(days);
    let in_day = secs % 86_400;
    format!(
        "r-{y:04}{m:02}{d:02}-{:02}{:02}-{head}-",
        in_day / 3600,
        in_day % 3600 / 60
    )
}

/// The `n`th id of `prefix` ([`prefix`]): two digits at least.
#[must_use]
pub fn nth(prefix: &str, n: u32) -> String {
    format!("{prefix}{n:02}")
}

/// The count `id` holds where it is one of `prefix`'s.
#[must_use]
pub fn count_of(id: &str, prefix: &str) -> Option<u32> {
    let n = id.strip_prefix(prefix)?;
    n.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| n.parse().ok())
        .flatten()
}

#[cfg(test)]
#[path = "id_tests.rs"]
mod tests;

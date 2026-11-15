// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GitHub's allowance, as a release reads its end: agh refuses a request the identity has no
//! allowance for and names when it returns (`agh: rate_limited exit=11 … reset=<epoch>`, the
//! final line of its command and what this tool's own client says in the same words,
//! [`crate::github_remote::words`]). A step that ended only on that waits for the reset and
//! runs again, once ([`crate::release`]); every other refusal ends the run as it did.

/// What agh's final line of a rate-limited refusal starts with, its exit code included.
const REFUSAL: &str = "agh: rate_limited exit=11";

/// The longest a release waits for an allowance: GitHub's window is an hour, so a reset
/// further ahead is no answer of its.
pub const LONGEST_WAIT_SECS: u64 = 3600;

/// What a run waits past the reset: the reset is GitHub's clock's, and a request sent in the
/// second it names can still be counted in the spent window.
pub const PAST_RESET_SECS: u64 = 15;

/// How often a waiting holder renews its line's lease: the heartbeat's own pace, a sixth of
/// the shortest lease, since a tree that only waits shows the heartbeat no work.
pub const RENEW_SECS: u64 = 300;

/// agh's rate-limited refusal naming `reset`, as much of its final line as a reader needs.
#[must_use]
pub fn line(reset: u64) -> String {
    format!("{REFUSAL} reset={reset}")
}

/// The reset one line names, where it holds agh's rate-limited refusal.
fn reset_of(line: &str) -> Option<u64> {
    let refusal = &line[line.find(REFUSAL)? + REFUSAL.len()..];
    refusal
        .split([' ', ')'])
        .find_map(|word| word.strip_prefix("reset="))
        .and_then(|reset| reset.parse().ok())
}

/// The latest reset agh's rate-limited refusals among `lines` name; `None` where none of them
/// is one, or none names a reset.
#[must_use]
pub fn reset_in<'l>(lines: impl IntoIterator<Item = &'l str>) -> Option<u64> {
    lines.into_iter().filter_map(reset_of).max()
}

/// When a run refused until `reset` runs again, read at `now`: [`PAST_RESET_SECS`] past the
/// reset. `None` where the reset is more than [`LONGEST_WAIT_SECS`] ahead (no window of
/// GitHub's is that long), or more than a minute gone (the refusal read is an old one's).
#[must_use]
pub fn again_at(reset: u64, now: u64) -> Option<u64> {
    (reset <= now.saturating_add(LONGEST_WAIT_SECS) && now <= reset.saturating_add(60))
        .then(|| reset.saturating_add(PAST_RESET_SECS))
}

#[cfg(test)]
#[path = "allowance_tests.rs"]
mod tests;

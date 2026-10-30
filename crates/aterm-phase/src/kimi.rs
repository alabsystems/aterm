// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kimi Code (`kimi`, Moonshot AI) CLI harness version and screen grammar.
//!
//! Measured on Kimi Code 2.3.1 signed out (`fixtures/kimi-2.3.1-*.txt`): the welcome box, the
//! composer (a rounded box whose first row is `│ > ` and the draft), the `context: N%` footer
//! under it, and the indented `Error: <why>` row a failed turn ends on. The quota and balance
//! texts are the patterns Kimi Code itself classifies a 429 by
//! (`KIMI_QUOTA_EXHAUSTED_MESSAGE_PATTERNS`); its concurrency throttle is a retryable rate limit,
//! not a wall. A busy turn is unmeasured, so no phase is authoritative.

use crate::wall::{Placement, Wall, WallKind};

/// The Kimi Code (`kimi`) harness version, `MAJOR.MINOR` with no third part.
///
/// Its minor goes up once per public release window in which its sources changed
/// ([`crate::CliHarness::sources`]); `targo --unverified ship harness-bump` writes the bump.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(0, 6, "0.6");

/// The usage limit is spent: Kimi Code's own quota patterns (lowercase).
const QUOTA: [&str; 3] = [
    "exceeded your current quota",
    "exceeded your current token quota",
    "exceeded_current_quota_error",
];

/// The account's balance is spent: Kimi Code's own balance patterns (lowercase).
const BALANCE: [&str; 5] = [
    "check your account balance",
    "insufficient balance",
    "recharge your account",
    "please recharge",
    "in arrears",
];

/// The login is gone (lowercase): no model is signed in, or the stored one is refused.
const AUTH: [&str; 4] = [
    "llm not set",
    "re-login required",
    "no token for \"kimi-code\"",
    "send \"/login\" to login",
];

/// A passing throttle (lowercase): a request over the account's concurrency, which Kimi Code
/// retries and which ends no quota.
const THROTTLE: [&str; 1] = ["concurrent request limit"];

/// The composer's rows `(top, caret, bottom)`: the last rounded box whose first inner row opens
/// with `│ >`, with Kimi Code's `context: N%` footer under it.
#[must_use]
pub fn composer_rows(rows: &[String]) -> Option<(usize, usize, usize)> {
    let caret = rows.iter().rposition(|r| caret_text(r).is_some())?;
    let top = caret.checked_sub(1)?;
    if !crate::text::trim_start(&rows[top]).starts_with('╭') {
        return None;
    }
    let bottom = caret
        + 1
        + rows[caret + 1..]
            .iter()
            .position(|r| crate::text::trim_start(r).starts_with('╰'))?;
    rows[bottom + 1..]
        .iter()
        .any(|r| context_row(r).is_some())
        .then_some((top, caret, bottom))
}

/// The draft text of a composer row: past `│ > ` (the caret row) and before its closing `│`.
fn caret_text(row: &str) -> Option<&str> {
    let inner = crate::text::trim_start(row).strip_prefix('│')?;
    let draft = inner.strip_prefix(" >")?;
    if !(draft.is_empty() || draft.starts_with(' ')) {
        return None;
    }
    let draft = crate::text::trim_end(draft);
    Some(crate::text::trim(draft.strip_suffix('│').unwrap_or(draft)))
}

/// The share of context a footer row says is used (`context: 0%`, the row's only words).
#[must_use]
pub fn context_row(row: &str) -> Option<u8> {
    crate::text::trim(row)
        .strip_prefix(crate::anchor!("kimi.footer.context"))?
        .strip_suffix('%')?
        .parse()
        .ok()
}

/// The composer as `(caret row, lines)`, the caret row's draft first ([`composer_rows`]).
#[must_use]
pub fn composer(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let (_, caret, bottom) = composer_rows(rows)?;
    let mut lines = vec![caret_text(&rows[caret]).unwrap_or_default().to_string()];
    for r in &rows[caret + 1..bottom] {
        let inner = crate::text::trim_start(r).strip_prefix('│').unwrap_or(r);
        let inner = crate::text::trim_end(inner);
        lines.push(crate::text::trim(inner.strip_suffix('│').unwrap_or(inner)).to_string());
    }
    Some((caret, lines))
}

/// Whether `rows` are Kimi Code's own screen: its welcome row, or its composer over its
/// `context: N%` footer ([`composer_rows`]).
#[must_use]
pub fn is_kimi_screen(rows: &[String]) -> bool {
    composer_rows(rows).is_some()
        || rows.iter().any(|r| {
            crate::text::trim_start(r)
                .strip_prefix('│')
                .is_some_and(|inner| crate::text::contains(inner, crate::anchor!("kimi.welcome")))
        })
}

/// The wall the last turn ended on: the `Error: …` row standing last above the composer, its
/// indented continuation rows joined, read as Kimi Code reads it — a spent quota
/// ([`WallKind::UsageSession`]: it names no window), a spent balance ([`WallKind::Spend`]), or a
/// login gone ([`WallKind::Auth`]). A concurrency throttle is none, and so is any other error.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    let (top, _, _) = composer_rows(rows)?;
    let last = rows[..top].iter().rposition(|r| !crate::text::blank(r))?;
    let opener = crate::anchor!("kimi.error");
    let at = (0..=last)
        .rev()
        .take_while(|&i| !crate::text::blank(&rows[i]))
        .find(|&i| crate::text::trim_start(&rows[i]).starts_with(opener))?;
    let indent = crate::phase::leading_spaces(&rows[at]);
    if (at + 1..=last).any(|i| crate::phase::leading_spaces(&rows[i]) < indent) {
        return None;
    }
    let message = rows[at..=last]
        .iter()
        .map(|r| crate::text::trim(r))
        .collect::<Vec<_>>()
        .join(" ");
    let said = message.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| said.contains(w));
    let kind = if has(&THROTTLE) {
        return None;
    } else if has(&QUOTA) {
        WallKind::UsageSession
    } else if has(&BALANCE) {
        WallKind::Spend
    } else if has(&AUTH) {
        WallKind::Auth
    } else {
        return None;
    };
    Some(Wall {
        kind,
        message,
        reset: None,
        row: at,
        placement: Placement::ErrorRow,
        under_call: false,
    })
}

#[cfg(test)]
#[path = "kimi_tests.rs"]
mod tests;

/// Kimi Code's screens, measured or hand-built (each file's head says which), for tests here and
/// in dependent crates; not part of the documented API.
#[doc(hidden)]
pub mod fixtures {
    /// Kimi Code 2.3.1 signed out at its idle composer, after the folder was trusted (MEASURED).
    pub const IDLE: &str = include_str!("fixtures/kimi-2.3.1-idle.txt");
    /// The same with `hello` typed and unsent (MEASURED).
    pub const DRAFT: &str = include_str!("fixtures/kimi-2.3.1-draft.txt");
    /// The turn a signed-out session ends on: `Error: LLM not set, …` (MEASURED).
    pub const SIGNED_OUT_TURN: &str = include_str!("fixtures/kimi-2.3.1-signed-out-turn.txt");
    /// A turn ended on the quota-exhausted 429 (HAND-BUILT on the measured error row's shape).
    pub const WALL_QUOTA: &str = include_str!("fixtures/kimi-2.3.1-hand-built-wall-quota.txt");
    /// A turn ended on the concurrency throttle's 429 (HAND-BUILT on the same shape): no wall.
    pub const CONCURRENT_LIMIT: &str =
        include_str!("fixtures/kimi-2.3.1-hand-built-concurrent-limit.txt");
}

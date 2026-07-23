// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-observe** — layer L0.5 of the RFC "The Reactive Surface": the
//! *semantic* surface predicates, built on the core
//! [Observation Kernel](aterm_core::terminal::WatcherSet).
//!
//! The point of this crate is a dependency boundary. The kernel in `aterm-core`
//! exposes a primitive that latches when a *content_seq* advances, a quiescence
//! deadline elapses, or an opaque [`RowMatch`](aterm_core::terminal::RowMatch)
//! fires — but it carries **no vocabulary**: it cannot turn a pattern string into
//! a matcher. That vocabulary (`regex`) lives **here**, so `aterm-core` takes no
//! **direct** `regex` dependency (RFC requirement R2; enforced by
//! [`tests::regex_is_not_in_aterm_core_production_deps`], which checks the core's
//! direct production deps — `regex` does still appear in the workspace's
//! transitive closure via `aterm-search`'s `regex` feature). The agent layer
//! (`aterm-agent`, L2) composes these predicates into turn-completion; it never
//! reaches into the core enum directly.

use std::sync::Arc;
use std::time::Duration;

use aterm_core::terminal::{RowMatch, RowRange, WatcherSpec};

/// Re-export the regex compile error so dependents (e.g. `aterm-agent`) can name
/// it without taking a direct `regex` dependency — the regex boundary stays at
/// this crate.
pub mod regex_compile_error {
    pub use regex::Error;
}

/// A pre-compiled regular-expression row matcher — the one place `regex` is used
/// in the watcher stack. The core stores it behind `dyn RowMatch` and can only
/// *evaluate* it, never construct it.
#[derive(Debug)]
pub struct RegexRowMatch {
    re: regex::Regex,
}

impl RowMatch for RegexRowMatch {
    #[inline]
    fn matches(&self, row: &str) -> bool {
        self.re.is_match(row)
    }
}

/// Maximum regex pattern length (bytes). A watcher pattern is untrusted input
/// (an `await`/agent control argument), so it is rejected before compilation to
/// bound parse/compile cost. Mirrors the search verb's `MAX_REGEX_PATTERN_LEN`
/// (`aterm-search`) so both regex entry points enforce the same ceiling.
const MAX_REGEX_PATTERN_LEN: usize = 1024;

/// Compiled-regex size limit (bytes) for [`regex::RegexBuilder::size_limit`].
/// Caps NFA memory/compile cost to 1 MiB — well below the crate's 10 MiB
/// default — so a pathological pattern (deep alternation, large bounded
/// repetition) cannot blow up compilation. Matches `aterm-search`.
const REGEX_SIZE_LIMIT: usize = 1 << 20; // 1 MiB

/// Lazy-DFA cache limit (bytes) for [`regex::RegexBuilder::dfa_size_limit`].
/// The DFA is built lazily while matching, so this bounds per-match memory even
/// for patterns that pass the NFA size gate. Matches `aterm-search`.
const REGEX_DFA_SIZE_LIMIT: usize = 1 << 20; // 1 MiB

/// Compile a regex row matcher. The returned `Arc<dyn RowMatch>` is what
/// [`Terminal::watch_rows`](aterm_core::terminal::Terminal::watch_rows) takes —
/// the core receives only the opaque handle.
///
/// The pattern is untrusted, so this bounds it at compile time: patterns longer
/// than [`MAX_REGEX_PATTERN_LEN`] are rejected up front, and the compiler is
/// held to [`REGEX_SIZE_LIMIT`]/[`REGEX_DFA_SIZE_LIMIT`] (vs. the crate's 10 MiB
/// default) so a crafted pattern cannot amplify a small string into a
/// multi-megabyte NFA/DFA. This mirrors the search verb (`aterm-search`); the
/// live PTY-driven watcher path shares the same choke point.
///
/// # Errors
/// Returns a [`regex::Error`] if `pattern` exceeds the length ceiling, exceeds
/// the compiled-size limits, or is not a valid regular expression.
pub fn row_matcher(pattern: &str) -> Result<Arc<dyn RowMatch>, regex::Error> {
    if pattern.len() > MAX_REGEX_PATTERN_LEN {
        return Err(regex::Error::Syntax(format!(
            "pattern exceeds maximum length ({MAX_REGEX_PATTERN_LEN} bytes)"
        )));
    }
    let re = regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build()?;
    Ok(Arc::new(RegexRowMatch { re }))
}

/// `IdleFor(dur)` — latch after `dur` of no content mutation (quiescence).
#[must_use]
pub fn idle_for(dur: Duration) -> WatcherSpec {
    WatcherSpec::IdleFor { dur }
}

/// `SeqAdvanced(after)` — latch once the content clock passes `after`.
#[must_use]
pub fn seq_advanced(after: u64) -> WatcherSpec {
    WatcherSpec::SeqAdvanced { after }
}

/// `BlockComplete` — latch on a completed/prompt-ready shell-integration block.
#[must_use]
pub fn block_complete() -> WatcherSpec {
    WatcherSpec::BlockComplete
}

/// The whole visible surface (every row) — the common [`RowRange`] for row
/// matching.
#[must_use]
pub fn anywhere() -> RowRange {
    RowRange::All
}

/// The inclusive visible-row span `start..=end`.
#[must_use]
pub fn rows(start: usize, end: usize) -> RowRange {
    RowRange::Span { start, end }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::{ClockReading, Terminal};
    use std::time::Instant;

    fn clock_at(base: Instant, off_ms: u64) -> ClockReading {
        ClockReading {
            monotonic: base + Duration::from_millis(off_ms),
            wall_ms: Some(off_ms),
        }
    }

    #[test]
    fn row_matcher_latches_on_real_engine_output() {
        // Bind aterm-observe -> aterm-core -> real engine: arm a regex row
        // matcher, paint a matching row through the real pipeline, assert latch.
        let base = Instant::now();
        let mut t = Terminal::new(24, 80);
        let m = row_matcher(r"PROMPT-READY").expect("compile");
        let id = t.watch_rows(m, anywhere(), base).expect("arm");
        assert!(t.watch_poll(id).is_none(), "pending before the row appears");

        t.process_at(b"working...\r\n", clock_at(base, 10));
        assert!(
            t.watch_poll(id).is_none(),
            "non-matching output does not latch"
        );

        t.process_at(b"PROMPT-READY\r\n", clock_at(base, 20));
        let sat = t
            .watch_poll(id)
            .expect("matching row latched on the real surface");
        assert!(sat.seq > 0);
    }

    #[test]
    fn row_matcher_latches_immediately_if_already_matching() {
        // Arm against a surface that ALREADY shows the row — must latch at arm
        // (the `watch_rows` immediate eval), not wait for the next change.
        let base = Instant::now();
        let mut t = Terminal::new(24, 80);
        t.process_at(b"ALREADY-HERE\r\n", clock_at(base, 5));
        let m = row_matcher("ALREADY-HERE").expect("compile");
        let id = t.watch_rows(m, anywhere(), base).expect("arm");
        assert!(
            t.watch_poll(id).is_some(),
            "an already-matching row latches at arm time"
        );
    }

    #[test]
    fn bad_pattern_is_a_clean_error_not_a_panic() {
        assert!(row_matcher(r"(unclosed").is_err());
    }

    #[test]
    fn row_matcher_rejects_overlong_pattern() {
        // A pattern at the ceiling still compiles (it is a valid regex — 1024
        // literal 'a's); one byte over is rejected before compilation so an
        // untrusted `await`/agent pattern cannot force an arbitrarily large
        // parse/compile. Mirrors the search verb's MAX_REGEX_PATTERN_LEN gate.
        let at_cap = "a".repeat(MAX_REGEX_PATTERN_LEN);
        assert_eq!(at_cap.len(), MAX_REGEX_PATTERN_LEN);
        assert!(
            row_matcher(&at_cap).is_ok(),
            "a pattern at the cap compiles"
        );

        let over_cap = "a".repeat(MAX_REGEX_PATTERN_LEN + 1);
        assert!(
            row_matcher(&over_cap).is_err(),
            "a pattern one byte over the cap is rejected"
        );
    }

    #[test]
    fn row_matcher_bounds_compiled_size() {
        // `(a{200}){200}` is a short (13-byte) pattern that compiles happily
        // under the regex crate's 10 MiB default but blows past our 1 MiB
        // REGEX_SIZE_LIMIT — the classic "small pattern, huge NFA" amplifier.
        // row_matcher must reject it, proving the size caps are wired (not just
        // the length gate). If this ever compiles, the size_limit call is gone.
        let amplifier = "(a{200}){200}";
        assert!(
            amplifier.len() <= MAX_REGEX_PATTERN_LEN,
            "under the length gate"
        );
        assert!(
            regex::Regex::new(amplifier).is_ok(),
            "sanity: the crate's default (10 MiB) accepts this pattern",
        );
        assert!(
            row_matcher(amplifier).is_err(),
            "the bounded builder must reject a pattern that exceeds 1 MiB compiled",
        );
    }

    /// RFC R2 purity, made a checkable invariant: `regex` must NOT appear in
    /// `aterm-core`'s **production** `[dependencies]` — only its
    /// `[dev-dependencies]`. The kernel stays vocabulary-free; the regex lives
    /// here. If someone adds `regex` to the core's production deps, this fails.
    #[test]
    fn regex_is_not_in_aterm_core_production_deps() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../aterm-core/Cargo.toml");
        let toml = std::fs::read_to_string(manifest).expect("read aterm-core Cargo.toml");
        // Slice the production [dependencies] section (up to the next table header).
        let deps = toml
            .split_once("\n[dependencies]\n")
            .map(|(_, rest)| rest.split("\n[").next().unwrap_or(rest))
            .unwrap_or("");
        assert!(
            !deps.lines().any(|l| l.trim_start().starts_with("regex")),
            "regex leaked into aterm-core's PRODUCTION dependencies — it must stay \
             in aterm-observe (RFC R2 purity). Production [dependencies]:\n{deps}"
        );
    }
}

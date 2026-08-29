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
//! a matcher. That vocabulary (`aterm-regex`, the first-party bounded Pike VM)
//! lives **here**, so `aterm-core` takes no **direct** regex-engine dependency
//! (RFC requirement R2; enforced by
//! [`tests::regex_is_not_in_aterm_core_production_deps`], which checks the core's
//! direct production deps — the engine does still appear in the workspace's
//! transitive closure via `aterm-search`'s `regex` feature). The agent layer
//! (`aterm-agent`, L2) composes these predicates into turn-completion; it never
//! reaches into the core enum directly.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aterm_core::terminal::{RowMatch, RowRange, WatcherSpec};

/// Re-export the regex compile error so dependents (e.g. `aterm-agent`) can name
/// it without taking a direct `aterm-regex` dependency — the regex boundary stays
/// at this crate. The path is unchanged across the engine swap; only the type it
/// names moved from `regex::Error` to [`aterm_regex::Error`].
pub mod regex_compile_error {
    pub use aterm_regex::Error;
}

/// Count of row evaluations abandoned because the pattern exhausted its scan
/// budget on that row. Process-wide, monotonic, and read back with
/// [`regex_budget_exhaustions`].
///
/// It exists because the interesting failure happens *behind a `bool`*. A
/// watcher matcher is handed to the core as an opaque `Arc<dyn RowMatch>` whose
/// only method answers yes or no, so a row the engine refused to finish
/// scanning has nowhere to say so — and the caller would see an `await` that
/// simply never latches, which is indistinguishable from output that never
/// arrived. A counter is the smallest thing that makes the difference visible
/// without a logging dependency this crate does not have (`aterm-core` and
/// `aterm-regex`, that is the whole list) and without changing the core's
/// vocabulary-free `RowMatch` contract.
static ROW_MATCH_BUDGET_EXHAUSTIONS: AtomicU64 = AtomicU64::new(0);

/// How many row evaluations have been abandoned on the scan budget since the
/// process started.
///
/// Zero is the normal state, and any non-zero value means some watcher is armed
/// with a pattern too expensive to run over the output it is watching: its rows
/// were reported as non-matches without ever being fully read, so an `await` on
/// it may never latch. See [`REGEX_STEP_LIMIT`] for the budget itself.
#[must_use]
pub fn regex_budget_exhaustions() -> u64 {
    ROW_MATCH_BUDGET_EXHAUSTIONS.load(Ordering::Relaxed)
}

/// A pre-compiled regular-expression row matcher — the one place the regex engine
/// is used in the watcher stack. The core stores it behind `dyn RowMatch` and
/// can only *evaluate* it, never construct it.
#[derive(Debug)]
pub struct RegexRowMatch {
    re: aterm_regex::Regex,
}

impl RegexRowMatch {
    /// Has this matcher ever abandoned a row on the scan budget?
    ///
    /// Sticky, and shared with the compiled pattern rather than with this
    /// wrapper, so it survives the `Arc<dyn RowMatch>` the core holds — a
    /// caller that kept its own handle to the concrete type can ask.
    #[must_use]
    pub fn budget_exhausted(&self) -> bool {
        self.re.step_limit_exceeded()
    }
}

impl RowMatch for RegexRowMatch {
    #[inline]
    fn matches(&self, row: &str) -> bool {
        match self.re.try_is_match(row) {
            Ok(hit) => hit,
            // FAIL CLOSED. The alternative — latching the watcher on a row the
            // engine never finished reading — would fire an agent's
            // `await match <re>` on evidence that was never gathered, and a
            // false latch releases a turn early. "Not yet" is the answer that
            // stays correct if the budget was the only thing in the way, so the
            // row is reported as a non-match and the fact that it was refused
            // rather than rejected is recorded where it can be read back.
            Err(_) => {
                ROW_MATCH_BUDGET_EXHAUSTIONS.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

/// Maximum regex pattern length (bytes). A watcher pattern is untrusted input
/// (an `await`/agent control argument), so it is rejected before compilation to
/// bound parse/compile cost. Mirrors the search verb's `MAX_REGEX_PATTERN_LEN`
/// (`aterm-search`) so both regex entry points enforce the same ceiling.
const MAX_REGEX_PATTERN_LEN: usize = 1024;

/// Compiled-regex size limit (bytes) for
/// [`aterm_regex::RegexBuilder::size_limit`]. Caps the NFA at 128 KiB — 2,048
/// instructions at `aterm-regex`'s 64-byte-per-instruction charge — well below
/// the engine's 10 MiB default. Matches `aterm-search`.
///
/// This bounds **both** axes, which is the reason it is not just a compile-time
/// guard. A Pike VM's scan is linear in the haystack, but the constant of that
/// linearity *is* the program size, so an untrusted pattern that compiles large
/// is also a pattern that scans slowly on every row of live PTY output.
/// Measured on the m21 box over one 4,096-column row: `(?:x?){2000}z` —
/// thirteen bytes, 4,002 instructions — took 37 ms under the old 1 MiB ceiling
/// and is refused outright under this one; the largest program that still fits
/// here costs ~18 ms on such a row, against ~150 ms at 1 MiB.
///
/// 128 KiB is the smallest ceiling that leaves a usable pattern space under
/// [`MAX_REGEX_PATTERN_LEN`]: 1,024 literal bytes compile to 1,025
/// instructions (65,600 bytes), so any ceiling at or below 64 KiB would reject
/// a plain 1,024-character literal — which
/// `row_matcher_rejects_overlong_pattern` requires to compile. The largest
/// built-in selection rule (IPv6) is 407 instructions, so real patterns keep
/// 5x headroom.
const REGEX_SIZE_LIMIT: usize = 128 * 1024; // 128 KiB

/// Lazy-DFA cache limit (bytes) for
/// [`aterm_regex::RegexBuilder::dfa_size_limit`]. Retained for source
/// compatibility and still passed: `aterm-regex` is a pure Pike VM, so there is
/// no lazy DFA for it to bound today, and the builder documents it as inert
/// rather than repurposing it silently. Per-match memory is already bounded by
/// [`REGEX_SIZE_LIMIT`] — the VM's thread set is capped by the program size.
/// Matches `aterm-search`.
const REGEX_DFA_SIZE_LIMIT: usize = 1 << 20; // 1 MiB

/// Scan budget (work units) for one row, passed to
/// [`aterm_regex::RegexBuilder::step_limit`]. Roughly 4.8 ms of matching per
/// row on the m21 box in release.
///
/// [`REGEX_SIZE_LIMIT`] bounds the *program*; this bounds the *scan*, and only
/// together do they bound the cost of a watcher. A Pike VM's search is linear
/// in the row with the program size as its constant, so the product of the two
/// is what a pattern actually costs — and both factors are chosen by whoever
/// typed the `await match <re>`. Measured: the heaviest program the 128 KiB
/// ceiling still admits costs ~16.7M units (19 ms) on a single 4,096-column
/// row, and a watcher is evaluated against every visible row on every content
/// change, so admitting that per row is admitting a live-output stall. This
/// ceiling refuses it after ~4.2M.
///
/// Nothing real comes near: the most expensive rule aterm itself ships (the
/// IPv6 selection pattern) needs ~3,100 units for one search over a full row,
/// so a working pattern keeps three orders of magnitude of headroom. Mirrors
/// `aterm-search`, which passes the same number for the same reason.
const REGEX_STEP_LIMIT: u64 = 1 << 22;

/// Compile a regex row matcher. The returned `Arc<dyn RowMatch>` is what
/// [`Terminal::watch_rows`](aterm_core::terminal::Terminal::watch_rows) takes —
/// the core receives only the opaque handle.
///
/// The pattern is untrusted, so this bounds it on every axis it has. Patterns
/// longer than [`MAX_REGEX_PATTERN_LEN`] are rejected up front; the compiler is
/// held to [`REGEX_SIZE_LIMIT`]/[`REGEX_DFA_SIZE_LIMIT`] (vs. the engine's 10 MiB
/// default) so a crafted pattern cannot amplify a small string into a
/// multi-megabyte NFA; and the matcher carries [`REGEX_STEP_LIMIT`], which
/// bounds the *scan* — the cost of running the admitted program over a row,
/// which the size ceiling alone does not bound. A row that exhausts it is
/// reported as a non-match and counted in [`regex_budget_exhaustions`]. This
/// mirrors the search verb (`aterm-search`); the live PTY-driven watcher path
/// shares the same choke point.
///
/// # Errors
/// Returns an [`aterm_regex::Error`] if `pattern` exceeds the length ceiling,
/// exceeds the compiled-size limits, or is not a valid regular expression.
pub fn row_matcher(pattern: &str) -> Result<Arc<dyn RowMatch>, aterm_regex::Error> {
    if pattern.len() > MAX_REGEX_PATTERN_LEN {
        return Err(aterm_regex::Error::Syntax(format!(
            "pattern exceeds maximum length ({MAX_REGEX_PATTERN_LEN} bytes)"
        )));
    }
    let re = aterm_regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .step_limit(REGEX_STEP_LIMIT)
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
        // under the regex crate's 10 MiB default but blows past our 128 KiB
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
            "the bounded builder must reject a pattern that exceeds 128 KiB compiled",
        );
    }

    /// The scan bound, which the size bound above cannot give.
    ///
    /// `(?:x?){1020}z` is thirteen bytes, passes the length gate, and compiles
    /// happily under [`REGEX_SIZE_LIMIT`] — 2,042 instructions is well inside
    /// 128 KiB. What it then costs is ~16.7M work units, ~19 ms in release, to
    /// cross ONE 4,096-column row; a watcher is evaluated against every visible
    /// row on every content change, so that is a live-output stall per frame
    /// from a pattern nothing else in this file refuses. [`REGEX_STEP_LIMIT`]
    /// is what refuses it.
    ///
    /// The refusal must be a NON-match rather than a latch: latching on a row
    /// the engine never finished reading would release an agent's `await` on
    /// evidence that was never gathered. And it must not be silent, which is
    /// what [`regex_budget_exhaustions`] is for — the counter is the only way
    /// the condition can escape a `dyn RowMatch` that returns `bool`.
    #[test]
    fn row_matcher_fails_closed_when_a_row_exhausts_the_scan_budget() {
        let pattern = "(?:x?){1020}z";
        assert!(
            pattern.len() <= MAX_REGEX_PATTERN_LEN,
            "under the length gate"
        );
        let matcher = row_matcher(pattern).expect("compiles: the size ceiling admits this one");
        let concrete = RegexRowMatch {
            re: aterm_regex::RegexBuilder::new(pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .step_limit(REGEX_STEP_LIMIT)
                .build()
                .expect("compiles"),
        };

        let before = regex_budget_exhaustions();
        let row = "x".repeat(4096);
        let started = std::time::Instant::now();
        assert!(
            !matcher.matches(&row),
            "an unfinishable row is a non-match, never a latch"
        );
        assert!(
            started.elapsed().as_secs() < 2,
            "the row took {:?}; the step budget is not bounding the scan",
            started.elapsed()
        );
        assert!(
            regex_budget_exhaustions() > before,
            "failing closed must be counted, or an `await` that never latches is unexplainable"
        );

        assert!(
            !concrete.budget_exhausted(),
            "a fresh matcher has nothing to report"
        );
        assert!(!concrete.matches(&row));
        assert!(
            concrete.budget_exhausted(),
            "and a refused row is on its record"
        );

        // A row it CAN finish still answers normally — the budget refuses the
        // expensive input, not the pattern.
        assert!(row_matcher(pattern).expect("compiles").matches("xxxz"));
    }

    /// Ordinary watcher patterns over ordinary rows never touch the budget.
    #[test]
    fn ordinary_patterns_never_reach_the_scan_budget() {
        let before = regex_budget_exhaustions();
        let row = "PROMPT-READY 66390b5c8f user@example.com 192.168.0.1 ERROR ".repeat(70);
        for pattern in [
            r"PROMPT-READY",
            r"\b[0-9a-f]{7,40}\b",
            r"\S+@\S+",
            r"(?i)error|warn",
        ] {
            let m = row_matcher(pattern).expect("compiles");
            assert!(
                m.matches(&row),
                "{pattern:?} must match a full-width row of its own content"
            );
        }
        assert_eq!(
            regex_budget_exhaustions(),
            before,
            "no real pattern comes within three orders of magnitude of the budget"
        );
    }

    /// RFC R2 purity, made a checkable invariant: a regex ENGINE must NOT appear
    /// in `aterm-core`'s **production** `[dependencies]` — only its
    /// `[dev-dependencies]`. The kernel stays vocabulary-free; the engine lives
    /// here. If someone adds one to the core's production deps, this fails.
    ///
    /// Both names are checked. `regex` is the retired third-party crate (still
    /// reachable as a dev-only oracle, so the assertion is not vacuous);
    /// `aterm-regex` is the first-party engine that replaced it. Dropping the
    /// second name is how this invariant would quietly stop guarding anything
    /// after the swap.
    #[test]
    fn regex_is_not_in_aterm_core_production_deps() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../aterm-core/Cargo.toml");
        let toml = std::fs::read_to_string(manifest).expect("read aterm-core Cargo.toml");
        // Slice the production [dependencies] section (up to the next table header).
        let deps = toml
            .split_once("\n[dependencies]\n")
            .map(|(_, rest)| rest.split("\n[").next().unwrap_or(rest))
            .unwrap_or("");
        for engine in ["regex", "aterm-regex"] {
            assert!(
                !deps.lines().any(|l| l.trim_start().starts_with(engine)),
                "{engine} leaked into aterm-core's PRODUCTION dependencies — it must \
                 stay in aterm-observe (RFC R2 purity). Production [dependencies]:\n{deps}"
            );
        }
    }
}

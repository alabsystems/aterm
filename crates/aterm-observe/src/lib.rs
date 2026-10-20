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
//! (RFC requirement R2; the engine does still appear in the workspace's
//! transitive closure via `aterm-search`'s `regex` feature). The agent layer
//! (`aterm-agent`, L2) composes these predicates into turn-completion; it never
//! reaches into the core enum directly.

use std::sync::Arc;

use aterm_core::terminal::{RowMatch, RowVerdict};

/// Re-export the regex compile error so dependents (e.g. `aterm-agent`) can name
/// it without taking a direct `aterm-regex` dependency — the regex boundary stays
/// at this crate. The path is unchanged across the engine swap; only the type it
/// names moved from `regex::Error` to [`aterm_regex::Error`].
pub mod regex_compile_error {
    pub use aterm_regex::Error;
}

/// A pre-compiled regular-expression row matcher — the one place the regex engine
/// is used in the watcher stack. The core stores it behind `dyn RowMatch` and
/// can only *evaluate* it, never construct it.
#[derive(Debug)]
pub struct RegexRowMatch {
    re: aterm_regex::Regex,
}

impl RowMatch for RegexRowMatch {
    #[inline]
    fn verdict(&self, row: &str) -> RowVerdict {
        // A refused scan (the engine's budget ran out) is UNDECIDED, never a
        // non-match. Either guess would latch some watcher on evidence that was
        // never gathered. A match would fire an agent's `await match <re>` early.
        // A non-match, which is what this used to answer, fires the inverse
        // predicate (`await gone`, `turn … settle=gone:`, the supervisor's busy
        // test) while the row is still on the screen. Undecided keeps both kinds
        // of wait pending, which stays correct if the budget was the only thing
        // in the way (see `RowMatch`, ruling 2026-10-10).
        match self.re.try_is_match(row) {
            Ok(matched) => matched.into(),
            Err(_) => RowVerdict::Undecided,
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
/// [`Terminal::watch_rows`](aterm_core::terminal::Terminal::watch_rows) and its
/// inverse [`Terminal::watch_rows_gone`](aterm_core::terminal::Terminal::watch_rows_gone)
/// take — the core receives only the opaque handle.
///
/// The pattern is untrusted, so this bounds it on every axis it has. Patterns
/// longer than [`MAX_REGEX_PATTERN_LEN`] are rejected up front; the compiler is
/// held to [`REGEX_SIZE_LIMIT`]/[`REGEX_DFA_SIZE_LIMIT`] (vs. the engine's 10 MiB
/// default) so a crafted pattern cannot amplify a small string into a
/// multi-megabyte NFA; and the matcher carries [`REGEX_STEP_LIMIT`], which
/// bounds the *scan* — the cost of running the admitted program over a row,
/// which the size ceiling alone does not bound. A row that exhausts it is
/// [`RowVerdict::Undecided`]: neither a match nor shown clear, so `await match`
/// and `await gone` both keep waiting. This mirrors the search verb
/// (`aterm-search`); the live PTY-driven watcher path shares the same choke
/// point.
///
/// # Errors
/// Returns an [`aterm_regex::Error`] if `pattern` exceeds the length ceiling,
/// exceeds the compiled-size limits, or is not a valid regular expression.
pub fn row_matcher(pattern: &str) -> Result<Arc<dyn RowMatch>, aterm_regex::Error> {
    Ok(Arc::new(RegexRowMatch {
        re: bounded_regex(pattern)?,
    }))
}

/// The pattern compiled under every bound [`row_matcher`] documents.
fn bounded_regex(pattern: &str) -> Result<aterm_regex::Regex, aterm_regex::Error> {
    if pattern.len() > MAX_REGEX_PATTERN_LEN {
        return Err(aterm_regex::Error::Syntax(format!(
            "pattern exceeds maximum length ({MAX_REGEX_PATTERN_LEN} bytes)"
        )));
    }
    aterm_regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .step_limit(REGEX_STEP_LIMIT)
        .build()
}

/// Wire size in bytes of an encoded watcher regex budget frame (`"OBSB"`).
pub const WATCHER_BUDGET_WIRE_BYTES: usize = 20;

/// Wire size in bytes of an encoded row-matcher verdict frame (`"OBSV"`).
pub const ROW_VERDICT_WIRE_BYTES: usize = 16;

#[inline]
const fn u16_le(b0: u8, b1: u8) -> u16 {
    (b0 as u16) | ((b1 as u16) << 8)
}

#[inline]
const fn u32_le(b0: u8, b1: u8, b2: u8, b3: u8) -> u32 {
    (b0 as u32) | ((b1 as u32) << 8) | ((b2 as u32) << 16) | ((b3 as u32) << 24)
}

#[inline]
#[expect(
    clippy::too_many_arguments,
    reason = "eight bytes as shifts, not `from_*_bytes`, so the Trust lane proves it (e9568a13f7)"
)]
const fn u64_le(b0: u8, b1: u8, b2: u8, b3: u8, b4: u8, b5: u8, b6: u8, b7: u8) -> u64 {
    (u32_le(b0, b1, b2, b3) as u64) | ((u32_le(b4, b5, b6, b7) as u64) << 32)
}

/// Encode a 20-byte watcher regex budget wire frame (`"OBSB"`).
#[inline]
#[must_use]
pub const fn encode_watcher_budget_wire(
    max_pattern_len: u32,
    size_limit: u32,
    step_limit: u64,
) -> [u8; WATCHER_BUDGET_WIRE_BYTES] {
    let pb = max_pattern_len.to_le_bytes();
    let sb = size_limit.to_le_bytes();
    let lb = step_limit.to_le_bytes();
    [
        b'O', b'B', b'S', b'B', pb[0], pb[1], pb[2], pb[3], sb[0], sb[1], sb[2], sb[3], lb[0],
        lb[1], lb[2], lb[3], lb[4], lb[5], lb[6], lb[7],
    ]
}

/// Decode a 20-byte watcher regex budget wire frame produced by
/// [`encode_watcher_budget_wire`].
///
/// Rejects frames whose `max_pattern_len`, `size_limit`, or `step_limit` is
/// zero or exceeds the hard ceilings enforced by [`row_matcher`].
#[inline]
#[must_use]
pub const fn decode_watcher_budget_wire(bytes: &[u8]) -> Option<(u32, u32, u64)> {
    if bytes.len() < WATCHER_BUDGET_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'O' || bytes[1] != b'B' || bytes[2] != b'S' || bytes[3] != b'B' {
        return None;
    }
    let max_pattern_len = u32_le(bytes[4], bytes[5], bytes[6], bytes[7]);
    let size_limit = u32_le(bytes[8], bytes[9], bytes[10], bytes[11]);
    let step_limit = u64_le(
        bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
    );
    if max_pattern_len == 0
        || (max_pattern_len as usize) > MAX_REGEX_PATTERN_LEN
        || size_limit == 0
        || (size_limit as usize) > REGEX_SIZE_LIMIT
        || step_limit == 0
        || step_limit > REGEX_STEP_LIMIT
    {
        return None;
    }
    Some((max_pattern_len, size_limit, step_limit))
}

/// Encode a 16-byte row-matcher verdict wire frame (`"OBSV"`).
#[inline]
#[must_use]
pub const fn encode_row_verdict_wire(
    verdict: RowVerdict,
    row_idx: u16,
    pattern_len: u8,
    seq: u64,
) -> [u8; ROW_VERDICT_WIRE_BYTES] {
    let rb = row_idx.to_le_bytes();
    let sb = seq.to_le_bytes();
    let v_byte = match verdict {
        RowVerdict::NoMatch => 0u8,
        RowVerdict::Match => 1u8,
        RowVerdict::Undecided => 2u8,
    };
    [
        b'O',
        b'B',
        b'S',
        b'V',
        v_byte,
        pattern_len,
        rb[0],
        rb[1],
        sb[0],
        sb[1],
        sb[2],
        sb[3],
        sb[4],
        sb[5],
        sb[6],
        sb[7],
    ]
}

/// Decode a 16-byte row-matcher verdict wire frame produced by
/// [`encode_row_verdict_wire`].
#[inline]
#[must_use]
pub const fn decode_row_verdict_wire(bytes: &[u8]) -> Option<(RowVerdict, u16, u8, u64)> {
    if bytes.len() < ROW_VERDICT_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'O' || bytes[1] != b'B' || bytes[2] != b'S' || bytes[3] != b'V' {
        return None;
    }
    let verdict = match bytes[4] {
        0 => RowVerdict::NoMatch,
        1 => RowVerdict::Match,
        2 => RowVerdict::Undecided,
        _ => return None,
    };
    let pattern_len = bytes[5];
    let row_idx = u16_le(bytes[6], bytes[7]);
    let seq = u64_le(
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    );
    Some((verdict, row_idx, pattern_len, seq))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::{ClockReading, RowRange, Terminal};
    use std::time::{Duration, Instant};

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
        let id = t.watch_rows(m, RowRange::All, base).expect("arm");
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
        let id = t.watch_rows(m, RowRange::All, base).expect("arm");
        assert!(
            t.watch_poll(id).is_some(),
            "an already-matching row latches at arm time"
        );
    }

    /// The inverse predicate, level-triggered: a surface with no matching row
    /// latches AT ARM (the `watch_rows_gone` immediate eval), so an agent asking
    /// "is the busy footer gone?" of an already-finished turn is told yes now.
    #[test]
    fn row_gone_latches_immediately_if_nothing_matches() {
        let base = Instant::now();
        let mut t = Terminal::new(24, 80);
        t.process_at(b"Done.\r\n", clock_at(base, 5));
        let m = row_matcher("esc to interrupt").expect("compile");
        let id = t.watch_rows_gone(m, RowRange::All, base).expect("arm");
        assert!(
            t.watch_poll(id).is_some(),
            "no matching row at arm => latched at arm time"
        );
    }

    /// The inverse of `row_matcher_latches_on_real_engine_output`, through the
    /// real pipeline: a visible matching row HOLDS `gone` pending across repaints
    /// that keep it on screen, and the paint that takes it away latches.
    #[test]
    fn row_gone_waits_for_the_matching_row_to_leave() {
        let base = Instant::now();
        let mut t = Terminal::new(24, 80);
        t.process_at(
            b"thinking\xe2\x80\xa6 (esc to interrupt)\r\n",
            clock_at(base, 5),
        );
        let m = row_matcher("esc to interrupt").expect("compile");
        let id = t.watch_rows_gone(m, RowRange::All, base).expect("arm");
        assert!(
            t.watch_poll(id).is_none(),
            "the footer is on screen => pending"
        );

        // More output that leaves the footer in place: still pending.
        t.process_at(b"still thinking\r\n", clock_at(base, 10));
        assert!(
            t.watch_poll(id).is_none(),
            "content advanced but the row is still visible => still pending"
        );

        // Clear the screen (home + ED 2): the footer leaves => latched.
        t.process_at(b"\x1b[H\x1b[2JDone.\r\n", clock_at(base, 20));
        let sat = t
            .watch_poll(id)
            .expect("the row leaving the real surface latched `gone`");
        assert!(sat.seq > 0);
    }

    /// `gone` WAITS ON A ROW THE SCAN BUDGET COULD NOT READ (ruling 2026-10-10).
    ///
    /// A 4,001-character row that really ends in the match exhausts
    /// [`REGEX_STEP_LIMIT`] under `(?:x?){1020}z`. The refusal used to read as
    /// "no match", so `await gone` latched with the row still on screen, and
    /// `turn … settle=gone:` and the supervisor's busy test went with it. Now
    /// both predicates keep waiting. `await match` is unchanged: it never
    /// latched on the refused row, and it latches once the row can be read.
    #[test]
    fn row_gone_waits_while_a_visible_row_exhausts_the_scan_budget() {
        let base = Instant::now();
        let mut t = Terminal::new(24, 4096);
        let heavy = format!("{}z\r\n", "x".repeat(4000));
        t.process_at(heavy.as_bytes(), clock_at(base, 5));
        let m = row_matcher("(?:x?){1020}z").expect("compile");
        let shown = t
            .watch_rows(m.clone(), RowRange::All, base)
            .expect("arm match");
        let gone = t.watch_rows_gone(m, RowRange::All, base).expect("arm gone");
        assert!(
            t.watch_poll(shown).is_none(),
            "a row the budget refused is not a match"
        );
        assert!(
            t.watch_poll(gone).is_none(),
            "a row the budget refused is not gone: it is still on screen"
        );

        // More output with the heavy row still visible: still pending.
        t.process_at(b"still working\r\n", clock_at(base, 10));
        assert!(
            t.watch_poll(gone).is_none(),
            "still unread => still pending"
        );

        // The screen is redrawn with a readable match: `await match` latches,
        // `gone` holds.
        t.process_at(b"\x1b[H\x1b[2Jxxxz\r\n", clock_at(base, 20));
        assert!(t.watch_poll(shown).is_some(), "a readable match latches");
        assert!(
            t.watch_poll(gone).is_none(),
            "a readable match holds `gone`"
        );

        // The match leaves: every row is read and clear, so `gone` latches.
        t.process_at(b"\x1b[H\x1b[2JDone.\r\n", clock_at(base, 30));
        assert!(t.watch_poll(gone).is_some(), "the row left => `gone`");
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
        // under a 10 MiB limit but blows past our 128 KiB REGEX_SIZE_LIMIT —
        // the classic "small pattern, huge NFA" amplifier.
        // row_matcher must reject it, proving the size caps are wired (not just
        // the length gate). If this ever compiles, the size_limit call is gone.
        let amplifier = "(a{200}){200}";
        assert!(
            amplifier.len() <= MAX_REGEX_PATTERN_LEN,
            "under the length gate"
        );
        assert!(
            aterm_regex::RegexBuilder::new(amplifier)
                .size_limit(10 << 20)
                .build()
                .is_ok(),
            "sanity: a 10 MiB limit accepts this pattern",
        );
        assert!(
            row_matcher(amplifier).is_err(),
            "the bounded builder must reject a pattern that exceeds 128 KiB compiled",
        );
    }

    /// Ordinary watcher patterns over ordinary rows never touch the budget: a
    /// row the budget refused would read as undecided, not as a match.
    #[test]
    fn ordinary_patterns_never_reach_the_scan_budget() {
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
    }

    #[test]
    fn watcher_budget_and_row_verdict_wire_roundtrip() {
        let bw = encode_watcher_budget_wire(1024, 128 * 1024, 1 << 22);
        assert_eq!(
            decode_watcher_budget_wire(&bw),
            Some((1024, 128 * 1024, 1 << 22))
        );
        let over_bw = encode_watcher_budget_wire(2048, 128 * 1024, 1 << 22);
        assert!(decode_watcher_budget_wire(&over_bw).is_none());

        for verdict in [
            RowVerdict::NoMatch,
            RowVerdict::Match,
            RowVerdict::Undecided,
        ] {
            let vw = encode_row_verdict_wire(verdict, 12, 16, 99);
            assert_eq!(decode_row_verdict_wire(&vw), Some((verdict, 12, 16, 99)));
        }
    }
}

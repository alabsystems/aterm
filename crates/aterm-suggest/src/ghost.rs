// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live-line state machine: what is actually on the glass, and what a key
//! does to it.
//!
//! [`Engine`] is a pure ranker over a corpus. [`Ghost`] is the thing a host
//! wires up: it holds the engine, tracks the line being typed, decides when to
//! recompute, and owns the accept / dismiss transitions that turn a suggestion
//! into bytes for the PTY.
//!
//! ## Why the clock is pinned per line
//!
//! Ranking is time-dependent (recency decay), so in principle the winner can
//! change between two keystrokes purely because time passed. Over a 24 h
//! half-life and a 200 ms inter-key gap that difference is numerically absurd,
//! but "absurd" is not "never", and a ghost that reshuffles under a stationary
//! buffer reads as a rendering fault.
//!
//! So the clock is sampled ONCE per line — at [`Ghost::begin_line`] — and every
//! keystroke on that line ranks against it. This buys correctness for the
//! optimization below and, independently, makes the ghost stable.
//!
//! ## Monotone candidate narrowing (the reason this is free)
//!
//! Typing forward only ever *shrinks* the candidate set: every command matching
//! the prefix `"git com"` also matches `"git co"`. Scores do not depend on the
//! buffer at all — only on the entry, the cwd and the (now pinned) clock. So if
//! the previous winner still matches the longer buffer, it is still the winner
//! of the smaller set, and no rescan is needed.
//!
//! That turns the common case — typing another character forward — from an
//! `O(corpus)` scan into a single `starts_with` plus an offset bump. A full
//! rescan happens only when the ghost actually dies (the user typed something
//! that diverges, or edited backwards).
//!
//! **The subset argument covers SCORING ONLY.** Three things are NOT covered,
//! and each was a real bug:
//!
//! * the refusals [`Engine::suggest`] makes before it consults the corpus —
//!   typing a space held a completion the engine would have refused. They now
//!   live in the shared [`Engine::accepts_context`], called from both paths;
//! * the filters it applies AFTER scoring — a completion truncated at a wide
//!   glyph could be whittled to a lone blank that still counted as "visible".
//!   The fast path re-checks `trim().is_empty()`;
//! * a corpus that CHANGES mid-line. `Engine::clear()` — the user-facing
//!   "forget my history" control — left the offending completion on glass,
//!   because narrowing never re-consults. The ghost now stamps
//!   [`Engine::generation`] at each full scan and refuses to narrow across a
//!   change.
//!
//! `narrowing_always_matches_a_full_rescan` and
//! `narrowing_matches_a_rescan_across_the_post_scoring_filters` pin the
//! equivalence against a brute-force recompute — the second over a corpus built
//! from the truncating and destructive cases, where the argument is least
//! obviously sound.

use crate::{Context, Engine, Source};

/// The suggestion currently on glass: ONE owned string plus how much of it the
/// user has since typed.
///
/// Narrowing advances `from` instead of re-boxing the shrinking tail, which is
/// what the fast path used to do — an allocation and a memcpy on every forward
/// keystroke, on the per-keystroke path.
///
/// Invariant: `from <= completion.len()`, and whenever a `Live` is retained,
/// `rest()` is non-blank. The moment that fails the whole `Live` is dropped, so
/// `visible()` never reports an empty or all-space ghost.
#[derive(Debug)]
struct Live {
    completion: Box<str>,
    from: usize,
    source: Source,
}

impl Live {
    /// The part still to paint.
    fn rest(&self) -> &str {
        &self.completion[self.from..]
    }
}

/// What a host should paint, and what the accept keys do.
///
/// Construct one per pane. Inert until the engine's mode is turned on.
#[derive(Debug)]
pub struct Ghost {
    engine: Engine,
    /// The suggestion currently on glass, if any.
    current: Option<Live>,
    /// The exact buffer `current` was computed for. Divergence is detected by
    /// comparing against this rather than by re-ranking.
    for_buffer: String,
    /// The clock this line ranks against (see the module docs). `None` between
    /// lines.
    line_now_ms: Option<u64>,
    /// The user dismissed the ghost on this line; stay silent until the line
    /// ends. Without this, Escape would be undone by the very next keystroke.
    dismissed: bool,
    /// [`Engine::generation`] as of the last FULL scan. Narrowing is only sound
    /// while the corpus it ranked against is unchanged.
    scanned_generation: u64,
}

impl Ghost {
    /// A ghost driving `engine`.
    #[must_use]
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            current: None,
            for_buffer: String::new(),
            line_now_ms: None,
            dismissed: false,
            scanned_generation: 0,
        }
    }

    /// The engine, for corpus updates ([`Engine::record`]) and config changes.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// The engine.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Start a fresh command line: OSC 133;B (input started), or any event that
    /// invalidates the line (resize, pane swap, screen switch).
    ///
    /// Samples the clock for the whole line and clears the dismissal latch.
    pub fn begin_line(&mut self, now_ms: u64) {
        self.current = None;
        self.for_buffer.clear();
        self.line_now_ms = Some(now_ms);
        self.dismissed = false;
    }

    /// End the current line (OSC 133;C — the user pressed Enter). Nothing is
    /// painted until the next [`begin_line`](Self::begin_line).
    pub fn end_line(&mut self) {
        self.current = None;
        self.for_buffer.clear();
        self.line_now_ms = None;
        self.dismissed = false;
    }

    /// Drop the suggestion for the rest of this line (Escape).
    pub fn dismiss(&mut self) {
        self.current = None;
        self.for_buffer.clear();
        self.dismissed = true;
    }

    /// Recompute for the current buffer. Call once per keystroke, after the
    /// host has read the buffer off the grid.
    ///
    /// Cheap by construction: an unchanged buffer is a no-op, and a buffer that
    /// merely extended the previous one keeps the current suggestion without
    /// touching the corpus (see the module docs).
    pub fn update(&mut self, ctx: &Context<'_>) {
        // A line that never began cannot paint: `begin_line` is driven by
        // OSC 133;B, so this also means "no shell integration, no ghost".
        let Some(now_ms) = self.line_now_ms else {
            self.current = None;
            return;
        };
        if self.dismissed {
            return;
        }
        // EVERY refusal `Engine::suggest` makes before consulting the corpus
        // must gate the fast path too, because the fast path never calls it.
        // One shared predicate, so the two cannot drift.
        if !self.engine.accepts_context(ctx) {
            self.current = None;
            self.for_buffer.clear();
            self.for_buffer.push_str(ctx.buffer);
            return;
        }
        if ctx.buffer == self.for_buffer {
            return; // nothing changed
        }
        // FAST PATH: the buffer grew, the corpus is unchanged, and the standing
        // suggestion still matches — so the previous winner is still the winner
        // of the (only ever smaller) candidate set. No scan.
        if self.scanned_generation == self.engine.generation()
            && let Some(cur) = &mut self.current
            && let Some(grown) = ctx.buffer.strip_prefix(self.for_buffer.as_str())
            && !grown.is_empty()
            && cur.rest().starts_with(grown)
        {
            cur.from += grown.len();
            // `trim`, not `is_empty`: the engine refuses an all-blank remainder,
            // which narrowing reaches when a completion truncated at a wide
            // glyph is whittled down to its trailing space. It paints nothing
            // yet would still latch a repaint per keystroke.
            if cur.rest().trim().is_empty() {
                self.current = None;
            }
            self.for_buffer.clear();
            self.for_buffer.push_str(ctx.buffer);
            return;
        }
        // SLOW PATH: divergence, a backwards edit, a changed corpus, or nothing
        // standing.
        self.current = self.engine.suggest(ctx, now_ms).map(|s| Live {
            completion: s.completion,
            from: 0,
            source: s.source,
        });
        self.scanned_generation = self.engine.generation();
        self.for_buffer.clear();
        self.for_buffer.push_str(ctx.buffer);
    }

    /// The text to paint immediately after the cursor, if any.
    #[must_use]
    pub fn visible(&self) -> Option<&str> {
        self.current.as_ref().map(Live::rest)
    }

    /// Which corpus produced what [`visible`](Self::visible) is showing.
    ///
    /// Exposed so a host can style sources differently and so accept rates can
    /// be measured per source — the only honest way to tune a ranker. `None`
    /// exactly when nothing is showing.
    #[must_use]
    pub fn source(&self) -> Option<Source> {
        self.current.as_ref().map(|l| l.source)
    }

    /// Accept the whole suggestion: the bytes to write to the PTY.
    ///
    /// Returns `None` when nothing is showing, so a host can bind this to a key
    /// that keeps its normal meaning (End, Right-arrow) whenever there is no
    /// ghost — the binding is only stolen when it would do something.
    #[must_use]
    pub fn accept_all(&mut self) -> Option<String> {
        let s = self.current.take()?;
        let text = s.rest().to_owned();
        // `clear`, NOT `push_str(&text)`. Appending makes `for_buffer` equal the
        // buffer the shell is about to echo, so the next `update` takes the
        // "nothing changed" early-out — and with `current` already taken the
        // line stays ghost-less for good. With `git status` and
        // `git status --short` both in the corpus, accepting the first must
        // still leave ` --short` on offer.
        self.for_buffer.clear();
        Some(text)
    }

    /// Accept one word of the suggestion (Alt-Right / Ctrl-Right).
    ///
    /// Partial accept is what makes a long suggestion usable: the user takes
    /// `git commit ` and then types their own message, instead of accepting a
    /// whole stale command line and deleting the tail. The word boundary is
    /// "run of separators, then run of non-separators", so accepting from
    /// `" --amend -m x"` yields `" --amend"` — leading space included, which is
    /// what makes repeated presses walk the line one token at a time.
    #[must_use]
    pub fn accept_word(&mut self) -> Option<String> {
        let cur = self.current.as_mut()?;
        let taken = word_slice(cur.rest()).to_owned();
        if taken.is_empty() {
            return None;
        }
        cur.from += taken.len();
        if cur.rest().is_empty() {
            self.current = None;
        }
        // Same reasoning as `accept_all`: force the next `update` to rescan
        // rather than short-circuit on an equal buffer.
        self.for_buffer.clear();
        Some(taken)
    }
}

/// The leading "separators then word" slice of `s` — see [`Ghost::accept_word`].
fn word_slice(s: &str) -> &str {
    let sep_end = s.find(|c: char| !is_word_sep(c)).unwrap_or(s.len());
    let rest = &s[sep_end..];
    let word_end = rest.find(is_word_sep).map_or(s.len(), |i| sep_end + i);
    &s[..word_end]
}

/// Word separators for partial accept. Shell-flavoured: `/` is NOT a separator,
/// so a path is taken whole (accepting `src/` then `main.rs` separately is
/// exactly the tedium partial accept exists to avoid).
fn is_word_sep(c: char) -> bool {
    c.is_whitespace() || matches!(c, '=' | ':' | ',' | ';' | '|' | '&')
}

#[cfg(test)]
mod tests;

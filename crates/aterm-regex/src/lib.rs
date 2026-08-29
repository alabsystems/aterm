// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-regex** — aterm's regular expressions, first-party.
//!
//! This crate exists to retire the `regex` crate from aterm's shipped graph. That
//! dependency cost four packages and 158,471 lines of third-party source
//! (`regex`, `regex-automata`, `regex-syntax`, `aho-corasick`) to provide
//! `Regex::new`, a size-limited builder, `find`, `find_iter`, `is_match` and one
//! error type at six call sites — `aterm-observe`, `aterm-selection`'s rules, and
//! four files of `aterm-search` — plus a seventh in `aterm-core`'s test-support
//! triggers, which is a dev-dependency and never shipped. Owner rule: a package
//! used this narrowly is a package we can debug and maintain ourselves.
//!
//! ## What it is
//!
//! Parse → NFA → **Pike VM**: the pattern becomes a Thompson automaton and the
//! search advances the whole set of live states one code point at a time. Match
//! time is linear in the length of the haystack, always — `(a|a)*b` against ten
//! thousand `a`s is a few milliseconds here and a hang in a backtracking engine.
//!
//! That is not a reduced substitute for what `regex` did. `regex` is also a
//! finite-automaton engine with the same linear guarantee, has no backreferences
//! and no look-around, and matches leftmost-first; so does this. The syntax floor
//! is set by the patterns aterm already ships in
//! `aterm-selection`'s `builtin_patterns` — URLs, IPv4/IPv6, email, UUID, git
//! hashes, quoted strings, Windows and unix paths — and every one of them is a
//! test here, checked against the `regex` crate as a differential oracle.
//!
//! ```
//! use aterm_regex::Regex;
//!
//! let re = Regex::new(r"\b[0-9a-fA-F]{7,40}\b")?;
//! let hash = re.find("commit 66390b5c8f is the release")
//!     .map(|m| (m.start(), m.end(), m.as_str()));
//! assert_eq!(hash, Some((7, 17, "66390b5c8f")));
//! # Ok::<(), aterm_regex::Error>(())
//! ```
//!
//! ## Bounds, because patterns here are user input
//!
//! Selection rules, the `await match <re>` verb and the search options all take
//! their pattern from a human or an agent, so the engine is bounded on both
//! axes. [`RegexBuilder::size_limit`] caps the compiled program in bytes and is
//! re-checked on every instruction emitted, so `(a{200}){200}` — thirteen bytes
//! of pattern, forty thousand instructions of automaton — fails fast instead of
//! allocating. Nesting is capped at 250 levels, so `((((…))))` returns an error
//! rather than overflowing the stack.
//!
//! The Pike VM's guarantee is worth stating precisely, because the loose version
//! of it — "linear time, therefore nothing can hang it" — is false, and this
//! crate shipped that loose version in writing until the numbers below were
//! measured. A search is linear **in the length of the haystack**: there is no
//! input that makes it exponential, which is the failure mode a backtracking
//! engine has and this one cannot. But the *constant* of that linearity is the
//! compiled program size, because the simulation advances every live thread at
//! every position. A scan costs `O(|haystack| × program)`, both factors are
//! chosen by whoever supplies the pattern and the content, and linear in one
//! factor is not a bound on the product.
//!
//! Measured on the m21 box in release, through the shipped bounded builder at a
//! 1 MiB `size_limit` — which admits roughly 16k instructions — against `regex`
//! as the oracle:
//!
//! | pattern | bytes | one 4,096-column row | a 3 MB haystack | oracle |
//! |---|---|---|---|---|
//! | `(?:x?){2000}z` | 13 | 42.7 ms | 30.25 s | 6.8 µs / 40 µs |
//! | `(?:x\|x\|x\|x\|x\|x\|x\|x){400}z` | 25 | 60.3 ms | 50.09 s | 855 µs / 1.01 ms |
//!
//! Thirteen bytes of pattern, inside every length gate in the tree, for a
//! 750,000x slowdown on the second column — reachable from a selection rule, an
//! `await match <re>` argument or the search box, which is to say from user
//! input. (Re-measured while fixing it, same box, same builder: 37.7 ms /
//! 28.66 s and 55.5 ms / 50.16 s — the same numbers to within the noise of a
//! loaded machine.)
//!
//! Two bounds answer that, and they answer different halves of it.
//! [`RegexBuilder::size_limit`] bounds the *program*; the call sites pass
//! 128 KiB rather than the 10 MiB default, which refuses both patterns above
//! outright and caps the admitted ones at ~2,048 instructions.
//! [`RegexBuilder::step_limit`] bounds the *scan*: it charges every position
//! visited, every prefilter byte skipped, every epsilon instruction entered and
//! every thread stepped against one budget, so it is the `|haystack| × program`
//! product itself that is capped rather than one of its factors. Exhausting it
//! ends the search with [`StepLimitExceeded`] — a distinct outcome, never a
//! quiet "no match", because a wrong answer is worse than a refusal.
//! `tests/bounds.rs` pins the compile half; the step-budget half is pinned in
//! this crate's own test modules.
//!
//! ## What it costs, measured
//!
//! `regex` is faster at *matching* and this crate is faster at *compiling*, and
//! both differences are structural rather than incidental. `regex` builds a lazy
//! DFA and drives it behind SIMD literal prefilters, so a warm scan is close to
//! `memchr`; a Pike VM walks a state set. The trade runs the other way at
//! compile time, where a DFA-backed engine has far more to build.
//!
//! Measured on the m21 box, release, min of three alternated runs, scanning
//! 20,000 terminal-style rows (~3 MB) with `find_iter`:
//!
//! | pattern | here | `regex` |
//! |---|---|---|
//! | literal `aterm` | 2.7 ms | 0.30 ms |
//! | `\b[0-9a-fA-F]{7,40}\b` (git hash) | 21.0 ms | 2.4 ms |
//! | the built-in IPv4 rule | 30.2 ms | 1.4 ms |
//! | the built-in URL rule | 14.0 ms | 1.1 ms |
//! | `(?i)ATERM` | 3.2 ms | 0.69 ms |
//! | a pattern that never matches | 1.0 ms | 0.23 ms |
//! | one compile of the IPv4 rule | 3.2 µs | 150 µs |
//! | one compile of a literal | 0.37 µs | 1.5 µs |
//!
//! So: five to twenty times slower per full-scrollback scan, forty-odd times
//! faster per compile. That second number is not a consolation prize — `aterm-search`
//! recompiles the pattern on **every keystroke**, and selection compiles eleven
//! rules at startup. The scan side is bounded work in bounded slices
//! (`BudgetedSearch` feeds rows in budgeted batches), and 30 ms is the worst
//! case over an entire 20k-row scrollback, not per frame.
//!
//! The gap that mattered most is already closed. A first-code-point prefilter
//! rejects a position with one array lookup per byte, and it survives a leading
//! assertion — `\bcommit` and `^ERROR` are what a terminal actually searches for,
//! and arming those took the never-matches case from 21.6 ms to 1.0 ms.
//!
//! Surviving a leading assertion is right for `\bcommit` and was wrong for
//! `^ERROR`: the prefilter would hunt down every `E` in the scrollback so the
//! simulation could fail `\A` on each one. A start-anchored program can only
//! match at offset 0, so [`compile`](compile) now proves that fact once
//! ([`Program::start_anchored`]) and the search stops rather than walks.
//! `benches/anchored_scan.rs` measures it over the shipped 100,000-row default,
//! paired and slot-alternated against the same binary with the fact forced off:
//!
//! | pattern | anchor fact off | on |
//! |---|---|---|
//! | `(?i)^ERROR` — the find bar's default | 7.42 ms | **2.10 ms** |
//! | `(?i)^\d+` | 10.10 ms | **1.45 ms** |
//! | `^\[` (a pattern that *does* match) | 2.82 ms | **1.19 ms** |
//! | `(?i)ERROR` — control | 16.99 ms | 16.64 ms |
//! | `(?im)^ERROR` — control, `StartLine` anchors nothing | 7.80 ms | 7.59 ms |
//! | `\bcommit` — control | 5.53 ms | 5.71 ms |
//!
//! The three controls sit inside a 3.1% identical-binary noise floor, which is
//! the point: the fast path must take the anchored patterns and nothing else.
//!
//! The other half of the gap is the CLASS path, and it is the case-insensitive
//! search — the find bar's default — that pays it. `(?i)ERROR` compiles to five
//! `Inst::Class` where the case-sensitive form compiles to five `Inst::Char`,
//! so the default search asks a lookup structure built for the whole of Unicode
//! about one ASCII byte, five times per candidate position. `Program`
//! now carries a dense `u128` per class covering U+0000..U+007F, built at the
//! moment the classes are frozen, and below U+0080 a membership test is a shift
//! and a test against sixteen bytes rather than a reach through two `Vec`
//! headers and a binary search:
//!
//! | pattern | binary search | ASCII bitmap |
//! |---|---|---|
//! | `(?i)ERROR` — the find bar's default | 16.90 ms | **13.76 ms** |
//!
//! Measured the same way; that lane's identical-binary control is 0.1% over
//! eleven rounds. The other lanes do not move outside their own noise, which
//! for `\bcommit` is ±15% on a loaded box and for `(?i)^\d+` is ±1.2% — a
//! reminder that the floor is per-arm and quoting one number for the suite
//! would be quoting the wrong one.
//!
//! What remained after that was not the simulation but the PREFILTER'S HIT
//! RATE, and the bench states the cost model as a measurement rather than an
//! argument: `(?i)QUARK` and `(?i)ERROR` are the same shape and length over the
//! same corpus and differ only in how common their lead byte is — 3.6 ms
//! against 13.2 ms. That gap is what a one-byte filter spends entering the
//! simulation at positions that die on their SECOND character.
//!
//! So [`Prefilter`](compile::Prefilter) now carries a second marking, for the
//! code point after the first, armed only when every path provably consumes
//! one. `(?i)ERROR` stops meaning "every `e` in the scrollback" and starts
//! meaning "every `e` followed by an `r`":
//!
//! | pattern | one byte | two bytes |
//! |---|---|---|
//! | `(?i)ERROR` | 12.87 ms | **8.39 ms** |
//! | `\bcommit` | 5.90 ms | **4.66 ms** |
//! | `(?im)^ERROR` | 7.23 ms | **5.96 ms** |
//! | `(?i)^ERROR` | 1.836 ms | 1.835 ms |
//!
//! The last row is the anchored fast path getting there first, as it should.
//! Per-lane controls were 0.0-1.8%.
//!
//! One more, from the same measurement: the second-byte test now runs BEFORE
//! the decode rather than after it. Everything else in the skip loop needs the
//! decoded code point — the viability walk compares whole `char`s — but an
//! ASCII lead byte IS its whole code point, so the next one starts at `pos + 1`
//! and can be tested with an array lookup. Doing it second meant paying a
//! decode on every marked byte in order to reject nearly all of them:
//! `(?i)ERROR` -5.0%, `(?im)^ERROR` -5.1%, `\bcommit` -1.9%, `(?i)QUARK`
//! -1.8%, against per-lane controls of 0.0-0.3%.
//!
//! Taken together, the find bar's default over the shipped 100,000-row
//! scrollback has gone from 16.90 ms per keystroke to 7.79 ms.
//!
//! ## Divergences from the `regex` crate, stated plainly
//!
//! Every one of these is a *refusal* — an [`Error`] naming the construct — never
//! a quiet reinterpretation. `tests/differential.rs` proves agreement on
//! everything else.
//!
//! * `\p{…}` / `\pL` Unicode property classes are not supported.
//! * Character-class set operations (`[a&&b]`, `[a--b]`, `[a~~b]`) and the
//!   nested classes that go with them are not supported. Escape the operator to
//!   match it literally.
//! * `(?-u)` byte-oriented mode is not supported: this engine matches `&str` by
//!   code point.
//! * `(?R)` CRLF mode is not supported.
//! * `\w`, and therefore `\b`, follow the *toolchain's* Unicode version through
//!   `char::is_alphabetic`, which is newer than the tables `regex` bundles. The
//!   two agree except on code points that gained the `Alphabetic` property in
//!   between; the differential test asserts every disagreement has exactly that
//!   shape. Measured against the locked oracle, `^\w$` disagrees on 4,662 code
//!   points, all in one direction — this engine is a strict superset, and `\d`,
//!   `\s`, `\D`, `\S` and `\W` agree exactly.
//!
//!   Two consequences that follow from it and are easy to miss:
//!
//!   1. **It reaches `\b`, and therefore a shipped selection rule.** Word
//!      boundaries are defined from `\w`, so they fall in different places next
//!      to those 4,662 code points. Concretely, `aterm-selection`'s `git_hash`
//!      rule is `\b[0-9a-fA-F]{7,40}\b`, and on `"deadbeef\u{088F}"` the oracle
//!      selects `deadbeef` while this engine selects nothing: U+088F is
//!      `Alphabetic` to the current toolchain, so there is no boundary after the
//!      `f`. The `url`, `email` and `semver` rules are unaffected.
//!   2. **The answer tracks the compiler, not this crate.** The `Alphabetic`
//!      term is read from `std` at compile time rather than frozen here, so two
//!      builds of the same aterm source on different toolchains can select
//!      differently. If build-to-build reproducibility is ever wanted over
//!      currency, the fix is to bake `Alphabetic` into a fourth range table
//!      beside `ND`/`MARK`/`PC`, pinned to a stated Unicode version and checked
//!      against `std` by a sibling of `fold_table_matches_std`.
//!
//! Nothing in the aterm tree uses any of the first four, and the built-in
//! patterns are the proof: they are all here, all passing.

#![forbid(unsafe_code)]

mod compile;
mod parse;
mod pikevm;
mod unicode;

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use compile::Program;
use parse::Flags;
use pikevm::Cache;

/// Default ceiling on a compiled program, in bytes. Matches the `regex` crate's
/// default so a call site that does not set one behaves the same.
const DEFAULT_SIZE_LIMIT: usize = 10 * (1 << 20);

/// Default lazy-DFA cache ceiling, in bytes. See
/// [`RegexBuilder::dfa_size_limit`], which explains why this engine has no DFA
/// for it to bound.
const DEFAULT_DFA_SIZE_LIMIT: usize = 2 * (1 << 20);

/// Default ceiling on the work one search may do, in units of
/// `positions × live threads`. See [`RegexBuilder::step_limit`] for what a unit
/// is and how this number was chosen.
const DEFAULT_STEP_LIMIT: u64 = 1 << 26;

/// A pattern that would not compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The pattern is not valid syntax, or uses a construct this engine
    /// refuses. The string is a rendered, caret-annotated message meant to be
    /// shown to whoever typed the pattern; `aterm-search` surfaces it through
    /// `SearchOptionsError::InvalidRegex`.
    ///
    /// Constructible on purpose: `aterm-observe` builds one directly to report
    /// its own pattern-length ceiling in the same shape as a compile failure.
    Syntax(String),
    /// The compiled program passed the byte ceiling set by
    /// [`RegexBuilder::size_limit`]. Carries the limit that was exceeded.
    CompiledTooBig(usize),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(msg) => f.write_str(msg),
            Self::CompiledTooBig(limit) => {
                write!(f, "Compiled regex exceeds size limit of {limit} bytes.")
            }
        }
    }
}

impl std::error::Error for Error {}

/// A search that ran out of budget before it could answer.
///
/// Distinct from "no match" **on purpose**. The two are not the same fact: a
/// completed search that found nothing has proved the haystack contains no
/// match, while an exhausted one has proved nothing at all. Collapsing them
/// would turn a refusal into a wrong answer, which is the worse of the two
/// failures — a watcher that never latches or a search that quietly loses rows
/// is a bug you cannot see. The fallible entry points
/// ([`Regex::try_is_match`], [`Regex::try_find`], [`Matches::step_limit_exceeded`])
/// hand this back so a call site can decide; the infallible ones fail closed
/// and record it on the [`Regex`] for [`Regex::step_limit_exceeded`] to read.
///
/// This is not an [`Error`]: `Error` is what a *pattern* can be wrong about and
/// is reported when compiling. Exhaustion is a property of a pattern-plus-
/// haystack pair, and only a search can discover it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepLimitExceeded {
    limit: u64,
}

impl StepLimitExceeded {
    pub(crate) fn new(limit: u64) -> Self {
        Self { limit }
    }

    /// The budget that was exhausted, in work units. See
    /// [`RegexBuilder::step_limit`].
    #[must_use]
    pub fn limit(&self) -> u64 {
        self.limit
    }
}

impl std::fmt::Display for StepLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "regex search exceeded its step budget of {} work units before it \
             could decide; the pattern is too expensive to run over this input",
            self.limit
        )
    }
}

impl std::error::Error for StepLimitExceeded {}

/// A compiled regular expression.
///
/// Cloning is cheap: the compiled program is shared, never rebuilt.
#[derive(Clone)]
pub struct Regex {
    prog: Arc<Program>,
    pattern: Arc<str>,
    /// Work ceiling for one search. See [`RegexBuilder::step_limit`].
    step_limit: u64,
    /// Sticky: set the first time any search on this program (or on a clone of
    /// it — the flag is shared, like the program) is cut short. It exists
    /// because the infallible entry points have nowhere else to put the fact,
    /// and because the call sites that consume a `Regex` through someone else's
    /// scan loop (`aterm-search`'s streaming engine matches rows in a module
    /// that never sees a `Result`) still have to be able to notice.
    cut_short: Arc<AtomicBool>,
}

impl Regex {
    /// Compile `pattern` with the default limits.
    ///
    /// # Errors
    /// [`Error::Syntax`] for a malformed or unsupported pattern,
    /// [`Error::CompiledTooBig`] if it needs more than 10 MiB of automaton.
    pub fn new(pattern: &str) -> Result<Self, Error> {
        RegexBuilder::new(pattern).build()
    }

    /// The pattern this was compiled from.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Does `text` contain a match anywhere?
    ///
    /// Cheaper than [`find`](Self::find): it may stop at the first match found
    /// rather than establishing which match is leftmost-first.
    ///
    /// **Fails closed.** A search that exhausts its
    /// [step budget](RegexBuilder::step_limit) reports `false` — the answer a
    /// caller can act on safely — and sets [`step_limit_exceeded`](Self::step_limit_exceeded).
    /// Use [`try_is_match`](Self::try_is_match) to be told instead of guessing.
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        self.try_is_match(text).unwrap_or(false)
    }

    /// [`is_match`](Self::is_match), reporting exhaustion instead of hiding it.
    ///
    /// # Errors
    /// [`StepLimitExceeded`] if the search ran out of budget before it could
    /// decide. Nothing about the haystack is implied by that: the pattern may
    /// or may not match it.
    pub fn try_is_match(&self, text: &str) -> Result<bool, StepLimitExceeded> {
        Ok(self.search(text, 0, true)?.is_some())
    }

    /// The leftmost-first match in `text`, if any.
    ///
    /// **Fails closed**, exactly as [`is_match`](Self::is_match) does: an
    /// exhausted search reports `None` and sets
    /// [`step_limit_exceeded`](Self::step_limit_exceeded).
    #[must_use]
    pub fn find<'t>(&self, text: &'t str) -> Option<Match<'t>> {
        self.try_find(text).unwrap_or(None)
    }

    /// [`find`](Self::find), reporting exhaustion instead of hiding it.
    ///
    /// # Errors
    /// [`StepLimitExceeded`] if the search ran out of budget before it could
    /// decide. No partial span is returned: a truncated search has not
    /// established which match is leftmost-first.
    pub fn try_find<'t>(&self, text: &'t str) -> Result<Option<Match<'t>>, StepLimitExceeded> {
        Ok(self
            .search(text, 0, false)?
            .map(|(start, end)| Match { text, start, end }))
    }

    /// The work ceiling one search on this pattern may spend. See
    /// [`RegexBuilder::step_limit`].
    #[must_use]
    pub fn step_limit(&self) -> u64 {
        self.step_limit
    }

    /// Has any search on this compiled pattern ever been cut short by the step
    /// budget?
    ///
    /// Sticky and shared with every clone, so a call site that scans through an
    /// infallible helper — `find_iter` inside somebody else's loop — can still
    /// tell the difference between "no matches" and "gave up". Never cleared:
    /// once a pattern has proved it can exhaust the budget on real content, the
    /// results it produced are not exhaustive and no later cheap row makes them
    /// so.
    #[must_use]
    pub fn step_limit_exceeded(&self) -> bool {
        self.cut_short.load(Ordering::Relaxed)
    }

    /// Every non-overlapping match in `text`, left to right.
    ///
    /// Zero-width matches are yielded (`\b` against `"ab cd"` yields four of
    /// them, at 0, 2, 3 and 5) and the iterator always makes progress: after an
    /// empty match it resumes at the next code point, and never reports two
    /// matches ending at the same offset in a row. Call sites that do not want
    /// empty matches filter them — `aterm-search` does exactly that.
    ///
    /// **Fails closed**: if a search along the way exhausts the
    /// [step budget](RegexBuilder::step_limit) the iterator ends there, and
    /// [`Matches::step_limit_exceeded`] says so. A caller that treats the
    /// yielded matches as the complete set without asking is reading a
    /// truncated list.
    #[must_use]
    pub fn find_iter<'r, 't>(&'r self, text: &'t str) -> Matches<'r, 't> {
        Matches {
            re: self,
            text,
            last_end: 0,
            last_match: None,
            cut_short: false,
        }
    }

    /// The leftmost-first match starting at or after byte offset `start`.
    ///
    /// Assertions still see the whole haystack: `^` matches at offset 0 of
    /// `text`, not at `start`.
    fn search(
        &self,
        text: &str,
        start: usize,
        earliest: bool,
    ) -> Result<Option<(usize, usize)>, StepLimitExceeded> {
        thread_local! {
            /// Per-thread scratch for the simulation, so a scan over ten
            /// thousand rows allocates once rather than ten thousand times.
            static CACHE: RefCell<Cache> = RefCell::new(Cache::new());
        }
        let limit = self.step_limit;
        let outcome = CACHE
            .try_with(|cell| {
                cell.try_borrow_mut().ok().map(|mut cache| {
                    pikevm::search(&self.prog, &mut cache, text, start, earliest, limit)
                })
            })
            .ok()
            .flatten()
            // A destroyed or already-borrowed thread-local is rare but not
            // impossible (a search inside a `Drop` during thread teardown), and
            // a fresh cache is always correct — only slower.
            .unwrap_or_else(|| {
                pikevm::search(&self.prog, &mut Cache::new(), text, start, earliest, limit)
            });
        if outcome.is_err() {
            // Recorded here, in the one place every entry point funnels
            // through, so no caller can lose the fact by taking a shortcut.
            self.cut_short.store(true, Ordering::Relaxed);
        }
        outcome
    }
}

impl std::fmt::Display for Regex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.pattern)
    }
}

impl std::fmt::Debug for Regex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::str::FromStr for Regex {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Self::new(s)
    }
}

/// One match: a byte range into the haystack it was found in.
///
/// [`start`](Self::start) and [`end`](Self::end) are byte offsets and always
/// land on code-point boundaries, so slicing the haystack with them is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match<'t> {
    text: &'t str,
    start: usize,
    end: usize,
}

impl<'t> Match<'t> {
    /// Byte offset of the first code point of the match.
    #[must_use]
    pub fn start(&self) -> usize {
        self.start
    }

    /// Byte offset just past the last code point of the match.
    #[must_use]
    pub fn end(&self) -> usize {
        self.end
    }

    /// The matched text.
    #[must_use]
    pub fn as_str(&self) -> &'t str {
        self.text.get(self.start..self.end).unwrap_or_default()
    }

    /// The match as a byte range.
    #[must_use]
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// Length of the match in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Is this a zero-width match?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Iterator over the non-overlapping matches in a haystack. See
/// [`Regex::find_iter`].
#[derive(Debug)]
pub struct Matches<'r, 't> {
    re: &'r Regex,
    text: &'t str,
    last_end: usize,
    last_match: Option<usize>,
    cut_short: bool,
}

impl Matches<'_, '_> {
    /// Did this iterator stop early because a search exhausted the
    /// [step budget](RegexBuilder::step_limit)?
    ///
    /// `false` means the iterator ran to the end of the haystack and the
    /// matches it yielded are every match there is. `true` means it gave up
    /// part-way: what it yielded is a prefix of the truth, not the truth.
    #[must_use]
    pub fn step_limit_exceeded(&self) -> bool {
        self.cut_short
    }
}

impl<'t> Iterator for Matches<'_, 't> {
    type Item = Match<'t>;

    fn next(&mut self) -> Option<Match<'t>> {
        loop {
            if self.last_end > self.text.len() {
                return None;
            }
            let (start, end) = match self.re.search(self.text, self.last_end, false) {
                Ok(found) => found?,
                Err(_) => {
                    // Fail closed and stay closed: fuse the iterator so a
                    // caller that keeps polling does not re-run the search that
                    // just proved too expensive, once per remaining position.
                    self.cut_short = true;
                    self.last_end = self.text.len().saturating_add(1);
                    return None;
                }
            };
            if start == end {
                // Empty match: step one code point so the iterator terminates,
                // and drop it if the previous match already ended here — which
                // is what stops `a*` on "aab" reporting an empty match at 2
                // right after the "aa" that ended there.
                self.last_end = end
                    + self
                        .text
                        .get(end..)
                        .and_then(|rest| rest.chars().next())
                        .map_or(1, char::len_utf8);
                if self.last_match == Some(end) {
                    continue;
                }
            } else {
                self.last_end = end;
            }
            self.last_match = Some(end);
            return Some(Match {
                text: self.text,
                start,
                end,
            });
        }
    }
}

impl std::iter::FusedIterator for Matches<'_, '_> {}

/// Compile a pattern with non-default limits or flags.
///
/// ```
/// use aterm_regex::RegexBuilder;
///
/// // The bounds aterm's untrusted-pattern entry points use: one on the
/// // program, one on the scan.
/// let re = RegexBuilder::new(r"\d{1,5}")
///     .size_limit(128 * 1024)
///     .dfa_size_limit(1 << 20)
///     .step_limit(1 << 22)
///     .build()?;
/// assert!(re.is_match("port 8080"));
/// assert!(!re.step_limit_exceeded(), "a real pattern never comes near the budget");
/// # Ok::<(), aterm_regex::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct RegexBuilder {
    pattern: String,
    size_limit: usize,
    dfa_size_limit: usize,
    step_limit: u64,
    flags: Flags,
}

impl RegexBuilder {
    /// Start building a `Regex` from `pattern`.
    #[must_use]
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
            size_limit: DEFAULT_SIZE_LIMIT,
            dfa_size_limit: DEFAULT_DFA_SIZE_LIMIT,
            step_limit: DEFAULT_STEP_LIMIT,
            flags: Flags::default(),
        }
    }

    /// Ceiling, in bytes, on the compiled program.
    ///
    /// Charged per instruction and re-checked on every one emitted, so a pattern
    /// that would exceed it fails after a few thousand instructions rather than
    /// allocating until it runs out of budget. The charge covers the whole cost
    /// of an instruction: the instruction itself plus the per-instruction slots
    /// it forces in the simulation's two thread lists.
    #[must_use]
    pub fn size_limit(mut self, bytes: usize) -> Self {
        self.size_limit = bytes;
        self
    }

    /// Ceiling, in bytes, on a lazy-DFA cache.
    ///
    /// **This engine has no DFA.** A Pike VM simulates the NFA directly, which
    /// is what buys the linear-time guarantee, and it holds no cache that could
    /// grow: per-search memory is already bounded by
    /// [`size_limit`](Self::size_limit) alone. The setting is accepted and
    /// recorded — [`get_dfa_size_limit`](Self::get_dfa_size_limit) reads it
    /// back — for source compatibility with the call sites that pass it, and it
    /// is what a lazy DFA would be bounded by if one were ever added. It is
    /// deliberately *not* silently repurposed as a second cap on the program:
    /// that would reject patterns the caller expected to compile.
    #[must_use]
    pub fn dfa_size_limit(mut self, bytes: usize) -> Self {
        self.dfa_size_limit = bytes;
        self
    }

    /// Read back the value passed to [`dfa_size_limit`](Self::dfa_size_limit).
    #[must_use]
    pub fn get_dfa_size_limit(&self) -> usize {
        self.dfa_size_limit
    }

    /// Ceiling on the work **one search** may do, in units of
    /// `positions × live threads`.
    ///
    /// This is the bound [`size_limit`](Self::size_limit) cannot give. A Pike
    /// VM's scan is linear in the haystack, but the constant of that linearity
    /// is the program size, and both factors come from untrusted input — so
    /// `size_limit` bounds one factor of a product and nothing bounds the
    /// product. The step budget bounds the product itself.
    ///
    /// A unit is one real piece of work: one input position visited, one byte
    /// the literal prefilter skips, one instruction entered while taking an
    /// epsilon closure, or one live thread advanced. Exhausting it ends the
    /// search with [`StepLimitExceeded`] rather than a span — see that type for
    /// why "no match" would be the wrong answer, and [`Regex::is_match`] for
    /// which entry points fail closed.
    ///
    /// The budget is spent per call, not per `Regex`: every
    /// [`find`](Regex::find), [`is_match`](Regex::is_match) and each step of a
    /// [`find_iter`](Regex::find_iter) starts from a full one. Scanning a
    /// thousand rows therefore costs at most a thousand budgets, which is what
    /// makes the ceiling a per-row cost the caller can reason about.
    ///
    /// ## Choosing the default: 2^26 = 67,108,864 units
    ///
    /// A unit is worth **1.15 ns** on the m21 box in release — measured across
    /// four programs of different shapes, which came in at 1.13, 1.15, 1.15 and
    /// 1.19 ns, so the count is a good proxy for time and not just for work.
    /// The default is therefore about 77 ms of scanning for the one search that
    /// actually exhausts it.
    ///
    /// It is pinned from below by the most expensive thing a bounded call site
    /// can legitimately be handed, and every number here is measured by
    /// `step_budget_tests`, which fails if the margins stop holding:
    ///
    /// * The heaviest program the call sites' 128 KiB `size_limit` admits at
    ///   all needs **~16.7M** units to cross one 4,096-column row
    ///   (`(?:x?){1020}z`, 19.3 ms). A pattern that compiles must be able to
    ///   run, so the default clears that with 4x to spare.
    /// * Every built-in `aterm-selection` rule is *four orders of magnitude*
    ///   below it. Over a full-width row of exactly the text each rule exists
    ///   to find, and again over 3 MB of it, the most expensive single search
    ///   any of them performs is **~3,100** units (the IPv6 rule); git hashes
    ///   cost ~260, emails ~325, URLs ~590. Nothing aterm ships can notice this
    ///   budget exists.
    ///
    /// What the default deliberately does *not* do is make one expensive row
    /// cheap: `(?:x?){2000}z` compiled under a 1 MiB ceiling costs 32.8M units
    /// (37.7 ms) on a single row, and the default lets that finish while
    /// refusing the same pattern's 3 MB scan. Call sites that take their
    /// pattern *and* their content from outside pass something tighter —
    /// `aterm-observe` and `aterm-search` pass 2^22, about 4.8 ms per row,
    /// which still clears the most expensive built-in rule by 1,000x.
    #[must_use]
    pub fn step_limit(mut self, steps: u64) -> Self {
        self.step_limit = steps;
        self
    }

    /// Read back the value passed to [`step_limit`](Self::step_limit).
    #[must_use]
    pub fn get_step_limit(&self) -> u64 {
        self.step_limit
    }

    /// Match without regard to case, as if the pattern began with `(?i)`.
    #[must_use]
    pub fn case_insensitive(mut self, yes: bool) -> Self {
        self.flags.case_insensitive = yes;
        self
    }

    /// Make `^` and `$` match at line boundaries, as if the pattern began with
    /// `(?m)`.
    #[must_use]
    pub fn multi_line(mut self, yes: bool) -> Self {
        self.flags.multi_line = yes;
        self
    }

    /// Make `.` match `\n` too, as if the pattern began with `(?s)`.
    #[must_use]
    pub fn dot_matches_new_line(mut self, yes: bool) -> Self {
        self.flags.dot_matches_new_line = yes;
        self
    }

    /// Ignore whitespace and `#` comments in the pattern, as if it began with
    /// `(?x)`.
    #[must_use]
    pub fn ignore_whitespace(mut self, yes: bool) -> Self {
        self.flags.ignore_whitespace = yes;
        self
    }

    /// Swap the greediness of every quantifier, as if the pattern began with
    /// `(?U)`.
    #[must_use]
    pub fn swap_greed(mut self, yes: bool) -> Self {
        self.flags.swap_greed = yes;
        self
    }

    /// Parse and compile.
    ///
    /// # Errors
    /// [`Error::Syntax`] for a malformed or unsupported pattern, and
    /// [`Error::CompiledTooBig`] when the automaton passes
    /// [`size_limit`](Self::size_limit).
    pub fn build(self) -> Result<Regex, Error> {
        let ast = parse::parse(&self.pattern, self.flags)?;
        let prog = compile::compile(&ast, self.size_limit)?;
        Ok(Regex {
            prog: Arc::new(prog),
            pattern: self.pattern.into(),
            step_limit: self.step_limit,
            cut_short: Arc::new(AtomicBool::new(false)),
        })
    }
}

/// Escape every character `pattern` uses as syntax, so the result matches
/// `pattern` literally.
#[must_use]
pub fn escape(pattern: &str) -> String {
    /// The characters this engine reads as syntax. `<` and `>` are pointedly
    /// absent: they are ordinary literals, and `\<` / `\>` are the word-boundary
    /// assertions, so escaping them would change what the pattern means.
    const META: &str = r"\.+*?()|[]{}^$#&-~";

    let mut out = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        // Whitespace is escaped too, so the result still means itself if it is
        // ever spliced into a pattern that turns on `(?x)`.
        if META.contains(c) || c.is_whitespace() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod step_budget_tests {
    use super::*;
    use std::time::Instant;

    /// The gate quoted in the bug report: the builder the untrusted-pattern
    /// call sites used when `(?:x?){2000}z` was measured at 42.7 ms per row and
    /// 30.25 s over a 3 MB haystack. Kept here exactly as it was so the test
    /// exercises the shape that actually hung, not a friendlier one.
    fn one_mib_gate(pattern: &str) -> Regex {
        RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
            .expect("the amplifiers compile under a 1 MiB ceiling — that is the problem")
    }

    /// The two amplifiers from the bug report, thirteen and twenty-five bytes.
    const AMPLIFIERS: [&str; 2] = ["(?:x?){2000}z", "(?:x|x|x|x|x|x|x|x){400}z"];

    /// What `aterm-observe` and `aterm-search` pass — kept in step with their
    /// own constants, and the number the amplifier test below is run at.
    const CALL_SITE_STEP_LIMIT: u64 = 1 << 22;

    /// One 4,096-column row and one 3 MB scrollback, both of them the input the
    /// amplifiers were measured against.
    fn row() -> String {
        "x".repeat(4096)
    }
    fn scrollback() -> String {
        "x".repeat(3 * 1024 * 1024)
    }

    /// Does a whole `find_iter` over `haystack` finish inside `limit` units?
    fn completes_within(pattern: &str, haystack: &str, limit: u64) -> bool {
        let re = RegexBuilder::new(pattern)
            .step_limit(limit)
            .build()
            .expect("compiles");
        let mut it = re.find_iter(haystack);
        while it.next().is_some() {}
        !it.step_limit_exceeded()
    }

    /// The smallest budget that lets a whole `find_iter` finish, to within 2%:
    /// double until it fits, then bisect. That is the number the default has to
    /// clear, and it is measured rather than reasoned about. (2% rather than
    /// exact because every bisection step rescans the haystack, and the
    /// assertions built on this compare against margins of 4x and up.)
    fn budget_needed(pattern: &str, haystack: &str) -> u64 {
        let mut hi = 1u64;
        while !completes_within(pattern, haystack, hi) {
            hi = hi.checked_mul(2).expect("a scan this expensive is the bug");
        }
        let mut lo = hi / 2;
        while lo / 64 + 1 < hi - lo {
            let mid = lo + (hi - lo) / 2;
            if completes_within(pattern, haystack, mid) {
                hi = mid
            } else {
                lo = mid
            }
        }
        hi
    }

    /// **The defect, at the budget the untrusted-pattern call sites pass.**
    /// Both amplifiers, through the exact gate they were measured on, over both
    /// haystacks — the 42.7 ms row and the 30.25 s scrollback — and every one
    /// of the four now stops immediately and *says* it stopped.
    #[test]
    fn the_amplifiers_terminate_promptly_instead_of_hanging() {
        let row = row();
        let scrollback = scrollback();
        for pattern in AMPLIFIERS {
            let re = RegexBuilder::new(pattern)
                .size_limit(1 << 20)
                .step_limit(CALL_SITE_STEP_LIMIT)
                .build()
                .expect("the amplifiers compile under a 1 MiB ceiling — that is the problem");
            for (what, haystack) in [("row", &row), ("3 MB scrollback", &scrollback)] {
                let started = Instant::now();
                let outcome = re.try_is_match(haystack);
                let elapsed = started.elapsed();
                assert_eq!(
                    outcome,
                    Err(StepLimitExceeded::new(CALL_SITE_STEP_LIMIT)),
                    "{pattern:?} on the {what} must report exhaustion"
                );
                assert!(
                    elapsed.as_secs() < 2,
                    "{pattern:?} on the {what} took {elapsed:?}; the budget is not bounding the scan"
                );
                assert!(!re.is_match(haystack), "the infallible form fails closed");
            }
        }
    }

    /// The same amplifiers under the *default* budget, which is deliberately
    /// looser: it exists to leave every pattern in the tree alone, and what it
    /// stops is the run-away scan rather than the expensive row. So the 3 MB
    /// haystack — the 30.25 s measurement, and the shape that is actually a
    /// hang — is refused, while one 4,096-column row is still answered.
    ///
    /// This is the honest boundary of the default, and the reason the call
    /// sites pass something tighter rather than inheriting it.
    #[test]
    fn the_default_budget_stops_the_runaway_scan() {
        for pattern in AMPLIFIERS {
            let re = one_mib_gate(pattern);
            let started = Instant::now();
            assert_eq!(
                re.try_is_match(&scrollback()),
                Err(StepLimitExceeded::new(DEFAULT_STEP_LIMIT)),
                "{pattern:?} over 3 MB took 30.25 s before the budget existed"
            );
            assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        }
    }

    /// Exhaustion is a **distinct outcome**, not a quiet "no match". The
    /// distinction is the whole point: the same pattern and haystack that
    /// report `Err` under a small budget report a real match under a large one,
    /// so reading `Err` as "no match" would be reading a wrong answer.
    #[test]
    fn exhaustion_never_masquerades_as_no_match() {
        let haystack = format!("{}z", "x".repeat(4096));
        let pattern = AMPLIFIERS[0];

        let starved = RegexBuilder::new(pattern)
            .step_limit(10_000)
            .build()
            .expect("compiles");
        assert_eq!(
            starved.try_find(&haystack).map(|m| m.map(|m| m.range())),
            Err(StepLimitExceeded::new(10_000)),
            "a starved search must refuse, not answer"
        );
        assert!(
            !starved.is_match(&haystack),
            "the infallible form fails closed"
        );
        assert!(
            starved.step_limit_exceeded(),
            "and records that it did, so failing closed is not the same as being silent"
        );

        let fed = RegexBuilder::new(pattern)
            .step_limit(u64::MAX)
            .build()
            .expect("compiles");
        assert_eq!(
            fed.try_find(&haystack)
                .expect("no exhaustion")
                .map(|m| m.range()),
            // 2,000 optional `x`s and then the `z`: the leftmost start that can
            // reach the end of a 4,096-`x` run is 4,097 - 2,001.
            Some(2096..4097),
            "the match the starved search refused to guess at is a real one"
        );
        assert!(!fed.step_limit_exceeded());
    }

    /// A truncated `find_iter` says it is truncated, and stays truncated: the
    /// iterator does not re-run the search that just proved too expensive at
    /// every remaining position.
    #[test]
    fn find_iter_reports_truncation_and_fuses() {
        // Cheap prefix, then a run that costs more than the budget allows.
        let haystack = format!("ab ab ab {}", "x".repeat(4096));
        let re = RegexBuilder::new("ab|(?:x?){2000}z")
            .size_limit(1 << 20)
            .step_limit(50_000)
            .build()
            .expect("compiles");
        let mut it = re.find_iter(&haystack);
        let found: Vec<_> = it.by_ref().map(|m| m.range()).collect();
        assert_eq!(
            found,
            vec![0..2, 3..5, 6..8],
            "the cheap matches are still reported"
        );
        assert!(
            it.step_limit_exceeded(),
            "and the truncation is on the record"
        );
        let started = Instant::now();
        for _ in 0..1_000 {
            assert!(it.next().is_none(), "a truncated iterator is fused");
        }
        assert!(
            started.elapsed().as_millis() < 500,
            "polling a fused iterator is free"
        );
        assert!(
            re.step_limit_exceeded(),
            "the regex carries the fact for infallible callers"
        );
    }

    /// The default budget is chosen to be invisible to everything aterm runs.
    /// This is the evidence for the number in [`RegexBuilder::step_limit`]'s
    /// docs: every built-in selection rule, over a full-width row of the text
    /// it is designed to match and over a 3 MB scrollback of it, needs a tiny
    /// fraction of the default.
    #[test]
    fn every_builtin_selection_rule_stays_far_inside_the_default() {
        // `aterm-selection`'s builtin_patterns.rs, verbatim, each paired with a
        // row of the content it exists to find (the expensive case for a rule
        // is text that keeps its threads alive, not text it rejects at once).
        let rules: [(&str, &str); 8] = [
            (
                r"(?:/(?:[a-zA-Z0-9._-]+/)*[a-zA-Z0-9._-]+|\.{1,2}/(?:[a-zA-Z0-9._-]+/)*[a-zA-Z0-9._-]+|[A-Za-z]:[/\\](?:[a-zA-Z0-9._-]+[/\\])*[a-zA-Z0-9._-]+)",
                "/usr/local/share/aterm/crates/aterm-regex/src/pikevm.rs ",
            ),
            (
                r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?)*\.[a-zA-Z]{2,}",
                "trex.m21@proton.me and someone.else@example.co.uk ",
            ),
            (
                r"(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)(?::\d{1,5})?",
                "192.168.001.254:8080 10.0.0.1 255.255.255.255 ",
            ),
            (
                r"\[?(?:(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|(?:[0-9a-fA-F]{1,4}:){1,7}:|(?:[0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|::(?:[0-9a-fA-F]{1,4}:){0,5}[0-9a-fA-F]{1,4}|::)\]?(?::\d{1,5})?",
                "[2001:0db8:85a3:0000:0000:8a2e:0370:7334]:443 fe80::1 ",
            ),
            (
                r"\b[0-9a-fA-F]{7,40}\b",
                "commit 66390b5c8f2a1b3c4d5e6f7a8b9c0d1e2f3a4b5c landed ",
            ),
            (
                r"'(?:[^'\\]|\\.)*'",
                "echo 'a quoted \\'string\\' here' | cat ",
            ),
            (
                r"`(?:[^`\\]|\\.)*`",
                "run `git log --oneline` and `cargo test` ",
            ),
            (
                r"v?\d+\.\d+\.\d+(?:-[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?(?:\+[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?",
                "aterm v0.47.0-rc.1+build.1787445038 released ",
            ),
        ];

        for (pattern, sample) in rules {
            // A full 4,096-column row of exactly the content the rule exists to
            // find — the expensive case for a rule is text that keeps its
            // threads alive, not text it rejects at the first code point.
            let row: String = sample.repeat(4096 / sample.len() + 1);
            let per_row = budget_needed(pattern, &row);
            println!("{per_row:>12} units/search  {pattern}");
            assert!(
                per_row <= 4_096,
                "{pattern:?} needs {per_row} units for one search over a full row; the docs \
                 claim every built-in rule is four orders of magnitude under the default \
                 budget of {DEFAULT_STEP_LIMIT}"
            );
            // And under the default the rule simply works, over a full row and
            // over a 1 MB scrollback of the same, with nothing cut off.
            let scrollback: String = sample.repeat(1024 * 1024 / sample.len() + 1);
            let re = Regex::new(pattern).expect("compiles");
            for haystack in [&row, &scrollback] {
                let mut it = re.find_iter(haystack);
                let count = it.by_ref().count();
                assert!(
                    count > 0 && !it.step_limit_exceeded(),
                    "{pattern:?} matched nothing"
                );
            }
            assert!(!re.step_limit_exceeded());
        }
    }

    /// The heaviest program the call sites' own 128 KiB `size_limit` admits,
    /// over a full-width row — the worst *legitimate* case a bounded call site
    /// can be handed, and the other end of the margin the default has to leave.
    #[test]
    fn the_worst_admitted_program_still_fits_the_default_budget() {
        for pattern in ["(?:x?){1020}z", "(?:x|x|x|x|x|x|x|x){100}z"] {
            let Ok(_) = RegexBuilder::new(pattern).size_limit(128 * 1024).build() else {
                continue;
            };
            let needed = budget_needed(pattern, &row());
            println!("{needed:>12} row  {pattern}  (heaviest admitted)");
            assert!(
                needed <= DEFAULT_STEP_LIMIT,
                "{pattern:?} needs {needed} units on one row, over the default budget of \
                 {DEFAULT_STEP_LIMIT}: a pattern the 128 KiB ceiling admits must still run"
            );
        }
    }

    /// The knob is real in both directions, and readable back.
    #[test]
    fn the_step_limit_is_the_thing_that_decides() {
        let pattern = AMPLIFIERS[0];
        let row = row();
        assert!(
            RegexBuilder::new(pattern)
                .size_limit(1 << 20)
                .step_limit(u64::MAX)
                .build()
                .expect("compiles")
                .try_is_match(&row)
                .is_ok(),
            "an unbounded budget still answers — slowly, which is the caller's choice"
        );
        assert!(
            RegexBuilder::new(pattern)
                .size_limit(1 << 20)
                .step_limit(1_000)
                .build()
                .expect("compiles")
                .try_is_match(&row)
                .is_err(),
            "a small one refuses"
        );
        assert_eq!(RegexBuilder::new("a").step_limit(7).get_step_limit(), 7);
        assert_eq!(RegexBuilder::new("a").get_step_limit(), DEFAULT_STEP_LIMIT);
        assert_eq!(
            Regex::new("a").expect("compiles").step_limit(),
            DEFAULT_STEP_LIMIT
        );
    }

    /// Ordinary patterns over ordinary rows are untouched: same matches, no
    /// exhaustion, whatever the budget is set to.
    #[test]
    fn ordinary_patterns_are_unaffected() {
        let text = "commit 66390b5c8f is the release; see http://example.com/x?y=1 (v1.2.3)";
        for pattern in [
            r"\b[0-9a-f]{7,40}\b",
            r"\w+",
            "a|ab",
            "(?i)COMMIT",
            r"\d+\.\d+\.\d+",
        ] {
            let unbounded = RegexBuilder::new(pattern)
                .step_limit(u64::MAX)
                .build()
                .expect("ok");
            let defaulted = Regex::new(pattern).expect("compiles");
            let tight = RegexBuilder::new(pattern)
                .step_limit(1 << 16)
                .build()
                .expect("ok");
            let spans = |re: &Regex| -> Vec<std::ops::Range<usize>> {
                re.find_iter(text).map(|m| m.range()).collect()
            };
            assert_eq!(spans(&defaulted), spans(&unbounded), "{pattern:?}");
            assert_eq!(spans(&tight), spans(&unbounded), "{pattern:?}");
            assert!(!defaulted.step_limit_exceeded() && !tight.step_limit_exceeded());
        }
    }

    /// The sticky flag is shared by clones, because the program is: a call site
    /// that clones a compiled rule into a watcher must not lose the fact that
    /// the rule has already proved too expensive.
    #[test]
    fn the_cut_short_flag_is_shared_with_clones() {
        let re = RegexBuilder::new(AMPLIFIERS[0])
            .size_limit(1 << 20)
            .step_limit(1_000)
            .build()
            .expect("compiles");
        let clone = re.clone();
        assert!(!clone.step_limit_exceeded());
        assert!(!re.is_match(&row()));
        assert!(
            clone.step_limit_exceeded(),
            "the clone sees what the original learned"
        );
    }
}

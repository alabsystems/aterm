// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's first-party regular expressions, used by `aterm-observe`, `aterm-search` and
//! `aterm-selection` in place of the `regex` crate.
//!
//! Parse → NFA → Pike VM: the pattern becomes a Thompson automaton and the search advances
//! every live state together, one code point at a time, so match time is linear in the
//! haystack (`(a|a)*b` against ten thousand `a`s takes milliseconds, not a backtracking
//! hang). As in `regex`, there are no backreferences or look-around, and matching is
//! leftmost-first. Every `aterm-selection` built-in pattern is a test here.
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
//! ## Bounds
//!
//! Patterns here are user input: selection rules, `await match <re>` and the search
//! options. Nesting is capped at 250 levels, so `((((…))))` errors instead of overflowing
//! the stack. A scan costs `O(|haystack| × program)` and the caller chooses both factors:
//! under a 1 MiB `size_limit` with no step budget, `(?:x?){2000}z` took 43 ms per
//! 4,096-column row and 30 s over 3 MB, where `regex` takes 7 µs and 40 µs (m21,
//! release). Two bounds cover the two factors:
//!
//! * [`RegexBuilder::size_limit`] bounds the program, checked on every instruction
//!   emitted, so `(a{200}){200}` fails fast instead of allocating. The call sites pass
//!   128 KiB (~2,048 instructions), which refuses `(?:x?){2000}z`.
//! * [`RegexBuilder::step_limit`] bounds the scan. Exhausting it yields
//!   [`StepLimitExceeded`], never a quiet "no match": a wrong answer is worse than a
//!   refusal.
//!
//! ## Cost
//!
//! `regex` scans faster (a lazy DFA behind SIMD prefilters); this crate compiles faster,
//! having no DFA to build. Over ~3 MB of terminal rows (m21, release) a full `find_iter`
//! is 5–20x slower (the git-hash rule: 21 ms against 2.4 ms), while compiling the IPv4
//! rule takes 3.2 µs against 150 µs. That suits `aterm-search`, which recompiles on every
//! keystroke and scans in budgeted batches (`BudgetedSearch`). The scan's fast paths: a
//! first-code-point prefilter that sees through a leading assertion (`\bcommit`,
//! `^ERROR`), a second-code-point prefilter, start anchoring for programs that can only
//! match at offset 0, and a `u128` ASCII bitmap per class.
//!
//! ## Divergences from the `regex` crate
//!
//! Each is a refusal, an [`Error`] naming the construct, never a reinterpretation.
//! `tests/differential.rs` pins everything else against answers recorded from `regex`.
//!
//! * `\p{…}` / `\pL` Unicode property classes.
//! * Character-class set operations (`[a&&b]`, `[a--b]`, `[a~~b]`) and the nested classes
//!   that go with them. Escape the operator to match it literally.
//! * `(?-u)` byte-oriented mode: this engine matches `&str` by code point.
//! * `(?R)` CRLF mode.
//!
//! Nothing in the aterm tree uses these. One difference is not a refusal: `\w`, and so
//! `\b`, use `char::is_alphabetic`, which follows the toolchain's Unicode version, newer
//! than the tables `regex` bundles. `^\w$` matched 4,662 code points `regex` rejects, and
//! none the other way; `\d` and `\s` agree exactly. It reaches the `git_hash` rule (on
//! `"deadbeef\u{088F}"` `regex` selects `deadbeef` and this engine nothing) and makes the
//! answer depend on the compiler; baking `Alphabetic` into a range table beside
//! `ND`/`MARK`/`PC` would pin it.

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

/// Default ceiling on the work one search may do; see [`RegexBuilder::step_limit`].
const DEFAULT_STEP_LIMIT: u64 = 1 << 26;

/// A pattern that would not compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Invalid or refused syntax, as a caret-annotated message for whoever typed the
    /// pattern (`aterm-search` surfaces it as `SearchOptionsError::InvalidRegex`). Public
    /// so `aterm-observe` can report its own pattern-length ceiling in the same shape.
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

/// A search that ran out of budget before it could answer.
///
/// Distinct from "no match": a completed search that found nothing proves there is no
/// match, an exhausted one proves nothing. [`Regex::try_is_match`] and [`Regex::try_find`]
/// return it; the infallible entry points fail closed and record it for
/// [`Regex::step_limit_exceeded`]. Not an [`Error`]: an `Error` is about the pattern and
/// found at compile time, while exhaustion belongs to a pattern-haystack pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepLimitExceeded {
    limit: u64,
}

impl StepLimitExceeded {
    #[inline]
    pub(crate) const fn new(limit: u64) -> Self {
        Self { limit }
    }
}

impl std::fmt::Display for StepLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "regex search exceeded its step budget of {} units",
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
    /// and because a call site that consumes a `Regex` through someone else's
    /// scan loop, which never sees a `Result`, still has to be able to notice.
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
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Does `text` contain a match anywhere?
    ///
    /// Cheaper than [`find`](Self::find): it may stop at the first match found. Fails
    /// closed: a search that exhausts its [step budget](RegexBuilder::step_limit) reports
    /// `false` and sets [`step_limit_exceeded`](Self::step_limit_exceeded);
    /// [`try_is_match`](Self::try_is_match) reports the exhaustion instead.
    #[inline]
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        self.try_is_match(text).unwrap_or(false)
    }

    /// [`is_match`](Self::is_match), reporting exhaustion instead of hiding it.
    ///
    /// # Errors
    /// [`StepLimitExceeded`] if the search ran out of budget before it could
    /// decide.
    #[inline]
    pub fn try_is_match(&self, text: &str) -> Result<bool, StepLimitExceeded> {
        let mut budget = self.step_limit;
        Ok(self.search(text, 0, true, &mut budget)?.is_some())
    }

    /// The leftmost-first match in `text`, if any.
    ///
    /// Fails closed like [`is_match`](Self::is_match): an exhausted search reports `None`
    /// and sets [`step_limit_exceeded`](Self::step_limit_exceeded).
    #[inline]
    #[must_use]
    pub fn find<'t>(&self, text: &'t str) -> Option<Match<'t>> {
        self.try_find(text).unwrap_or(None)
    }

    /// [`find`](Self::find), reporting exhaustion instead of hiding it.
    ///
    /// # Errors
    /// [`StepLimitExceeded`] if the search ran out of budget before it could
    /// decide; no partial span is returned.
    #[inline]
    pub fn try_find<'t>(&self, text: &'t str) -> Result<Option<Match<'t>>, StepLimitExceeded> {
        let mut budget = self.step_limit;
        Ok(self
            .search(text, 0, false, &mut budget)?
            .map(|(start, end)| Match { text, start, end }))
    }

    /// The work ceiling one search on this pattern may spend. See
    /// [`RegexBuilder::step_limit`].
    #[inline]
    #[must_use]
    pub const fn step_limit(&self) -> u64 {
        self.step_limit
    }

    /// Has any search on this pattern been cut short by the step budget?
    ///
    /// Sticky and shared with every clone, so a caller scanning through an infallible
    /// helper can tell "no matches" from "gave up". Never cleared: the results the
    /// pattern produced are not exhaustive, whatever later rows cost.
    #[inline]
    #[must_use]
    pub fn step_limit_exceeded(&self) -> bool {
        self.cut_short.load(Ordering::Relaxed)
    }

    /// Every non-overlapping match in `text`, left to right.
    ///
    /// Zero-width matches are yielded (`\b` on `"ab cd"` yields 0, 2, 3 and 5). After an
    /// empty match the iterator resumes at the next code point, and it never reports two
    /// matches ending at the same offset in a row; `aterm-search` filters empty matches.
    ///
    /// The whole iteration shares one [step budget](RegexBuilder::step_limit). It fails
    /// closed: on exhaustion the iterator ends, the yielded matches are a prefix, and
    /// [`Matches::step_limit_exceeded`] and [`Regex::step_limit_exceeded`] record it.
    #[inline]
    #[must_use]
    pub const fn find_iter<'r, 't>(&'r self, text: &'t str) -> Matches<'r, 't> {
        self.find_iter_with_limit(text, self.step_limit)
    }

    /// [`find_iter`](Self::find_iter), held to `step_limit` units instead of the pattern's
    /// own [`RegexBuilder::step_limit`].
    ///
    /// For a caller whose fair budget depends on the haystack: `aterm-search` matches a
    /// soft-wrapped run as one line of any length and scales the budget with it. Fails
    /// closed like `find_iter`: [`Matches::step_limit_exceeded`] says whether it reached
    /// the end, and the pattern records an exhausted search as
    /// [`step_limit_exceeded`](Self::step_limit_exceeded).
    #[inline]
    #[must_use]
    pub const fn find_iter_with_limit<'r, 't>(
        &'r self,
        text: &'t str,
        step_limit: u64,
    ) -> Matches<'r, 't> {
        Matches {
            re: self,
            text,
            last_end: 0,
            last_match: None,
            budget: step_limit,
            cut_short: false,
        }
    }

    /// The leftmost-first match starting at or after byte offset `start`,
    /// charged against `budget` and leaving it holding what remains.
    ///
    /// Assertions still see the whole haystack: `^` matches at offset 0 of
    /// `text`, not at `start`.
    fn search(
        &self,
        text: &str,
        start: usize,
        earliest: bool,
        budget: &mut u64,
    ) -> Result<Option<(usize, usize)>, StepLimitExceeded> {
        thread_local! {
            /// Per-thread scratch, so a scan over many rows allocates once.
            static CACHE: RefCell<Cache> = RefCell::new(Cache::new());
        }
        let outcome = CACHE
            .try_with(|cell| {
                cell.try_borrow_mut().ok().map(|mut cache| {
                    pikevm::search(&self.prog, &mut cache, text, start, earliest, budget)
                })
            })
            .ok()
            .flatten()
            // A destroyed or already-borrowed thread-local is rare but not
            // impossible (a search inside a `Drop` during thread teardown), and
            // a fresh cache is always correct — only slower.
            .unwrap_or_else(|| {
                pikevm::search(&self.prog, &mut Cache::new(), text, start, earliest, budget)
            });
        if outcome.is_err() {
            // Every entry point funnels through here, so none can lose the fact.
            self.cut_short.store(true, Ordering::Relaxed);
        }
        outcome.map_err(|_| StepLimitExceeded::new(self.step_limit))
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
    #[inline]
    #[must_use]
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Byte offset just past the last code point of the match.
    #[inline]
    #[must_use]
    pub const fn end(&self) -> usize {
        self.end
    }

    /// The matched text.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &'t str {
        self.text.get(self.start..self.end).unwrap_or_default()
    }

    /// The match as a byte range.
    #[inline]
    #[must_use]
    pub const fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// Length of the match in bytes.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    /// Is this a zero-width match?
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
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
    /// What is left of the one step budget the whole iteration shares.
    budget: u64,
    cut_short: bool,
}

impl Matches<'_, '_> {
    /// Did this iterator stop early because a search exhausted the
    /// [step budget](RegexBuilder::step_limit)?
    ///
    /// `false` means it yielded every match in the haystack; `true` means what it yielded
    /// is a prefix. Unlike the sticky [`Regex::step_limit_exceeded`], this is per
    /// haystack, so a caller scanning many lines learns which ones it could not finish.
    #[inline]
    #[must_use]
    pub const fn step_limit_exceeded(&self) -> bool {
        self.cut_short
    }
}

impl<'t> Iterator for Matches<'_, 't> {
    type Item = Match<'t>;

    #[inline]
    fn next(&mut self) -> Option<Match<'t>> {
        loop {
            if self.last_end > self.text.len() {
                return None;
            }
            let searched = self
                .re
                .search(self.text, self.last_end, false, &mut self.budget);
            let (start, end) = match searched {
                Ok(found) => found?,
                Err(_) => {
                    // Fuse, so further polls do not re-run the search that just ran
                    // out of budget at every remaining position.
                    self.cut_short = true;
                    self.last_end = self.text.len().saturating_add(1);
                    return None;
                }
            };
            if start == end {
                // Empty match: step one code point so the iterator terminates, and
                // drop it if the previous match ended here, so `a*` on "aab" reports
                // no empty match at 2 right after "aa".
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

impl std::error::Error for Error {}

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
            step_limit: DEFAULT_STEP_LIMIT,
            flags: Flags::default(),
        }
    }

    /// Ceiling, in bytes, on the compiled program.
    ///
    /// Checked on every instruction emitted, so an oversized pattern fails early instead
    /// of allocating. Each instruction is charged with the slots it forces in the
    /// simulation's two thread lists.
    #[inline]
    #[must_use]
    pub const fn size_limit(mut self, bytes: usize) -> Self {
        self.size_limit = bytes;
        self
    }

    /// Ceiling, in bytes, on a lazy-DFA cache: accepted for source compatibility and
    /// ignored. This engine has no DFA, and per-search memory is already bounded by
    /// [`size_limit`](Self::size_limit). It is not repurposed as a second program cap,
    /// which would reject patterns the caller expects to compile.
    #[inline]
    #[must_use]
    pub const fn dfa_size_limit(self, _bytes: usize) -> Self {
        self
    }

    /// Ceiling on the work one search may do, in units of `positions × live threads`.
    ///
    /// [`size_limit`](Self::size_limit) bounds the program, but a scan costs program ×
    /// haystack and both come from untrusted input; this bounds the product. A unit is one
    /// input position visited, one byte the prefilter skips, one instruction entered in an
    /// epsilon closure, or one live thread advanced. Exhausting it ends the search with
    /// [`StepLimitExceeded`] rather than a span; [`Regex::is_match`] says which entry
    /// points fail closed.
    ///
    /// Each [`find`](Regex::find), [`is_match`](Regex::is_match) and
    /// [`find_iter`](Regex::find_iter) call starts from a full budget, and the searches a
    /// `find_iter` runs, one per match, share it. Leftmost-first iteration can re-walk the
    /// rest of the haystack per match: `\w+=|\w` over `n` word characters has each
    /// one-character match in hand at once but must walk the higher-priority `\w+=` thread
    /// to the end of the run first, `n²/2` positions in all with no single search over
    /// `n`. A budget per search would bound nothing.
    ///
    /// The default, 2^26 units, is about 77 ms of scanning (a unit is ~1.15 ns on the m21
    /// box, release, across programs of different shapes). It sits above what anything that
    /// compiles needs: the heaviest program the call sites' 128 KiB `size_limit` admits,
    /// `(?:x?){1020}z`, needs ~16.7M units per 4,096-column row, and every built-in
    /// `aterm-selection` rule at most ~252,000 (IPv6) to find every match in a full row;
    /// `step_budget_tests` checks both. It does not make one expensive row cheap:
    /// `(?:x?){2000}z` under a 1 MiB ceiling finishes a row (32.8M units) and is refused
    /// over 3 MB. Call sites that take both pattern and content from outside
    /// (`aterm-observe`, `aterm-search`) pass 2^22, about 4.8 ms per row.
    #[inline]
    #[must_use]
    pub const fn step_limit(mut self, steps: u64) -> Self {
        self.step_limit = steps;
        self
    }

    /// Read back the value passed to [`step_limit`](Self::step_limit).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn get_step_limit(&self) -> u64 {
        self.step_limit
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

impl StepLimitExceeded {
    /// Return the step budget (in work units) that was exceeded.
    #[inline]
    #[must_use]
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Return `true` iff `used <= self.limit`.
    #[inline]
    #[must_use]
    pub const fn within_budget(self, used: u64) -> bool {
        used <= self.limit
    }

    /// Remaining step budget after spending `used` units, saturating at `0`.
    #[inline]
    #[must_use]
    pub const fn remaining_budget(self, used: u64) -> u64 {
        self.limit.saturating_sub(used)
    }
}

/// Return the default compiled program size ceiling (`10 MiB`).
#[inline]
#[must_use]
pub const fn default_size_limit() -> usize {
    DEFAULT_SIZE_LIMIT
}

/// Return the default per-search step budget (`2^26` work units).
#[inline]
#[must_use]
pub const fn default_step_limit() -> u64 {
    DEFAULT_STEP_LIMIT
}

/// Clamp a requested program `size_limit` into `[min_bytes, DEFAULT_SIZE_LIMIT]`.
#[inline]
#[must_use]
pub const fn clamp_size_limit(bytes: usize, min_bytes: usize) -> usize {
    let hi = if bytes > DEFAULT_SIZE_LIMIT {
        DEFAULT_SIZE_LIMIT
    } else {
        bytes
    };
    if hi < min_bytes && min_bytes <= DEFAULT_SIZE_LIMIT {
        min_bytes
    } else {
        hi
    }
}

/// Clamp a requested search `step_limit` into `[min_steps, DEFAULT_STEP_LIMIT]`.
#[inline]
#[must_use]
pub const fn clamp_step_limit(steps: u64, min_steps: u64) -> u64 {
    let hi = if steps > DEFAULT_STEP_LIMIT {
        DEFAULT_STEP_LIMIT
    } else {
        steps
    };
    if hi < min_steps && min_steps <= DEFAULT_STEP_LIMIT {
        min_steps
    } else {
        hi
    }
}

/// Debit `cost` work units from `remaining`, returning `Some(next_remaining)`
/// when `cost <= remaining` or `None` when the step budget is exhausted.
#[inline]
#[must_use]
pub const fn debit_step_budget(remaining: u64, cost: u64) -> Option<u64> {
    remaining.checked_sub(cost)
}

/// Return `true` iff `[start, end)` is a well-ordered span within `haystack_len`.
#[inline]
#[must_use]
pub const fn valid_match_span(start: usize, end: usize, haystack_len: usize) -> bool {
    start <= end && end <= haystack_len
}

/// Length of a validated match span `[start, end)`, or `0` if inverted.
#[inline]
#[must_use]
pub const fn match_span_len(start: usize, end: usize) -> usize {
    end.saturating_sub(start)
}

/// Return `true` iff two match spans `[a_start, a_end)` and `[b_start, b_end)`
/// are non-overlapping and ordered (`a_start <= a_end <= b_start <= b_end`).
#[inline]
#[must_use]
pub const fn disjoint_ordered_spans(
    a_start: usize,
    a_end: usize,
    b_start: usize,
    b_end: usize,
) -> bool {
    a_start <= a_end && a_end <= b_start && b_start <= b_end
}

/// Return `true` iff ASCII byte `b` is a regex syntax metacharacter escaped by [`escape`].
#[inline]
#[must_use]
pub const fn is_ascii_meta_byte(b: u8) -> bool {
    matches!(
        b,
        b'\\'
            | b'.'
            | b'+'
            | b'*'
            | b'?'
            | b'('
            | b')'
            | b'|'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'^'
            | b'$'
            | b'#'
            | b'&'
            | b'-'
            | b'~'
    )
}

/// Encode a `(start, end)` byte-offset match span into 16 little-endian bytes.
#[inline]
#[must_use]
pub const fn encode_match_span_le(start: u64, end: u64) -> [u8; 16] {
    [
        (start & 0xFF) as u8,
        ((start >> 8) & 0xFF) as u8,
        ((start >> 16) & 0xFF) as u8,
        ((start >> 24) & 0xFF) as u8,
        ((start >> 32) & 0xFF) as u8,
        ((start >> 40) & 0xFF) as u8,
        ((start >> 48) & 0xFF) as u8,
        ((start >> 56) & 0xFF) as u8,
        (end & 0xFF) as u8,
        ((end >> 8) & 0xFF) as u8,
        ((end >> 16) & 0xFF) as u8,
        ((end >> 24) & 0xFF) as u8,
        ((end >> 32) & 0xFF) as u8,
        ((end >> 40) & 0xFF) as u8,
        ((end >> 48) & 0xFF) as u8,
        ((end >> 56) & 0xFF) as u8,
    ]
}

/// Decode 16 little-endian bytes into a `(start, end)` byte-offset match span.
#[inline]
#[must_use]
pub const fn decode_match_span_le(bytes: [u8; 16]) -> (u64, u64) {
    let start = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let end = (bytes[8] as u64)
        | ((bytes[9] as u64) << 8)
        | ((bytes[10] as u64) << 16)
        | ((bytes[11] as u64) << 24)
        | ((bytes[12] as u64) << 32)
        | ((bytes[13] as u64) << 40)
        | ((bytes[14] as u64) << 48)
        | ((bytes[15] as u64) << 56);
    (start, end)
}

#[cfg(test)]
mod step_budget_tests {
    use super::*;

    /// `pattern` under a 1 MiB `size_limit` and the default step budget.
    fn one_mib_gate(pattern: &str) -> Regex {
        RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
            .expect("the amplifiers compile under a 1 MiB ceiling — that is the problem")
    }

    /// Thirteen- and twenty-five-byte patterns that compile to thousands of instructions.
    const AMPLIFIERS: [&str; 2] = ["(?:x?){2000}z", "(?:x|x|x|x|x|x|x|x){400}z"];

    /// The step limit `aterm-observe` and `aterm-search` pass; keep it equal to theirs.
    const CALL_SITE_STEP_LIMIT: u64 = 1 << 22;

    /// One 4,096-column row and a 3 MB scrollback.
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

    /// The smallest budget that lets a whole `find_iter` finish, to within 2%: double,
    /// then bisect. Not exact because each step rescans the haystack, and the assertions
    /// compare against margins of 4x and up.
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

    /// At the call sites' budget both amplifiers stop at once, over a row and over 3 MB,
    /// and report exhaustion.
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
                assert_eq!(
                    re.try_is_match(haystack),
                    Err(StepLimitExceeded::new(CALL_SITE_STEP_LIMIT)),
                    "{pattern:?} on the {what} must report exhaustion"
                );
                assert!(!re.is_match(haystack), "the infallible form fails closed");
            }
        }
    }

    /// The default budget refuses each amplifier's 3 MB scan. It still answers one row,
    /// which is why the call sites pass a tighter budget.
    #[test]
    fn the_default_budget_stops_the_runaway_scan() {
        let scrollback = scrollback();
        for pattern in AMPLIFIERS {
            let re = one_mib_gate(pattern);
            assert_eq!(
                re.try_is_match(&scrollback),
                Err(StepLimitExceeded::new(DEFAULT_STEP_LIMIT)),
                "{pattern:?} over 3 MB took 30.25 s before the budget existed"
            );
        }
    }

    /// Exhaustion is not "no match": a starved search refuses where a fed one finds a real
    /// match.
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

    /// A truncated `find_iter` reports it and stays fused.
    #[test]
    fn find_iter_reports_truncation_and_fuses() {
        // Three cheap matches (~16,000 units each: every start thread walks the
        // amplifier's ~4,000 epsilon instructions), then a run that costs ~16M.
        let haystack = format!("ab ab ab {}", "x".repeat(4096));
        let re = RegexBuilder::new("ab|(?:x?){2000}z")
            .size_limit(1 << 20)
            .step_limit(200_000)
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
        for _ in 0..1_000 {
            assert!(it.next().is_none(), "a truncated iterator is fused");
        }
        assert!(
            re.step_limit_exceeded(),
            "the regex carries the fact for infallible callers"
        );
    }

    /// A `find_iter` is held to one budget, not one per match: `\w+=|\w` over a row of `n`
    /// word characters walks `n²/2` positions in all, though no single search walks more
    /// than `n`.
    #[test]
    fn a_find_iter_is_held_to_one_budget_not_one_per_match() {
        let row = row();
        let n = row.len();
        let re = RegexBuilder::new(r"\w+=|\w")
            .size_limit(128 * 1024)
            .step_limit(CALL_SITE_STEP_LIMIT)
            .build()
            .expect("compiles");

        // One search walks the whole row, and is cheap.
        let one_search = budget_needed_for_one_search(r"\w+=|\w", &row);
        assert!(
            one_search.saturating_mul(16) < CALL_SITE_STEP_LIMIT,
            "one search over the row needs {one_search} units; the budget is {CALL_SITE_STEP_LIMIT}"
        );
        assert_eq!(
            re.try_find(&row).map(|m| m.map(|m| m.range())),
            Ok(Some(0..1))
        );

        // The iteration runs `n` such searches over a shrinking suffix: about
        // `n / 2` searches' worth of walking, which no per-row budget covers.
        let mut it = re.find_iter(&row);
        let found = it.by_ref().count();
        assert!(
            it.step_limit_exceeded(),
            "{found} of {n} matches yielded and no exhaustion reported: the \
             iteration walked ~{} units against a budget of {CALL_SITE_STEP_LIMIT}",
            u64::try_from(n / 2)
                .unwrap_or(u64::MAX)
                .saturating_mul(one_search)
        );
        assert!(found < n, "a truncated iteration yields a prefix");
        assert!(
            re.step_limit_exceeded(),
            "and records it for infallible callers"
        );

        // A linear iteration under the same budget yields all `n` matches.
        let linear = RegexBuilder::new(r"\w")
            .size_limit(128 * 1024)
            .step_limit(CALL_SITE_STEP_LIMIT)
            .build()
            .expect("compiles");
        let mut it = linear.find_iter(&row);
        assert_eq!(it.by_ref().count(), n);
        assert!(!it.step_limit_exceeded());
    }

    /// The smallest budget one `try_find` over `haystack` needs, to within 2%.
    fn budget_needed_for_one_search(pattern: &str, haystack: &str) -> u64 {
        let fits = |limit: u64| {
            RegexBuilder::new(pattern)
                .step_limit(limit)
                .build()
                .expect("compiles")
                .try_find(haystack)
                .is_ok()
        };
        let mut hi = 1u64;
        while !fits(hi) {
            hi = hi
                .checked_mul(2)
                .expect("a search this expensive is the bug");
        }
        let mut lo = hi / 2;
        while lo / 64 + 1 < hi - lo {
            let mid = lo + (hi - lo) / 2;
            if fits(mid) { hi = mid } else { lo = mid }
        }
        hi
    }

    /// Every built-in selection rule finds every match in a full row within an eighth of
    /// the call sites' budget, and completes under the default: the evidence for the
    /// numbers in [`RegexBuilder::step_limit`].
    #[test]
    fn every_builtin_selection_rule_stays_far_inside_the_default() {
        // `aterm-selection`'s built-in patterns, verbatim, each with a sample of what it
        // finds: text that keeps a rule's threads alive is its expensive case.
        let rules: [(&str, &str); 11] = [
            (
                r#"(?i)(?:https?|ftp|file)://[^\s<>\[\](){}'"`,;|\\^]+[^\s<>\[\](){}'"`,;|\\^.!?:]"#,
                "see https://example.com/path/to/page?q=1&r=2 and ftp://host/file ",
            ),
            (
                r"(?:~|\.{1,2}|[a-zA-Z0-9._-]+)?(?:/[a-zA-Z0-9._-]+)+/?|[A-Za-z]:[/\\](?:[a-zA-Z0-9._-]+[/\\])*[a-zA-Z0-9._-]+",
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
                r#""(?:[^"\\]|\\.)*""#,
                "echo \"a quoted \\\"string\\\" here\" | cat ",
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
                r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
                "id 123e4567-e89b-12d3-a456-426614174000 done ",
            ),
            (
                r"v?\d+\.\d+\.\d+(?:-[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?(?:\+[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?",
                "aterm v0.47.0-rc.1+build.1787445038 released ",
            ),
        ];

        for (pattern, sample) in rules {
            let row: String = sample.repeat(4096 / sample.len() + 1);
            let per_row = budget_needed(pattern, &row);
            println!("{per_row:>12} units/row  {pattern}");
            assert!(
                per_row <= CALL_SITE_STEP_LIMIT / 8,
                "{pattern:?} needs {per_row} units to find every match in a full row; the \
                 docs claim every built-in rule is far inside even the call sites' budget of \
                 {CALL_SITE_STEP_LIMIT}, let alone the default of {DEFAULT_STEP_LIMIT}"
            );
            // Under the default the rule completes over a row and a 1 MB scrollback.
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

    /// The heaviest program a 128 KiB `size_limit` admits finishes a full row under the
    /// default budget.
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

    /// Ordinary patterns give the same matches under any budget, without exhaustion.
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

    /// Clones share the sticky cut-short flag.
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

    /// `find_iter_with_limit` spends the budget it is handed, not the pattern's own, and
    /// reports truncation per haystack.
    #[test]
    fn find_iter_with_limit_spends_the_budget_it_is_handed() {
        let text = format!("{} needle", "x".repeat(10_000));
        let needed = budget_needed("needle", &text);
        let tight = RegexBuilder::new("needle")
            .step_limit(needed / 2)
            .build()
            .expect("compiles");
        let mut it = tight.find_iter_with_limit(&text, needed);
        assert_eq!(it.next().map(|m| m.range()), Some(10_001..10_007));
        assert_eq!(it.next(), None);
        assert!(!it.step_limit_exceeded(), "the larger budget finishes");

        let roomy = Regex::new("needle").expect("compiles");
        let mut it = roomy.find_iter_with_limit(&text, needed / 2);
        assert_eq!(it.next(), None);
        assert!(it.step_limit_exceeded(), "the smaller budget does not");
        assert!(roomy.step_limit_exceeded(), "and the pattern records it");

        // Each haystack starts from its own budget and its own verdict.
        let mut it = roomy.find_iter_with_limit("a needle", needed / 2);
        assert_eq!(it.next().map(|m| m.range()), Some(2..8));
        assert_eq!(it.next(), None);
        assert!(!it.step_limit_exceeded());
    }

    #[test]
    fn step_budget_and_span_const_helpers_invariants() {
        let err = StepLimitExceeded::new(100);
        assert_eq!(err.limit(), 100);
        assert!(err.within_budget(100));
        assert!(!err.within_budget(101));
        assert_eq!(err.remaining_budget(40), 60);
        assert_eq!(err.remaining_budget(200), 0);
        assert_eq!(default_size_limit(), 10 * (1 << 20));
        assert_eq!(default_step_limit(), 1 << 26);
        assert_eq!(clamp_size_limit(100, 1024), 1024);
        assert_eq!(clamp_size_limit(usize::MAX, 1024), default_size_limit());
        assert_eq!(clamp_step_limit(10, 1000), 1000);
        assert_eq!(clamp_step_limit(u64::MAX, 1000), default_step_limit());
        assert_eq!(debit_step_budget(50, 20), Some(30));
        assert_eq!(debit_step_budget(50, 60), None);
        assert!(valid_match_span(2, 8, 10));
        assert!(!valid_match_span(8, 2, 10));
        assert_eq!(match_span_len(2, 8), 6);
        assert!(disjoint_ordered_spans(0, 3, 3, 7));
        assert!(!disjoint_ordered_spans(0, 4, 3, 7));
        assert!(is_ascii_meta_byte(b'*'));
        assert!(!is_ascii_meta_byte(b'a'));
        let sp = encode_match_span_le(123, 456);
        assert_eq!(decode_match_span_le(sp), (123, 456));
    }
}

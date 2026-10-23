// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linear-time Thompson NFA / Pike VM regular expression engine.
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

/// Default ceiling on a compiled program, in bytes.
const DEFAULT_SIZE_LIMIT: usize = 10 * (1 << 20);

/// Default ceiling on the work one search may do; see [`RegexBuilder::step_limit`].
const DEFAULT_STEP_LIMIT: u64 = 1 << 26;

/// A pattern that would not compile.
#[derive(Clone, Debug, PartialEq, Eq)]
/// Pattern compilation error.
pub enum Error {
    /// Invalid or unsupported syntax with a caret-annotated diagnostic.
    Syntax(String),
    /// Compiled NFA exceeded [`RegexBuilder::size_limit`].
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Error returned when a search exhausts its step budget.
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
#[derive(Clone)]
/// A compiled regular expression with a shared, cheaply cloneable program.
pub struct Regex {
    prog: Arc<Program>,
    pattern: Arc<str>,
    /// Work ceiling for one search.
    step_limit: u64,
    /// Sticky flag set the first time any search on this shared program is cut short.
    cut_short: Arc<AtomicBool>,
}

impl Regex {
    /// Compile `pattern` with default limits.
    /// # Errors
    /// Returns [`Error::Syntax`] or [`Error::CompiledTooBig`] if compilation fails.
    pub fn new(pattern: &str) -> Result<Self, Error> {
        RegexBuilder::new(pattern).build()
    }

    /// The pattern this was compiled from.
    #[inline]
    #[must_use]
    /// Original pattern string.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Does `text` contain a match anywhere?
    #[inline]
    #[must_use]
    /// Return `true` if `text` matches anywhere.
    pub fn is_match(&self, text: &str) -> bool {
        self.try_is_match(text).unwrap_or(false)
    }

    /// [`is_match`](Self::is_match), reporting exhaustion instead of hiding it.
    ///
    /// # Errors
    /// [`StepLimitExceeded`] if the search ran out of budget before it could
    /// decide.
    #[inline]
    /// Like [`is_match`](Self::is_match), but reports step-limit exhaustion.
    /// # Errors
    /// Returns [`StepLimitExceeded`] if the search exhausts [`step_limit`](Self::step_limit).
    pub fn try_is_match(&self, text: &str) -> Result<bool, StepLimitExceeded> {
        let mut budget = self.step_limit;
        Ok(self.search(text, 0, true, &mut budget)?.is_some())
    }

    /// The leftmost-first match in `text`, if any.
    #[inline]
    #[must_use]
    /// Leftmost-first match in `text`, or `None` on no match or step-limit exhaustion.
    pub fn find<'t>(&self, text: &'t str) -> Option<Match<'t>> {
        self.try_find(text).unwrap_or(None)
    }

    /// [`find`](Self::find), reporting exhaustion instead of hiding it.
    ///
    /// # Errors
    /// [`StepLimitExceeded`] if the search ran out of budget before it could
    /// decide; no partial span is returned.
    #[inline]
    /// Like [`find`](Self::find), but reports step-limit exhaustion.
    /// # Errors
    /// Returns [`StepLimitExceeded`] if the search exhausts [`step_limit`](Self::step_limit).
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
    /// Per-search work ceiling ([`RegexBuilder::step_limit`]).
    pub const fn step_limit(&self) -> u64 {
        self.step_limit
    }

    /// Has any search on this pattern been cut short by the step budget?
    #[inline]
    #[must_use]
    /// Whether any search on this pattern (or its clones) has exceeded the step budget.
    pub fn step_limit_exceeded(&self) -> bool {
        self.cut_short.load(Ordering::Relaxed)
    }

    /// Every non-overlapping match in `text`, left to right.
    #[inline]
    #[must_use]
    /// Iterator over non-overlapping leftmost-first matches in `text`.
    pub const fn find_iter<'r, 't>(&'r self, text: &'t str) -> Matches<'r, 't> {
        self.find_iter_with_limit(text, self.step_limit)
    }

    /// [`find_iter`](Self::find_iter), held to `step_limit` units instead of the pattern's
    /// own [`RegexBuilder::step_limit`].
    #[inline]
    #[must_use]
    /// Like [`find_iter`](Self::find_iter), using `step_limit` instead of the pattern's budget.
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

    /// Leftmost-first match starting at or after `start`, charged against `budget`.
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
            .unwrap_or_else(|| {
                pikevm::search(&self.prog, &mut Cache::new(), text, start, earliest, budget)
            });
        if outcome.is_err() {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// A single match as a byte slice and range into the haystack.
pub struct Match<'t> {
    text: &'t str,
    start: usize,
    end: usize,
}

impl<'t> Match<'t> {
    /// Byte offset of the first code point of the match.
    #[inline]
    #[must_use]
    /// Inclusive start byte offset of the match.
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Byte offset just past the last code point of the match.
    #[inline]
    #[must_use]
    /// Exclusive end byte offset of the match.
    pub const fn end(&self) -> usize {
        self.end
    }

    /// The matched text.
    #[inline]
    #[must_use]
    /// Matched substring.
    pub fn as_str(&self) -> &'t str {
        self.text.get(self.start..self.end).unwrap_or_default()
    }

    /// The match as a byte range.
    #[inline]
    #[must_use]
    /// Match byte range `start..end`.
    pub const fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// Length of the match in bytes.
    #[inline]
    #[must_use]
    /// Length of the match in bytes.
    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    /// Is this a zero-width match?
    #[inline]
    #[must_use]
    /// Whether the match is zero-width.
    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Iterator over the non-overlapping matches in a haystack. See
/// [`Regex::find_iter`].
#[derive(Debug)]
/// Iterator over non-overlapping matches in a haystack ([`Regex::find_iter`]).
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
    #[inline]
    #[must_use]
    /// Whether iteration stopped early because a search exhausted the step budget.
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
                    self.cut_short = true;
                    self.last_end = self.text.len().saturating_add(1);
                    return None;
                }
            };
            if start == end {
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
/// Builder for compiling a [`Regex`] with custom limits.
pub struct RegexBuilder {
    pattern: String,
    size_limit: usize,
    step_limit: u64,
    flags: Flags,
}

impl RegexBuilder {
    /// Start building a `Regex` from `pattern`.
    #[must_use]
    /// Create a builder for `pattern`.
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
            size_limit: DEFAULT_SIZE_LIMIT,
            step_limit: DEFAULT_STEP_LIMIT,
            flags: Flags::default(),
        }
    }

    /// Ceiling, in bytes, on the compiled program.
    #[inline]
    #[must_use]
    /// Set the byte size ceiling on the compiled NFA program.
    pub const fn size_limit(mut self, bytes: usize) -> Self {
        self.size_limit = bytes;
        self
    }

    /// Ceiling, in bytes, on a lazy-DFA cache: accepted for source compatibility and
    /// ignored.
    #[inline]
    #[must_use]
    /// Accepted for source compatibility with `regex`; ignored by the Pike VM.
    pub const fn dfa_size_limit(self, _bytes: usize) -> Self {
        self
    }

    /// Ceiling on the work one search may do, in units of `positions × live threads`.
    #[inline]
    #[must_use]
    /// Set the per-search work ceiling in `positions * live_threads` units.
    pub const fn step_limit(mut self, steps: u64) -> Self {
        self.step_limit = steps;
        self
    }

    /// Read back the value passed to [`step_limit`](Self::step_limit).
    #[inline]
    #[must_use]
    #[cfg(test)]
    /// Return the configured [`step_limit`](Self::step_limit).
    pub const fn get_step_limit(&self) -> u64 {
        self.step_limit
    }

    /// Parse and compile the pattern.
    /// # Errors
    /// Returns [`Error::Syntax`] or [`Error::CompiledTooBig`] if compilation fails.
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
/// Escape all regex metacharacters in `pattern` so it matches literally.
pub fn escape(pattern: &str) -> String {
    /// The characters this engine reads as syntax.
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
    /// Return the step budget (in work units) that was exceeded.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Return `true` iff `used <= self.limit`.
    #[inline]
    #[must_use]
    /// Return `true` if and only if `used <= self.limit`.
    pub const fn within_budget(self, used: u64) -> bool {
        used <= self.limit
    }

    /// Remaining step budget after spending `used` units, saturating at `0`.
    #[inline]
    #[must_use]
    /// Remaining step budget after spending `used` units, saturating at `0`.
    pub const fn remaining_budget(self, used: u64) -> u64 {
        self.limit.saturating_sub(used)
    }
}

/// Return the default compiled program size ceiling (`10 MiB`).
#[inline]
#[must_use]
/// Return the default compiled program size ceiling (`10 MiB`).
pub const fn default_size_limit() -> usize {
    DEFAULT_SIZE_LIMIT
}

/// Return the default per-search step budget (`2^26` work units).
#[inline]
#[must_use]
/// Return the default per-search step budget (`2^26` work units).
pub const fn default_step_limit() -> u64 {
    DEFAULT_STEP_LIMIT
}

/// Clamp a requested program `size_limit` into `[min_bytes, DEFAULT_SIZE_LIMIT]`.
#[inline]
#[must_use]
/// Clamp a requested program `size_limit` into `[min_bytes, DEFAULT_SIZE_LIMIT]`.
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
/// Clamp a requested search `step_limit` into `[min_steps, DEFAULT_STEP_LIMIT]`.
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
/// Debit `cost` work units from `remaining`, returning `None` when `cost > remaining`.
pub const fn debit_step_budget(remaining: u64, cost: u64) -> Option<u64> {
    remaining.checked_sub(cost)
}

/// Return `true` iff `[start, end)` is a well-ordered span within `haystack_len`.
#[inline]
#[must_use]
/// Return `true` iff `[start, end)` is a well-ordered span within `haystack_len`.
pub const fn valid_match_span(start: usize, end: usize, haystack_len: usize) -> bool {
    start <= end && end <= haystack_len
}

/// Length of a validated match span `[start, end)`, or `0` if inverted.
#[inline]
#[must_use]
/// Length of a validated match span `[start, end)`, or `0` if inverted.
pub const fn match_span_len(start: usize, end: usize) -> usize {
    end.saturating_sub(start)
}

/// Return `true` iff two match spans `[a_start, a_end)` and `[b_start, b_end)`
/// are non-overlapping and ordered (`a_start <= a_end <= b_start <= b_end`).
#[inline]
#[must_use]
/// Return `true` iff `[a_start, a_end)` and `[b_start, b_end)` are non-overlapping and ordered.
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
/// Whether ASCII byte `b` is a regex syntax metacharacter escaped by [`escape`].
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
/// Encode a `(start, end)` byte-offset match span into 16 little-endian bytes.
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
/// Decode 16 little-endian bytes into a `(start, end)` byte-offset match span.
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

    fn one_mib_gate(pattern: &str) -> Regex {
        RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
            .expect("the amplifiers compile under a 1 MiB ceiling — that is the problem")
    }

    const AMPLIFIERS: [&str; 2] = ["(?:x?){2000}z", "(?:x|x|x|x|x|x|x|x){400}z"];

    const CALL_SITE_STEP_LIMIT: u64 = 1 << 22;

    fn row() -> String {
        "x".repeat(4096)
    }
    fn scrollback() -> String {
        "x".repeat(3 * 1024 * 1024)
    }

    fn completes_within(pattern: &str, haystack: &str, limit: u64) -> bool {
        let re = RegexBuilder::new(pattern)
            .step_limit(limit)
            .build()
            .expect("compiles");
        let mut it = re.find_iter(haystack);
        while it.next().is_some() {}
        !it.step_limit_exceeded()
    }

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
            Some(2096..4097),
            "the match the starved search refused to guess at is a real one"
        );
        assert!(!fed.step_limit_exceeded());
    }

    #[test]
    fn find_iter_reports_truncation_and_fuses() {
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

    #[test]
    fn a_find_iter_is_held_to_one_budget_not_one_per_match() {
        let row = row();
        let n = row.len();
        let re = RegexBuilder::new(r"\w+=|\w")
            .size_limit(128 * 1024)
            .step_limit(CALL_SITE_STEP_LIMIT)
            .build()
            .expect("compiles");

        let one_search = budget_needed_for_one_search(r"\w+=|\w", &row);
        assert!(
            one_search.saturating_mul(16) < CALL_SITE_STEP_LIMIT,
            "one search over the row needs {one_search} units; the budget is {CALL_SITE_STEP_LIMIT}"
        );
        assert_eq!(
            re.try_find(&row).map(|m| m.map(|m| m.range())),
            Ok(Some(0..1))
        );

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

        let linear = RegexBuilder::new(r"\w")
            .size_limit(128 * 1024)
            .step_limit(CALL_SITE_STEP_LIMIT)
            .build()
            .expect("compiles");
        let mut it = linear.find_iter(&row);
        assert_eq!(it.by_ref().count(), n);
        assert!(!it.step_limit_exceeded());
    }

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

    #[test]
    fn every_builtin_selection_rule_stays_far_inside_the_default() {
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
                "first.last@example.test and someone.else@example.co.uk ",
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

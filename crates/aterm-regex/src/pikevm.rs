// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The simulation: every live NFA state, advanced together one code point at a time.
//!
//! A backtracker explores one path at a time and can be made to explore exponentially
//! many (`(a|a)*b`). A Pike VM keeps the whole set of live states and enters each at most
//! once per position, so work is bounded by `program size × input length`.
//!
//! ## The step budget
//!
//! Both factors of that product come from untrusted input, so [`search`] charges every
//! unit of work to a budget: one per input position, per byte the prefilter skips, per
//! instruction entered in an epsilon closure, and per live thread stepped. The budget is
//! taken by `&mut` and left holding what remains, because a `find_iter` runs one search
//! per match and they share it. When it runs out the search returns [`Exhausted`]
//! (surfaced as [`StepLimitExceeded`](crate::StepLimitExceeded)) and no span: a truncated
//! search has not established which match is leftmost-first. The check runs once per
//! position, so a search overshoots by at most one position's work and the inner loops
//! carry no check.
//!
//! ## Priority
//!
//! Matching is leftmost-first, not leftmost-longest: `a|ab` matches `a`, and `(?:a|ab)c`
//! still matches `abc`. The live states are an ordered list, and three rules keep the
//! order meaningful:
//!
//! * The epsilon closure is a depth-first walk that follows [`Inst::Split`]'s `a` branch
//!   to exhaustion before `b`, so states enter the list in preference order.
//! * The thread for "start matching here" is appended at the end of the list, below every
//!   thread from an earlier start. That makes the match leftmost.
//! * When a thread reaches [`Inst::Match`], the match is recorded and every lower-priority
//!   thread is discarded. Higher-priority threads survive and may overwrite it with a
//!   longer match, which is how a greedy `a+` keeps going.

use crate::compile::{Inst, Prefilter, Program};
use crate::parse::Assertion;
use crate::unicode;

/// One live NFA state, and the offset the match it belongs to began at.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Thread {
    pub(crate) pc: usize,
    pub(crate) start: usize,
}

/// An ordered set of live states, deduplicated by program counter.
///
/// `visited` is a generation-stamped array rather than a cleared one, so
/// starting a new position is O(1) instead of O(program). Only consuming
/// instructions and [`Inst::Match`] reach `dense`; the epsilon instructions are
/// marked visited and dropped, which keeps the stepped list as short as the
/// automaton's real branching factor.
#[derive(Default)]
struct ThreadList {
    dense: Vec<Thread>,
    visited: Vec<u32>,
    generation: u32,
}

impl ThreadList {
    #[inline]
    fn resize(&mut self, n: usize) {
        if self.visited.len() != n {
            self.visited.clear();
            self.visited.resize(n, 0);
            self.generation = 0;
        }
        self.begin();
    }

    /// Start a fresh position: everything marked so far becomes unmarked.
    #[inline]
    fn begin(&mut self) {
        self.dense.clear();
        match self.generation.checked_add(1) {
            Some(g) => self.generation = g,
            None => {
                self.visited.fill(0);
                self.generation = 1;
            }
        }
    }

    /// Mark `pc` visited, reporting whether it already was.
    #[inline]
    fn mark(&mut self, pc: usize) -> bool {
        match self.visited.get_mut(pc) {
            Some(slot) if *slot == self.generation => true,
            Some(slot) => {
                *slot = self.generation;
                false
            }
            None => true,
        }
    }
}

/// Reusable scratch space for the simulation, sized to the program on entry.
#[derive(Default)]
pub(crate) struct Cache {
    clist: ThreadList,
    nlist: ThreadList,
    stack: Vec<usize>,
}

impl Cache {
    #[inline]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// Find the leftmost-first match at or after `start`.
///
/// `text` is always the whole haystack: assertions see the code points that really
/// surround a position, so `\ba` finds one match in `"aa"`, as `regex` does when it
/// resumes a `find_iter`.
///
/// With `earliest`, return as soon as any thread reaches [`Inst::Match`]; the span is then
/// not necessarily leftmost-first, so this is only for `is_match`.
///
/// `budget` is the work this search may still charge (see the module docs) and is left
/// holding what remains, so the searches of one `find_iter` share it. Exhausting it
/// returns `Err(Exhausted)`, never a span.
#[inline]
pub(crate) fn search(
    prog: &Program,
    cache: &mut Cache,
    text: &str,
    start: usize,
    earliest: bool,
    budget: &mut u64,
) -> Result<Option<(usize, usize)>, Exhausted> {
    let mut spent = 0;
    let outcome = simulate(prog, cache, text, start, earliest, *budget, &mut spent);
    *budget = budget.saturating_sub(spent);
    outcome
}

/// A search that ran out of budget before it could answer. The public face of
/// this is [`StepLimitExceeded`](crate::StepLimitExceeded), which carries the
/// configured limit; a search only knows what was left of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Exhausted;

/// [`search`]'s simulation, charging every unit of work to `steps` and giving up once more
/// than `step_limit` are charged. Saturating, so the count never wraps under the limit.
fn simulate(
    prog: &Program,
    cache: &mut Cache,
    text: &str,
    start: usize,
    earliest: bool,
    step_limit: u64,
    steps: &mut u64,
) -> Result<Option<(usize, usize)>, Exhausted> {
    let Cache {
        clist,
        nlist,
        stack,
    } = cache;
    clist.resize(prog.insts.len());
    nlist.resize(prog.insts.len());

    let mut matched: Option<(usize, usize)> = None;
    // `\A` holds only at offset 0, so a start-anchored program searched from a later
    // offset (as `find_iter` does after a match at 0) has no match.
    if prog.start_anchored && start > 0 {
        return Ok(None);
    }
    let mut pos = start;
    loop {
        // One unit for arriving at this position at all, so a haystack that
        // keeps no thread alive is still charged for being walked.
        *steps = steps.saturating_add(1);
        if matched.is_none() {
            if prog.start_anchored {
                // An anchored program starts threads only at 0; once the list empties
                // nothing can revive it.
                if pos > 0 && clist.dense.is_empty() {
                    return Ok(None);
                }
            } else if clist.dense.is_empty()
                && let Some(first) = prog.first.as_ref()
            {
                let skipped = skip_to_candidate(prog, first, text, pos);
                if skipped != pos {
                    // One unit per skipped byte: a 3 MB haystack of skipped bytes is real
                    // work.
                    *steps = steps.saturating_add(u64::try_from(skipped - pos).unwrap_or(u64::MAX));
                    pos = skipped;
                    // The visited marks describe the position just left: a failing
                    // assertion (as in `ab\Bc`) marks instructions without adding a thread,
                    // and a stale mark would drop the new start thread. Nothing is live, so
                    // retiring the generation is free.
                    clist.begin();
                }
            }
            *steps = steps.saturating_add(add(clist, stack, prog, prog.start, pos, pos, text));
        }
        let ch = text.get(pos..).and_then(|rest| rest.chars().next());
        nlist.begin();
        let mut i = 0;
        while let Some(&t) = clist.dense.get(i) {
            i += 1;
            // One unit per live thread stepped.
            *steps = steps.saturating_add(1);
            match prog.insts.get(t.pc) {
                Some(&Inst::Char { c, next }) => {
                    if Some(c) == ch {
                        let at = pos + c.len_utf8();
                        *steps =
                            steps.saturating_add(add(nlist, stack, prog, next, t.start, at, text));
                    }
                }
                Some(&Inst::Class { class, next }) => {
                    if let Some(c) = ch
                        && class_matches(prog, class, c)
                    {
                        let at = pos + c.len_utf8();
                        *steps =
                            steps.saturating_add(add(nlist, stack, prog, next, t.start, at, text));
                    }
                }
                Some(&Inst::Match) => {
                    matched = Some((t.start, pos));
                    // Cut: every thread after this one is lower priority, and a
                    // lower-priority thread may never beat a match in hand.
                    break;
                }
                _ => {}
            }
        }
        if matched.is_some() && earliest {
            return Ok(matched);
        }
        core::mem::swap(clist, nlist);
        // Checked once per position, after it is paid for: a search that has spent more
        // than `step_limit` without answering is refused, even with a match in hand, since
        // the live threads could still extend it. Only `earliest`, which returned above,
        // may answer past the budget.
        if *steps > step_limit {
            return Err(Exhausted);
        }
        let Some(c) = ch else { break };
        pos += c.len_utf8();
        if clist.dense.is_empty() && matched.is_some() {
            break;
        }
    }
    Ok(matched)
}

/// Does code point `c` belong to `prog.classes[class]`?
///
/// Below U+0080 the answer is one bit of the class's ASCII bitmap; above it, a binary
/// search of the set's ranges, then its perl classes. Terminal rows are mostly ASCII, and
/// the costliest patterns are all classes: `(?i)ERROR`, the find bar's default, compiles
/// to five `Inst::Class`.
#[inline]
fn class_matches(prog: &Program, class: usize, c: char) -> bool {
    let code = c as u32;
    if code < 128 {
        return prog
            .class_ascii
            .get(class)
            .is_some_and(|bits| bits >> code & 1 != 0);
    }
    prog.classes.get(class).is_some_and(|set| set.matches(c))
}

/// Advance past bytes that cannot begin a match.
///
/// Sound only because the caller checks the thread list is empty: then only a fresh start
/// thread could match, and [`Prefilter`] is a superset of what it survives its first step
/// on. The scan steps by byte; continuation bytes are never marked, so it may pass through
/// a multi-byte code point but stops only on a boundary.
#[inline]
fn skip_to_candidate(prog: &Program, first: &Prefilter, text: &str, mut pos: usize) -> usize {
    let bytes = text.as_bytes();
    while let Some(&b) = bytes.get(pos) {
        if first.lead_bytes[b as usize] {
            // When this byte is ASCII, test the second byte before decoding: an ASCII
            // lead byte is its whole code point, so the next one starts at `pos + 1`, and
            // a decode plus the walk over `pcs` costs far more than a lookup.
            if b < 0x80
                && let Some(second) = first.second_bytes.as_ref()
                && let Some(&next_byte) = bytes.get(pos + 1)
                && !second[next_byte as usize]
            {
                // Resume past this code point, as below.
                pos += 1;
                continue;
            }
            // A marked byte only means "some candidate might start here". Confirm
            // against the instructions themselves before handing back a position.
            let Some(c) = text.get(pos..).and_then(|rest| rest.chars().next()) else {
                break;
            };
            let viable = first.pcs.iter().any(|&pc| match prog.insts.get(pc) {
                Some(&Inst::Char { c: want, .. }) => want == c,
                Some(&Inst::Class { class, .. }) => class_matches(prog, class, c),
                _ => true,
            });
            if viable {
                // One more byte of evidence, when the pattern has a second code point.
                if let Some(second) = first.second_bytes.as_ref() {
                    let after = pos + c.len_utf8();
                    // No next byte: the simulation decides. The filter excludes only
                    // on positive evidence.
                    if let Some(&next_byte) = bytes.get(after)
                        && !second[next_byte as usize]
                    {
                        // Nothing can start at `pos`. Resume at the next code point,
                        // not after the second byte, which may itself begin a match.
                        pos = after;
                        continue;
                    }
                }
                break;
            }
            pos += c.len_utf8();
            continue;
        }
        pos += 1;
    }
    pos.min(text.len())
}

/// Add `pc` and its epsilon closure to `list`, in preference order.
///
/// An explicit-stack DFS, so nesting depth costs heap, not stack. Pushing `b` before `a`
/// enters `a`'s whole subtree first. `mark` deduplicates and terminates the walk, so an
/// epsilon cycle such as `()*`'s is entered once.
///
/// Returns the number of instructions entered, for the step budget: that is where an
/// amplifier such as `(?:x?){2000}z` spends itself, ~4,000 at every position.
#[inline]
fn add(
    list: &mut ThreadList,
    stack: &mut Vec<usize>,
    prog: &Program,
    pc: usize,
    thread_start: usize,
    at: usize,
    text: &str,
) -> u64 {
    let mut entered: u64 = 0;
    stack.push(pc);
    while let Some(pc) = stack.pop() {
        entered = entered.saturating_add(1);
        if list.mark(pc) {
            continue;
        }
        match prog.insts.get(pc) {
            Some(&Inst::Jump { next }) => stack.push(next),
            Some(&Inst::Split { a, b }) => {
                stack.push(b);
                stack.push(a);
            }
            Some(&Inst::Assert { kind, next }) => {
                if satisfied(kind, text, at) {
                    stack.push(next);
                }
            }
            Some(&(Inst::Char { .. } | Inst::Class { .. } | Inst::Match)) => {
                list.dense.push(Thread {
                    pc,
                    start: thread_start,
                });
            }
            None => {}
        }
    }
    entered
}

/// Evaluate a zero-width assertion at byte offset `at` of `text`.
#[inline]
fn satisfied(kind: Assertion, text: &str, at: usize) -> bool {
    let before = || text.get(..at).and_then(|s| s.chars().next_back());
    let after = || text.get(at..).and_then(|s| s.chars().next());
    let word_before = || before().is_some_and(unicode::is_word);
    let word_after = || after().is_some_and(unicode::is_word);
    match kind {
        Assertion::StartText => at == 0,
        Assertion::EndText => at == text.len(),
        Assertion::StartLine => at == 0 || before() == Some('\n'),
        Assertion::EndLine => at == text.len() || after() == Some('\n'),
        Assertion::WordBoundary => word_before() != word_after(),
        Assertion::NotWordBoundary => word_before() == word_after(),
        Assertion::WordStart => !word_before() && word_after(),
        Assertion::WordEnd => word_before() && !word_after(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile;
    use crate::parse::{Flags, parse};

    /// Every assertion, at every position of a haystack that exercises it.
    #[test]
    fn assertions_read_the_surrounding_code_points() {
        let text = "ab cd";
        let positions = |kind: Assertion| -> Vec<usize> {
            (0..=text.len())
                .filter(|&at| satisfied(kind, text, at))
                .collect()
        };
        assert_eq!(positions(Assertion::StartText), vec![0]);
        assert_eq!(positions(Assertion::EndText), vec![5]);
        assert_eq!(positions(Assertion::WordBoundary), vec![0, 2, 3, 5]);
        assert_eq!(positions(Assertion::NotWordBoundary), vec![1, 4]);
        assert_eq!(positions(Assertion::WordStart), vec![0, 3]);
        assert_eq!(positions(Assertion::WordEnd), vec![2, 5]);

        let lines = "a\nb";
        assert!(satisfied(Assertion::StartLine, lines, 0));
        assert!(satisfied(Assertion::StartLine, lines, 2));
        assert!(!satisfied(Assertion::StartLine, lines, 1));
        assert!(satisfied(Assertion::EndLine, lines, 1));
        assert!(satisfied(Assertion::EndLine, lines, 3));
        assert!(!satisfied(Assertion::EndLine, lines, 2));

        // A combining mark is a word code point, so there is no boundary inside
        // `a` + U+0301.
        let marked = "a\u{301}b c";
        let bounds: Vec<usize> = (0..=marked.len())
            .filter(|&at| {
                marked.is_char_boundary(at) && satisfied(Assertion::WordBoundary, marked, at)
            })
            .collect();
        assert_eq!(bounds, vec![0, 4, 5, 6]);
    }

    /// One cache, reused across programs of different sizes, must not carry
    /// stale marks between runs.
    #[test]
    fn a_cache_survives_being_resized_between_programs() {
        let small =
            compile(&parse("a", Flags::default()).expect("parses"), usize::MAX).expect("compiles");
        let large = compile(
            &parse("(?:abc){20}", Flags::default()).expect("parses"),
            usize::MAX,
        )
        .expect("compiles");
        let mut cache = Cache::new();
        let big = "abc".repeat(20);
        for _ in 0..50 {
            assert_eq!(
                search(&small, &mut cache, "zza", 0, false, &mut { u64::MAX }),
                Ok(Some((2, 3)))
            );
            assert_eq!(
                search(&large, &mut cache, &big, 0, false, &mut { u64::MAX }),
                Ok(Some((0, 60)))
            );
            assert_eq!(
                search(&small, &mut cache, "", 0, false, &mut { u64::MAX }),
                Ok(None)
            );
        }
    }

    /// The budget is enforced, and an exhausted search returns no span.
    #[test]
    fn the_step_budget_stops_the_simulation() {
        let ast = parse("a+b", Flags::default()).expect("parses");
        let prog = compile(&ast, usize::MAX).expect("compiles");
        let text = "a".repeat(10_000);
        let mut cache = Cache::new();

        assert_eq!(
            search(&prog, &mut cache, &text, 0, false, &mut { u64::MAX }),
            Ok(None),
            "with budget to spare the answer is a completed no-match"
        );
        assert_eq!(
            search(&prog, &mut cache, &text, 0, false, &mut 100),
            Err(Exhausted),
            "and a hundred units cannot cross ten thousand positions"
        );
        // A starved search refuses even over a haystack that matches.
        let matching = format!("{}b", "a".repeat(10_000));
        assert_eq!(
            search(&prog, &mut cache, &matching, 0, false, &mut 100),
            Err(Exhausted)
        );
        assert_eq!(
            search(&prog, &mut cache, &matching, 0, false, &mut { u64::MAX }),
            Ok(Some((0, 10_001)))
        );
    }

    /// Bytes the prefilter skips are charged, so a haystack no position can start a match
    /// on still costs its length.
    #[test]
    fn the_prefilter_skip_is_charged_by_the_byte() {
        let ast = parse("needle", Flags::default()).expect("parses");
        let prog = compile(&ast, usize::MAX).expect("compiles");
        assert!(prog.first.is_some(), "this pattern arms the prefilter");
        let text = "x".repeat(100_000);
        let mut cache = Cache::new();
        assert_eq!(
            search(&prog, &mut cache, &text, 0, false, &mut 1_000),
            Err(Exhausted),
            "100,000 skipped bytes are 100,000 units of real work"
        );
        assert_eq!(
            search(&prog, &mut cache, &text, 0, false, &mut { u64::MAX }),
            Ok(None)
        );
    }

    /// The generation stamp wraps rather than growing without bound, and a wrap
    /// must not make every state look visited.
    #[test]
    fn the_visited_generation_wraps_cleanly() {
        let mut list = ThreadList::default();
        list.resize(4);
        list.generation = u32::MAX;
        list.visited.fill(u32::MAX);
        list.begin();
        assert_eq!(list.generation, 1);
        assert!(
            !list.mark(0),
            "a wrapped generation must not report stale marks"
        );
        assert!(list.mark(0), "and must still deduplicate");
        assert!(!list.mark(3));
        assert!(
            list.mark(9),
            "an out-of-range pc is treated as already seen"
        );
    }
}

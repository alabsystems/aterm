// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The simulation: every NFA state that is alive, advanced together, one code
//! point at a time.
//!
//! A backtracker explores one path at a time and can be made to explore
//! exponentially many of them — the reason `(a|a)*b` is a denial-of-service in
//! most languages. A Pike VM instead keeps the whole *set* of live states and
//! steps it forward once per input code point. Each state is entered at most
//! once per position, so the work is bounded by `program size × input length`,
//! full stop. That linear-time guarantee is what the `regex` crate gives and
//! what this replacement has to keep.
//!
//! ## The step budget, and why linear is not the same as bounded
//!
//! `program size × input length` is a *product*, and both factors are set by
//! untrusted input: the pattern picks the first, the haystack picks the second.
//! Linear in the haystack still means 30 seconds when the constant is sixteen
//! thousand instructions (see the crate docs for the measured pair). So
//! [`search`] takes a `step_limit` and charges every unit of work against it —
//! one per input position visited, one per byte the prefilter skips, one per
//! instruction entered in the epsilon closure, and one per live thread stepped.
//! That sum *is* the `positions × live threads` product the paragraph above
//! bounds only in theory, counted for real.
//!
//! When the budget runs out the search returns [`StepLimitExceeded`] and no
//! span at all. It deliberately does not return the best match found so far:
//! a truncated search has not established that an earlier or longer match does
//! not exist, so any span it could hand back might be the wrong one, and a
//! wrong answer is worse than a refusal. The check runs once per input position
//! rather than once per work unit, so the overshoot is bounded by one
//! position's worth of work (at most the program size) and the inner loops stay
//! branch-free.
//!
//! ## Priority, and why the order of the thread list is the whole algorithm
//!
//! Matching is *leftmost-first*, not leftmost-longest: `a|ab` matches just `a`,
//! and `(?:a|ab)c` still matches `abc`, because alternation is a preference and
//! not a choice. The set of live states is therefore kept as an ordered list,
//! and three rules keep that order meaningful:
//!
//! * The epsilon closure is a depth-first walk that follows [`Inst::Split`]'s
//!   `a` branch to exhaustion before touching `b`, so states enter the list in
//!   preference order.
//! * A new thread for "start matching here" is appended at the *end* of the
//!   list, below every thread from an earlier starting position. That is what
//!   makes the match leftmost.
//! * When a thread reaches [`Inst::Match`], the match is recorded and every
//!   lower-priority thread in the current list is discarded. Higher-priority
//!   threads survive and may still overwrite it with a longer match — which is
//!   exactly how a greedy `a+` keeps going.

use crate::StepLimitExceeded;
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
    fn resize(&mut self, n: usize) {
        if self.visited.len() != n {
            self.visited.clear();
            self.visited.resize(n, 0);
            self.generation = 0;
        }
        self.begin();
    }

    /// Start a fresh position: everything marked so far becomes unmarked.
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

/// Reusable scratch space for the simulation. Sized to the program on entry and
/// then reused, so a search over ten thousand rows allocates once.
#[derive(Default)]
pub(crate) struct Cache {
    clist: ThreadList,
    nlist: ThreadList,
    stack: Vec<usize>,
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// Find the leftmost-first match at or after `start`.
///
/// `text` is always the *whole* haystack, never a slice of it: `^`, `\b` and
/// friends are evaluated against the code points that really surround a
/// position, so `\ba` finds one match in `"aa"` and not two — which is what the
/// `regex` crate does when it resumes a `find_iter`.
///
/// With `earliest`, the search returns as soon as any thread reaches
/// [`Inst::Match`]. The span it reports is then not necessarily the leftmost
/// -first one, so this is only for `is_match`, which asks a yes/no question.
///
/// `step_limit` bounds the work — see the module docs. Exhausting it returns
/// `Err(StepLimitExceeded)` and never a span: a search that was cut short has
/// not established which match is leftmost-first, or whether one exists at all.
pub(crate) fn search(
    prog: &Program,
    cache: &mut Cache,
    text: &str,
    start: usize,
    earliest: bool,
    step_limit: u64,
) -> Result<Option<(usize, usize)>, StepLimitExceeded> {
    let Cache { clist, nlist, stack } = cache;
    clist.resize(prog.insts.len());
    nlist.resize(prog.insts.len());

    // Work charged so far. Saturating everywhere: a `u64` of work units is
    // unreachable in this universe, and saturation keeps the accounting from
    // wrapping back under the limit if one ever were.
    let mut steps: u64 = 0;
    let mut matched: Option<(usize, usize)> = None;
    let mut pos = start;
    loop {
        // One unit for arriving at this position at all, so a haystack that
        // keeps no thread alive is still charged for being walked.
        steps = steps.saturating_add(1);
        if matched.is_none() {
            if clist.dense.is_empty()
                && let Some(first) = prog.first.as_ref()
            {
                let skipped = skip_to_candidate(prog, first, text, pos);
                if skipped != pos {
                    // The prefilter's own walk is one array lookup per byte, so
                    // it is charged by the byte: cheap per unit, but not free,
                    // and a 3 MB haystack of nothing but skipped bytes is real
                    // work the budget has to see.
                    steps = steps
                        .saturating_add(u64::try_from(skipped - pos).unwrap_or(u64::MAX));
                    pos = skipped;
                    // The list's visited marks describe the closure at the
                    // position we just left. Empty of threads it may be, but a
                    // stale mark is not harmless: the assertions in a pattern
                    // like `ab\Bc` mark instructions without ever putting a
                    // thread in `dense`, and the new start thread would then be
                    // deduplicated against a position it never visited and
                    // silently dropped. Retiring the generation costs nothing
                    // here — there is nothing live to retire.
                    clist.begin();
                }
            }
            steps = steps
                .saturating_add(add(clist, stack, prog, prog.start, pos, pos, text));
        }
        let ch = text.get(pos..).and_then(|rest| rest.chars().next());
        nlist.begin();
        let mut i = 0;
        while let Some(&t) = clist.dense.get(i) {
            i += 1;
            // One unit per live thread stepped. Summed over the loop this is
            // the `positions × live threads` product the budget exists to cap.
            steps = steps.saturating_add(1);
            match prog.insts.get(t.pc) {
                Some(&Inst::Char { c, next }) => {
                    if Some(c) == ch {
                        let at = pos + c.len_utf8();
                        steps = steps
                            .saturating_add(add(nlist, stack, prog, next, t.start, at, text));
                    }
                }
                Some(&Inst::Class { class, next }) => {
                    if let Some(c) = ch
                        && prog.classes.get(class).is_some_and(|set| set.matches(c))
                    {
                        let at = pos + c.len_utf8();
                        steps = steps
                            .saturating_add(add(nlist, stack, prog, next, t.start, at, text));
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
        // Charged once per position, after the position is fully paid for, so
        // the rule is simply: a search that spends more than `step_limit`
        // without having answered yet is refused. A match already in hand is
        // not a licence to keep going — the threads still alive are the ones
        // that could extend it, and running them past the budget is exactly the
        // cost being refused. The one search that outruns the budget and still
        // answers is the `earliest` one above, which returned already: a
        // positive answer is complete evidence and needs no more work.
        if steps > step_limit {
            return Err(StepLimitExceeded::new(step_limit));
        }
        let Some(c) = ch else { break };
        pos += c.len_utf8();
        if clist.dense.is_empty() && matched.is_some() {
            break;
        }
    }
    Ok(matched)
}

/// Advance past bytes that cannot begin a match.
///
/// Sound only because the caller checks the thread list is empty first: with no
/// thread alive, the only thing that could match at a position is a fresh start
/// thread, and [`Prefilter`] is a superset of the code points such a thread
/// survives its first step on.
///
/// The scan is byte-oriented, and steps by one byte rather than one code point.
/// That is deliberate and safe: continuation bytes are never marked, so the walk
/// can pass *through* the middle of a multi-byte code point but can only ever
/// stop on a boundary. It is the difference between a decode plus a set of class
/// tests per code point and one array lookup per byte, over text that is
/// overwhelmingly going to be skipped.
fn skip_to_candidate(prog: &Program, first: &Prefilter, text: &str, mut pos: usize) -> usize {
    let bytes = text.as_bytes();
    while let Some(&b) = bytes.get(pos) {
        if first.lead_bytes[b as usize] {
            // A marked byte only means "some candidate might start here". Confirm
            // against the instructions themselves before handing back a position.
            let Some(c) = text.get(pos..).and_then(|rest| rest.chars().next()) else {
                break;
            };
            let viable = first.pcs.iter().any(|&pc| match prog.insts.get(pc) {
                Some(&Inst::Char { c: want, .. }) => want == c,
                Some(&Inst::Class { class, .. }) => {
                    prog.classes.get(class).is_some_and(|set| set.matches(c))
                }
                _ => true,
            });
            if viable {
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
/// The walk is an explicit-stack DFS, so a pattern of any nesting depth costs
/// heap and not stack. Pushing `b` before `a` means `a` is popped first and its
/// whole subtree is entered before `b` is looked at, which is the preference
/// order the match semantics need. `mark` both deduplicates and terminates the
/// walk: an epsilon cycle such as the one `()*` compiles to is entered once.
///
/// Returns the number of instructions *entered*, which the caller charges to
/// the step budget: that is where an amplifier such as `(?:x?){2000}z` really
/// spends itself — four thousand epsilon instructions walked at every single
/// position of the haystack. (Returned rather than taken as an eighth `&mut`
/// parameter, which would put this function over clippy's argument ceiling.)
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
                list.dense.push(Thread { pc, start: thread_start });
            }
            None => {}
        }
    }
    entered
}

/// Evaluate a zero-width assertion at byte offset `at` of `text`.
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

    /// A search with the budget wound out of the way, so the tests below see
    /// only the matching semantics they are about.
    fn find(pattern: &str, text: &str, start: usize) -> Option<(usize, usize)> {
        let ast = parse(pattern, Flags::default()).expect("parses");
        let prog = compile(&ast, usize::MAX).expect("compiles");
        search(&prog, &mut Cache::new(), text, start, false, u64::MAX).expect("budget is unbounded")
    }

    /// Every assertion, at every position of a haystack that exercises it.
    #[test]
    fn assertions_read_the_surrounding_code_points() {
        let text = "ab cd";
        let positions = |kind: Assertion| -> Vec<usize> {
            (0..=text.len()).filter(|&at| satisfied(kind, text, at)).collect()
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
            .filter(|&at| marked.is_char_boundary(at) && satisfied(Assertion::WordBoundary, marked, at))
            .collect();
        assert_eq!(bounds, vec![0, 4, 5, 6]);
    }

    /// Alternation is a preference, not a choice: the first branch that can
    /// match wins even when a later one would match more.
    #[test]
    fn matching_is_leftmost_first_not_leftmost_longest() {
        assert_eq!(find("a|ab", "xab", 0), Some((1, 2)));
        assert_eq!(find("ab|a", "xab", 0), Some((1, 3)));
        // But the preference never costs an overall match.
        assert_eq!(find("(?:a|ab)c", "abc", 0), Some((0, 3)));
    }

    /// Leftmost beats preference: a thread from an earlier start always wins,
    /// because the start thread is appended below every live one.
    #[test]
    fn an_earlier_start_always_wins() {
        assert_eq!(find("ab|b", "ab", 0), Some((0, 2)));
        assert_eq!(find("b|ab", "ab", 0), Some((0, 2)));
        assert_eq!(find("a*", "bbaa", 0), Some((0, 0)), "the empty match at 0 is leftmost");
    }

    /// `start` resumes the search without changing what the assertions see: the
    /// haystack is never sliced, so `^` still means offset zero.
    #[test]
    fn resuming_keeps_the_whole_haystack_in_view() {
        assert_eq!(find("^a", "aa", 0), Some((0, 1)));
        assert_eq!(find("^a", "aa", 1), None, "`^` is not the resume point");
        assert_eq!(find(r"\ba", "aa", 1), None, "nor is `\\b`");
        assert_eq!(find(r"\bc", "ab cd", 2), Some((3, 4)));
    }

    /// An epsilon cycle — the one `()*` compiles to — must be entered once and
    /// then left, not looped forever.
    #[test]
    fn epsilon_cycles_terminate() {
        assert_eq!(find("()*", "ab", 0), Some((0, 0)));
        assert_eq!(find("(?:a*)*b", "aaab", 0), Some((0, 4)));
        assert_eq!(find("(|a)*", "aa", 0), Some((0, 0)));
        assert_eq!(find("(a|)*", "aa", 0), Some((0, 2)));
    }

    /// `earliest` is only allowed to change *which* match is reported, never
    /// whether one exists.
    #[test]
    fn the_earliest_shortcut_preserves_existence() {
        let ast = parse(r"\d+|[a-c]+", Flags::default()).expect("parses");
        let prog = compile(&ast, usize::MAX).expect("compiles");
        for text in ["", "x", "abc", "x42", "42abc", "zzz"] {
            let full = search(&prog, &mut Cache::new(), text, 0, false, u64::MAX).expect("budget");
            let quick = search(&prog, &mut Cache::new(), text, 0, true, u64::MAX).expect("budget");
            assert_eq!(full.is_some(), quick.is_some(), "{text:?}");
        }
    }

    /// The prefilter may skip code points, but never a match.
    #[test]
    fn the_prefilter_never_skips_a_match() {
        let ast = parse("needle", Flags::default()).expect("parses");
        let prog = compile(&ast, usize::MAX).expect("compiles");
        assert!(prog.first.is_some(), "this pattern should arm the prefilter");
        let mut cache = Cache::new();
        for pad in 0..40usize {
            let text = format!("{}needle{}", "x".repeat(pad), "y".repeat(pad));
            assert_eq!(
                search(&prog, &mut cache, &text, 0, false, u64::MAX),
                Ok(Some((pad, pad + 6))),
                "pad {pad}"
            );
        }
    }

    /// Skipping ahead retires the visited marks that belong to the position it
    /// skipped from. Without that, a failing assertion mid-pattern marks
    /// instructions while leaving the thread list empty, and the start thread at
    /// the skipped-to position is deduplicated against a closure that never
    /// happened — the match simply vanishes.
    #[test]
    fn skipping_ahead_retires_the_marks_it_leaves_behind() {
        // `\-?\B\d`: the `-` at 2 is consumed, `\B` fails at 3 leaving marks and
        // no threads, and the prefilter then skips to the `0` at 8.
        assert_eq!(find(r"\-?\B\d", "\u{e9}-bx\u{4f60}0\u{4f60}", 0), Some((8, 9)));
        assert_eq!(find(r"ab\Bc", "abxabc", 0), Some((3, 6)));
        assert_eq!(find(r"a\bz|q", "ab qz", 0), Some((3, 4)));
        assert_eq!(find(r"x\B\d", "xy x1", 0), Some((3, 5)));
    }

    /// One cache, reused across programs of different sizes, must not carry
    /// stale marks between runs.
    #[test]
    fn a_cache_survives_being_resized_between_programs() {
        let small = compile(&parse("a", Flags::default()).expect("parses"), usize::MAX)
            .expect("compiles");
        let large = compile(&parse("(?:abc){20}", Flags::default()).expect("parses"), usize::MAX)
            .expect("compiles");
        let mut cache = Cache::new();
        let big = "abc".repeat(20);
        for _ in 0..50 {
            assert_eq!(search(&small, &mut cache, "zza", 0, false, u64::MAX), Ok(Some((2, 3))));
            assert_eq!(search(&large, &mut cache, &big, 0, false, u64::MAX), Ok(Some((0, 60))));
            assert_eq!(search(&small, &mut cache, "", 0, false, u64::MAX), Ok(None));
        }
    }

    /// The budget is charged and enforced, and an exhausted search hands back
    /// no span at all — the whole reason it is a `Result` and not an `Option`.
    #[test]
    fn the_step_budget_stops_the_simulation() {
        let ast = parse("a+b", Flags::default()).expect("parses");
        let prog = compile(&ast, usize::MAX).expect("compiles");
        let text = "a".repeat(10_000);
        let mut cache = Cache::new();

        assert_eq!(
            search(&prog, &mut cache, &text, 0, false, u64::MAX),
            Ok(None),
            "with budget to spare the answer is a completed no-match"
        );
        assert_eq!(
            search(&prog, &mut cache, &text, 0, false, 100),
            Err(StepLimitExceeded::new(100)),
            "and a hundred units cannot cross ten thousand positions"
        );
        // The refusal is about the work, not the outcome: the same starved
        // search over a haystack that *does* match still refuses rather than
        // reporting the span it was about to find.
        let matching = format!("{}b", "a".repeat(10_000));
        assert_eq!(
            search(&prog, &mut cache, &matching, 0, false, 100),
            Err(StepLimitExceeded::new(100))
        );
        assert_eq!(
            search(&prog, &mut cache, &matching, 0, false, u64::MAX),
            Ok(Some((0, 10_001)))
        );
    }

    /// Bytes the prefilter skips are charged too. Without that, the cheapest
    /// possible haystack — megabytes of text no position can start a match on —
    /// would be walked in a single unaccounted step, and the budget would stop
    /// describing the time the search actually takes.
    #[test]
    fn the_prefilter_skip_is_charged_by_the_byte() {
        let ast = parse("needle", Flags::default()).expect("parses");
        let prog = compile(&ast, usize::MAX).expect("compiles");
        assert!(prog.first.is_some(), "this pattern arms the prefilter");
        let text = "x".repeat(100_000);
        let mut cache = Cache::new();
        assert_eq!(
            search(&prog, &mut cache, &text, 0, false, 1_000),
            Err(StepLimitExceeded::new(1_000)),
            "100,000 skipped bytes are 100,000 units of real work"
        );
        assert_eq!(search(&prog, &mut cache, &text, 0, false, u64::MAX), Ok(None));
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
        assert!(!list.mark(0), "a wrapped generation must not report stale marks");
        assert!(list.mark(0), "and must still deduplicate");
        assert!(!list.mark(3));
        assert!(list.mark(9), "an out-of-range pc is treated as already seen");
    }
}

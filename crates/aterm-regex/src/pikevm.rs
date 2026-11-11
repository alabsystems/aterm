// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The simulation: every live NFA state, advanced together one code point at a time.

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

/// A search that ran out of budget before it could answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Exhausted;

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
    if prog.start_anchored && start > 0 {
        return Ok(None);
    }
    let mut pos = start;
    loop {
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
                    *steps = steps.saturating_add(u64::try_from(skipped - pos).unwrap_or(u64::MAX));
                    pos = skipped;
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
                    break;
                }
                _ => {}
            }
        }
        if matched.is_some() && earliest {
            return Ok(matched);
        }
        core::mem::swap(clist, nlist);
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
                pos += 1;
                continue;
            }
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
                    if let Some(&next_byte) = bytes.get(after)
                        && !second[next_byte as usize]
                    {
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
#[path = "pikevm_tests.rs"]
mod tests;
